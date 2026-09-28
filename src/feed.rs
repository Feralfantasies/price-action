//! The Massive.com stocks WebSocket: real-time market data for live sessions.
//!
//! This module is **transport only** — it connects, authenticates with the API
//! key, subscribes to one symbol's feed channel, and re-establishes on drops
//! with capped exponential backoff (2s → 30s). It decodes frames into typed
//! [`RawEvent`]s and hands them out through a channel; turning those events
//! into `Bar`s (deduplicating window updates, aggregating ticks, spotting data
//! holes) happens in [`crate::live`], kept socket-free so it is testable
//! without a network. No venue is ever touched: data in only.
//!
//! Wire protocol (verified against
//! <https://massive.com/docs/websocket/quickstart> and the `stocks` channel
//! pages): every server frame is a JSON **array** of event objects (a bare
//! object is accepted defensively). After connecting the server sends
//! `[{"ev":"status","status":"connected"}]`. The client then authenticates
//! (`{"action":"auth","params":"<key>"}`; success arrives as a `status` event)
//! and subscribes (`{"action":"subscribe","params":"AM.<SYM>"}`). Aggregate
//! events (`AM`) carry OHLC + volume for one **minute** window with Unix-ms
//! window start/end in `s`/`e`, re-emitted with fresh numbers as trades land
//! within the same window (same `s`) — consumers must dedupe on it. Tick
//! events (`T`) carry price `p`, size `s` and trade time `t`.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;

/// How long a connected socket may go silent before it is treated as dead and
/// the feeder reconnects. 90s comfortably covers slow minutes while bounding
/// real-drop detection.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(90);
/// Grace for handshake/status waits inside one connection attempt.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Reconnect pauses double from 2s up to this cap.
const MAX_BACKOFF_SECS: u64 = 30;
/// First (and shortest) reconnect pause; also the floor `backoff_for` applies.
const MIN_BACKOFF_SECS: u64 = 2;

/// The concrete socket type behind `connect_async`.
type WsSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// One decoded upstream event after framing is stripped.
#[derive(Debug, Clone)]
pub enum RawEvent {
    /// An aggregate window update (`AM`): full-window OHLC + volume for one
    /// minute; repeated updates share `start_ms`.
    Window(WindowUpdate),
    /// A single tick trade (`T`).
    Tick(TickTrade),
    /// The feed connection dropped; the feeder will reconnect. Gives sessions
    /// a data-hole signal to log between bars (the detailed reason has
    /// already gone to stderr with attempt numbers).
    FeedInterrupted,
}

/// One per-minute aggregate window as emitted by the feed.
#[derive(Debug, Clone)]
pub struct WindowUpdate {
    /// Symbol the window is for (as sent upstream).
    pub symbol: String,
    /// Unix ms start of the window — the dedupe key across re-emissions.
    pub start_ms: u64,
    /// Unix ms end of the window (`None` when upstream omitted it; treat as
    /// `start + one interval`).
    pub end_ms: Option<u64>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Window volume in shares.
    pub volume: f64,
}

/// One tick-level trade.
#[derive(Debug, Clone)]
pub struct TickTrade {
    /// Symbol the trade is for (as sent upstream).
    pub symbol: String,
    pub price: f64,
    /// Unix ms when the trade occurred (SIP timestamp).
    pub ts_ms: u64,
    /// Share quantity of this trade.
    pub size: f64,
}

/// One raw event object as it appears in a frame envelope. Unknown fields are
/// ignored so upstream additions never break decoding; overloading is by `ev`
/// (the same field means different things per feed type — e.g. `s` is the
/// window start on aggregates and the trade size on ticks).
#[derive(Debug, Deserialize)]
struct WireEvent {
    #[serde(default)]
    ev: Option<String>,
    #[serde(default)]
    sym: Option<String>,
    #[serde(default)]
    o: Option<f64>,
    #[serde(default)]
    h: Option<f64>,
    #[serde(default)]
    l: Option<f64>,
    #[serde(default)]
    c: Option<f64>,
    /// Window end (aggregates); Unix ms.
    #[serde(default, rename = "e")]
    window_end: Option<u64>,
    /// Unix ms: window start (`AM`/`A`). Trade size for ticks travels in `v`
    /// (tick volume), not here.
    #[serde(default)]
    s: Option<u64>,
    /// Tick price (`T`) or tick volume/trade size on both feed types; on
    /// aggregates this is the window's share volume.
    #[serde(default)]
    v: Option<u64>,
    /// Tick price (`T`).
    #[serde(default)]
    p: Option<f64>,
    /// Tick timestamp, Unix ms (`T`).
    #[serde(default)]
    t: Option<u64>,
}

/// Decodes one WebSocket text frame into the recognizable data events.
///
/// Status and unknown feed types decode to no events; frames that do not parse
/// are skipped entirely (forward progress beats one bad message).
#[must_use]
pub fn decode_frame(frame: &str) -> Vec<RawEvent> {
    let objects = match serde_json::from_str::<Vec<WireEvent>>(frame.trim()) {
        Ok(events) => events,
        // Defensive: a single bare object rather than an array.
        Err(_) => match serde_json::from_str::<WireEvent>(frame.trim()) {
            Ok(single) => vec![single],
            Err(_) => return Vec::new(),
        },
    };

    objects
        .iter()
        .filter_map(|wire| match wire.ev.as_deref() {
            Some("AM" | "A") => aggregate_event(wire),
            Some("T") => tick_event(wire),
            _ => None, // `status` and other feed types are not bar material
        })
        .collect()
}

fn aggregate_event(wire: &WireEvent) -> Option<RawEvent> {
    let (symbol, start_ms) = (wire.sym.as_deref()?.to_string(), wire.s?);
    Some(RawEvent::Window(WindowUpdate {
        symbol,
        start_ms,
        end_ms: wire.window_end,
        open: wire.o?,
        high: wire.h?,
        low: wire.l?,
        close: wire.c?,
        volume: shares_to_f64(wire.v.unwrap_or(0)), // `v` = window volume in shares
    }))
}

fn tick_event(wire: &WireEvent) -> Option<RawEvent> {
    let (symbol, ts_ms) = (wire.sym.as_deref()?.to_string(), wire.t?);
    Some(RawEvent::Tick(TickTrade {
        symbol,
        price: wire.p?,
        ts_ms,
        // On `T` events `s` is the trade size (the window-start field only
        // carries that meaning on aggregate feed types).
        size: shares_to_f64(wire.s.unwrap_or(0)),
    }))
}

/// Converts a wire share count (`u64`) into the `f64` the bar model carries,
/// without an `as` cast (`as_conversions` is warn, and CI runs `-D warnings`).
/// Counts beyond `u32` saturate rather than wrapping: a single trade or one
/// minute window larger than ~4.29e9 shares is not something a market
/// produces, and saturation keeps a bar's volume monotone and finite.
fn shares_to_f64(raw: u64) -> f64 {
    f64::from(u32::try_from(raw).unwrap_or(u32::MAX))
}

/// Which native channel feeds the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedChannel {
    /// Per-minute OHLCV windows (`AM`) — one bar per traded minute.
    Minute,
    /// Tick trades (`T`); [`crate::live`] aggregates them into 1-second bars.
    Ticks,
}

/// Connection settings for one feed. Built by [`crate::live`] from the loaded
/// `Config`; its manual [`std::fmt::Debug`] impl redacts the API key so an
/// accidental `{:?}` never leaks it (mirrors `Config`).
#[derive(Clone)]
pub struct FeedSettings {
    /// Full WebSocket URL, e.g. `wss://socket.massive.com/stocks`.
    pub url: String,
    /// Massive API key — secret; redacted in Debug output, never printed.
    pub api_key: String,
    /// Subscribe parameter: `AM.<SYM>` or `T.<SYM>` (one subscription is the
    /// documented feed limit this session respects per asset class).
    pub subscribe_param: String,
    /// Case-insensitive symbol filter for inbound events.
    pub symbol: String,
}

impl FeedSettings {
    /// Builds settings for a stocks host (`socket.massive.com` real-time or
    /// `delayed.massive.com` 15-minute delayed), an API key and channel.
    #[must_use]
    pub fn for_stocks(host: &str, api_key: String, symbol: &str, channel: FeedChannel) -> Self {
        let sym = symbol.trim().to_ascii_uppercase();
        Self {
            url: format!("wss://{}/stocks", host.trim()),
            api_key,
            subscribe_param: match channel {
                FeedChannel::Minute => format!("AM.{sym}"),
                FeedChannel::Ticks => format!("T.{sym}"),
            },
            symbol: sym,
        }
    }

    /// Cheap sanity checks before touching the network.
    ///
    /// # Errors
    ///
    /// [`crate::error::Error::MarketData`] when the URL lacks `wss://`, holds
    /// whitespace, or a required part is empty.
    pub fn validate(&self) -> Result<(), crate::error::Error> {
        if !self.url.starts_with("wss://") || self.url.chars().any(char::is_whitespace) {
            return Err(crate::error::Error::MarketData(format!(
                "feed URL must be wss:// with no whitespace: `{}`",
                self.url
            )));
        }
        if self.api_key.trim().is_empty() {
            return Err(crate::error::Error::MarketData(
                "live feed API key is empty (set PRICE_ACTION_MASSIVE_API_KEY or api_key in the config file)".into(),
            ));
        }
        if self.subscribe_param.is_empty() || self.symbol.trim().is_empty() {
            return Err(crate::error::Error::MarketData(
                "feed subscription requires a symbol".into(),
            ));
        }
        Ok(())
    }
}

impl std::fmt::Debug for FeedSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedSettings")
            .field("url", &self.url)
            .field("api_key", &"[redacted]")
            .field("subscribe_param", &self.subscribe_param)
            .field("symbol", &self.symbol)
            .finish()
    }
}

/// Installs the process-level rustls crypto provider (ring).
///
/// rustls 0.23 needs exactly one provider selected; without one it panics deep
/// inside `ClientConfig::builder()` on first TLS use. Enabling `ring` on the
/// `rustls` dependency already selects it automatically — this call is
/// belt-and-braces so a future feature change surfaces here as an
/// [`crate::error::Error::MarketData`] instead of a panic (the panic lints are
/// deny in this crate). Already-installed is not an error: the provider is
/// process-wide, and this may be called once per entry point.
///
/// # Errors
///
/// [`crate::error::Error::MarketData`] when no provider could be installed,
/// i.e. the `ring` feature is missing from the build.
pub fn install_crypto_provider() -> Result<(), crate::error::Error> {
    match rustls::crypto::ring::default_provider().install_default() {
        Ok(()) => Ok(()),
        // `Err` carries the provider back when one was already installed, which
        // is the success case for a second call in the same process.
        Err(_already_installed) => Ok(()),
    }
}

/// Spawns the feeder task.
///
/// One connection per loop pass, capped-exponential-backoff reconnects between
/// passes, forever — or until `tx`'s receiver is dropped (session shutdown),
/// which also stops the loop promptly.
#[must_use]
pub fn spawn(settings: FeedSettings, tx: mpsc::Sender<RawEvent>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut attempt = 1u32;
        loop {
            if tx.is_closed() {
                return;
            }

            match stream_once(&settings, &tx).await {
                StreamOutcome::ReceiverGone => return, // clean shutdown: no marker on exit
                StreamOutcome::Dropped(reason) => eprintln!(
                    "price-action feed: connection #{attempt} ended ({reason}); will reconnect"
                ),
            }

            // Tell the session data may have been missed until the next bar;
            // repeated failures before any bar arrive coalesce into one marker
            // there (the session owns that bookkeeping).
            if tx.send(RawEvent::FeedInterrupted).await.is_err() {
                return; // receiver went away mid-shutdown: stop quietly
            }

            if tx.is_closed() {
                return;
            }
            let delay = backoff_for(attempt);
            eprintln!(
                "price-action feed: retry in {}s (attempt {} next)",
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

/// What one connection lifetime ended in.
#[derive(Debug)]
enum StreamOutcome {
    /// The session closed its channel end — stop reconnecting, no log noise.
    ReceiverGone,
    /// The socket/endpoint failed; `reason` names it (never secrets).
    Dropped(String),
}

/// Reconnect pause before attempt `n` (1-indexed, after a failure).
///
/// 2s, 4s, 8s, 16s, 30s, 30s, … The doubling saturates at `u64::MAX` long
/// before the cap matters, so a long-running feeder cannot overflow.
#[must_use]
pub fn backoff_for(attempt: u32) -> Duration {
    let seconds = 2u64.saturating_pow(attempt).min(MAX_BACKOFF_SECS);
    Duration::from_secs(seconds.max(MIN_BACKOFF_SECS))
}

/// One connection lifetime. Returns `ReceiverGone` on clean session shutdown;
/// callers should treat it as terminal for the feeder.
async fn stream_once(settings: &FeedSettings, tx: &mpsc::Sender<RawEvent>) -> StreamOutcome {
    let connect = tokio_tungstenite::connect_async(settings.url.clone())
        .await
        .map_err(|e| format!("connect failed: {e}"));
    let mut ws = match connect {
        // The HTTP upgrade response carries nothing this feeder needs.
        Ok((ws, _response)) => ws,
        Err(reason) => return StreamOutcome::Dropped(reason),
    };

    if auth_handshake(&settings.api_key, &mut ws).await.is_err() {
        return StreamOutcome::Dropped(
            "authentication did not succeed within the deadline".to_string(),
        );
    }
    eprintln!(
        "price-action feed: connected and authenticating subscription for {} …",
        settings.subscribe_param
    );

    let sub_payload = {
        let mut obj = serde_json::Map::new();
        obj.insert("action".into(), serde_json::json!("subscribe"));
        obj.insert("params".into(), serde_json::json!(settings.subscribe_param));
        serde_json::Value::Object(obj).to_string()
    };
    if ws.send(WsMessage::Text(sub_payload.into())).await.is_err() {
        return StreamOutcome::Dropped("sending subscription failed".to_string());
    }

    loop {
        let frame = match tokio::time::timeout(SILENCE_TIMEOUT, ws.next()).await {
            Ok(Some(Ok(frame))) => frame,
            Ok(Some(Err(e))) => {
                return StreamOutcome::Dropped(format!("socket error: {e}"));
            }
            Ok(None) => return StreamOutcome::Dropped("server closed the connection".into()),
            Err(_elapsed) => {
                return StreamOutcome::Dropped(format!(
                    "no data for {}s; assuming the feed stalled",
                    SILENCE_TIMEOUT.as_secs()
                ));
            }
        };

        if tx.is_closed() {
            return StreamOutcome::ReceiverGone;
        }

        // One frame may carry several events; emit each that matches.
        for event in decode_frame(&frame_to_text(&frame)) {
            if tx.is_closed() {
                return StreamOutcome::ReceiverGone;
            }
            let symbol_match = match &event {
                RawEvent::Window(w) => w.symbol.eq_ignore_ascii_case(&settings.symbol),
                RawEvent::Tick(t) => t.symbol.eq_ignore_ascii_case(&settings.symbol),
                // Synthetic (never produced by decode_frame, sent separately);
                // covered to keep the match exhaustive.
                RawEvent::FeedInterrupted => false,
            };
            if !symbol_match {
                continue;
            }
            if tx.send(event).await.is_err() {
                return StreamOutcome::ReceiverGone;
            }
        }
    }
}

/// A `WsMessage` rendered as its UTF-8 text (non-text frames yield empty —
/// their decode result is just "no events").
fn frame_to_text(frame: &WsMessage) -> String {
    match frame {
        WsMessage::Text(t) => t.as_str().to_string(),
        // Binary frames are not part of the protocol: non-UTF-8 yields empty,
        // whose decode result is simply "no events".
        WsMessage::Binary(payload) => {
            std::str::from_utf8(payload.as_ref()).map_or_else(|_| String::new(), str::to_string)
        }
        _ => String::new(),
    }
}

/// Sends the authentication message and waits until a `status` frame
/// settles it: failure wording (unauthorized/error family) ends the attempt
/// immediately; an auth-success framing accepts. The plain `connected`
/// acknowledgement that precedes auth (still in the socket buffer when this
/// runs) is ignored — never mistaken for success.
async fn auth_handshake(api_key: &str, ws: &mut WsSocket) -> Result<(), ()> {
    let auth_payload = serde_json::json!({ "action": "auth", "params": api_key }).to_string();
    if ws.send(WsMessage::Text(auth_payload.into())).await.is_err() {
        return Err(());
    }

    loop {
        // A clean EOF or a missed deadline ends this attempt; the caller
        // reconnects rather than treating it as an auth rejection.
        let Ok(Some(Ok(frame))) = tokio::time::timeout(HANDSHAKE_TIMEOUT, ws.next()).await else {
            return Err(());
        };

        if let Some(verdict) = auth_verdict(&frame_to_text(&frame)) {
            return match verdict {
                AuthVerdict::Accepted => Ok(()),
                AuthVerdict::Rejected => Err(()),
            };
        }
    }
}

/// Classifies a frame's `status` event as an authentication outcome, if any.
/// Frames without an auth-relevant status (the pre-auth "connected" ack, data
/// frames) return `None` so the wait simply continues.
#[derive(Debug, PartialEq, Eq)]
enum AuthVerdict {
    Accepted,
    Rejected,
}

/// Joins a status frame's `status` and `message` fields into one lowercase
/// searchable string (`"auth_success authenticated"`).
fn status_phrase(event: &serde_json::Value) -> Option<String> {
    if event.get("ev").and_then(|v| v.as_str()) != Some("status") {
        return None;
    }
    let status = event
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = event
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(format!("{status} {message}"))
}

fn auth_verdict(frame_text: &str) -> Option<AuthVerdict> {
    let events = frame_events(frame_text)?;
    for event in &events {
        let Some(phrase) = status_phrase(event) else {
            continue; // not a status frame: no verdict from this object
        };
        if phrase.is_empty() {
            continue;
        }
        // Documented success wordings: "auth_success" / "authenticated"
        // (any of the documented failure wordings also count as failure, checked first).
        let failed =
            phrase.contains("unauthorized") || phrase.contains("error") || phrase.contains("fail");
        let succeeded = phrase.contains("auth_success") || phrase.contains("authenticated");
        if !failed && !succeeded {
            continue; // e.g. the pre-auth "connected" ack: keep waiting
        }
        return Some(if failed {
            AuthVerdict::Rejected
        } else {
            AuthVerdict::Accepted
        });
    }
    None
}

/// All event objects in a frame, accepting the documented array shape and, as
/// a defensive fallback, a bare object.
fn frame_events(frame_text: &str) -> Option<Vec<serde_json::Value>> {
    let trimmed = frame_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
        return Some(list);
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .filter(serde_json::Value::is_object)
        .map(|obj| vec![obj])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)] // asserts the exact decoded wire values on purpose

    use super::*;

    fn fixture(name: &str) -> &'static str {
        match name {
            "am" => {
                r#"[{"ev":"AM","sym":"AAPL","v":12345,"o":150.85,"c":152.90,"h":153.17,"l":150.50,"s":1611082800000,"e":1611082860000}]"#
            }
            "t" => {
                r#"[{"ev":"T","sym":"MSFT","p":215.9721,"s":100,"t":1611082428813},{"ev":"T","sym":"AAPL","p":150.10,"s":200,"t":1611082428814}]"#
            }
            "status" => {
                r#"[{"ev":"status","status":"connected","message":"Connected Successfully"}]"#
            }
            other => panic!("test fixture {other:?} not registered"),
        }
    }

    #[test]
    fn decodes_aggregate_events_with_window_fields() {
        let events = decode_frame(fixture("am"));
        assert_eq!(events.len(), 1);
        match &events[0] {
            RawEvent::Window(w) => {
                assert_eq!(w.symbol, "AAPL");
                assert_eq!(w.start_ms, 1_611_082_800_000);
                assert_eq!(w.end_ms, Some(1_611_082_860_000));
                assert!((w.open - 150.85).abs() < f64::EPSILON);
                assert!((w.high - 153.17).abs() < f64::EPSILON);
                assert!((w.low - 150.50).abs() < f64::EPSILON);
                assert!((w.close - 152.90).abs() < f64::EPSILON);
                assert_eq!(w.volume, 12_345.0); // wire `v` (tick volume) → share count
            }
            RawEvent::Tick(_) | RawEvent::FeedInterrupted => panic!("expected a window event"),
        }
    }

    #[test]
    fn decodes_tick_events_and_keeps_each_symbol() {
        let events = decode_frame(fixture("t"));
        assert_eq!(events.len(), 2);
        match &events[0] {
            RawEvent::Tick(t) => {
                assert_eq!(t.symbol, "MSFT");
                assert!((t.price - 215.9721).abs() < f64::EPSILON);
                assert_eq!(t.size, 100.0);
                assert_eq!(t.ts_ms, 1_611_082_428_813);
            }
            RawEvent::Window(_) | RawEvent::FeedInterrupted => panic!("expected a tick"),
        }
        match &events[1] {
            RawEvent::Tick(t) => assert_eq!(t.symbol, "AAPL"),
            _ => panic!("AAPL event lost"),
        }
    }

    #[test]
    fn status_and_bogus_frames_decode_to_nothing() {
        assert!(decode_frame(fixture("status")).is_empty());
        assert!(decode_frame(not_json()).is_empty());
        assert!(decode_frame("").is_empty());
        // Unknown feed types (quotes etc.) are ignored.
        let quotes = r#"[{"ev":"Q","sym":"AAPL","bp":150.0}]"#;
        assert!(decode_frame(quotes).is_empty());
    }

    #[test]
    fn a_bare_object_frame_is_accepted_defensively() {
        let frame =
            r#"{"ev":"AM","sym":"AAPL","v":1,"o":1.0,"c":2.0,"h":2.0,"l":1.0,"s":1000,"e":61000}"#;
        let events = decode_frame(frame);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn auth_verdict_accepts_success_and_rejects_unauthorized_ignores_connect_ack() {
        // The pre-auth connection acknowledgement must NOT settle the wait.
        let connected =
            r#"[{"ev":"status","status":"connected","message":"Connected Successfully"}]"#;
        assert_eq!(auth_verdict(connected), None, "connect ack is neutral");
        // Documented success frames (both observed wordings).
        let auth_success = r#"[{"ev":"status","status":"auth_success","message":"authenticated"}]"#;
        assert_eq!(auth_verdict(auth_success), Some(AuthVerdict::Accepted));
        let plain_authenticated = r#"[{"ev":"status","status":"authenticated"}]"#;
        assert_eq!(
            auth_verdict(plain_authenticated),
            Some(AuthVerdict::Accepted)
        );
        // Failure wordings reject.
        let bad_key = r#"[{"ev":"status","status":"unauthorized","message":"bad key"}]"#;
        assert_eq!(auth_verdict(bad_key), Some(AuthVerdict::Rejected));
        // Data frames carry no verdict at all.
        assert_eq!(auth_verdict(fixture("am")), None);
    }

    #[test]
    fn backoff_doubles_from_two_seconds_to_thirty() {
        assert_eq!(backoff_for(1), Duration::from_secs(2));
        assert_eq!(backoff_for(2), Duration::from_secs(4));
        assert_eq!(backoff_for(3), Duration::from_secs(8));
        assert_eq!(backoff_for(4), Duration::from_secs(16));
        assert_eq!(backoff_for(5), Duration::from_secs(30)); // capped
        assert_eq!(backoff_for(50), Duration::from_secs(30));
    }

    #[test]
    fn settings_validate_and_redact_debug() {
        let bad = FeedSettings {
            url: "http://socket.massive.com/stocks".into(),
            api_key: "k".into(),
            subscribe_param: "AM.AAPL".into(),
            symbol: "AAPL".into(),
        };
        assert!(matches!(
            bad.validate(),
            Err(crate::error::Error::MarketData(_))
        ));

        let good = FeedSettings {
            url: "wss://socket.massive.com/stocks".into(),
            api_key: "top-secret-key-value".into(),
            subscribe_param: "AM.AAPL".into(),
            symbol: "AAPL".into(),
        };
        assert!(good.validate().is_ok());
        let debug = format!("{good:?}");
        assert!(debug.contains("[redacted]"), "{debug}");
        assert!(!debug.contains("top-secret-key-value"), "{debug}");
    }

    #[test]
    fn for_stocks_builds_urls_and_channels() {
        let minute = FeedSettings::for_stocks(
            "socket.massive.com",
            "key".into(),
            "aapl ",
            FeedChannel::Minute,
        );
        assert_eq!(minute.url, "wss://socket.massive.com/stocks");
        assert_eq!(minute.subscribe_param, "AM.AAPL"); // trimmed + uppercased
        let ticks = FeedSettings::for_stocks(
            "delayed.massive.com",
            "key".into(),
            "NVDA",
            FeedChannel::Ticks,
        );
        assert_eq!(ticks.url, "wss://delayed.massive.com/stocks");
        assert_eq!(ticks.subscribe_param, "T.NVDA");
    }

    fn not_json() -> &'static str {
        "definitely, not; json"
    }
}
