---
type: Reference
title: Configuration
description: Three-layer configuration precedence (env vars, TOML file, compiled defaults), the complete settings table (including the live-feed and Telegram keys), and validation rules.
tags: [configuration, env-vars, toml]
status: draft
sources:
  - id: config-src
    resource: /src/config.rs
    title: Config module source (layering, validation, tests)
  - id: example-toml
    resource: /price-action.example.toml
    title: Example TOML config file
generated: { by: pi-agent/qwen3.8-max, at: 2026-09-28T14:22:52Z }
---

Trading options resolve in strict precedence order — **environment variables
override the config file, which overrides compiled defaults**.[^config-src]

1. **Environment variables** (`PRICE_ACTION_*`) — highest priority; lets container deployments override anything without touching files.
2. **Config file** — TOML at the path named by `PRICE_ACTION_CONFIG` (default: `price-action.toml` in the working directory). A **missing file is not an error**; an unreadable or malformed one is. Every key is optional; a copy of [`price-action.example.toml`](../price-action.example.toml) is committed as the reference.[^example-toml]
3. **Compiled defaults** — `Config::default()`.

> Load once at startup (`Config::load` / `Config::load_for_replay`) and pass the
> result through the application; never read `std::env` directly afterwards.

## Complete settings table

| Setting | Env var | Config-file key | Default | Meaning & constraints |
|---|---|---|---|---|
| Instrument symbol | `PRICE_ACTION_SYMBOL` | `symbol` | `AAPL` | Must be non-empty after trim. Cosmetic during replay (no orders). |
| Quantity per trade | `PRICE_ACTION_QUANTITY` | `quantity` | `1` | ≥ 1. Sizes the paper account's position: every bar's notional (and therefore committed funds, each leg's fee, the running cash/equity and every trade's P/L) scales with it, and whether an entry is affordable at all depends on it — see [Paper Trading Accounting](paper-trading-accounting.md). |
| Trading mode | `PRICE_ACTION_MODE` | `mode` | `paper` | `"paper"` or `"live"` (case-insensitive) — any other value is rejected as an unknown mode.[^config-src] Live also requires a broker URL in the normal path, and live trading is not implemented yet (see [Overview](overview.md)). Cosmetic during replay. |
| Bar interval (secs) | `PRICE_ACTION_BAR_INTERVAL_SECS` | `bar_interval_secs` | `60` | ≥ 1. Documents the interval bars are assumed to be at; affects what "consecutive" means to a strategy fed that data. Cosmetic during replay. |
| Rolling-window size | `PRICE_ACTION_SERIES_CAPACITY` | `series_capacity` | `500` | Bars retained by a `BarSeries` (see [Market Data Model](market-data-model.md)). ≥ 1; not exercised by the current strategy or replay. |
| Strategy threshold | `PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD` | `consecutive_closes_threshold` | `3` | Consecutive higher/lower closes before the example strategy signals (see [Consecutive Closes Strategy](strategy-consecutive-closes.md)). ≥ 1; one of the replay settings that changes what you see. |
| Paper starting balance | `PRICE_ACTION_STARTING_BALANCE` | `starting_balance` | `10000` | Funds the replay report's paper account (f64); must be finite and strictly greater than 0. Replay-only: no live path exists yet, so nothing else reads it. |
| Paper trade fee (basis points) | `PRICE_ACTION_TRADE_FEE_BPS` | `trade_fee_bps` | `5` | Fee charged per side of each paper trade, in basis points of that leg's notional (`bps / 10_000`; default 5 = 0.05%). ≥ 0; zero disables fees exactly. See [Replay Workflow](replay-workflow.md). |
| Broker API base URL | `PRICE_ACTION_BROKER_URL` | `broker_url` | _(unset)_ | Required when running normally with mode `live`; an empty/whitespace value is treated as absent and rejected in that case. Never needed for replay (see below). |
| Broker API key | `PRICE_ACTION_BROKER_API_KEY` | `broker_api_key` | _(unset)_ | Secret — prefer the env var or a mounted secret over committing it to a file, and never commit it to the repo. Redacted in all debug output of `Config`. |
| Config-file path | `PRICE_ACTION_CONFIG` | — | `price-action.toml` | Only an env var; names where the config file (precedence layer 2) is read from. |
| Live feed source | `PRICE_ACTION_LIVE_FEED_SOURCE` | `live_feed_source` | `massive` | Which market data `live` streams from: `"massive"` (Massive.com stocks WebSocket — authenticated, needs `massive_api_key`) or `"kraken"` (Kraken Spot WebSocket v2 `ohlc` channel — **public, no key**; bars are its candles at `bar_interval_secs`, minute channel only). Case-insensitive and trimmed; any other value is rejected naming `live_feed_source`. Live-session only. See [Live Market-Data Session](live-market-data-session.md). |
| Live feed host | `PRICE_ACTION_LIVE_FEED_HOST` | `live_feed_host` | `socket.massive.com` | Bare hostname, **no scheme, path, userinfo (`@`) or whitespace**: `live` builds `wss://<host>/stocks` from it. Use `delayed.massive.com` for the 15-minute-delayed feed. Rejected when empty or when it contains whitespace or `/ : @ \`. Live-session only. See [Live Market-Data Session](live-market-data-session.md). |
| Live feed channel | `PRICE_ACTION_LIVE_FEED_CHANNEL` | `live_feed_channel` | `minute` | `"minute"` (per-minute OHLCV windows, `AM.<SYM>`) or `"ticks"` (tick trades, `T.<SYM>`, aggregated into per-second bars). Case-insensitive and trimmed; any other value is rejected naming `live_feed_channel`. Live-session only. |
| Massive.com API key | `PRICE_ACTION_MASSIVE_API_KEY` | `massive_api_key` | _(unset)_ | **Secret.** Required by the `live` subcommand **when the source is Massive** (that feed is authenticated); never needed for replay, the no-args path, or a Kraken live session — that endpoint is public and no key is read for it. Prefer the env var or a mounted secret over committing it to a file. Redacted in all debug output of `Config`. |
| Live session CSV directory | `PRICE_ACTION_LIVE_CSV_DIR` | `live_csv_dir` | `./sessions` | Where `live` persists each session's bars as a replay-compatible CSV (`live-<SYMBOL>-<UTC stamp>.csv`, created if absent). Must not be empty. Nothing is written when a session saw no bars. |
| Telegram bot token | `PRICE_ACTION_TELEGRAM_BOT_TOKEN` | `telegram_bot_token` | _(unset)_ | **Secret.** Optional. Redacted in all debug output of `Config`. See the both-or-neither rule below and [Telegram Notifications](telegram-notifications.md). |
| Telegram chat id | `PRICE_ACTION_TELEGRAM_CHAT_ID` | `telegram_chat_id` | _(unset)_ | Optional; may be negative for a group or channel. Not a credential (it is shown in `Debug` and in delivery log lines). Must be paired with the token. |

Semantics worth pinning down:

- **Empty environment variables are treated as unset** (not as empty values).[^config-src]
- **Unknown config-file keys are rejected** (`deny_unknown_fields`) so typos
  fail fast with the offending key named; unknown *env vars* are ignored
  (they may belong to other tooling sharing the process).
- Unparseable env values (e.g. `PRICE_ACTION_QUANTITY=lots`) error naming the
  exact variable.
- **`Config`'s `Debug` impl redacts all three credentials** —
  `broker_api_key`, `massive_api_key` and `telegram_bot_token` each render as
  `[redacted]` when present — so formatting a config in logs or test output can
  never leak one; all other fields remain visible for diagnostics.[^config-src]
- **Telegram delivery is both-or-neither.** With neither key set, daily
  summaries go to the console only — a supported configuration, not an error.
  With exactly one set, `live` fails fast naming the *missing* half (and never
  echoing the value that is present), because a half-configured notifier is a
  typo. The rule is enforced by `notify::TelegramNotifier::from_config`, not by
  `Config`, so replay and the no-args path are unaffected.[^config-src]

## The three load paths

| Entry point | Used by | Validation | Notes |
|---|---|---|---|
| `Config::load()` | No-args binary path (normal run) | General **and** execution rules (live mode ⇒ non-empty `broker_url`) | Enforces everything a real run would. |
| `Config::load_for_replay()` | The `replay` subcommand | General rules only; the live-mode `broker_url` requirement is skipped | Justified because replay executes on an in-memory `PaperBroker` and can never place orders; invalid symbol/quantity/threshold still refused. See [Replay Workflow](replay-workflow.md). |
| `Config::load_for_live()` | The `live` subcommand | Everything `load()` enforces **plus** live-market-data rules for the selected `live_feed_source`: Massive needs a non-empty `massive_api_key`; Kraken needs `live_feed_channel = "minute"` and a `bar_interval_secs` naming one of its allowed candle intervals (no key at all) | Justified because the feed is authenticated. It gates only `live` — replay and the no-args path never touch the feed, so they do not need a market-data key. Live *execution* remains unimplemented regardless of this path. See [Live Market-Data Session](live-market-data-session.md). |

General validation also covers the live-only keys on **every** path (a malformed
`live_feed_host`, an unknown `live_feed_source` or empty `live_csv_dir` is
rejected even for a replay), so a typo cannot survive until the moment a session
tries to connect. The *source-specific* rules (Massive's key, Kraken's channel
and candle interval) gate only `live`, since they constrain a connection that
only that subcommand makes.

`Config::load_from(path, env)` and `Config::load_live_from(path, env)` (explicit
file + injected env lookup) keep full validation semantics and are the testing
seams: every precedence rule above has a test built on them.[^config-src]

## Updating configuration

Adding a new setting means touching **all three layers** plus docs in one PR:

1. Add the field to `Config`, its default, `FileConfig` (TOML surface), the env
   override, and the relevant validation; mirror the naming convention
   (`PRICE_ACTION_<SNAKE_KEY>` ↔ file key).
2. Add it (commented with constraints) to [`price-action.example.toml`](../price-action.example.toml).
3. Update **this document**'s table, [README.md](../README.md)'s configuration
   section, and — if the setting is meaningful to replay — the
   [Replay Workflow](replay-workflow.md) A/B examples.
4. Add tests: default application, file override, env-over-file precedence,
   invalid-value error naming the source (follow the existing test patterns).
5. Append an entry to [`log.md`](log.md) — same commit stack, per
   [AGENTS.md](../AGENTS.md).

[^config-src]: `src/config.rs`: layering impl, validation split, redacting Debug impl, and its test module

[^example-toml]: `price-action.example.toml` as committed
