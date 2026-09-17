---
type: Reference
title: Paper Trading Accounting
description: How the replay report prices the strategy's decisions as a funded paper account — execution model, flat basis-point fees per side, insufficient-funds skipping, UTC-day bucketing and session roll-ups.
tags: [replay, accounting, paper-trading, fees, p/l]
status: draft
sources:
  - id: accountingsrc
    resource: /src/accounting.rs
    title: Paper-account source (state machine, day math, hand-computed test vectors)
  - id: configsrc
    resource: /src/config.rs
    title: Config knobs that feed the account (starting_balance, trade_fee_bps)
generated: { by: pi-agent/use_this, at: 2026-09-17T07:05:00Z }
---

The replay report is not a passive signal trace: it also runs each bar's
decision through a **funded paper account** (the `PaperAccount` struct) so the
output shows capital committed, running available funds, realized P/L per
closed trade, per-UTC-day totals and session totals. Replay remains read-only
by design — the account exists only to *report* what the strategy's decisions
would have cost or earned.[^accountingsrc]

All money is `f64` (validated finite upstream by configuration), shown at two
decimals in the report; any field labelled *net* already includes fees. The
model deliberately has **no leverage beyond** the configured `quantity`,
**no slippage** beyond the flat fee rate, and **no persistence**.

## Two settings feed the account

| Setting (env / TOML) | Default | Meaning |
|---|---|---|
| `PRICE_ACTION_STARTING_BALANCE` / `starting_balance` | `10000` | Free funds before the first trade; must be finite and strictly positive ([Configuration](configuration.md)). |
| `PRICE_ACTION_TRADE_FEE_BPS` / `trade_fee_bps` | `5` | Fee per side of each paper trade, in **basis points** of that leg's notional: rate = `bps / 10_000`, so the default charges 0.05%. Zero disables fees exactly; negative values are rejected by config validation. |

## Execution model (the rules to re-check any trace line against)

- **Entry price:** every entry — and, on a reversal, *both* legs of it — is
  assumed to execute at the **close of the bar whose post-bar signal caused
  the position change**. That is precisely the bar the corresponding trace
  line annotates with `cash=`/`equity=`, so any reported figure can be derived
  from one line of output.[^accountingsrc]
- **Notional & committed funds:** an entry sizes `quantity × close`. A long
  pays that amount out of free funds (plus the entry fee); a short has it
  **reserved as collateral** — its free funds drop by the same notional plus
  fee, but that capital is tied up for the life of the position and returns
  at exit.
- **Exit:** releases the committed notional, credits signed gross P/L, then
  charges the exit fee on the *exit* bar's notional:
  - long: `gross = (exit − entry) × quantity`
  - short: `gross = (entry − exit) × quantity`

  `net = gross − (entry fee + exit fee)`; net is what every roll-up sums.
- **Insufficient funds:** if `notional + entry fee > free funds`, the entry is
  **skipped** — the trace still prints the raw strategy signal with an
  `(insufficient funds)` note, no position opens, and free funds never go
  negative (they cannot *become* negative through any operation). A skipped
  reversal leaves the old leg already exited at the same close, recorded as a
  closed trade.
- **Equity mark:** after every bar, `equity = free funds + open position
  marked at that bar's close` — shares valued at the close for longs,
  collateral plus unrealized (entry − close) × quantity for shorts. While
  flat, equity equals free funds; the session total marks its final bar at
  the file's last close.

## Roll-ups in the report

The account accumulates exactly what the report prints (see the full output
shape in [Replay Workflow](replay-workflow.md)):[^accountingsrc]

- **Closed paper trades** — one row per trade in *exit* order, with entry/exit
  bar context (UTC day + price), investment, gross P/L, fees and net P/L.
- **Totals per UTC day (24h)** — ISO `YYYY-MM-DD` calendar days; a day appears
  when it saw at least one entry *or* exit, and its net column only includes
  trades that **exited** that day (a trade open across midnight books to both
  days). Days are computed from the integer seconds with Hinnant's civil-date
  algorithm in plain `i64` — deterministic UTC, no stdlib time types, immune
  to the process timezone. Timestamps before epoch clamp to day 0 (well-formed
  bar files never regress; same contract as the trace's `t=` column).
- **Session totals** — starting balance, final free funds, final equity
  (marked at the last close), realized P/L net of all fees, and total fees
  paid on every leg.

## What this model is *not*

This is a teaching-grade cost model for reviewing settings, not a broker
simulation: fills are always at the bar close (no intrabar slippage or
partial fills), position sizing is the fixed `quantity` only (no margining,
pyramiding or stop-losses), fees ignore venue minimums/tiers, and nothing is
written to disk. It also cannot represent a venue rejecting an order for any
reason other than lack of account funds — which replay has no reason to model.

[^accountingsrc]: `src/accounting.rs`: state machine in `PaperAccount::on_bar`, `exit_position`, equity mark, day arithmetic, and the test module with hand-computed reference values (long/short round trips, reversal, zero-fee exactness, insufficient-funds path, known UTC dates)

[^configsrc]: `src/config.rs`: `starting_balance` (finite, > 0) and `trade_fee_bps` (unsigned bps) in all three precedence layers with error-naming tests
