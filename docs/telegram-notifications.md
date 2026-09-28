---
type: Reference
title: Telegram Notifications
description: How the live session's daily summaries are delivered to Telegram — configuration, the sendMessage request, secret handling, non-fatal delivery, and why the HTTPS client is hand-rolled.
tags: [telegram, notifications, delivery, tls, secrets]
status: draft
sources:
  - id: notifyrs
    resource: /src/notify.rs
    title: Notifier source (request building, TLS connect, response verdict, tests)
  - id: configsrc
    resource: /src/config.rs
    title: Telegram configuration keys and their redacting Debug impl
  - id: mainrs
    resource: /src/main.rs
    title: Summary consumer task that prints and delivers each DailySummary
  - id: livers
    resource: /src/live.rs
    title: DailySummary (the payload) and the channel it is emitted over
generated: { by: pi-agent/qwen3.8-max, at: 2026-09-28T02:25:00Z }
---

A [live session](live-market-data-session.md) produces one
[`DailySummary`](../src/live.rs) per closed UTC day (plus a `partial` one at
shutdown). `src/notify.rs` is the only thing that can put one somewhere other
than the console: it POSTs the rendered summary to the Telegram Bot API.[^notifyrs][^livers]

Delivery is **always best-effort**. A Telegram outage, a revoked token, a wrong
chat id or a rate limit produces one log line — never a lost bar, a stopped
session or a failed run.[^notifyrs][^mainrs]

## Configuration

Two keys, both optional, both in the normal three-layer precedence
([Configuration](configuration.md)):[^configsrc]

| Setting | Env var | Config-file key | Default |
|---|---|---|---|
| Telegram bot token | `PRICE_ACTION_TELEGRAM_BOT_TOKEN` | `telegram_bot_token` | _(unset)_ |
| Telegram chat id | `PRICE_ACTION_TELEGRAM_CHAT_ID` | `telegram_chat_id` | _(unset)_ |

The combination rules are enforced by `TelegramNotifier::from_config`, which the
`live` subcommand calls before any bar arrives:[^notifyrs]

| Token | Chat id | Result |
|---|---|---|
| unset | unset | `Ok(None)` — **console-only delivery**, a supported configuration, not an error |
| set | unset | `Err`, naming `telegram_chat_id` as the missing half |
| unset | set | `Err`, naming `telegram_bot_token` as the missing half |
| set | set | `Ok(Some(notifier))`, then `validate()` |

A half-configured notifier is treated as a typo rather than an intent, and the
error **names the missing key without echoing the value that is present** — a
test pins that, because the token must never appear in a message.[^notifyrs]

`validate()` rejects a blank/whitespace token, a token containing whitespace (it
travels in the request path, where a space would corrupt the request line), and
a blank chat id. The host is **not** configurable: `api.telegram.org:443` is
pinned so the TLS server name and the `Host` header cannot disagree.[^notifyrs]

Get a token from `@BotFather` and a chat id from the bot's `getUpdates`. Both
belong in the environment or a mounted secret, never in a committed TOML file
(see the secrets rule in [AGENTS.md](../AGENTS.md)).

## What is sent

The body is the summary's own `render()` output — the same text printed to the
console, so the two views cannot drift:[^livers]

```text
price-action paper trading — AAPL — 2021-01-19
bars: 390   entries: 2   exits: 1
fees paid: 1.23
realized P/L (net of fees): -12.50
ending balance: cash 9987.50 / equity 9990.25
open position: Short
paper account only — no orders were placed, not financial advice.
```

The trailing line is load-bearing: a summary that travelled to a phone must
still say it is a paper account and not advice. See
[Disclaimer & Risk](disclaimer-risk.md).[^livers]

Messages longer than Telegram's 4096-character `sendMessage` limit are trimmed
on **character** boundaries (never byte slicing, so a multi-byte character is
not split in half) and marked ` …[truncated]`. A daily summary is ~8 lines, so
this only ever fires on an abnormal rendering.[^notifyrs]

## The request

One HTTP/1.1 request, `Connection: close` so the response ends at EOF:[^notifyrs]

```http
POST /bot<token>/sendMessage HTTP/1.1
Host: api.telegram.org
Content-Type: application/json
Content-Length: <n>
Connection: close
User-Agent: price-action/paper-trading

{"chat_id":"<chat id>","text":"<summary>","disable_web_page_preview":true}
```

The JSON envelope is built with `serde_json::json!`, so newlines and any
user-supplied text are escaped by the serializer rather than by hand.
`disable_web_page_preview` is set because a summary is plain text and a preview
would fetch links for nothing. `Content-Length` is the body's byte length; a
test asserts it matches exactly, because a wrong value would hang the
exchange.[^notifyrs]

## Transport and timeouts

TLS is `rustls` with the **`ring`** provider selected explicitly via
`builder_with_provider`, and **bundled `webpki-roots`** for the CA store —
`FROM scratch` has no `/etc/ssl/certs`, and no OpenSSL/native-tls is permitted
anywhere in this repo ([Container Image & Release](container-image-and-release.md)).
Selecting the provider explicitly rather than relying on the process default is
what keeps this path out of rustls's "no `CryptoProvider`" panic, which the
crate's deny-by-default panic lints would otherwise be papering over.[^notifyrs]

| Budget | Value | Why |
|---|---|---|
| TCP connect | 10s | A dead network fails fast instead of eating the whole request budget |
| Whole request (handshake + write + read) | 20s | Bounds a hung endpoint so the summary consumer cannot stall forever |

Both are per delivery attempt; there is no retry. A missed daily summary is
logged, and the next day's summary still goes out.[^notifyrs]

## Response handling

Telegram always answers with a JSON envelope. The verdict parser tolerates both
`Content-Length` and `chunked` framing by taking the outermost `{ … }` after the
header block, and it never panics on garbage:[^notifyrs]

| Response | Outcome |
|---|---|
| 2xx with `"ok":true` | Success |
| 2xx with no parseable envelope | Failure — a 2xx is not trusted on its own |
| `"ok":false` | Failure quoting Telegram's own `description` and `error_code` (e.g. `Bad Request: chat not found`, `Unauthorized`, `Too Many Requests: retry after 3`) |
| Non-2xx with no envelope | Failure quoting the HTTP status text |
| No readable status line, non-UTF-8 bytes | Failure |

Error text deliberately quotes **Telegram's** words, never the request: the
request line contains the token, so echoing it would leak a credential into a
log. Tests assert a refusal error contains neither the token nor `POST`.[^notifyrs]

## Why the HTTPS client is hand-rolled

A general HTTP client would be the normal choice; here it is not, for reasons
specific to this repo:[^notifyrs]

1. **Zero new transitive dependencies.** `rustls`, `tokio-rustls` and
   `webpki-roots` are already in the tree for the
   [live feed](live-market-data-session.md). Pinning them in `Cargo.toml` adds
   nothing to the graph; `reqwest` would pull in hyper/tower plus its own TLS
   provider selection.
2. **The scratch image constraint is binding.** Static musl binary, `FROM
   scratch`, rustls only. A second TLS provider is exactly the kind of change
   that breaks the image at *runtime*, where a local `docker build` will not
   catch it.
3. **The protocol surface is genuinely tiny** — one POST, one JSON response —
   and it is tested against an in-memory `tokio::io::duplex` stream, so response
   handling is covered without a network or a real token.
4. The repo's stated dependency policy is "intentionally minimal"
   ([Development Workflow](development-workflow.md)).

The trade-off is ~150 lines of HTTP/1.1 that a reviewer must read instead of a
battle-tested client. That is a deliberate, reviewable choice — if a second HTTP
endpoint is ever needed, the right move is to adopt a real client then, not to
grow this one.

## Secrets

The bot token exists in exactly three places: the resolved `Config` field, the
`TelegramNotifier` struct, and the request path of one POST. It is never
logged, never formatted into an error, and `TelegramNotifier`'s `Debug`
implementation is manual so `token` always renders as `[redacted]` — mirroring
`Config` (which redacts `broker_api_key`, `massive_api_key` **and**
`telegram_bot_token`) and `feed::FeedSettings`. The chat id *is* shown in
`Debug` and in success/failure log lines: it is not a credential, and Telegram
quotes it in its own errors anyway.[^notifyrs][^configsrc][^mainrs]

## Failure behaviour in the running binary

The summary consumer task prints each summary, then delivers it. On failure it
logs and continues to the next summary; on success it logs the day and chat id.
The session is never awaiting Telegram, because summaries travel over a bounded
channel (16 slots) consumed on a separate task — the session only blocks if that
buffer fills, which needs ~16 undelivered days.[^mainrs][^livers]

[^notifyrs]: `src/notify.rs`: module doc (rationale + secret policy), `TELEGRAM_HOST`/timeouts/`MAX_MESSAGE_CHARS`, `TelegramNotifier::{from_config, validate, send, request_bytes}`, `fit_message`, `connect_tls`, `send_over`, `verdict_from_response`, `json_body`, the redacting `Debug` impl, and its test module (duplex-injected transport, refusal/rate-limit/malformed responses, token-leak assertions)

[^configsrc]: `src/config.rs`: `telegram_bot_token` / `telegram_chat_id` in all three precedence layers, and the redacting `Debug` impl

[^mainrs]: `src/main.rs`: `run_live`'s summary consumer task (console print, then best-effort `notifier.send`, logging both outcomes)

[^livers]: `src/live.rs`: `DailySummary` (fields, `render()` and its paper-only framing), `SUMMARY_CHANNEL_SIZE`, `deliver_summary`
