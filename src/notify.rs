//! Telegram delivery for the live session's daily summaries.
//!
//! One job: POST a message to `api.telegram.org/bot<token>/sendMessage` and
//! report whether Telegram accepted it. Delivery is **always best-effort** —
//! the caller logs a failure and keeps trading, because a notification channel
//! being down must never cost market data or stop a session.
//!
//! # Why a hand-rolled HTTPS POST and not an HTTP client crate
//!
//! This crate ships as a statically linked musl binary in a `FROM scratch`
//! image ([`docs/container-image-and-release.md`]), whose binding rules are
//! "static" and "`rustls`, never OpenSSL/native-tls". The feed already pulls in
//! `rustls`, `tokio-rustls` and `webpki-roots` (bundled CA roots — scratch has
//! no `/etc/ssl/certs`), so sending one JSON POST over that same stack adds
//! **no new transitive dependency**, while a general HTTP client would pull in
//! hyper/tower and a second TLS provider. The protocol surface needed here is
//! genuinely one request and one JSON response, and it is unit-tested against
//! an in-memory duplex stream rather than the network.
//!
//! # Secrets
//!
//! The bot token travels in the request path, so it is never logged, never put
//! in an error message, and redacted from this type's [`std::fmt::Debug`].
//! Telegram's own error text is surfaced (it names the chat id or the method,
//! never the token).

use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_rustls::TlsConnector;

use crate::config::Config;
use crate::error::Error;

/// Telegram's Bot API host. Fixed on purpose: this is not a knob users need,
/// and pinning it keeps the TLS server name and the `Host` header in agreement.
pub const TELEGRAM_HOST: &str = "api.telegram.org";

/// HTTPS port for [`TELEGRAM_HOST`].
const TELEGRAM_PORT: u16 = 443;

/// Telegram rejects messages longer than this (Bot API `sendMessage` limit).
const MAX_MESSAGE_CHARS: usize = 4096;

/// Whole-request budget: DNS/TCP, TLS handshake, write and read. Telegram is
/// normally sub-second; this only exists so a hung endpoint cannot stall the
/// summary consumer forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Grace for the TCP connect alone, so a dead network fails fast instead of
/// consuming the whole request budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A configured Telegram destination.
///
/// The [`std::fmt::Debug`] implementation is manual so `token` is always
/// redacted (mirrors `Config` and `feed::FeedSettings`): an accidental `{:?}`
/// in a log line or an assertion failure can never leak the credential.
#[derive(Clone)]
pub struct TelegramNotifier {
    /// Bot token from `BotFather`. Secret — redacted in `Debug`, never logged.
    token: String,
    /// Destination chat id (may be negative for groups/channels).
    chat_id: String,
}

impl TelegramNotifier {
    /// Builds a notifier from resolved configuration.
    ///
    /// Returns `Ok(None)` when **neither** Telegram setting is present — daily
    /// summaries then go to the console only, which is a supported
    /// configuration, not an error.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when exactly one of the two settings is present (a
    /// half-configured notifier is a typo, not an intent), or when either
    /// value is blank/whitespace.
    pub fn from_config(config: &Config) -> Result<Option<Self>, Error> {
        match (&config.telegram_bot_token, &config.telegram_chat_id) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(Error::Config(
                "telegram_bot_token is set but telegram_chat_id is not; set \
                 PRICE_ACTION_TELEGRAM_CHAT_ID (or unset the token) — summaries need both"
                    .to_string(),
            )),
            (None, Some(_)) => Err(Error::Config(
                "telegram_chat_id is set but telegram_bot_token is not; set \
                 PRICE_ACTION_TELEGRAM_BOT_TOKEN (or unset the chat id) — summaries need both"
                    .to_string(),
            )),
            (Some(token), Some(chat_id)) => {
                let notifier = Self {
                    token: token.clone(),
                    chat_id: chat_id.clone(),
                };
                notifier.validate()?;
                Ok(Some(notifier))
            }
        }
    }

    /// Cheap sanity checks before touching the network.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the token or chat id is empty/whitespace, or the
    /// token contains whitespace (it travels in the request path, where a
    /// space would corrupt the request line — and never reveals the value).
    pub fn validate(&self) -> Result<(), Error> {
        if self.token.trim().is_empty() {
            return Err(Error::Config(
                "telegram_bot_token is empty (set PRICE_ACTION_TELEGRAM_BOT_TOKEN)".to_string(),
            ));
        }
        if self.token.chars().any(char::is_whitespace) {
            return Err(Error::Config(
                "telegram_bot_token contains whitespace; a BotFather token has none".to_string(),
            ));
        }
        if self.chat_id.trim().is_empty() {
            return Err(Error::Config(
                "telegram_chat_id is empty (set PRICE_ACTION_TELEGRAM_CHAT_ID)".to_string(),
            ));
        }
        Ok(())
    }

    /// The chat id this notifier delivers to. Safe to log (it is not a
    /// credential, and Telegram's own error messages quote it anyway).
    #[must_use]
    pub fn chat_id(&self) -> &str {
        &self.chat_id
    }

    /// Sends `text` to the configured chat.
    ///
    /// # Errors
    ///
    /// [`Error::Execution`] when the connection, TLS handshake, write or read
    /// fails or times out, and [`Error::MarketData`] is never used here — a
    /// Telegram rejection (bad chat id, revoked token, rate limit) also comes
    /// back as [`Error::Execution`] quoting Telegram's own `description`. None
    /// of these messages contain the token.
    pub async fn send(&self, text: &str) -> Result<(), Error> {
        let request = self.request_bytes(text);
        let stream = connect_tls().await?;
        let response = tokio::time::timeout(REQUEST_TIMEOUT, send_over(stream, &request))
            .await
            .map_err(|_elapsed| {
                Error::Execution(format!(
                    "Telegram did not answer within {}s",
                    REQUEST_TIMEOUT.as_secs()
                ))
            })??;
        verdict_from_response(&response)
    }

    /// The exact bytes to put on the wire: a `sendMessage` POST with the
    /// message as a JSON body (so newlines and any user text are escaped by
    /// `serde_json`, never by hand) and `Connection: close` so the response
    /// ends at EOF.
    fn request_bytes(&self, text: &str) -> Vec<u8> {
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": fit_message(text),
            // A summary is plain text; a preview would fetch links for nothing.
            "disable_web_page_preview": true,
        })
        .to_string();

        let mut request = format!(
            "POST /bot{token}/sendMessage HTTP/1.1\r\n\
             Host: {TELEGRAM_HOST}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             User-Agent: price-action/paper-trading\r\n\
             \r\n",
            token = self.token,
            len = body.len(),
        )
        .into_bytes();
        request.extend_from_slice(body.as_bytes());
        request
    }
}

impl std::fmt::Debug for TelegramNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramNotifier")
            .field("token", &"[redacted]")
            .field("chat_id", &self.chat_id)
            .finish()
    }
}

/// Appended to a message trimmed to [`MAX_MESSAGE_CHARS`], so a reader can
/// tell a truncated summary from a complete one. Module-level so the tests
/// assert against the real constant rather than a hand-typed copy.
const TRUNCATION_MARK: &str = " …[truncated]";

/// Trims `text` to Telegram's message limit, marking that it was trimmed.
/// Char-based (never byte slicing) so a multi-byte character is not split.
fn fit_message(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE_CHARS {
        return text.to_string();
    }
    let keep = MAX_MESSAGE_CHARS.saturating_sub(TRUNCATION_MARK.chars().count());
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(TRUNCATION_MARK);
    out
}

/// Establishes a TLS connection to Telegram over `rustls` with **bundled**
/// webpki roots and the `ring` provider selected explicitly.
///
/// The provider is passed to `builder_with_provider` rather than relying on
/// the process default, so this path cannot panic if the global default is
/// ever unset (the panic lints are deny in this crate).
///
/// # Errors
///
/// [`Error::Execution`] when the host name is not usable as a TLS server name,
/// the TCP connect fails or times out, or the TLS handshake fails.
async fn connect_tls() -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    // `builder_with_provider` selects ring explicitly, so this path cannot hit
    // the process-default-provider panic a bare `builder()` can. It returns the
    // `WantsVersions` state, hence the explicit protocol-version step.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let client_config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Execution(format!("cannot build a TLS client config: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();

    let domain =
        rustls::pki_types::ServerName::try_from(TELEGRAM_HOST.to_string()).map_err(|_| {
            Error::Execution(format!("`{TELEGRAM_HOST}` is not a valid TLS server name"))
        })?;

    let tcp = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect((TELEGRAM_HOST, TELEGRAM_PORT)),
    )
    .await
    .map_err(|_elapsed| {
        Error::Execution(format!(
            "cannot reach {TELEGRAM_HOST}:{TELEGRAM_PORT} within {}s",
            CONNECT_TIMEOUT.as_secs()
        ))
    })?
    .map_err(|e| Error::Execution(format!("cannot reach {TELEGRAM_HOST}:{TELEGRAM_PORT}: {e}")))?;

    TlsConnector::from(Arc::new(client_config))
        .connect(domain, tcp)
        .await
        .map_err(|e| Error::Execution(format!("TLS handshake with {TELEGRAM_HOST} failed: {e}")))
}

/// Writes one request and reads the whole response off an already-established
/// stream.
///
/// Split out from [`TelegramNotifier::send`] because it is the seam tests
/// inject: a `tokio::io::duplex` pair stands in for the socket, so response
/// handling is covered without a network (or a bot token).
///
/// # Errors
///
/// [`Error::Execution`] on write/read failure, or when the peer returns bytes
/// that are not valid UTF-8 (an HTTP response always is).
async fn send_over<S>(mut stream: S, request: &[u8]) -> Result<String, Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .write_all(request)
        .await
        .map_err(|e| Error::Execution(format!("cannot write the Telegram request: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| Error::Execution(format!("cannot flush the Telegram request: {e}")))?;
    // `Connection: close` means the response ends at EOF.
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| Error::Execution(format!("cannot read the Telegram response: {e}")))?;
    String::from_utf8(raw).map_err(|e| {
        Error::Execution(format!(
            "the Telegram response was not valid UTF-8 ({} bytes discarded)",
            e.utf8_error().error_len().unwrap_or(0)
        ))
    })
}

/// Decides whether Telegram accepted the message.
///
/// # Errors
///
/// [`Error::Execution`] when the response has no status line, is not a 2xx, or
/// carries `"ok": false`. The message quotes Telegram's own `description` and
/// `error_code` — never the request, which contains the token.
fn verdict_from_response(response: &str) -> Result<(), Error> {
    let status_ok = status_line_is_success(response).ok_or_else(|| {
        Error::Execution("the Telegram response carried no readable HTTP status line".to_string())
    })?;

    // Telegram always answers with a JSON envelope; a non-JSON body (a proxy
    // error page, say) is only worth reporting alongside the status.
    match json_body(response).and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok())
    {
        Some(value) if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => Ok(()),
        Some(value) => {
            let description = value
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no description given");
            let code = value
                .get("error_code")
                .and_then(serde_json::Value::as_i64)
                .map_or_else(String::new, |c| format!(" (error_code {c})"));
            Err(Error::Execution(format!(
                "Telegram refused the message: {description}{code}"
            )))
        }
        None if status_ok => Err(Error::Execution(
            "Telegram returned 2xx but no parseable JSON envelope".to_string(),
        )),
        None => Err(Error::Execution(format!(
            "Telegram returned a non-success HTTP status ({})",
            status_text(response)
        ))),
    }
}

/// True when the first line of `response` is an HTTP status line with a 2xx
/// code; `None` when there is no first line at all.
fn status_line_is_success(response: &str) -> Option<bool> {
    let line = response.lines().next()?;
    // "HTTP/1.1 200 OK" → the status code is the second whitespace-separated
    // field. Parsed, never sliced, so a malformed line just reads as failure.
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

/// The JSON envelope of an HTTP response, tolerant of both `Content-Length`
/// and `chunked` framing: the envelope is the outermost `{ … }` in whatever
/// follows the header block.
fn json_body(response: &str) -> Option<&str> {
    let body = match response.split_once("\r\n\r\n") {
        Some((_head, body)) => body,
        None => response.split_once("\n\n").map_or(response, |(_h, b)| b),
    };
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end < start {
        return None;
    }
    // `get` rather than slicing: `string_slice`/`indexing_slicing` are deny
    // lints outside tests, and a range miss should read as "no body".
    body.get(start..=end)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)] // bounded test offsets

    use super::*;
    use tokio::io::duplex;

    /// A notifier with an obviously fake token. The value must never appear in
    /// a log, an error, or a `Debug` rendering — several tests assert that.
    fn notifier() -> TelegramNotifier {
        TelegramNotifier {
            token: "FAKE-TOKEN-DO-NOT-LOG".to_string(),
            chat_id: "-1001234567890".to_string(),
        }
    }

    /// A minimal HTTP/1.1 response with `body` as its JSON envelope.
    fn http_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            len = body.len()
        )
    }

    /// Runs one request/response exchange over an in-memory duplex pair: the
    /// injected transport seam, so no test needs a socket or a real token.
    /// Returns the client-side verdict plus the request the "server" received.
    async fn exchange(server_response: String) -> (Result<(), Error>, String) {
        let (client, mut server) = duplex(16 * 1024);
        let request = notifier().request_bytes("price-action paper trading — AAPL — 2021-01-19");
        let responder = tokio::spawn(async move {
            // Read until the whole request (headers + declared body) has
            // arrived, then answer and close so the client sees EOF.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = server.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
                let complete = std::str::from_utf8(&buf)
                    .is_ok_and(|text| buf.len() >= expected_request_len(text));
                if complete {
                    break;
                }
            }
            server.write_all(server_response.as_bytes()).await.unwrap();
            server.shutdown().await.unwrap();
            String::from_utf8_lossy(&buf).to_string()
        });
        // `send_over` yields an owned String; the verdict parser borrows.
        let result = send_over(client, &request)
            .await
            .and_then(|response| verdict_from_response(&response));
        let seen_request = responder.await.unwrap_or_default();
        (result, seen_request)
    }

    /// The total request length implied by a partially-read request's own
    /// `Content-Length` header (so the test server knows when the body is in).
    fn expected_request_len(text: &str) -> usize {
        let headers_end = text.find("\r\n\r\n").map_or(0, |i| i + 4);
        let declared = text
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        headers_end + declared
    }

    #[test]
    fn the_request_is_a_sendmessage_post_with_the_body_escaped() {
        let request = notifier().request_bytes("line one\nline two \"quoted\"");
        let text = String::from_utf8_lossy(&request);
        let (head, body) = text.split_once("\r\n\r\n").expect("header/body split");

        assert!(head.starts_with("POST /bot"), "{head}");
        assert!(head.contains("/sendMessage HTTP/1.1"), "{head}");
        assert!(head.contains("Host: api.telegram.org"), "{head}");
        assert!(head.contains("Connection: close"), "{head}");
        assert!(head.contains("Content-Type: application/json"), "{head}");

        // The JSON envelope carries the chat id and escapes newlines itself.
        let value: serde_json::Value = serde_json::from_str(body).expect("valid JSON body");
        assert_eq!(value["chat_id"], "-1001234567890");
        assert_eq!(value["text"], "line one\nline two \"quoted\"");
        assert!(
            !body.contains('\n'),
            "newlines must be escaped, not literal"
        );

        // Content-Length must match the body exactly, or the request hangs.
        let declared = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .expect("Content-Length present");
        assert_eq!(declared.trim().parse::<usize>().unwrap(), body.len());
    }

    #[test]
    fn the_token_never_leaks_into_debug_renderings() {
        let debug = format!("{:?}", notifier());
        assert!(debug.contains("[redacted]"), "{debug}");
        assert!(!debug.contains("FAKE-TOKEN-DO-NOT-LOG"), "{debug}");
        // The chat id is not a credential and is useful in logs.
        assert!(debug.contains("-1001234567890"), "{debug}");
    }

    #[test]
    fn messages_longer_than_telegrams_limit_are_trimmed_not_split() {
        // Multi-byte characters throughout: trimming must stay on char
        // boundaries rather than cutting a UTF-8 sequence in half.
        let long: String = "é".repeat(MAX_MESSAGE_CHARS + 500);
        let fitted = fit_message(&long);
        assert!(
            fitted.chars().count() <= MAX_MESSAGE_CHARS,
            "{}",
            fitted.chars().count()
        );
        assert!(fitted.ends_with(TRUNCATION_MARK), "{fitted}");
        // Nothing but the original text and the mark survived the trim.
        assert!(fitted
            .chars()
            .all(|c| c == 'é' || TRUNCATION_MARK.contains(c)));
        // Exactly at the limit, not under it.
        assert_eq!(fitted.chars().count(), MAX_MESSAGE_CHARS);

        // Short text passes through untouched.
        assert_eq!(fit_message("short"), "short");
        let exact: String = "x".repeat(MAX_MESSAGE_CHARS);
        assert_eq!(fit_message(&exact), exact);
    }

    #[test]
    fn a_success_envelope_is_accepted() {
        let response = http_response("200 OK", r#"{"ok":true,"result":{"message_id":42}}"#);
        assert!(verdict_from_response(&response).is_ok());
    }

    #[test]
    fn a_refusal_quotes_telegram_not_the_request() {
        let response = http_response(
            "400 Bad Request",
            r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#,
        );
        let err = verdict_from_response(&response).unwrap_err().to_string();
        assert!(err.contains("chat not found"), "{err}");
        assert!(err.contains("400"), "{err}");
        assert!(!err.contains("FAKE-TOKEN"), "{err}");
        assert!(
            !err.contains("sendMessage"),
            "the path holds the token: {err}"
        );
    }

    #[test]
    fn rate_limits_and_server_errors_are_reported_as_failures() {
        let limited = http_response(
            "429 Too Many Requests",
            r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 3"}"#,
        );
        assert!(verdict_from_response(&limited).is_err());
        let broken = http_response("500 Internal Server Error", "<html>oops</html>");
        let err = verdict_from_response(&broken).unwrap_err().to_string();
        assert!(err.contains("500"), "{err}");
    }

    #[test]
    fn malformed_responses_fail_without_panicking() {
        assert!(verdict_from_response("").is_err());
        assert!(verdict_from_response("not http at all").is_err());
        // 2xx but no parseable envelope: still a failure, not a silent success.
        let empty_ok = http_response("200 OK", "");
        assert!(verdict_from_response(&empty_ok).is_err());
        // A chunked body still yields the envelope.
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1f\r\n{\"ok\":true,\"result\":{}}\r\n0\r\n\r\n";
        assert!(verdict_from_response(chunked).is_ok());
    }

    #[test]
    fn json_body_tolerates_both_framings() {
        let plain = http_response("200 OK", r#"{"ok":true}"#);
        assert_eq!(json_body(&plain), Some(r#"{"ok":true}"#));
        let no_headers = r#"{"ok":true}"#;
        assert_eq!(json_body(no_headers), Some(no_headers));
        assert_eq!(json_body("HTTP/1.1 200 OK\r\n\r\n"), None);
    }

    #[test]
    fn config_with_neither_setting_means_console_only() {
        let config = Config::default();
        assert!(config.telegram_bot_token.is_none());
        assert!(config.telegram_chat_id.is_none());
        assert!(TelegramNotifier::from_config(&config).unwrap().is_none());
    }

    #[test]
    fn a_half_configured_notifier_is_rejected_naming_the_missing_half() {
        let token_only = Config {
            telegram_bot_token: Some("abc".to_string()),
            ..Config::default()
        };
        let err = TelegramNotifier::from_config(&token_only)
            .unwrap_err()
            .to_string();
        assert!(err.contains("telegram_chat_id"), "{err}");
        assert!(!err.contains("abc"), "the token must not be echoed: {err}");

        let chat_only = Config {
            telegram_chat_id: Some("123".to_string()),
            ..Config::default()
        };
        let err = TelegramNotifier::from_config(&chat_only)
            .unwrap_err()
            .to_string();
        assert!(err.contains("telegram_bot_token"), "{err}");
    }

    #[test]
    fn blank_and_whitespace_values_are_rejected() {
        let blank = Config {
            telegram_bot_token: Some("   ".to_string()),
            telegram_chat_id: Some("123".to_string()),
            ..Config::default()
        };
        assert!(TelegramNotifier::from_config(&blank).is_err());

        let spaced_token = TelegramNotifier {
            token: "has space".to_string(),
            chat_id: "123".to_string(),
        };
        let err = spaced_token.validate().unwrap_err().to_string();
        assert!(err.contains("whitespace"), "{err}");
        assert!(!err.contains("has space"), "{err}");
    }

    #[tokio::test]
    async fn an_accepted_message_is_delivered_over_the_injected_transport() {
        let (result, seen_request) = exchange(http_response(
            "200 OK",
            r#"{"ok":true,"result":{"message_id":1}}"#,
        ))
        .await;
        assert!(result.is_ok(), "{}", result.unwrap_err());
        // What actually went over the wire is a sendMessage POST. The request
        // the *server* received necessarily contains the token — which is
        // exactly why no log line or error may ever quote it.
        assert!(seen_request.starts_with("POST /bot"), "{seen_request}");
        assert!(
            seen_request.contains("/sendMessage HTTP/1.1"),
            "{seen_request}"
        );
        assert!(
            seen_request.contains("price-action paper trading"),
            "{seen_request}"
        );
    }

    #[tokio::test]
    async fn a_refusal_over_the_injected_transport_is_reported_as_an_error() {
        let (result, _seen) = exchange(http_response(
            "401 Unauthorized",
            r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#,
        ))
        .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unauthorized"), "{err}");
        assert!(
            !err.contains("POST"),
            "the request line must not be echoed: {err}"
        );
        assert!(!err.contains("FAKE-TOKEN"), "{err}");
    }
}
