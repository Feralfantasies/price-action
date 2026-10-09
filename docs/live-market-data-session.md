---
type: Reference
title: Live Market-Data Session
description: The `live` subcommand — streaming real-time bars (from the Massive.com stocks WebSocket or Kraken's public Spot WebSocket v2 OHLC channel), paper-trading them against a funded fake balance, and reporting/persisting the result.
tags: [live, market-data, websocket, paper-trading, massive, kraken]
status: draft
sources:
  - id: feedrs
    resource: /src/feed.rs
    title: Feed module source (wire protocol, framing, reconnect/backoff, tests)
  - id: krakenrs
    resource: /src/kraken.rs
    title: Kraken market-data source (REST candles, WS v2 OHLC feed, pair resolution, tests)
  - id: livers
    resource: /src/live.rs
    title: Live session source (bar shaping, session loop, summaries, persistence, tests)
  - id: mainrs
    resource: /src/main.rs
    title: Binary entry point (`live` argv handling, runtime, Ctrl-C, summary consumer)
  - id: replayrs
    resource: /src/replay.rs
    title: Shared ReplaySession + render_report seam used by both paths
  - id: configsrc
    resource: /src/config.rs
    title: Live-session configuration keys and load_for_live validation
generated: { by: pi-agent/qwen3.8-max, at: 2026-09-28T14:22:52Z }
---

`price-action live` answers a different question from
[replay](replay-workflow.md): not *"what would these settings have done on
recorded history?"* but *"what is this strategy doing **right now**, against
real market data, with fake money?"* It streams real-time bars — from the
Massive.com stocks WebSocket (the default source) or from Kraken's public Spot
WebSocket v2 OHLC channel — and runs them through **the same engine, strategy
and funded paper account** offline replay uses.[^livers][^replayrs][^krakenrs]

## The guarantee that matters most

**A live session cannot place an order.** The feed is data-in only — it
connects, authenticates, subscribes and reads; there is no order path in it.
Execution stays on the in-memory [`PaperBroker`](execution-layer.md) and the
funded [`PaperAccount`](paper-trading-accounting.md), exactly as in replay.
"Live" describes the *market data*, never the execution. `mode = "live"` (a
broker-execution setting) remains unimplemented and the no-args path still
refuses it — see [Overview](overview.md).[^livers][^mainrs]

Every number a live session reports is therefore *what the configured strategy
would have done*, at the fee rate you configured. It is not a track record, not
a profit claim and not a recommendation; see
[Disclaimer & Risk](disclaimer-risk.md).

## Invocation

```sh
PRICE_ACTION_SYMBOL=AAPL \
PRICE_ACTION_MASSIVE_API_KEY=<your key> \
  cargo run -- live
```

`live` takes **no arguments** — everything is configuration
([Configuration](configuration.md)). It exits `1` with a usage error when given
any. When the source is Massive it exits `1` naming `massive_api_key` if no key
resolves, because that feed is authenticated; a Kraken session reads no key at
all and fails only on its own settings (see below). A session runs until
**Ctrl-C** (or the event channel closing), then flushes, prints the report and
names the persisted CSV.[^mainrs]

## Choosing the market-data source

`live_feed_source` (env `PRICE_ACTION_LIVE_FEED_SOURCE`, case-insensitive and
trimmed) picks where the bars come from. It gates only `live`: replay and the
no-args path never read it.[^config-src]

| Source | Endpoint | Auth | Bar cadence |
|---|---|---|---|
| `massive` (default) | `wss://<live_feed_host>/stocks` | API key required (`PRICE_ACTION_MASSIVE_API_KEY`) | `AM` minute windows, or `T` ticks (`live_feed_channel`) |
| `kraken` | `wss://ws.kraken.com/v2` `ohlc` channel | **none** — a public data endpoint; no key exists for it and none is read | Kraken's candles at `bar_interval_secs` |

Selecting `kraken` changes the *transport and the bar cadence* and nothing else:
the session loop, `BarShaper`'s hold-until-next-window rule, the paper account,
the mock-trade log, the daily summaries and the persisted CSV are the same code
either way. Two Kraken-specific rules are enforced by
`Config::load_for_live()` (`validate_live_market_data`) before the session
starts, so a mistake fails before any connection:

- `live_feed_channel` must be `"minute"` — Kraken's v2 OHLC channel streams
  candles, there is no tick channel here; `ticks` is rejected naming the setting.
- `bar_interval_secs` must name one of Kraken's own candle intervals
  (`60`, `300`, `900`, `1800`, `3600`, `86400`); anything else is rejected
  naming `bar_interval_secs`.

The pair is resolved to Kraken's **display** form before the banner prints
(`XBT/USD` → `BTC/USD`, via the legacy-alias table in `src/kraken.rs`), so the
banner, the session log line, the mock-trade lines and the persisted CSV
filename all name the symbol actually streamed.

```sh
PRICE_ACTION_LIVE_FEED_SOURCE=kraken \
PRICE_ACTION_SYMBOL=XBT/USD \
PRICE_ACTION_BAR_INTERVAL_SECS=60 \
  cargo run -- live
```

Configuration is loaded through `Config::load_for_live()`: full `Config::load`
validation **plus** a resolvable market-data API key. Replay and the no-args
path are unaffected by the live-only keys.[^configsrc]

## What comes off the wire

Transport lives in `src/feed.rs` (Massive) and `src/kraken.rs` (Kraken); both
are deliberately socket-only — no bar logic, no clocks, no account state.[^feedrs][^krakenrs]

### Massive.com stocks feed

| Aspect | Behaviour |
|---|---|
| URL | `wss://<live_feed_host>/stocks` — `socket.massive.com` (real-time) or `delayed.massive.com` (15-minute delayed) |
| Framing | Every server frame is a JSON **array** of event objects; a bare object is accepted defensively; unparseable frames are skipped (forward progress beats one bad message) |
| Handshake | Server sends `{"ev":"status","status":"connected"}`; the client then sends `{"action":"auth","params":"<key>"}` and waits for a status verdict |
| Auth verdict | `auth_success` / `authenticated` accept; wordings containing `unauthorized`, `error` or `fail` reject. The pre-auth `connected` ack is **neutral** — it must not be mistaken for success |
| Subscribe | `{"action":"subscribe","params":"AM.<SYM>"}` (minute windows) or `T.<SYM>` (ticks) |
| `AM` events | One **minute** window of OHLC + volume, Unix-ms window start/end in `s`/`e`, re-emitted with fresh numbers as trades land inside the same window |
| `T` events | One tick trade: price `p`, size `s`, trade time `t` (Unix ms) |
| Other events | `status`, quotes and unknown feed types decode to **no** bar material |
| Symbol filter | Events for other symbols are dropped (case-insensitive match) |
| Secrets | The API key is only ever written into the auth frame; `FeedSettings`' `Debug` impl redacts it |

### Kraken Spot WebSocket v2

| Aspect | Behaviour |
|---|---|
| URL | `wss://ws.kraken.com/v2` — fixed; public channels only. The authenticated host is deliberately never used, so there is nothing to authenticate to |
| Handshake | no auth frame: the client sends `{"method":"subscribe","params":{"channel":"ohlc","symbol":["BTC/USD"],"interval":<mins>}}` and waits for the `{"method":"subscribe", …, "success":true\|false}` ack (15s grace). Data/heartbeat/status frames arriving before the ack do not settle the wait |
| Ack rejection | carries Kraken's own `error` string (e.g. *"Currency pair not supported XBT/USD"*), logged verbatim — it names the offending symbol and holds no credentials. That is why v2 gets a modern display pair: legacy wsnames, altnames and internal keys are all rejected by v2 itself |
| `ohlc` **update** frames | one candle: `open`/`high`/`low`/`close`/`volume` plus both window edges as ISO-8601 UTC strings (`interval_begin`, `timestamp`), re-emitted with fresh numbers while the candle is in flight — the same hold-until-next-starts cadence the shaper implements |
| `ohlc` **snapshot** frames | deliberately **ignored**: they are warm-up history (closed candles from long past) and booking them into a running session would price trades against stale prices; the in-flight candle arrives with final numbers through subsequent updates anyway |
| `heartbeat` / `status` | decode to no events — which is what keeps the silence timeout from firing on an idle pair |
| Symbol filter | data items for any other symbol are dropped (case-insensitive match against the resolved display pair) |
| Unusable values | a non-finite or negative price/volume drops that item (the same policy the session applies to a rejected event) |
| Volume display | the report formats volume as an integer (`v={:>10.0}`), so Kraken's fractional lot volumes (e.g. `0.31561871`) render as `0`/`1`. The persisted CSV keeps full `f64` precision — a rendering artifact, not data loss, and the shipping strategy never reads volume |
| Secrets | none: no key exists for this endpoint, so nothing is redacted and nothing can leak |

### Reconnection

The feeder owns reconnects and never gives up: one connection per loop pass,
with capped exponential backoff **2s → 4s → 8s → 16s → 30s → 30s…**
(`backoff_for`). A connection that stays silent for 90s is treated as dead;
handshake/status waits are bounded at 15s. After any drop the feeder sends a
`FeedInterrupted` marker so the session can be honest about the hole, then
retries. It stops promptly when the session drops the receiver (shutdown).[^feedrs]

## Events → bars (`BarShaper`)

`src/live.rs` turns events into `Bar`s without touching a socket or a clock —
every timestamp comes from the event, which is what makes the shaper
unit-testable and a session reproducible.[^livers]

**Minute mode (`live_feed_channel = "minute"`, the default).** The feed
re-emits the same window repeatedly while it fills, so the shaper *holds* the
window in flight and only emits it when the **next** window starts (or the
session flushes). Emitting on first sight would trade on a partial minute:
wrong close, wrong high/low, wrong volume. Consequences:

- Re-emissions sharing `start_ms` replace the held numbers — the bar carries
  the window's **final** values.
- A window older than the one in flight is stale (out-of-order) and dropped.
- A window whose end is absent or non-advancing is treated as one interval
  long, so the dedupe anchor is always strictly after its start.
- Bars lag the market by up to one window. That is the price of correct bars.

The window length is the **source's**, not a constant: `60_000` ms for Massive's
minute windows, `bar_interval_secs × 1000` for a Kraken session subscribed to
that candle interval. Both the missing-`end_ms` fallback and the gap test below
scale with it, so a session on 5-minute candles is not flagged as a data hole
for every normally-spaced bar.[^livers]

**Tick mode (`live_feed_channel = "ticks"`).** Trades accumulate into a bucket
per UTC second — first price is the open, last is the close, high/low widen,
size sums — and the bucket becomes a bar when a trade from a *different* second
arrives (or on flush). A tick that arrives with a timestamp **older** than the
second currently accumulating is out-of-order and is **dropped**: it emits no
older bar, and closing or replacing the held bucket on its arrival would have
to truncate that bucket's true high/low/volume — the same rule minute mode
applies to out-of-order windows.[^livers]

**Data holes are reported, never hidden.** A jump of ≥ 1 full source window
between windows annotates the *next* bar with `[feed gap: ~<n>s of missing windows
before this bar]`; a jump of ≥ 2s between traded seconds annotates with
`[feed gap: ~<n>s of missing ticks before this bar]`. A `FeedInterrupted`
marker becomes `[feed interrupted; reconnecting]`, merged onto the same line
when both apply. Annotations are appended to that bar's trace line by
`ReplaySession::annotate_last_trace`, so the finished report shows them —
offline replay never calls it, because a file has no holes. Interruptions that
arrive *before* the first bar cannot be attached to anything and are counted
and reported on stderr instead.[^livers][^replayrs]

Unusable values (non-finite or negative price/volume) are rejected with
`Error::MarketData` rather than silently booked into the account. Inside the
session loop such a shaping failure **drops the offending event only** — one
line on stderr and the feed keeps flowing (the same posture as the feeder
skipping unparseable frames); errors raised *after* a bar is shaped — i.e. by
`ReplaySession::on_bar` booking it — still end the session.[^livers]

## One session, one shared pipeline

Every shaped bar goes through `ReplaySession::on_bar` — the same code path
offline replay uses — so entries, exits, fees, collateral, insufficient-funds
skips, cash/equity marks, per-UTC-day roll-ups and session totals are computed
by exactly one implementation. The report is rendered by the same
`render_report`, with `source = "live"` as the only difference from a replay
rendering.[^replayrs]

Each bar also produces a **mock-trade log line** the moment a paper trade
closes, on stdout:

```text
[2021-01-19T18:00:00Z] MOCK TRADE #1 LONG  AAPL x1 entry=101 (2021-01-19) -> exit=100 (2021-01-20) | committed=101.00 fees=0.1005 (5 bps/side) gross P/L=-1.00 net P/L=-1.10 | cash=9998.90 equity=9998.90
```

That is the "how much it would have put down, including fees" answer in real
time: symbol and side, the configured `quantity`, entry and exit prices with
their UTC days, the notional **committed**, the **fees** with the bps rate that
produced them, gross and net P/L, and the account's **cash/equity** right after
the bar.[^livers]

## Daily summaries

A session tracks the UTC day in progress and emits a
[`DailySummary`](telegram-notifications.md) when that day closes — i.e. when
the first bar of the next UTC day arrives — plus a final one marked `partial`
for the day the session stopped in. Contents: day, symbol, bars, entries,
exits, fees paid, realized P/L net of fees, ending cash, ending equity and the
position still open. A day with no trades still reports (zero activity,
unchanged balance): silence is not the same as "no data".[^livers]

Entries are counted the way
[`PaperAccount::per_day_totals`](paper-trading-accounting.md) counts them — one
per **funded position opened** — so a reversal books an exit *and* an entry on
that day, and an entry the account could not fund books neither. Counting the
raw strategy signal instead would disagree with the report's own per-UTC-day
table; the tests assert the two views reconcile.[^livers]

Summaries go out over a bounded channel to a separate consumer task, so a slow
or dead notifier can never stall the session. The binary prints each one to the
console and, when Telegram is configured, delivers it there too — delivery
failures are logged and trading continues.[^mainrs][^livers]

## Persistence and exact re-replay

On stop the session writes every bar it consumed to
`<live_csv_dir>/live-<SYMBOL>-<UTC stamp>.csv` in the
[bar file format](bar-file-format.md), creating the directory if needed. `<SYMBOL>`
is whatever the session actually traded — Massive's uppercased ticker (`AAPL`),
or Kraken's resolved display pair (`BTC/USD`, even when configured as `XBT/USD`).
When no bar arrived, nothing is written and the report says so.[^livers]

That file is the audit trail: re-replaying it offline reproduces the session
**by construction**, because both paths share one pipeline. The traces are
byte-identical apart from the live-only `[feed gap: …]` annotations, which a
file has no way to know about — a test pins exactly that.[^livers][^replayrs]

```sh
cargo run -- replay sessions/live-AAPL-20260928T133000Z.csv
```

## Shutdown

Ctrl-C ends a session cleanly: the loop breaks, the in-flight window or tick
bucket is flushed as the final bar, the partial day is summarized, the report
is assembled (marking any open position at the last close, exactly like
replay), and the bars are persisted. The binary drains the summary consumer
before dropping the runtime, so the last day's summary is printed rather than
cancelled mid-flight.[^mainrs][^livers]

## What a live session is *not*

- **Not live execution.** No order is sent anywhere; the only broker is
  in-memory. `mode = "live"` remains configuration-only.
- **Not multi-symbol.** One subscription per session; run one process per
  symbol.
- **No history backfill.** The session starts from the connection onward. For
  anything spanning days, replay a recorded file instead.
- **No latency edge.** Minute mode deliberately lags one window to avoid
  trading on partial bars, and the delayed host lags 15 minutes.
- **Not a performance claim.** Fees are a flat bps rate you configured; there
  is no slippage, no partial fill, no venue minimum and no queue.

[^feedrs]: `src/feed.rs`: module doc (wire protocol), `decode_frame`, `auth_verdict`, `spawn`, `backoff_for`, `FeedSettings` (redacting `Debug`), and its socket-free test module

[^livers]: `src/live.rs`: module doc, `BarShaper` (hold-until-next-window dedupe, tick bucketing, gap notes), `SessionState`/`DayAccumulator`, `run_session`, `log_mock_trade`, `persist_bars`, and its test module (including the day-boundary and re-replay reconciliation tests)

[^mainrs]: `src/main.rs`: `run_live` (argv shape, `load_for_live`, crypto provider, feeder spawn, summary consumer, Ctrl-C shutdown) and the `USAGE` line

[^replayrs]: `src/replay.rs`: `ReplaySession` (the shared per-bar pipeline), `annotate_last_trace`, `render_report(config, report, source, out)`

[^configsrc]: `src/config.rs`: `load_for_live`/`load_live_from`, `validate_live_market_data`, `live_feed_host`/`live_feed_channel`/`massive_api_key`/`live_csv_dir`

[^krakenrs]: `src/kraken.rs`: module doc (wire facts, verified live), `resolve_from_tables`/`resolve_pair`, `KrakenSettings`, `spawn_v2`/`v2_stream_once` (reconnect + `FeedInterrupted`), `decode_v2_frame` (update-only, symbol filter), `v2_subscribe_verdict`, `ALLOWED_INTERVALS_SECS`, `LEGACY_ASSET_NAMES`
