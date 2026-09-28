---
okf_version: "0.2"
---

# price-action — Knowledge Bundle

Open Knowledge Format ([OKF v0.2](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md))
bundle documenting the `price-action` repository: a Rust trading engine whose
strategies are driven by raw price movement (OHLCV bars) rather than derived
indicators. Two ways to feed it bars: a **replay** mode that validates settings
against historic data, and a **live** mode that streams real-time data from the
Massive.com stocks WebSocket. Both price every decision as a funded paper
account (fees, realized P/L per trade, per-UTC-day and session totals), and a
live session additionally summarizes each closed UTC day — optionally delivered
to Telegram. **Execution is paper-only in both modes: no order is ever placed.**

**Any agent working in this repository MUST read this bundle before making any
change and MUST keep it current when behaviour changes** — see
[AGENTS.md](../AGENTS.md), which governs every change to this repo.

# Start Here

* [Overview](overview.md) — What the project is, its architecture pipeline, its scope (and explicit non-goals: no live *execution*, ever, so far).
* [Replay Workflow](replay-workflow.md) — The sanctioned workflow for verifying settings against historic bars. Replay is read-only by design; its report now includes funded paper-accounting roll-ups.
* [Live Market-Data Session](live-market-data-session.md) — The `live` subcommand: streaming real-time bars from the Massive.com WebSocket and paper-trading them against a fake balance, through the very same pipeline replay uses.

# Architecture Concepts

* [Trading Engine](engine.md) — The bar → signal → position loop and its failure-retry invariant.
* [Consecutive Closes Strategy](strategy-consecutive-closes.md) — The only shipping strategy: N consecutive higher/lower closes.
* [Market Data Model](market-data-model.md) — `Bar` (validated OHLCV) and the `BarSeries` rolling window.
* [Execution Layer](execution-layer.md) — The `Broker` trait, `Position`, and the in-memory `PaperBroker`.
* [Paper Trading Accounting](paper-trading-accounting.md) — How replay **and live sessions** price the strategy's decisions: entry/exit at bar closes, flat bps fees per side, insufficient-funds skipping, UTC-day bucketing, session roll-ups.
* [Telegram Notifications](telegram-notifications.md) — How a live session's daily summaries reach Telegram: configuration, the `sendMessage` request, secret handling, non-fatal delivery.

# Data Formats

* [OHLCV Bar File Format](bar-file-format.md) — The CSV format replay consumes and a live session persists: schema, header rule, validation, and lossless round-trip.
* [Sample Bar File](sample-bars.md) — The bundled 25-bar synthetic demo dataset (shaped like real trades, not exchange data).

# Configuration & Operations

* [Configuration](configuration.md) — Three-layer precedence (env → TOML file → defaults), the full settings table (including the live-feed and Telegram keys), and validation rules.
* [Container Image & Release](container-image-and-release.md) — The `FROM scratch` image, its static/rustls constraints, CI jobs, and how releases land on GHCR with git tags.
* [Development Workflow](development-workflow.md) — Toolchain, lints (pedantic + panic-prevention set), verification commands, MSRV notes, and where the sample data lives.
* [Disclaimer & Risk](disclaimer-risk.md) — Educational-purpose status: no financial advice, substantial risk of loss, paper-only execution today (including when the market data is real).

# Bundle Maintenance

* [Bundle Update Guide](bundle-update-guide.md) — How to keep this bundle in sync with the code: when a doc must change, how, and which frontmatter fields get touched (the rule for agents working here).
* [Update Log](log.md) — Chronological history of changes to this bundle.
