---
type: Reference
title: Overview
description: What price-action is, its architecture pipeline, and its scope — paper-only execution, with market data either replayed from files or streamed live.
tags: [overview, architecture, scope]
status: draft
sources:
  - id: readme
    resource: /README.md
    title: Repository README
  - id: librs
    resource: /src/lib.rs
    title: Crate root module documentation
generated: { by: pi-agent/qwen3.8-max, at: 2026-10-16T15:30:00Z }
---

`price-action` is an automated price-action trading project written in Rust.
Its strategies are driven by **raw price movement** (OHLCV bars) rather than
derived indicators such as moving averages or stochastic oscillators.[^readme]

The crate is organised as a pipeline:[^librs]

```text
market → strategy → engine → execution
  ▲                │
  └────── config resolves every knob of the run

market data arrives either
  • replay  — bars read from a CSV file        (offline, step 1)
  • kraken  — committed candles over REST      (`kraken backtest`, real crypto)
  • live    — bars streamed from a WebSocket   (Massive.com or Kraken)
all feed the SAME engine + strategy + funded paper account
```

| Crate | Role | Doc |
|---|---|---|
| `src/market.rs` | Raw price data: `Bar` (OHLCV) and `BarSeries` rolling window | [Market Data Model](market-data-model.md) |
| `src/strategy.rs` | `Signal`, the `Strategy` trait, the `ConsecutiveCloses` strategy | [Consecutive Closes Strategy](strategy-consecutive-closes.md) |
| `src/engine.rs` | The trading loop: bar → signal → broker position | [Trading Engine](engine.md) |
| `src/execution.rs` | `Broker` trait, `Position`, in-memory `PaperBroker` | [Execution Layer](execution-layer.md) |
| `src/csv.rs` | OHLCV CSV reading/writing (the replay data format) | [OHLCV Bar File Format](bar-file-format.md) |
| `src/replay.rs` | Replay runner + `ReplaySession`, the per-bar pipeline both paths share, + the human-readable report (trace, trade tables, roll-ups) | [Replay Workflow](replay-workflow.md) |
| `src/accounting.rs` | Funded paper account the report prices with: fees, P/L, UTC-day bucketing | [Paper Trading Accounting](paper-trading-accounting.md) |
| `src/feed.rs` | The Massive.com stocks WebSocket: framing, auth, subscribe, reconnect/backoff (transport only) | [Live Market-Data Session](live-market-data-session.md) |
| `src/kraken.rs` | Kraken market data: public REST candles + pair resolution, Spot WebSocket v2 transport, and the optional read-only signed request path | [Kraken Market Data & Keys](kraken-market-data.md) |
| `src/live.rs` | Live sessions: event → bar shaping, gap notes, the session loop, daily summaries, CSV persistence | [Live Market-Data Session](live-market-data-session.md) |
| `src/notify.rs` | Telegram delivery of a live session's daily summaries (`sendMessage` over rustls) | [Telegram Notifications](telegram-notifications.md) |
| `src/config.rs` | Layered configuration: env → config file → defaults | [Configuration](configuration.md) |
| `src/error.rs` | Shared `Error` type (`MarketData` / `Execution` / `Strategy` / `Config`) | — |
| `src/main.rs` | Binary entry point: no-args readiness check, the `replay`, `kraken backtest`, `kraken check-keys` and `live` subcommands | [Development Workflow](development-workflow.md) |

# Scope (current state)

- **Execution is paper only. Live trading is not implemented.** `mode = "live"`
  passes validation in normal startup (it then requires a non-empty
  `broker_url`) but the binary refuses to actually run live mode — the no-args
  path returns `"live trading is not implemented yet; run in paper mode"`.
  Today there is exactly one broker implementation: the in-memory
  `PaperBroker`, which tracks a position but never places orders.
- **Market data now has three paths.** Recorded bars are replayed from a CSV
  (`replay <bars.csv>`); recent **real** crypto candles are fetched from
  Kraken's public REST API (`kraken backtest`); and real-time bars are streamed
  from a WebSocket (`live`) — the Massive.com stocks feed or Kraken's public
  Spot v2 OHLC channel — see
  [Live Market-Data Session](live-market-data-session.md) and
  [Kraken Market Data & Keys](kraken-market-data.md). The paths
  converge immediately: each feeds the same
  `ReplaySession`, so entries, fees, roll-ups and trace lines come
  from one implementation. **Broker/venue adapters still do not exist** — the
  feeds are data-in only and cannot place an order; even the optional
  read-only Kraken API-key pair can only read one balance endpoint.
- **"Live" describes the data, never the execution.** A live session trades a
  funded fake balance, logs each mock trade with its size, committed notional,
  per-side fees and running cash/equity, persists its bars as a
  replay-compatible CSV, and summarizes each closed UTC day (optionally to
  Telegram, see [Telegram Notifications](telegram-notifications.md)). Nothing
  it reports is a track record or a recommendation — see
  [Disclaimer & Risk](disclaimer-risk.md).
- The [sample bar file](sample-bars.md) is **synthetic**: it is shaped like
  real trades but is *not* actual exchange prices. A live session's bars *are*
  real market data, but the money attached to them is not. This project is for
  educational and research purposes only and is **not financial advice**.

[^readme]: `README.md` ("strategies driven by raw price movement, not by derived indicators")

[^librs]: `src/lib.rs` crate doc

# Reading order for agents

1. This page (overview) → 2. [Configuration](configuration.md) →
   3. [Replay Workflow](replay-workflow.md), then the architecture concepts
   relevant to what you are changing — plus
   [Live Market-Data Session](live-market-data-session.md) and
   [Telegram Notifications](telegram-notifications.md) for anything touching
   `live`. [AGENTS.md](../AGENTS.md) makes this order mandatory before any
   change and requires keeping these docs updated afterwards.
