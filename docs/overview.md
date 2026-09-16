---
type: Reference
title: Overview
description: What price-action is, its architecture pipeline, and its current scope — paper-only, no market-data adapters yet.
tags: [overview, architecture, scope]
status: draft
sources:
  - id: readme
    resource: /README.md
    title: Repository README
  - id: librs
    resource: /src/lib.rs
    title: Crate root module documentation
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

`price-action` is an automated price-action trading project written in Rust.
Its strategies are driven by **raw price movement** (OHLCV bars) rather than
derived indicators such as moving averages or stochastic oscillators.[^readme]

The crate is organised as a pipeline:[^librs]

```text
market → strategy → engine → execution
  ▲                │
  └────── config resolves every knob of the run
         (replay + csv feed historic bars back in)
```

| Crate | Role | Doc |
|---|---|---|
| `src/market.rs` | Raw price data: `Bar` (OHLCV) and `BarSeries` rolling window | [Market Data Model](market-data-model.md) |
| `src/strategy.rs` | `Signal`, the `Strategy` trait, the `ConsecutiveCloses` strategy | [Consecutive Closes Strategy](strategy-consecutive-closes.md) |
| `src/engine.rs` | The trading loop: bar → signal → broker position | [Trading Engine](engine.md) |
| `src/execution.rs` | `Broker` trait, `Position`, in-memory `PaperBroker` | [Execution Layer](execution-layer.md) |
| `src/csv.rs` | OHLCV CSV reading/writing (the replay data format) | [OHLCV Bar File Format](bar-file-format.md) |
| `src/replay.rs` | Replay runner + human-readable per-bar report | [Replay Workflow](replay-workflow.md) |
| `src/config.rs` | Layered configuration: env → config file → defaults | [Configuration](configuration.md) |
| `src/error.rs` | Shared `Error` type (`MarketData` / `Execution` / `Strategy` / `Config`) | — |
| `src/main.rs` | Binary entry point: no-args readiness check and the `replay` subcommand | [Development Workflow](development-workflow.md) |

# Scope (current state)

- **Paper only. Live trading is not implemented.** `mode = "live"` passes
  validation in normal startup (it then requires a non-empty `broker_url`) but
  the binary refuses to actually run live mode — the no-args path returns
  `"live trading is not implemented yet; run in paper mode"`. Today there is
  exactly one broker implementation: the in-memory `PaperBroker`, which tracks
  a position but never places orders.
- **No market-data source adapters exist.** Broker/venue APIs and live feeds
  are deliberately out of scope for the current scaffold.[^librs] Until they
  exist, the binary either reports readiness (no arguments) or replays recorded
  bars (`replay <bars.csv>`). Replay is the intended way to exercise the full
  pipeline offline, and it is always step 1 before any strategy is trusted.
- The [sample bar file](sample-bars.md) is **synthetic**: it is shaped like
  real trades but is *not* actual exchange prices. This project is for
  educational and research purposes only and is **not financial advice** — see
  [Disclaimer & Risk](disclaimer-risk.md).

[^readme]: `README.md` ("strategies driven by raw price movement, not by derived indicators")

[^librs]: `src/lib.rs` crate doc

# Reading order for agents

1. This page (overview) → 2. [Configuration](configuration.md) →
   3. [Replay Workflow](replay-workflow.md), then the architecture concepts
   relevant to what you are changing. [AGENTS.md](../AGENTS.md) makes this
   order mandatory before any change and requires keeping these docs updated
   afterwards.
