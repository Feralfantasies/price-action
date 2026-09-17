---
type: Reference
title: Configuration
description: Three-layer configuration precedence (env vars, TOML file, compiled defaults), the complete settings table, and validation rules.
tags: [configuration, env-vars, toml]
status: draft
sources:
  - id: config-src
    resource: /src/config.rs
    title: Config module source (layering, validation, tests)
  - id: example-toml
    resource: /price-action.example.toml
    title: Example TOML config file
generated: { by: pi-agent/use_this, at: 2026-09-17T07:05:00Z }
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
| Quantity per trade | `PRICE_ACTION_QUANTITY` | `quantity` | `1` | ≥ 1. Cosmetic during replay. |
| Trading mode | `PRICE_ACTION_MODE` | `mode` | `paper` | `"paper"` or `"live"` (case-insensitive) — any other value is rejected as an unknown mode.[^config-src] Live also requires a broker URL in the normal path, and live trading is not implemented yet (see [Overview](overview.md)). Cosmetic during replay. |
| Bar interval (secs) | `PRICE_ACTION_BAR_INTERVAL_SECS` | `bar_interval_secs` | `60` | ≥ 1. Documents the interval bars are assumed to be at; affects what "consecutive" means to a strategy fed that data. Cosmetic during replay. |
| Rolling-window size | `PRICE_ACTION_SERIES_CAPACITY` | `series_capacity` | `500` | Bars retained by a `BarSeries` (see [Market Data Model](market-data-model.md)). ≥ 1; not exercised by the current strategy or replay. |
| Strategy threshold | `PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD` | `consecutive_closes_threshold` | `3` | Consecutive higher/lower closes before the example strategy signals (see [Consecutive Closes Strategy](strategy-consecutive-closes.md)). ≥ 1; one of the replay settings that changes what you see. |
| Paper starting balance | `PRICE_ACTION_STARTING_BALANCE` | `starting_balance` | `10000` | Funds the replay report's paper account (f64); must be finite and strictly greater than 0. Replay-only: no live path exists yet, so nothing else reads it. |
| Paper trade fee (basis points) | `PRICE_ACTION_TRADE_FEE_BPS` | `trade_fee_bps` | `5` | Fee charged per side of each paper trade, in basis points of that leg's notional (`bps / 10_000`; default 5 = 0.05%). ≥ 0; zero disables fees exactly. See [Replay Workflow](replay-workflow.md). |
| Broker API base URL | `PRICE_ACTION_BROKER_URL` | `broker_url` | _(unset)_ | Required when running normally with mode `live`; an empty/whitespace value is treated as absent and rejected in that case. Never needed for replay (see below). |
| Broker API key | `PRICE_ACTION_BROKER_API_KEY` | `broker_api_key` | _(unset)_ | Secret — prefer the env var or a mounted secret over committing it to a file, and never commit it to the repo. Redacted in all debug output of `Config`. |
| Config-file path | `PRICE_ACTION_CONFIG` | — | `price-action.toml` | Only an env var; names where the config file (precedence layer 2) is read from. |

Semantics worth pinning down:

- **Empty environment variables are treated as unset** (not as empty values).[^config-src]
- **Unknown config-file keys are rejected** (`deny_unknown_fields`) so typos
  fail fast with the offending key named; unknown *env vars* are ignored
  (they may belong to other tooling sharing the process).
- Unparseable env values (e.g. `PRICE_ACTION_QUANTITY=lots`) error naming the
  exact variable.
- **`Config`'s `Debug` impl redacts `broker_api_key`** — formatting a config in
  logs or test output can never leak the credential; all other fields remain
  visible for diagnostics.[^config-src]

## The two load paths

| Entry point | Used by | Validation | Notes |
|---|---|---|---|
| `Config::load()` | No-args binary path (normal run) | General **and** execution rules (live mode ⇒ non-empty `broker_url`) | Enforces everything a real run would. |
| `Config::load_for_replay()` | The `replay` subcommand | General rules only; the live-mode `broker_url` requirement is skipped | Justified because replay executes on an in-memory `PaperBroker` and can never place orders; invalid symbol/quantity/threshold still refused. See [Replay Workflow](replay-workflow.md). |

`Config::load_from(path, env)` (explicit file + injected env lookup) keeps
full validation semantics and is the testing seam: every precedence rule above
has a test built on it.[^config-src]

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
