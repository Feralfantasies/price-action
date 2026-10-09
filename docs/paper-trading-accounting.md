---
type: Reference
title: Paper Trading Accounting
description: How the replay report — and the live session — price the strategy's decisions as a funded paper account: execution model, flat basis-point fees per side, insufficient-funds skipping, configurable leverage with required-margin tracking and forced liquidation, UTC-day bucketing and session roll-ups.
tags: [replay, live, accounting, paper-trading, fees, leverage, margin, p/l]
status: draft
sources:
  - id: accountingsrc
    resource: /src/accounting.rs
    title: Paper-account source (state machine, day math, hand-computed test vectors)
  - id: configsrc
    resource: /src/config.rs
    title: Config knobs that feed the account (starting_balance, trade_fee_bps)
  - id: liversrc
    resource: /src/live.rs
    title: Live session source (same account, daily summaries)
generated: { by: pi-agent/qwen3.8-max, at: 2026-09-28T02:25:00Z }
---

The replay report is not a passive signal trace: it also runs each bar's
decision through a **funded paper account** (the `PaperAccount` struct) so the
output shows capital committed, running available funds, realized P/L per
closed trade, per-UTC-day totals and session totals. Replay remains read-only
by design — the account exists only to *report* what the strategy's decisions
would have cost or earned.[^accountingsrc]

**This is the same account a [live session](live-market-data-session.md)
trades.** Both paths feed bars into one `ReplaySession`, so every rule below
applies identically whether the bars came from a CSV file or from a WebSocket;
that is exactly why re-replaying a session's persisted CSV reproduces it. A
live session adds two things on top, without changing any pricing rule: a
mock-trade log line per close (quantity, committed notional, per-side fees with
the bps rate, gross/net P/L, running cash/equity) and a
[`DailySummary`](telegram-notifications.md) per closed UTC day.[^liversrc]

All money is `f64` (validated finite upstream by configuration), shown at two
decimals in the report; any field labelled *net* already includes fees. The
model deliberately has **no leverage beyond** the configured `quantity` unless
`max_leverage` is raised (see the margin rules below), **no slippage** beyond
the flat fee rate, and **no persistence of the account itself** — a live
session persists its *bars* (so the run is reproducible), but the account state
is rebuilt by re-replaying them rather than stored.

## Three settings feed the account

| Setting (env / TOML) | Default | Meaning |
|---|---|---|
| `PRICE_ACTION_QUANTITY` / `quantity` | `1` | Units per position. Sizes every notional in the model — committed/collateralized funds, each leg's fee, cash/equity movements and P/L all scale with it, and affordability of an entry depends on it ([Configuration](configuration.md)). |
| `PRICE_ACTION_STARTING_BALANCE` / `starting_balance` | `10000` | Free funds before the first trade; must be finite and strictly positive ([Configuration](configuration.md)). |
| `PRICE_ACTION_TRADE_FEE_BPS` / `trade_fee_bps` | `5` | Fee per side of each paper trade, in **basis points** of that leg's notional: rate = `bps / 10_000`, so the default charges 0.05%. Zero disables fees exactly; negative values are rejected by config validation. |
| `PRICE_ACTION_MAX_LEVERAGE` / `max_leverage` | `1` | Maximum leverage the paper account may put on a position: `1` = unleveraged, the documented default. Integer `1..100`; anything else is rejected naming `max_leverage`. Above 1 it multiplies the notional a position controls (and therefore its fees and P/L) **without** committing more capital — see the margin rules below ([Configuration](configuration.md)). |

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
  `(insufficient funds)` note and no position opens. A skipped reversal leaves
  the old leg already exited at the same close, recorded as a closed trade.
- **Equity mark:** after every bar, `equity = free funds + open position
  marked at that bar's close` — shares valued at the close for longs,
  collateral plus unrealized (entry − close) × quantity for shorts. While
  flat, equity equals free funds; the session total marks its final bar at
  the file's last close. Per the insolvency policy below this value floors
  at 0.
- **Insolvency (defined, not silent):** an unrealized loss never touches free
  funds — only the mark moves (and the finished equity floor of 0 applies),
  so a losing position is *visible* in declining `equity=` until it is
  closed. When closing a position would drive free funds below zero (a
  realized loss beyond what closing frees), free funds **floor at 0** from
  that bar on; the shortfall is a write-off, never negative debt — this is a
  paper account with no counterparty to owe. Closed-trade rows always keep
  the position's full arithmetic P/L (gross, fees, net) regardless of any
  write-off, so per-trade results stay auditable. There is **no margining,
  liquidation cascade or stop-loss** on top of this floor at 1x: it *is* the
  whole insolvency model; the leveraged forced-liquidation test below is the
  only addition, and it never applies to an unleveraged run.
- **Leverage (`max_leverage` > 1):** a position controls `quantity × leverage`
  units of the instrument, so its **exposure** — the notional fees and P/L are
  priced on — is `quantity × leverage × close`, while the capital it actually
  commits is the **required margin** `quantity × close`. Leverage buys exposure
  per unit of capital, it does not buy more capital, which is why sizing up is
  what leverage is for in reality. Both legs' fees price the leveraged notional
  and gross P/L scales by `leverage`:
  - long: `gross = (exit − entry) × quantity × leverage`
  - short: `gross = (entry − exit) × quantity × leverage`
  An entry is still skipped when `required margin + entry fee > free funds`,
  so a run can be fundable at 1x and skipped at 5x on the same bar.
- **Forced liquidation (leveraged runs only):** before any strategy decision
  for a bar, the account marks the open position at that bar's close; when
  marked equity can no longer support the position's required margin, a real
  venue would force-close it, so the simulation closes the position **at that
  bar's close** and records the trade as `liquidated` (the trace line carries a
  `[liquidated]` marker). The maintenance margin used is the position's
  required margin — an approximation, since Kraken's real rule is tiered and
  demands more than the initial margin; this is deliberately the simplest
  breach test. Unleveraged (`1x`) runs never liquidate, and their report output
  stays byte-identical to the historical shape.

## Roll-ups in the report

The account accumulates exactly what the report prints (see the full output
shape in [Replay Workflow](replay-workflow.md)):[^accountingsrc]

- **Closed paper trades** — one row per trade in *exit* order, with entry/exit
  bar context (UTC day + price), investment, gross P/L, fees and net P/L. A
  leveraged run (`max_leverage` > 1) adds three columns — `required margin`,
  `exposure` and `liquidated` — so a 1x and a leveraged report over identical
  bars are visibly different; an unleveraged run prints the historical table
  shape exactly.
- **Totals per UTC day (24h)** — ISO `YYYY-MM-DD` calendar days; a day appears
  when it saw at least one entry *or* exit, and its net column only includes
  trades that **exited** that day (a trade open across midnight books to both
  days). A position still open at the end of the session counts as an entry
  on its entry day. Days are computed from the integer seconds with Hinnant's
  civil-date algorithm in plain `i64` — deterministic UTC, no stdlib time
  types, immune to the process timezone. Timestamps before epoch clamp to day
  0 (well-formed bar files never regress; same contract as the trace's `t=`
  column).
- **Session totals** — starting balance, final free funds, final equity
  (marked at the last close), realized P/L over closed trades in exit order,
  and total fees on **every leg booked so far — including the entry leg of a
  position still open when the session ends.** A leveraged run additionally
  reports `forced liquidations` and `peak required margin`, and the report
  header carries a `leverage=Nx` line spelling out what the numbers price.

## What this model is *not*

This is a teaching-grade cost model for reviewing settings, not a broker
simulation: fills are always at the bar close (no intrabar slippage or
partial fills), position sizing is the fixed `quantity` scaled by the
configured `max_leverage` (no pyramiding, trailing stops or stop-losses), fees
ignore venue minimums/tiers, and no account state is written to disk. The one
margining rule that *does* exist is the forced-liquidation test above, whose
maintenance margin is the position's required margin — Kraken's real rule is
tiered and stricter, so read a leveraged run as an **optimistic bound on
survival**, not a venue simulation. It also cannot represent a venue rejecting an order
for any reason other than lack of account funds — which neither path has any
reason to model, because neither can reach a venue.

A live session inherits every one of those limits, and adds one of its own:
its bars arrive with **holes** (missing windows/ticks, feed interruptions),
which are annotated in the trace but *not* simulated as market conditions. So
a live session's totals are only as good as the data that arrived — read the
`[feed gap: …]` annotations before trusting them.

[^accountingsrc]: `src/accounting.rs`: state machine in `PaperAccount::on_bar`, `exit_position`, equity mark, day arithmetic, and the test module with hand-computed reference values (long/short round trips, reversal, zero-fee exactness, insufficient-funds path, known UTC dates)

[^configsrc]: `src/config.rs`: `starting_balance` (finite, > 0), `trade_fee_bps` (unsigned bps) and `max_leverage` (integer `1..100`) in all three precedence layers with error-naming tests

[^liversrc]: `src/live.rs`: `SessionState::new` (constructs the same `ReplaySession`/`PaperAccount`), `log_mock_trade`, `DayAccumulator::record` (entries counted as funded position openings, matching `per_day_totals`), and the reconciliation test asserting a live day's summary equals the report's per-UTC-day roll-up
