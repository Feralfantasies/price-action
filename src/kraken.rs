//! Kraken public market data for crypto pairs: REST candles and the Spot
//! WebSocket **v2** feed.
//!
//! This module is **transport and decoding only** — no bar logic, no clocks,
//! no account state. Like [`crate::feed`] (Massive.com) it connects,
//! subscribes to one pair, re-establishes on drops with the same capped
//! exponential backoff ([`crate::feed::backoff_for`]) and emits
//! [`crate::feed::RawEvent`]s, so a [`crate::live`] session consumes Kraken
//! windows through the very same shaper/bookkeeping code path. **No order
//! path exists anywhere**: data in only, paper accounting downstream.
//!
//! ## Wire facts (verified live against the public endpoints)
//!
//! - REST candles: `GET https://api.kraken.com/0/public/OHLC?pair=<ALTNAME>&interval=<MINUTES>`.
//!   The `interval` parameter is a **minute label** from the documented set
//!   (see [`validate_interval_secs`] — 10-minute candles are rejected by WS v2,
//!   720-minute by REST). At most ~721 rows come back; each row is
//!   `[ts(unix secs), open, high, low, close, vwap, volume, count]` with the
//!   money fields as strings — and the **last row is the still-forming
//!   candle**: trading it would price a partial window, so [`fetch_candles`]
//!   drops every trailing row whose end (`ts + interval`) has not passed by
//!   `now`. REST errors arrive as a 2xx envelope with a non-empty `error`
//!   array.
//! - REST pair tables: `GET /0/public/AssetPairs` (internal-key → entry
//!   carrying `altname`/`base`/`quote`) and `GET /0/public/Assets`
//!   (asset-code → entry with `altname`). One round of both (fetched once per
//!   resolution) feeds the pure [`resolve_from_tables`] matcher, which resolves
//!   every supported input form — display pair (`BTC/USD`), legacy wsname
//!   (`XBT/USD`), altname (`XBTUSD`) or internal key (`XXBTZUSD`) — into the
//!   two canonical names both endpoints need. Public endpoints: no secrets.
//! - Spot WebSocket v2: `wss://ws.kraken.com/v2`. Public channels — including
//!   OHLC — need **no authentication** (the authenticated endpoint is a
//!   different host, deliberately not used here). Subscribe with
//!   `{"method":"subscribe","params":{"channel":"ohlc","symbol":["BTC/USD"],"interval":1}}`;
//!   the reply is a `{"method":"subscribe", …, "success": true|false}` frame
//!   that on failure carries the server's own `error` string (observed live:
//!   *"Currency pair not supported XBT/USD"*,
//!   *"Currency pair not in ISO 4217-A3 format XBTUSD"*). That is why v2 gets
//!   a **modern display pair** derived from the asset altnames plus the small
//!   legacy-alias table (`XBT` → `BTC`, [`LEGACY_ASSET_NAMES`]): legacy
//!   wsnames, altnames and internal keys are all rejected by v2 itself. Data
//!   frames are `{"channel":"ohlc","type":"snapshot"|"update", …, "data":[…]}`
//!   where every data item carries both window edges as ISO-8601 UTC strings
//!   (`interval_begin`, `timestamp`) and re-emits the in-flight candle with
//!   fresh numbers — exactly the hold-until-next-starts cadence the
//!   [`crate::live`] shaper implements for Massive minute windows. On
//!   subscribe a snapshot of recent closed candles plus the open one arrives;
//!   then stream updates continue from there. The server also sends
//!   `{"channel":"heartbeat"}` roughly once per second and a `status` update
//!   on connect; both decode to no events, which is what keeps the silence
//!   timeout from firing on an idle pair.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use rustls::ClientConfig;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// REST host for the public `/0/public` endpoints.
pub const REST_HOST: &str = "api.kraken.com";
/// Spot WebSocket **v2** endpoint (public channels do not authenticate).
const WS_URL: &str = "wss://ws.kraken.com/v2";

/// Grace for a TCP connect alone, so a dead network fails fast (the budget
/// [`crate::notify`] uses as well).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Grace for the full request/response exchange over one connection. Candles
/// payloads are bounded (~721 rows) and `Connection: close` makes the response
/// end at EOF — anything slower than this is a hung peer, not a slow answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a connected socket may go silent before it is treated as dead and
/// reconnected. v2 heartbeats arrive ~1/s, so 90s of silence means the link is
/// gone — same value [`crate::feed`] uses, keeping session behaviour consistent
/// across vendors.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(90);
/// Grace for the subscription acknowledgement after the subscribe frame has
/// been written; a missed ack ends the attempt like any dropped connection.
const ACK_TIMEOUT: Duration = Duration::from_secs(15);

/// Candle intervals both Kraken endpoints accept, expressed in seconds.
///
/// The REST and v2 label sets coincide (verified live): all six are accepted;
/// 10-minute candles are rejected by v2 and 720-minute candles by REST.
pub const ALLOWED_INTERVALS_SECS: [u32; 6] = [60, 300, 900, 1_800, 3_600, 14_400];

/// Legacy Kraken asset name → the modern display name Spot WebSocket v2
/// recognizes. Only mismatches are listed (verified live: `XBT/USD` is
/// rejected with *"Currency pair not supported"* while `BTC/USD` streams
/// fine); every other asset resolves through its own altname, which v2 accepts
/// as-is (`ETH`, `USDT`, `USD`, …).
const LEGACY_ASSET_NAMES: [(&str, &str); 1] = [("XBT", "BTC")];

/// Canonical names for one Kraken market, as the two endpoints consume them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairResolution {
    /// REST pair form (the `AssetPairs` table's `altname`, e.g. `XBTUSD`) — what
    /// `/0/public/OHLC?pair=…` accepts for every market probed. The display
    /// form (`BTC/USD`) happens to work on that endpoint too, but the altname
    /// is the one that always does.
    pub altname: String,
    /// Spot WebSocket v2 display pair (e.g. `BTC/USD`) — what the subscribe
    /// frame wants. A live session also filters inbound events against it.
    pub ws_symbol: String,
}

/// Connection settings for one Kraken market: a [`PairResolution`] plus a
/// candle interval. Carries no secrets (public endpoints), so the derived
/// [`std::fmt::Debug`] is honest as-is.
#[derive(Debug, Clone)]
pub struct KrakenSettings {
    /// WebSocket v2 display pair (e.g. `BTC/USD`).
    pub ws_symbol: String,
    /// REST altname for the candles endpoint (e.g. `XBTUSD`).
    pub rest_pair: String,
    /// Candle interval in seconds; must pass [`validate_interval_secs`].
    pub interval_secs: u32,
}

impl KrakenSettings {
    /// Builds settings from a resolution and a candle interval in seconds.
    #[must_use]
    pub fn new(resolution: PairResolution, interval_secs: u32) -> Self {
        Self {
            ws_symbol: resolution.ws_symbol,
            rest_pair: resolution.altname,
            interval_secs,
        }
    }

    /// Cheap sanity checks before touching the network.
    ///
    /// # Errors
    ///
    /// [`crate::error::Error::MarketData`] when a part is empty/malformed or
    /// the interval is outside the documented allowed set.
    pub fn validate(&self) -> Result<(), crate::error::Error> {
        let (base, quote) = self.ws_symbol.split_once('/').ok_or_else(|| {
            crate::error::Error::MarketData(format!(
                "kraken ws symbol must be a `<BASE>/<QUOTE>` display pair: `{}`",
                self.ws_symbol
            ))
        })?;
        if base.trim().is_empty() || quote.trim().is_empty() {
            return Err(crate::error::Error::MarketData(format!(
                "kraken ws symbol `{}` has an empty side of the pair",
                self.ws_symbol
            )));
        }
        if self.ws_symbol.chars().any(char::is_whitespace) {
            return Err(crate::error::Error::MarketData(format!(
                "kraken ws symbol must not contain whitespace: `{}`",
                self.ws_symbol
            )));
        }
        if self.rest_pair.is_empty() || !self.rest_pair.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(crate::error::Error::MarketData(
                "kraken REST pair must be a non-empty alphabetic altname".to_string(),
            ));
        }
        validate_interval_secs(self.interval_secs)
    }
}

/// Validates a candle interval against the set both Kraken endpoints accept.
/// Shared by configuration-time and runtime checks so one rule governs both.
///
/// # Errors
///
/// [`crate::error::Error::MarketData`] naming the value and the allowed set.
pub fn validate_interval_secs(secs: u32) -> Result<(), crate::error::Error> {
    if ALLOWED_INTERVALS_SECS.contains(&secs) {
        return Ok(());
    }
    Err(crate::error::Error::MarketData(format!(
        "kraken interval must be one of {} seconds (1m/5m/15m/30m/1h/4h), got {secs}",
        ALLOWED_INTERVALS_SECS
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// The REST/v2 minute label for an already-validated interval in seconds.
#[must_use]
pub const fn interval_label_secs(secs: u32) -> u32 {
    match secs {
        60 => 1,
        300 => 5,
        900 => 15,
        1_800 => 30,
        3_600 => 60,
        // `validate_interval_secs` only ever permits the six listed values.
        _ => 240,
    }
}

/// Maps one side of a pair to the modern display name Spot WebSocket v2
/// recognizes.
///
/// The mapping trims, uppercases and then applies [`LEGACY_ASSET_NAMES`] — a
/// pure observation table (Kraken's own deprecation of `XBT`/`XETH`-style
/// aliases is observed live, not guessed), so no network lookup happens here.
#[must_use]
pub fn modernize_side(raw: &str) -> String {
    let upper = raw.trim().to_ascii_uppercase();
    LEGACY_ASSET_NAMES
        .iter()
        .find(|(legacy, _)| *legacy == upper)
        .map(|(_, modern)| (*modern).to_string())
        .unwrap_or(upper)
}

/// Hinnant's civil-date constant: days between 0000-03-01 and 1970-01-01.
const CIVIL_EPOCH_OFFSET: i64 = 719_468;

/// Days from the Unix epoch for a calendar date (Gregorian, proleptic); the
/// inverse of the date side used by [`iso8601_to_epoch`]. Works for any year
/// ≥ 0 — only non-negative years reach it after validation.
///
/// Pure integer math, no stdlib time conversion (no local-timezone leakage).
#[allow(clippy::arithmetic_side_effects)] // bounded date math (same policy as `accounting::utc_day`)
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp: i64 = if month > 2 {
        i64::from(month) - 3
    } else {
        i64::from(month) + 9
    }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 305]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - CIVIL_EPOCH_OFFSET
}

/// The number of days in `month` (1–12) of `year`, with the leap-year rule.
#[must_use]
const fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29, // February leap
        _ => 28,
    }
}

/// Parses a UTC `Z` ISO-8601 timestamp into Unix seconds.
///
/// Accepts exactly the wire shape — `YYYY-MM-DDTHH:MM:SS`, an optional
/// fractional part of arbitrary precision (the v2 snapshot frames carry
/// nanoseconds; candle edges are whole seconds and the fraction is ignored),
/// and a trailing `Z`. Anything else — other offsets, missing fields, out-of-
/// range components, non-digits — returns `None` rather than guessing. All
/// integer arithmetic: no stdlib time conversions, no timezone leakage.
#[allow(clippy::arithmetic_side_effects)] // bounded date math (same policy as `accounting`)
#[must_use]
pub fn iso8601_to_epoch(raw: &str) -> Option<i64> {
    let body = raw.trim().strip_suffix('Z')?;

    // Fractional seconds follow the seconds field; everything from the first
    // '.' on is ignored (bounded above so a runaway string cannot hide as a
    // valid timestamp).
    let whole = match body.find('.') {
        Some(pos) => match body.get(0..pos) {
            Some(part) if part.len() <= 48 => part,
            _ => return None,
        },
        None => body,
    };

    let (date_part, time_parts) = whole.split_once('T')?;

    // Date: YYYY-MM-DD (year validated 1..=9999 below). `get` rather than
    // slicing: the panic lints forbid slice-index access outside tests.
    let mut date_fields = date_part.split('-');
    let (y_s, m_s, d_s) = (
        date_fields.next()?,
        date_fields.next()?,
        date_fields.next()?,
    );
    if date_fields.next().is_some() || y_s.len() != 4 || m_s.len() != 2 || d_s.len() != 2 {
        return None;
    }

    // Time: HH:MM:SS, each field two digits.
    let mut time_fields = time_parts.split(':');
    let (hh, mm, ss) = (
        time_fields.next()?,
        time_fields.next()?,
        time_fields.next()?,
    );
    if time_fields.next().is_some() || hh.len() != 2 || mm.len() != 2 || ss.len() != 2 {
        return None;
    }

    let year: i64 = y_s.parse().ok()?;
    let month: u32 = m_s.parse().ok()?;
    let day: u32 = d_s.parse().ok()?;
    let hour: i64 = hh.parse().ok()?;
    let minute: i64 = mm.parse().ok()?;
    let second: i64 = ss.parse().ok()?;

    // `parse::<u32>()` accepts "12" and "09" both; enforce the ranges a
    // calendar timestamp must satisfy (and reject year 0 / >9999).
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > days_in_month(year, month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    #[allow(clippy::arithmetic_side_effects)] // bounded date math (same policy as `accounting`)
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(secs)
}

// ─────────────────────── REST transport (hand-rolled HTTPS) ─────────────────

/// Percent-encodes `value` for use in a query string.
///
/// The only values encoded in practice are alphabetic altname pairs, but the
/// function is total: everything outside `[A-Za-z0-9.-]` becomes its `%XX`
/// escape (byte-wise over UTF-8), so an odd future input can never inject a
/// query separator.
#[must_use]
pub fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' => {
                out.push(char::from(b)); // safe: single-byte ASCII range
            }
            other => {
                // `%XX` by hand (no `format!` into a `String`): the nibbles are in
                // 0..=15, looked up in a canonical uppercase table (RFC-3986 form)
                // via `get` rather than indexed — the defaults exist only to keep
                // this path panic-free (panics are denied).
                const HEX_UPPER: &[u8] = b"0123456789ABCDEF";
                let hi = usize::from((other >> 4) & 0x0F);
                let lo = usize::from(other & 0x0F);
                out.push('%');
                out.push(char::from(HEX_UPPER.get(hi).copied().unwrap_or(b'0')));
                out.push(char::from(HEX_UPPER.get(lo).copied().unwrap_or(b'0')));
            }
        }
    }
    out
}

/// The exact request bytes for one public GET. `Connection: close` makes the
/// response end at EOF, and the user agent identifies this paper-trading
/// client (no credentials exist to leak in a public API request).
#[must_use]
pub(crate) fn rest_request_bytes(path_query: &str) -> Vec<u8> {
    format!(
        "GET {path_query} HTTP/1.1\r\n\
         Host: {REST_HOST}\r\n\
         Accept: */*\r\n\
         Connection: close\r\n\
         User-Agent: price-action/paper-trading\r\n\
         \r\n"
    )
    .into_bytes()
}

/// Establishes a TLS connection to the REST host over `rustls` with **bundled**
/// webpki roots and the `ring` provider selected explicitly — the same binding
/// rule, rationale and failure notes as [`crate::notify`] (static musl image
/// `FROM scratch`, never OpenSSL/native-tls; ring cross-builds where
/// aws-lc-rs would not).
async fn connect_tls_rest(
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, crate::error::Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let client_config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| {
            crate::error::Error::MarketData(format!(
                "cannot build a TLS client config for {REST_HOST}: {e}"
            ))
        })?
        .with_root_certificates(roots)
        .with_no_client_auth();

    let domain = ServerName::try_from(REST_HOST.to_string()).map_err(|_| {
        crate::error::Error::MarketData(format!("`{REST_HOST}` is not a valid TLS server name"))
    })?;

    let tcp = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect((REST_HOST, 443)),
    )
    .await
    .map_err(|_elapsed| {
        crate::error::Error::MarketData(format!(
            "cannot reach {REST_HOST}:443 within {}s",
            CONNECT_TIMEOUT.as_secs()
        ))
    })?
    .map_err(|e| crate::error::Error::MarketData(format!("cannot reach {REST_HOST}:443: {e}")))?;

    tokio_rustls::TlsConnector::from(Arc::new(client_config))
        .connect(domain, tcp)
        .await
        .map_err(|e| {
            crate::error::Error::MarketData(format!("TLS handshake with {REST_HOST} failed: {e}"))
        })
}

/// Writes one REST request and reads the whole response off an already-open
/// stream. Split out from [`http_get`] because it is the seam unit tests
/// inject: a `tokio::io::duplex` pair stands in for the socket, exactly as in
/// [`crate::notify`].
async fn read_response<S>(mut stream: S, request: &[u8]) -> Result<String, crate::error::Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream.write_all(request).await.map_err(|e| {
        crate::error::Error::MarketData(format!("cannot write the kraken REST request: {e}"))
    })?;
    stream.flush().await.map_err(|e| {
        crate::error::Error::MarketData(format!("cannot flush the kraken REST request: {e}"))
    })?;
    // `Connection: close` means the response ends at EOF.
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.map_err(|e| {
        crate::error::Error::MarketData(format!("cannot read the kraken REST response: {e}"))
    })?;
    String::from_utf8(raw).map_err(|e| {
        crate::error::Error::MarketData(format!(
            "the kraken REST response was not valid UTF-8 ({} bytes discarded)",
            e.utf8_error().error_len().unwrap_or(0)
        ))
    })
}

/// One public REST GET against [`REST_HOST`]: connect, send, read to EOF. The
/// whole exchange is fenced by [`REQUEST_TIMEOUT`] so a peer that accepts and
/// then stalls still fails. Status-line and Kraken-error decoding happen in
/// [`get_json`], the only caller.
async fn http_get(path_query: &str) -> Result<String, crate::error::Error> {
    let request = rest_request_bytes(path_query);
    let stream = connect_tls_rest().await?;
    let result = tokio::time::timeout(REQUEST_TIMEOUT, read_response(stream, &request))
        .await
        .map_err(|_elapsed| {
            crate::error::Error::MarketData(format!(
                "kraken REST call of `{path_query}` did not answer within {}s",
                REQUEST_TIMEOUT.as_secs()
            ))
        })?;
    let text = result?;
    Ok(text)
}

/// True when the response's first line is an HTTP status line carrying a 2xx
/// code; `None` when there is no readable first line at all. The line is
/// parsed, never sliced (`indexing_slicing`/`string_slice` are deny lints).
fn status_line_is_success(response: &str) -> Option<bool> {
    let line = response.lines().next()?;
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .map(|code| (200..300).contains(&code))
}

/// The status line rendered for error messages (or `<unknown>`).
fn status_text(response: &str) -> String {
    response
        .lines()
        .next()
        .unwrap_or("<unknown>")
        .trim()
        .to_string()
}

/// The JSON envelope of an HTTP response: the outermost `{ … }` in whatever
/// follows the header block. Tolerant of both `Content-Length` and a chunked
/// body (the braces bracket Kraken's JSON either way).
fn json_envelope(response: &str) -> Option<&str> {
    let body = match response.split_once("\r\n\r\n") {
        Some((_head, body)) => body,
        None => response.split_once("\n\n").map_or(response, |(_h, b)| b),
    };
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end < start {
        return None;
    }
    // `get` rather than a slice expression: out-of-range reads should read as
    // "no body", and the panic lints forbid slicing outside tests.
    body.get(start..=end)
}

/// One public REST GET, decoded to JSON — or an error naming what went wrong.
///
/// Kraken reports business errors inside a 2xx envelope with a non-empty
/// `error` array (e.g. `["EQuery:Unknown asset pair"]`); the first message is
/// surfaced verbatim because it names the input, and none of these strings
/// can carry credentials (public endpoints only).
async fn get_json(path_query: &str) -> Result<Value, crate::error::Error> {
    let text = http_get(path_query).await?;
    decode_rest_reply(&text, path_query)
}

/// Decodes one kraken HTTP response: status-line check, envelope extraction and
/// the Kraken business-error array. Pure over text — the seam unit tests drive
/// with captured bodies (no network needed).
///
/// # Errors
///
/// [`crate::error::Error::MarketData`] on a non-2xx status (quoted), a body
/// without a JSON envelope, or a non-empty `error` array (first message quoted —
/// Kraken's own strings name the offending input and carry no credentials).
fn decode_rest_reply(response: &str, path_query: &str) -> Result<Value, crate::error::Error> {
    if !matches!(status_line_is_success(response), Some(true)) {
        return Err(crate::error::Error::MarketData(format!(
            "kraken REST `{path_query}` answered with a non-success status ({})",
            status_text(response)
        )));
    }
    let envelope = json_envelope(response).ok_or_else(|| {
        crate::error::Error::MarketData(format!(
            "kraken REST `{path_query}` returned no JSON envelope in its body"
        ))
    })?;
    let value: Value = serde_json::from_str(envelope).map_err(|e| {
        crate::error::Error::MarketData(format!("cannot parse the kraken JSON reply: {e}"))
    })?;
    if let Some(errors) = value.get("error").and_then(Value::as_array) {
        if let Some(first) = errors.first() {
            let message = first.as_str().unwrap_or("<empty error string>");
            return Err(crate::error::Error::MarketData(format!(
                "kraken REST `{path_query}` reported: {message}"
            )));
        }
    }
    Ok(value)
}

// ───────────────────────────── pair resolution ──────────────────────────────

/// Normalizes a user-facing pair form for table matching.
///
/// Slashed forms go through [`modernize_side`], so e.g. the legacy wsname
/// `XBT/USD` becomes the v2 display form `BTC/USD` that the matcher knows;
/// non-slashed forms (altname / internal key) are just trimmed and uppercased.
#[must_use]
pub fn normalize_pair_input(input: &str) -> String {
    let trimmed = input.trim();
    match trimmed.split_once('/') {
        Some((base, quote)) => format!("{}/{}", modernize_side(base), modernize_side(quote)),
        None => modernize_side(trimmed),
    }
}

/// Resolves a normalized pair form against one Kraken table snapshot — the
/// pure heart of [`resolve_pair`], unit-testable without any network.
///
/// `pairs_value` / `assets_value` are the JSON bodies' `result` objects:
/// `AssetPairs` (internal key → entry with `altname`, `base`, `quote`) and
/// `Assets` (asset code → entry with `altname`). For every market entry three
/// lookup keys are built — its altname, its internal key uppercased, and the
/// **modern display form** derived from the asset altnames through
/// [`modernize_side`] — and `input_norm` (already through
/// [`normalize_pair_input`]) is looked up in each. The first hit wins.
/// Entries whose asset codes are missing from the assets table cannot yield a
/// display name and are skipped, not guessed at.
#[must_use]
pub fn resolve_from_tables(
    pairs_value: &Value,
    assets_value: &Value,
    input_norm: &str,
) -> Option<PairResolution> {
    let want = input_norm.trim().to_ascii_uppercase();
    if want.is_empty() {
        return None;
    }

    // Asset display names (table key is the asset code).
    let mut asset_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if let Some(obj) = assets_value.as_object() {
        for (code, entry) in obj {
            if let Some(name) = entry.get("altname").and_then(Value::as_str) {
                asset_names.insert(code.to_ascii_uppercase(), name.trim().to_ascii_uppercase());
            }
        }
    }

    // Display form from an entry's base/quote codes (both sides must know).
    let display_of = |entry: &Value| -> Option<String> {
        let base_code = entry.get("base")?.as_str()?.trim().to_ascii_uppercase();
        let quote_code = entry.get("quote")?.as_str()?.trim().to_ascii_uppercase();
        let base = asset_names.get(&base_code)?;
        let quote = asset_names.get(&quote_code)?;
        Some(format!(
            "{}/{}",
            modernize_side(base),
            modernize_side(quote)
        ))
    };

    // Build the three lookup maps over every market in one pass.
    let mut by_altname: std::collections::HashMap<String, &String> =
        std::collections::HashMap::new();
    let mut by_internal: std::collections::HashMap<String, &String> =
        std::collections::HashMap::new();
    let mut by_display: std::collections::HashMap<String, &String> =
        std::collections::HashMap::new();
    let pairs_obj = pairs_value.as_object()?;
    for (internal_key, entry) in pairs_obj {
        let altname = match entry.get("altname").and_then(Value::as_str) {
            Some(name) if !name.trim().is_empty() => name.trim().to_ascii_uppercase(),
            _ => continue, // no usable REST name: cannot serve this market
        };
        let internal = internal_key.trim().to_ascii_uppercase();

        by_internal.entry(internal.clone()).or_insert(internal_key);
        by_altname.entry(altname).or_insert(internal_key);
        if let Some(display) = display_of(entry) {
            by_display.entry(display).or_insert(internal_key);
        }
    }

    // First hit anywhere resolves the market (try every lookup map in turn —
    // a miss on one form is not a miss overall).
    let matched: Option<&String> = by_altname
        .get(&want)
        .copied()
        .or_else(|| by_internal.get(&want).copied())
        .or_else(|| by_display.get(&want).copied());
    let key = matched?;
    let entry = pairs_obj.get(key.as_str())?;

    let altname = entry.get("altname")?.as_str()?.to_string();
    // A market can match on its altname or internal key yet still lack asset
    // entries; without both codes known there is no honest display name to
    // hand v2, so the resolution fails rather than guessing.
    let ws_symbol = display_of(entry)?;
    Some(PairResolution { altname, ws_symbol })
}

/// Resolves a user-facing pair form into the names both endpoints need.
///
/// Accepted forms (case-insensitive, trimmed): modern display pair
/// (`BTC/USD`), legacy wsname (`XBT/USD` — sides are modernized before
/// matching), altname (`XBTUSD`) and internal key (`XXBTZUSD`). Two public
/// REST table fetches feed the pure [`resolve_from_tables`] matcher, so the
/// rule set is one implementation with a socket-free seam.
///
/// # Errors
///
/// [`crate::error::Error::MarketData`] on network/decoding failure (with Kraken
/// error messages quoted) or when no market matches — then the error names the
/// input and lists the working examples.
pub async fn resolve_pair(input: &str) -> Result<PairResolution, crate::error::Error> {
    let normalized = normalize_pair_input(input);
    if normalized.is_empty() || normalized == "/" {
        return Err(crate::error::Error::MarketData(
            "kraken pair was empty".to_string(),
        ));
    }

    let pairs_value = get_json("/0/public/AssetPairs")
        .await?
        .get("result")
        .cloned()
        .ok_or_else(|| {
            crate::error::Error::MarketData(
                "kraken AssetPairs reply had no result object".to_string(),
            )
        })?;
    let assets_value = get_json("/0/public/Assets")
        .await?
        .get("result")
        .cloned()
        .ok_or_else(|| {
            crate::error::Error::MarketData("kraken Assets reply had no result object".to_string())
        })?;

    resolve_from_tables(&pairs_value, &assets_value, &normalized).ok_or_else(|| {
        crate::error::Error::MarketData(format!(
            "kraken has no market matching `{normalized}` (e.g. `BTC/USD`, `XBT/USD`, `XBTUSDT`)"
        ))
    })
}

// ─────────────────────────────── REST candles ───────────────────────────────

/// Fetches recent committed candles for one market over the public REST
/// endpoint and returns them oldest-first as validated [`crate::market::Bar`]s.
///
/// `now_secs` (Unix) is injected rather than read inside so the
/// uncommitted-candle rule is deterministic under test; callers pass real
/// wall-clock seconds. Kraken's last row is the **still-forming** candle, and
/// every trailing row whose window end (`ts + interval_secs`) has not passed
/// by `now_secs` is dropped — a backtest must never price a partial bar.
///
/// # Errors
///
/// [`crate::error::Error::MarketData`] on transport/decoding failure (see
/// [`get_json`]), a Kraken error envelope, an unexpected result shape or any
/// malformed row; also when nothing is left after dropping uncommitted rows.
pub async fn fetch_candles(
    settings: &KrakenSettings,
    now_secs: i64,
) -> Result<Vec<crate::market::Bar>, crate::error::Error> {
    settings.validate()?;
    let path = format!(
        "/0/public/OHLC?pair={}&interval={}",
        encode_query_value(&settings.rest_pair),
        interval_label_secs(settings.interval_secs)
    );
    let value = get_json(&path).await?;
    parse_candles(&value, settings.interval_secs, now_secs)
}

/// Decodes one `/0/public/OHLC` reply (`result` → first market → rows) into
/// bars. Pure — the seam unit tests drive with captured envelopes.
///
/// Verified row shape: `[ts(unix secs), open, high, low, close, vwap, volume,
/// count]` with the money fields as strings; rows arrive oldest-first (they
/// are still sorted and de-duplicated defensively, a repeated stamp keeping its
/// LAST occurrence — Kraken re-emits in-flight windows). Trailing rows whose
/// end has not passed `now_secs` are dropped (the forming-candle rule from
/// [`fetch_candles`]).
///
/// # Errors
///
/// [`crate::error::Error::MarketData`] when a Kraken error array is present,
/// the result shape is unusable, or any row is malformed (named by index) or
/// carries non-finite/negative money values.
#[allow(clippy::arithmetic_side_effects)] // bounded candle math (same policy as `accounting`)
pub fn parse_candles(
    value: &Value,
    interval_secs: u32,
    now_secs: i64,
) -> Result<Vec<crate::market::Bar>, crate::error::Error> {
    let bad = |what: String| crate::error::Error::MarketData(what);

    if let Some(errors) = value.get("error").and_then(Value::as_array) {
        if let Some(first) = errors.first() {
            return Err(bad(format!(
                "kraken OHLC reported: {}",
                first.as_str().unwrap_or("<empty error string>")
            )));
        }
    }

    let entries = value
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("kraken OHLC reply had no result object".to_string()))?;
    let (market_key, rows_value) = entries
        .iter()
        .next()
        .ok_or_else(|| bad("kraken OHLC result carries no market data".to_string()))?;
    let rows = rows_value.as_array().ok_or_else(|| {
        bad(format!(
            "kraken OHLC market `{market_key}` does not carry an array of rows"
        ))
    })?;

    // One row: [ts, o, h, l, c, vwap, volume, count]; prices+volume are
    // strings on the wire (parse them as such — `as_str` first keeps a future
    // numeric shape from silently flowing through).
    let mut by_ts: std::collections::BTreeMap<i64, (f64, f64, f64, f64, f64)> =
        std::collections::BTreeMap::new();
    for (idx, row) in rows.iter().enumerate() {
        let cells = match row.as_array() {
            Some(cells) if cells.len() >= 8 => cells,
            _ => {
                return Err(bad(format!(
                    "candle row {idx} is not an array of at least 8 fields"
                )))
            }
        };
        let ts: i64 = cells
            .first()
            .and_then(Value::as_u64)
            .and_then(|t| i64::try_from(t).ok())
            .ok_or_else(|| bad(format!("candle row {idx} carries a non-integer timestamp")))?;
        let money = |pos: usize, name: &str| -> Result<f64, crate::error::Error> {
            let cell = cells
                .get(pos)
                .ok_or_else(|| bad(format!("candle row {idx} is too short for a {name}")))?;
            let raw = cell
                .as_str()
                .ok_or_else(|| bad(format!("candle row {idx} has a non-string {name}")))?;
            let parsed: f64 = raw.parse().map_err(|_| {
                bad(format!(
                    "candle row {idx} has an unparseable {name}: `{raw}`"
                ))
            })?;
            if !parsed.is_finite() || parsed < 0.0 {
                return Err(bad(format!(
                    "candle row {idx} carries an unusable {name}: {parsed}"
                )));
            }
            Ok(parsed)
        };
        let (open, high, low, close, volume) = (
            money(1, "open")?,
            money(2, "high")?,
            money(3, "low")?,
            money(4, "close")?,
            money(6, "volume")?,
        );
        // Last write wins on a repeated stamp: the re-emitted window is fresher.
        by_ts.insert(ts, (open, high, low, close, volume));
    }

    let interval = i64::from(interval_secs);
    let mut bars = Vec::new();
    for (ts, (open, high, low, close, volume)) in by_ts {
        // The forming-candle rule: a window only counts once its end has
        // passed at `now` — this also chops any trailing partial windows.
        if ts.saturating_add(interval) > now_secs {
            continue;
        }
        bars.push(
            crate::market::Bar::new(
                UNIX_EPOCH + Duration::from_secs(u64::try_from(ts).unwrap_or(0)),
                open,
                high,
                low,
                close,
                volume,
            )
            .map_err(|e| bad(format!("candle at {ts}s is not a valid bar: {e}")))?,
        );
    }

    if bars.is_empty() {
        return Err(bad(
            "kraken returned no committed candles for the requested window".to_string(),
        ));
    }
    Ok(bars)
}

// ─────────────────────── Spot WebSocket v2 feeder ───────────────────────────

/// Converts one v2 OHLC data item into the shared [`crate::feed::WindowUpdate`]
/// (the type the [`crate::live`] shaper already understands) or `None` when a
/// field is missing/unusable. Every timestamp comes from the wire, never from
/// a local clock — the same reproducibility property `crate::feed` documents.
#[allow(clippy::arithmetic_side_effects)] // bounded window-edge math
fn ohlc_item_to_window(item: &Value) -> Option<crate::feed::RawEvent> {
    let symbol = item.get("symbol")?.as_str()?.trim().to_string();
    if symbol.is_empty() {
        return None;
    }
    let open = item.get("open")?.as_f64()?;
    let high = item.get("high")?.as_f64()?;
    let low = item.get("low")?.as_f64()?;
    let close = item.get("close")?.as_f64()?;
    let volume = item.get("volume")?.as_f64()?;
    // Unusable values are dropped, never booked (the `live.rs` policy): each
    // operand checked explicitly rather than looped in a named tuple.
    if !open.is_finite()
        || open < 0.0
        || !high.is_finite()
        || high < 0.0
        || !low.is_finite()
        || low < 0.0
        || !close.is_finite()
        || close < 0.0
        || !volume.is_finite()
        || volume < 0.0
    {
        return None;
    }

    // Both window edges ship as ISO-8601 UTC strings; the candle's start is
    // `interval_begin` and its end the deprecated-but-present `timestamp`. If
    // that field is ever absent from the wire, assume one full interval — the
    // same defensive rule the Massive shaper applies for a missing window end
    // (the dedupe key is the start anyway; a dropped field must not kill the
    // whole feed).
    let start_secs = iso8601_to_epoch(item.get("interval_begin")?.as_str()?)?;
    let end_secs = if let Some(text) = item.get("timestamp").and_then(Value::as_str) {
        iso8601_to_epoch(text)?
    } else {
        let minutes: i64 = item
            .get("interval")
            .and_then(Value::as_u64)
            .and_then(|raw| u32::try_from(raw).ok())
            .map(i64::from)?;
        start_secs.saturating_add(minutes * 60)
    };
    if end_secs <= start_secs {
        return None; // a window that ends at or before it starts says nothing
    }

    Some(crate::feed::RawEvent::Window(crate::feed::WindowUpdate {
        symbol,
        start_ms: secs_to_ms(start_secs),
        end_ms: Some(secs_to_ms(end_secs)),
        open,
        high,
        low,
        close,
        volume,
    }))
}

/// Unix seconds → milliseconds (candle stamps never predate the epoch in
/// practice; a negative parse clamps to 0 rather than wrapping).
#[allow(clippy::arithmetic_side_effects)] // bounded ms conversion
#[must_use]
fn secs_to_ms(secs: i64) -> u64 {
    let ms = secs.saturating_mul(1_000);
    u64::try_from(ms).unwrap_or(0)
}

/// Decodes one v2 text frame into recognizable data events.
///
/// Only `ohlc` **update** frames produce bars. Snapshot frames — delivered
/// once per connection as warm-up and containing *closed* history plus the
/// in-flight candle — are deliberately ignored: replaying stale history into a
/// running paper session would book trades against long-past prices, and the
/// in-flight candle arrives with final numbers through subsequent updates
/// anyway (the shaper's hold-until-next logic then closes it). `heartbeat`,
/// `status` and every other message (including the subscribe ack, which
/// [`v2_subscribe_verdict`] settles) decode to nothing. Frames that fail to
/// parse are skipped entirely — forward progress beats one bad message (the
/// same stance [`crate::feed`] takes). Data items for any symbol other than
/// `expected_symbol` are dropped.
///
/// This function is total by design: it never fails, an unusable item or frame
/// simply contributes nothing (and a genuinely broken stream shows up as
/// silence, which the feeder's timeout converts into a reconnect).
#[must_use]
pub fn decode_v2_frame(frame: &str, expected_symbol: &str) -> Vec<crate::feed::RawEvent> {
    let Ok(value) = serde_json::from_str::<Value>(frame.trim()) else {
        return Vec::new();
    };
    if !matches!(value.get("channel").and_then(Value::as_str), Some("ohlc")) {
        return Vec::new(); // heartbeat / status / ack / unknown: not bar material
    }
    match value.get("type").and_then(Value::as_str) {
        Some("update") => {}
        _ => return Vec::new(), // snapshots are warm-up only (see above)
    }
    let Some(items) = value.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(ohlc_item_to_window)
        .filter(|event| {
            matches!(
                event,
                crate::feed::RawEvent::Window(w) if w.symbol.eq_ignore_ascii_case(expected_symbol)
            )
        })
        .collect()
}

/// The settlement of one subscription attempt.
///
/// The v2 reply to a subscribe request carries `"success": true|false` and,
/// on failure, the server's own `error` string (e.g. *"Currency pair not
/// supported XBT/USD*").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V2SubscribeVerdict {
    /// `"success": true` — the subscription is live and data frames may follow.
    Accepted,
    /// `"success": false`; carries Kraken's own error text (log it verbatim —
    /// it names the offending symbol and contains no credentials).
    Rejected(String),
}

/// Extracts the subscription verdict from a frame, if that is what it is.
///
/// Only frames with `"method": "subscribe"` settle anything; data, heartbeat
/// and status frames return `None` (the ack wait simply keeps waiting on
/// them).
///
/// # Errors
///
/// Never; unparseable input yields `None` like any non-ack frame.
#[must_use]
pub fn v2_subscribe_verdict(frame: &str) -> Option<V2SubscribeVerdict> {
    let value = serde_json::from_str::<Value>(frame.trim()).ok()?;
    if !matches!(
        value.get("method").and_then(Value::as_str),
        Some("subscribe")
    ) {
        return None;
    }
    match value.get("success").and_then(Value::as_bool) {
        Some(true) => Some(V2SubscribeVerdict::Accepted),
        Some(false) => {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("<no error text in the rejection>")
                .to_string();
            Some(V2SubscribeVerdict::Rejected(message))
        }
        // A subscribe frame without a success field is not a settled verdict.
        None => None,
    }
}

/// One connection lifetime for one v2 socket. Returns [`V2StreamOutcome`] so
/// the loop caller can tell a clean shutdown (stop, no marker) from a drop
/// (marker + backoff reconnect). The subscribe request itself is built here —
/// the payload contains only the public pair and interval; nothing secret is
/// ever written to either of these endpoints.
async fn v2_stream_once(
    settings: &KrakenSettings,
    tx: &mpsc::Sender<crate::feed::RawEvent>,
) -> V2StreamOutcome {
    let connect = tokio_tungstenite::connect_async(WS_URL)
        .await
        .map_err(|e| format!("connect failed: {e}"));
    let mut ws = match connect {
        // The HTTP upgrade response carries nothing this feeder needs.
        Ok((ws, _response)) => ws,
        Err(reason) => return V2StreamOutcome::Dropped(reason),
    };

    // Subscribe first — v2 opens without authentication for public channels,
    // so the only pre-data handshake is the subscribe acknowledgement.
    let sub_payload = serde_json::json!({
        "method": "subscribe",
        "params": {
            "channel": "ohlc",
            "symbol": [settings.ws_symbol],
            "interval": interval_label_secs(settings.interval_secs),
        },
    })
    .to_string();
    if let Err(_e) = ws.send(WsMessage::Text(sub_payload.into())).await {
        return V2StreamOutcome::Dropped("sending the subscription failed".to_string());
    }

    // Wait for the settle: data/heartbeat/status frames arrive before it and
    // must not end the wait; only a subscribe-method frame carries a verdict.
    loop {
        let ok_some_ok_frame = tokio::time::timeout(ACK_TIMEOUT, ws.next())
            .await
            .ok()
            .flatten();
        let Some(Ok(frame)) = ok_some_ok_frame else {
            return V2StreamOutcome::Dropped("no subscription acknowledgement in time".into());
        };
        if tx.is_closed() {
            return V2StreamOutcome::ReceiverGone;
        }
        match v2_subscribe_verdict(&frame_to_text(&frame)) {
            Some(V2SubscribeVerdict::Accepted) => break,
            Some(V2SubscribeVerdict::Rejected(message)) => {
                // Kraken says the subscription cannot exist (bad pair form).
                // Retrying with identical parameters would reject identically
                // forever — but we do not know whether it is transient or a
                // pair problem at this depth, so treat it as a drop: the
                // backoff loop retries and the repeated rejection becomes
                // visible in the log instead of silently ending the session.
                return V2StreamOutcome::Dropped(format!(
                    "kraken rejected the subscription: {message}"
                ));
            }
            // Not a subscribe frame yet (status / data / heartbeat): re-loop.
            None => {}
        }
    }

    eprintln!(
        "price-action kraken feed: v2 subscription active for {}",
        settings.ws_symbol
    );

    loop {
        let frame = match tokio::time::timeout(SILENCE_TIMEOUT, ws.next()).await {
            Ok(Some(Ok(frame))) => frame,
            Ok(Some(Err(e))) => return V2StreamOutcome::Dropped(format!("socket error: {e}")),
            Ok(None) => return V2StreamOutcome::Dropped("server closed the connection".into()),
            Err(_elapsed) => {
                return V2StreamOutcome::Dropped(format!(
                    "no data for {}s; assuming the feed stalled",
                    SILENCE_TIMEOUT.as_secs()
                ))
            }
        };

        if tx.is_closed() {
            return V2StreamOutcome::ReceiverGone;
        }

        for event in decode_v2_frame(&frame_to_text(&frame), &settings.ws_symbol) {
            if tx.send(event).await.is_err() {
                return V2StreamOutcome::ReceiverGone;
            }
        }
    }
}

/// What one v2 connection lifetime ended in (mirror of `crate::feed`).
#[derive(Debug)]
enum V2StreamOutcome {
    /// The session closed its channel end — stop, no reconnect, no marker.
    ReceiverGone,
    /// The socket/endpoint failed; the reason names it (never secrets).
    Dropped(String),
}

/// Spawns the v2 feeder task.
///
/// One connection per loop pass, forever, with the same capped-exponential-
/// backoff policy as [`crate::feed`] (`backoff_for` — 2s → 30s) and
/// [`crate::feed::RawEvent::FeedInterrupted`] markers after every drop so a
/// session can report data holes between bars. Stops promptly
/// when `tx`'s receiver is dropped (session shutdown).
#[must_use]
pub fn spawn_v2(
    settings: KrakenSettings,
    tx: mpsc::Sender<crate::feed::RawEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut attempt = 1u32;
        loop {
            if tx.is_closed() {
                return;
            }

            match v2_stream_once(&settings, &tx).await {
                V2StreamOutcome::ReceiverGone => return, // clean shutdown: no marker on exit
                V2StreamOutcome::Dropped(reason) => eprintln!(
                    "price-action kraken feed: v2 connection #{attempt} ended ({reason}); will reconnect"
                ),
            }

            if tx
                .send(crate::feed::RawEvent::FeedInterrupted)
                .await
                .is_err()
            {
                return; // receiver went away mid-shutdown: stop quietly
            }
            if tx.is_closed() {
                return;
            }
            let delay = crate::feed::backoff_for(attempt);
            eprintln!(
                "price-action kraken feed: retry in {}s (attempt {} next)",
                delay.as_secs(),
                attempt.saturating_add(1)
            );
            tokio::select! {
                () = tokio::time::sleep(delay) => {},
                () = tx.closed() => return,
            }
            attempt = attempt.saturating_add(1);
        }
    })
}

/// A `WsMessage` rendered as its UTF-8 text (non-text frames yield empty —
/// whose decode result is simply "no events", like in [`crate::feed`]).
fn frame_to_text(frame: &WsMessage) -> String {
    match frame {
        WsMessage::Text(t) => t.as_str().to_string(),
        // Binary frames are not part of the v2 protocol: non-UTF-8 yields
        // empty, which decodes to nothing.
        WsMessage::Binary(payload) => {
            std::str::from_utf8(payload.as_ref()).map_or_else(|_| String::new(), str::to_string)
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)] // bounded test literals
    #![allow(clippy::float_cmp)] // asserts the exact decoded wire values on purpose

    use super::*;
    use crate::feed::{RawEvent, WindowUpdate};

    // ── fixtures captured verbatim from wss://ws.kraken.com/v2 (2026-10-06) ─

    const HEARTBEAT: &str = r#"{"channel":"heartbeat"}"#;
    const STATUS_UPDATE: &str = r#"{"channel":"status","type":"update","data":[{"version":"2.0.10","system":"online","api_version":"v2","connection_id":7734965360043499011,"upcoming_maintenance":[],"emergency":[]}]}"#;
    const SUBSCRIBE_ACK_ACCEPTED: &str = r#"{"method":"subscribe","result":{"channel":"ohlc","interval":1,"snapshot":true,"symbol":"BTC/USD","warnings":["timestamp is deprecated, use interval_begin"]},"success":true,"time_in":"2026-10-06T18:06:01.981684Z","time_out":"2026-10-06T18:06:01.981723Z"}"#;
    const SUBSCRIBE_ACK_REJECTED: &str = r#"{"error":"Currency pair not supported XBT/USD","method":"subscribe","success":false,"symbol":"XBT/USD","time_in":"2026-10-06T17:39:49.588049Z","time_out":"2026-10-06T17:39:49.588083Z"}"#;
    // First OHLC update after the 18:06 boundary (interval_begin 18:06 → ts 18:07).
    const UPDATE_1806_OPENING: &str = r#"{"channel":"ohlc","type":"update","timestamp":"2026-10-06T18:06:02.264627814Z","data":[{"symbol":"BTC/USD","open":85754.8,"high":85754.8,"low":85754.7,"close":85754.8,"trades":5,"volume":0.00126522,"vwap":85754.7,"interval_begin":"2026-10-06T18:06:00.000000000Z","interval":1,"timestamp":"2026-10-06T18:07:00.000000Z"}]}"#;
    // A re-emission of the same window (same interval_begin, fresher numbers).
    const UPDATE_1806_REEMITTED: &str = r#"{"channel":"ohlc","type":"update","timestamp":"2026-10-06T18:06:02.918011Z","data":[{"symbol":"BTC/USD","open":85754.8,"high":85754.8,"low":85754.7,"close":85754.7,"trades":12,"volume":0.51707722,"vwap":85754.7,"interval_begin":"2026-10-06T18:06:00.000000000Z","interval":1,"timestamp":"2026-10-06T18:07:00.000000Z"}]}"#;
    // Snapshot (warm-up, ignored by design): two closed candles + nothing else
    // needed to pin "snapshot never yields bars".
    const SNAPSHOT_TWO_ITEMS: &str = r#"{"channel":"ohlc","type":"snapshot","timestamp":"2026-10-06T18:06:01.982191308Z","data":[{"symbol":"BTC/USD","open":85702.8,"high":85702.8,"low":85697.9,"close":85697.9,"trades":36,"volume":0.69367951,"vwap":85700.0,"interval_begin":"2026-10-06T17:57:00.000000000Z","interval":1,"timestamp":"2026-10-06T17:58:00.000000Z"},{"symbol":"BTC/USD","open":85697.9,"high":85697.9,"low":85686.0,"close":85690.7,"trades":40,"volume":0.08767812,"vwap":85687.5,"interval_begin":"2026-10-06T17:58:00.000000000Z","interval":1,"timestamp":"2026-10-06T17:59:00.000000Z"}]}"#;

    // ── REST envelope fixture (captured /0/public/OHLC, trimmed to 12 rows) ─

    const OHLC_REPLY_BODY: &str = r#"{"error": [], "result": {"BTC/USD": [[1791318900, "85624.2", "85624.2", "85624.0", "85624.2", "85624.0", "0.56478953", 53], [1791318960, "85624.2", "85624.3", "85624.1", "85624.2", "85624.2", "0.21140203", 26], [1791319020, "85624.3", "85625.2", "85619.4", "85622.0", "85624.3", "0.72638896", 52], [1791319080, "85622.0", "85622.0", "85606.4", "85609.0", "85610.0", "0.60234036", 53], [1791319140, "85609.0", "85640.9", "85609.0", "85640.9", "85633.7", "0.38500332", 69], [1791319200, "85640.9", "85640.9", "85639.2", "85639.3", "85640.1", "0.51442570", 56], [1791319260, "85639.3", "85659.3", "85639.3", "85646.5", "85646.2", "0.55612548", 68], [1791319320, "85646.4", "85646.5", "85636.9", "85637.0", "85639.6", "0.15080781", 31], [1791319380, "85637.0", "85637.0", "85624.3", "85624.4", "85632.4", "0.17031954", 64], [1791319440, "85624.3", "85624.4", "85619.6", "85619.6", "85624.1", "0.09677781", 42], [1791319500, "85619.6", "85639.1", "85619.5", "85639.1", "85622.9", "0.10297694", 38], [1791319560, "85639.0", "85643.5", "85639.0", "85643.5", "85643.3", "0.04989358", 18]]}}"#;

    fn window_of(events: Vec<RawEvent>) -> WindowUpdate {
        match events.into_iter().next() {
            Some(RawEvent::Window(w)) => w,
            other => panic!("expected one window event, got {other:?}"),
        }
    }

    fn http200(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            len = body.len()
        )
    }

    // ── interval & alias helpers ─────────────────────────────────────────────

    #[test]
    fn intervals_map_to_their_minute_labels() {
        assert_eq!(interval_label_secs(60), 1);
        assert_eq!(interval_label_secs(300), 5);
        assert_eq!(interval_label_secs(900), 15);
        assert_eq!(interval_label_secs(1_800), 30);
        assert_eq!(interval_label_secs(3_600), 60);
        assert_eq!(interval_label_secs(14_400), 240);
    }

    #[test]
    fn non_allowed_intervals_are_rejected_naming_the_set() {
        for bad in [10 * 60u32, 720 * 60, 5_000, 0] {
            let err = validate_interval_secs(bad).unwrap_err();
            assert!(err.to_string().contains("kraken interval"), "{err:?}");
        }
        for good in ALLOWED_INTERVALS_SECS {
            assert!(validate_interval_secs(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn legacy_asset_names_map_to_their_modern_display_forms() {
        assert_eq!(modernize_side(" xbt "), "BTC"); // trimmed + uppercased + aliased
        assert_eq!(modernize_side("XBT"), "BTC");
        assert_eq!(modernize_side("eth"), "ETH"); // no alias needed
        assert_eq!(modernize_side("USDT"), "USDT");
    }

    #[test]
    fn pair_input_normalization_uppercases_and_modernizes_each_side() {
        assert_eq!(normalize_pair_input("btc/usd"), "BTC/USD");
        assert_eq!(normalize_pair_input("XBT/USD"), "BTC/USD"); // legacy wsname sides
        assert_eq!(normalize_pair_input("  xbtusdt "), "XBTUSDT");
        assert_eq!(normalize_pair_input("xxbtzUSD"), "XXBTZUSD");
    }

    #[test]
    fn query_encoding_leaves_ordinary_chars_and_escapes_the_rest() {
        assert_eq!(encode_query_value("XBTUSD"), "XBTUSD");
        assert_eq!(encode_query_value("BTC/USD"), "BTC%2FUSD");
        assert_eq!(encode_query_value("a b-c.d~e"), "a%20b-c.d%7Ee");
    }

    // ── ISO-8601 UTC parser (anchors independently computed outside Rust) ────

    #[test]
    fn iso_timestamps_parse_to_their_unix_seconds() {
        assert_eq!(iso8601_to_epoch("1970-01-01T00:00:00Z"), Some(0));
        // Fractional part (any precision) is ignored — candle edges are whole seconds.
        assert_eq!(
            iso8601_to_epoch("2024-02-29T23:59:59.999Z"),
            Some(1_709_251_199)
        );
        assert_eq!(
            iso8601_to_epoch("2026-10-06T18:06:00Z"),
            Some(1_791_309_960)
        );
        // The v2 snapshot shape carries nanoseconds after the seconds field.
        assert_eq!(
            iso8601_to_epoch("2026-10-06T18:07:00.000000000Z"),
            Some(1_791_310_020)
        );
    }

    #[test]
    fn iso_timestamps_reject_everything_but_the_wire_shape() {
        for bad in [
            "",
            "1970-01-01T00:00:00",       // no trailing Z
            "1970-01-01 00:00:00Z",      // not a T separator
            "2026-1-06T18:06:00Z",       // one-digit month
            "2026-10-6T18:06:00Z",       // one-digit day
            "2024-02-30T00:00:00Z",      // no such day (Feb 30)
            "2023-02-29T00:00:00Z",      // non-leap year February
            "2026-10-06T24:00:00Z",      // hour out of range
            "2026-10-06T18:60:00Z",      // minute out of range
            "2026-10-06T18:06:60Z",      // second out of range
            "0000-01-01T00:00:00Z",      // year 0 is not a Gregorian year
            "99999-01-01T00:00:00Z",     // five-digit year (field-length rule)
            "2026-10-06T18:06:00+00:00", // offsets are not the wire shape
            "2026-13-06T18:06:00Z",      // month out of range
        ] {
            assert_eq!(iso8601_to_epoch(bad), None, "{bad} must be rejected");
        }
    }

    // ── v2 frame decoding (fixtures captured verbatim from the live feed) ────

    #[test]
    fn heartbeats_status_and_acks_carry_no_bar_material() {
        assert!(decode_v2_frame(HEARTBEAT, "BTC/USD").is_empty());
        assert!(decode_v2_frame(STATUS_UPDATE, "BTC/USD").is_empty());
        assert!(decode_v2_frame(SUBSCRIBE_ACK_ACCEPTED, "BTC/USD").is_empty());
        assert!(decode_v2_frame("definitely not json", "BTC/USD").is_empty());
        assert!(decode_v2_frame("", "BTC/USD").is_empty());
    }

    #[test]
    fn update_frames_decode_to_windows_with_both_edges() {
        let events = decode_v2_frame(UPDATE_1806_OPENING, "BTC/USD");
        let window = window_of(events);
        assert_eq!(window.symbol, "BTC/USD");
        // interval_begin 2026-10-06T18:06:00Z and timestamp 18:07:00Z (OS-verified).
        assert_eq!(window.start_ms, 1_791_309_960_000);
        assert_eq!(window.end_ms, Some(1_791_310_020_000));
        assert!((window.open - 85754.8).abs() < f64::EPSILON);
        assert!((window.high - 85754.8).abs() < f64::EPSILON);
        assert!((window.low - 85754.7).abs() < f64::EPSILON);
        assert!((window.close - 85754.8).abs() < f64::EPSILON);
        assert_eq!(window.volume, 0.001_265_22);
    }

    #[test]
    fn re_emitted_windows_share_their_start_and_carry_fresher_numbers() {
        let first = window_of(decode_v2_frame(UPDATE_1806_OPENING, "BTC/USD"));
        let second = window_of(decode_v2_frame(UPDATE_1806_REEMITTED, "BTC/USD"));
        // Same start: the shaper's hold-until-next logic dedupes on it.
        assert_eq!(first.start_ms, second.start_ms);
        assert_eq!(first.end_ms, second.end_ms);
        // Fresher numbers must survive as distinct values (close moved).
        assert!((second.close - 85754.7).abs() < f64::EPSILON);
        assert_ne!(first.close, second.close);
    }

    #[test]
    fn snapshots_are_warmup_only_and_never_yield_bars() {
        // Even though every item is valid bar material: replaying closed history
        // into a running session would book trades against long-past prices.
        assert!(decode_v2_frame(SNAPSHOT_TWO_ITEMS, "BTC/USD").is_empty());
    }

    #[test]
    fn events_for_other_symbols_are_dropped() {
        let other = r#"{"channel":"ohlc","type":"update","timestamp":"2026-10-06T18:06:02.000Z","data":[{"symbol":"ETH/USD","open":3000.0,"high":3001.0,"low":2999.0,"close":3000.5,"trades":1,"volume":1.0,"vwap":3000.1,"interval_begin":"2026-10-06T18:06:00.000000000Z","interval":1,"timestamp":"2026-10-06T18:07:00.000000Z"}]}"#;
        assert!(decode_v2_frame(other, "BTC/USD").is_empty());
    }

    #[test]
    fn unusable_window_values_are_dropped_not_booked() {
        let nan = r#"{"channel":"ohlc","type":"update","timestamp":"t","data":[{"symbol":"BTC/USD","open":null}]}"#;
        assert!(decode_v2_frame(nan, "BTC/USD").is_empty());
        // Missing edges: no start at all.
        let missing_edges = r#"{"channel":"ohlc","type":"update","timestamp":"t","data":[{"symbol":"BTC/USD","open":1.0,"high":2.0,"low":1.0,"close":2.0,"volume":1.0}]}"#;
        assert!(decode_v2_frame(missing_edges, "BTC/USD").is_empty());
    }

    // ── v2 subscribe acknowledgement ─────────────────────────────────────────

    #[test]
    fn subscribe_verdicts_settle_only_on_subscribe_frames() {
        assert_eq!(
            v2_subscribe_verdict(SUBSCRIBE_ACK_ACCEPTED),
            Some(V2SubscribeVerdict::Accepted)
        );
        assert_eq!(
            v2_subscribe_verdict(SUBSCRIBE_ACK_REJECTED),
            Some(V2SubscribeVerdict::Rejected(
                "Currency pair not supported XBT/USD".to_string()
            ))
        );
        // Data, status and heartbeat frames never settle the wait; unparseable
        // input reads the same.
        assert_eq!(v2_subscribe_verdict(UPDATE_1806_OPENING), None);
        assert_eq!(v2_subscribe_verdict(HEARTBEAT), None);
        assert_eq!(v2_subscribe_verdict(STATUS_UPDATE), None);
        assert_eq!(v2_subscribe_verdict("garbage"), None);
    }

    // ── REST request/response handling (in-memory seam, no network) ─────────

    #[test]
    fn rest_requests_are_minimal_gets_with_close_semantics() {
        let bytes = rest_request_bytes("/0/public/OHLC?pair=XBTUSD&interval=1");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.starts_with("GET /0/public/OHLC?pair=XBTUSD&interval=1 HTTP/1.1"),
            "{text}"
        );
        assert!(text.contains("Host: api.kraken.com"), "{text}");
        assert!(text.contains("Connection: close"), "{text}");
        // No credentials in the request, ever (public endpoints only).
        assert!(
            !text.to_ascii_lowercase().contains("authorization"),
            "{text}"
        );
    }

    #[test]
    fn rest_replies_decode_the_envelope_and_quote_kraken_errors() {
        let ok = http200(r#"{"error": [], "result": {"BTC/USD": []}}"#);
        assert!(decode_rest_reply(&ok, "/0/public/OHLC").is_ok());

        let business_error = http200(r#"{"error": ["EQuery:Unknown asset pair"],"result": {}}"#);
        let err = decode_rest_reply(&business_error, "/0/public/AssetPairs?pair=NOPE").unwrap_err();
        assert!(
            err.to_string().contains("EQuery:Unknown asset pair"),
            "{err}"
        );

        let non_2xx =
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let err = decode_rest_reply(non_2xx, "/0/public/OHLC").unwrap_err();
        assert!(err.to_string().contains("503"), "{err}");

        let no_json = http200("<html>proxy failure</html>");
        let err = decode_rest_reply(&no_json, "/0/public/OHLC").unwrap_err();
        assert!(err.to_string().contains("no JSON envelope"), "{err}");
    }

    // ── candles: shaping rules over the captured 12-row body ─────────────────

    fn ohlc_value() -> Value {
        serde_json::from_str(OHLC_REPLY_BODY).expect("fixture parses")
    }

    #[test]
    fn parse_candles_keeps_every_committed_row_oldest_first() {
        let bars = parse_candles(&ohlc_value(), 60, 1_791_319_621).expect("all committed");
        assert_eq!(bars.len(), 12);
        // First row (OS-verified: ts 1791318900, close "85624.2").
        let first = bars.first().expect("a bar");
        assert_eq!(
            first
                .timestamp()
                .duration_since(UNIX_EPOCH)
                .expect("post-epoch fixture")
                .as_secs(),
            1_791_318_900
        );
        assert!((first.close() - 85624.2).abs() < f64::EPSILON);
        // Last row kept when committed (ts 1791319560, close "85643.5").
        let last = bars.last().expect("a bar");
        assert!((last.close() - 85643.5).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_candles_drops_the_still_forming_last_candle() {
        // `now` mid-way through the last candle (end 1791319620 > now): that
        // row must not reach callers — pricing it would trade a partial bar.
        let bars = parse_candles(&ohlc_value(), 60, 1_791_319_590).expect("parsed");
        assert_eq!(bars.len(), 11, "the forming last row is dropped");
        // The newest surviving candle is the previous one (ts 1791319500,
        // close "85639.1").
        let last = bars.last().expect("a bar");
        assert!((last.close() - 85639.1).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_candles_reports_kraken_errors_and_bad_shapes() {
        let err_envelope =
            serde_json::json!({"error": ["EGeneral:Invalid arguments"], "result": {}});
        let err = parse_candles(&err_envelope, 60, 1_791_319_621).unwrap_err();
        assert!(
            err.to_string().contains("EGeneral:Invalid arguments"),
            "{err}"
        );

        let empty_result = serde_json::json!({"error": [], "result": {}});
        let err = parse_candles(&empty_result, 60, 1_791_319_621).unwrap_err();
        assert!(err.to_string().contains("no market data"), "{err}");

        // Everything still uncommitted: no bars at all.
        let none = parse_candles(&ohlc_value(), 60, 1_791_318_900).unwrap_err();
        assert!(none.to_string().contains("no committed candles"), "{none}");
    }

    // ── pair resolution: the pure matcher over captured table shapes ─────────

    fn captured_tables() -> (Value, Value) {
        // Shapes mirror the live AssetPairs/Assets replies (entries trimmed to
        // the fields the resolver reads): XXBTZUSD is Kraken's BTC/USDC market.
        let pairs = serde_json::json!({
            "XXBTZUSD": { "altname": "XBTUSD", "base": "XXBT", "quote": "ZUSD" },
            "SOLUSDT":  { "altname": "SOLUSDT", "base": "SOL", "quote": "USDT" },
            // Asset unknown to the Assets table: no display name, still resolvable
            // by altname/internal key.
            "MXYUSD":   { "altname": "MXYUSD", "base": "MMXY", "quote": "ZUSD" }
        });
        let assets = serde_json::json!({
            "XXBT": { "altname": "XBT" },
            "ZUSD": { "altname": "USD" },
            "USDT": { "altname": "USDT" },
            "SOL":  { "altname": "SOL" }
        });
        (pairs, assets)
    }

    #[test]
    fn every_user_form_of_a_pair_resolves_to_both_canonical_names() {
        let (pairs, assets) = captured_tables();
        for input in ["BTC/USD", "XBT/USD", " xbt/usd ", "xbtusd", "xxbtzUSD"] {
            let normalized = normalize_pair_input(input);
            let resolved = resolve_from_tables(&pairs, &assets, &normalized)
                .unwrap_or_else(|| panic!("{input:?} must resolve"));
            assert_eq!(
                resolved,
                PairResolution {
                    altname: "XBTUSD".into(),
                    ws_symbol: "BTC/USD".into()
                },
                "{input:?}"
            );
        }
    }

    #[test]
    fn resolution_without_asset_lookups_derives_altname_markets_but_keeps_them_displayable_only_when_possible(
    ) {
        // (Name folded to one line for the test harness; see body.) The market
        // whose assets are unknown cannot yield a display name: altname and
        // internal forms still resolve nothing, because every resolution needs
        // the ws pair — so both must miss rather than guess.
        let (pairs, assets) = captured_tables();
        assert!(resolve_from_tables(&pairs, &assets, "MXYUSD").is_none());
        assert!(resolve_from_tables(&pairs, &assets, "MMXYZUSD").is_none());
    }

    #[test]
    fn unknown_pairs_resolve_to_nothing() {
        let (pairs, assets) = captured_tables();
        assert!(resolve_from_tables(&pairs, &assets, "DOGE/NOTHING").is_none());
        // An altname that exists on Kraken but is absent from this table
        // snapshot must not resolve either: no match beats a wrong market.
        assert!(resolve_from_tables(&pairs, &assets, "XBTUSDT").is_none());
        assert!(resolve_from_tables(&pairs, &assets, "").is_none());
        assert!(resolve_from_tables(&pairs, &assets, "/").is_none());
    }

    #[test]
    fn settings_validation_rejects_malformed_pairs_and_intervals() {
        let bad_ws = KrakenSettings::new(
            PairResolution {
                altname: "XBTUSD".into(),
                ws_symbol: "NOSLASH".into(),
            },
            60,
        );
        assert!(bad_ws.validate().is_err());

        let bad_interval = KrakenSettings::new(
            PairResolution {
                altname: "XBTUSD".into(),
                ws_symbol: "BTC/USD".into(),
            },
            42,
        );
        assert!(bad_interval.validate().is_err());

        let ok = KrakenSettings::new(
            PairResolution {
                altname: "XBTUSD".into(),
                ws_symbol: "BTC/USD".into(),
            },
            60,
        );
        assert!(ok.validate().is_ok());
        // Debug carries no secrets to leak (there are none) but is honest.
        let rendered = format!("{ok:?}");
        assert!(rendered.contains("BTC/USD"), "{rendered}");
    }
}
