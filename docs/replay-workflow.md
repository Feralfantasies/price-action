---
type: Playbook
title: Replay Workflow
description: The sanctioned workflow for verifying how the configured strategy reacts to historic bars — replay is read-only by design.
tags: [replay, verification, workflow]
status: draft
sources:
  - id: readme
    resource: /README.md
    title: Repository README (quick-start and replay sections)
  - id: replayrs
    resource: /src/replay.rs
    title: Replay module source (report shape and roll-ups)
  - id: accountingsrc
    resource: /src/accounting.rs
    title: Paper-account accounting source (fees, P/L, day bucketing, tests)
  - id: mainrs
    resource: /src/main.rs
    title: Binary entry-point source
generated: { by: pi-agent/use_this, at: 2026-09-17T14:30:00Z }
---

Replay answers the question *"do these settings behave the way I expect on
real market history?"* It feeds a [bar file](bar-file-format.md) through the
**same shared application engine + strategy** used by every run — but
**always against an in-memory `PaperBroker`, regardless of what `mode` the
configuration sets, and prints every signal.[^readme] This is always step 1 before any strategy is
trusted anywhere near real money.

## Guarantees (by design)

- **Read-only, always.** Replay runs on a `PaperBroker` no matter the
  configured mode and **never sends orders to any broker**; it reports what
  the strategy *would* decide. It does, however, *simulate accounting costs for
  reporting*: funds are committed (or reserved as collateral) at entries,
  flat per-side fees are charged, and cash/equity are updated bar by bar —
  all in memory; nothing is sent or persisted.[^readme][^replayrs]
- **No broker setup needed.** Replay loads configuration through
  `Config::load_for_replay()`: identical layering and general validation to a
  real run, but the execution-only rule (live mode demanding `broker_url`) is
  skipped because paper-broker replay has no use for it. An invalid symbol,
  quantity or threshold is still refused, just like for a real run.[^mainrs]
- **Full audit trail.** One line per bar in file order, with OHLCV and the
  signal after that bar; `(entry)` marks transitions from flat into a position
  and the footer counts them. Each trace line also carries the paper account's
  `cash=` (free funds) and `equity=` (free funds plus any open position marked
  at that close) immediately after that bar, and unfunded entries print an
  `(insufficient funds)` note instead.[^accountingsrc]

## Invocation

```sh
# Build (only requirement is Rust via rustup)
cargo build --release

# Smoke test: no arguments loads config and reports readiness
cargo run

# Replay the bundled synthetic sample
cargo run -- replay samples/sample-bars.csv
```

The `replay` subcommand takes **exactly one argument** (the bar-file path).
A bare `replay`, extra arguments, or any other first argument are rejected with
a usage error and exit code 1.[^mainrs]

### Expected shape of the output

```text
price-action replay - 25 bars
symbol=AAPL mode=Paper quantity=1 strategy=consecutive-closes threshold=3
paper account: starting_balance=10000.00 trade_fee_bps=5 (per side, on the notional)
------------------------------------------------------------------------------
t=1725458400    o=211.98    h=212.97    l=211.59    c=212.50    v=  45731000 -> Flat  cash=10000.00 equity=10000.00
...
t=1725461100    o=213.90    h=214.86    l=213.62    c=214.55    v=  47924000 -> Long   (entry)  cash=9785.34 equity=9999.89
...
------------------------------------------------------------------------------
closed paper trades (net of fees):
  #  side  entry day  entry @ exit day   exit @ invested gross P/L fees net P/L
  1. long  2024-09-04 214.55  2024-09-04 214.20 214.55   -0.35     0.21 -0.56
  ...

totals per UTC day (24h):
  day        entries exits realized P/L (net)
  2024-09-04 2       2     -0.88

session totals:
  starting balance                    10000.00
  final available funds               9999.12
  final equity (marked at last close) 9999.12
  realized P/L, net of fees (2)       -0.88
  fees paid (all legs)                0.43

result: signal=Flat entries=2 of 25 bars - paper execution only, no orders placed
```

- `t` is the bar's Unix-second timestamp; prices are shown to 2 decimals in
  the trace (files themselves are not truncated — see the
  [bar file format](bar-file-format.md)).
- The signal shown is the strategy's position **after** that bar.
- `cash=`/`equity=` are the paper account's free funds and total value right
  after this bar; they move on entries (capital locked + entry fee), exits
  (capital released ± realized P/L − exit fee) and while open (equity tracks
  the close).
- An empty result line (`entries=N of M bars`) plus final `signal=` summarises
  the signal trace; the footer reminds you this was paper execution only.

## How the paper account is priced

The trade tables and totals are produced by a funded **paper account** that
prices every bar's decision: entries and exits at the close of the causing
bar, fixed `quantity` sizing, flat basis-point fees per side (default 5 bps =
0.05%), reserved collateral for shorts, skip-and-note when free funds run out,
equity marked at each bar's close, and UTC-calendar-day roll-ups. The complete
rule set — including how to re-check any reported figure from a single trace
line — is documented in [Paper Trading Accounting](paper-trading-accounting.md)
(`src/accounting.rs`, test-covered). Replay remains read-only: the account
reports what the decisions *would* have cost or earned; it places no orders,
persists nothing, and has no live path behind it.

## Verifying how a setting changes behaviour

Environment variables override everything ([Configuration](configuration.md)),
which makes A/B-testing on the same dataset trivial — run the same file twice:

```sh
# Default threshold (signal after 3 consecutive higher/lower closes)
PRICE_ACTION_SYMBOL=AAPL cargo run -- replay samples/sample-bars.csv | tail -1
# result: signal=Flat entries=2 of 25 bars ...

# Stricter: require 5 in a row — fewer, later entries
PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD=5 \
  cargo run -- replay samples/sample-bars.csv | tail -1
# result: signal=Flat entries=0 of 25 bars ...
```

The paper account obeys the same settings, so A/B runs differ in the trade
tables and session totals exactly as much as in the trace. Example — a funded
account can survive a run that otherwise trades:

```sh
# Same signals (threshold 1) but starting_balance too small for a 213 close:
PRICE_ACTION_STARTING_BALANCE=100 PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD=1 \
  cargo run -- replay samples/sample-bars.csv | grep -m 2 "insufficient funds"
t=... -> Long   (insufficient funds)  cash=100.00 equity=100.00
```


Diffing full traces shows exactly when signals diverge:

```sh
cargo run -- replay samples/sample-bars.csv > trace-default.txt
PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD=2 \
  cargo run -- replay samples/sample-bars.csv > trace-t2.txt
diff -u trace-default.txt trace-t2.txt
```

The intended loop: *change one setting → replay the same historic data →
confirm the signal behaviour moved in a way you can explain.* If it doesn't,
the setting (or your expectation of the strategy) is wrong before any market
is even connected.

## Replay against Docker

The container works identically; mount the bar file read-only:

```sh
docker build -t price-action .
docker run --rm \
  -v "$PWD/aapl-daily.csv:/data/bars.csv:ro" \
  -e PRICE_ACTION_SYMBOL=AAPL \
  -e PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD=3 \
  price-action replay /data/bars.csv
```

## Hygiene when validating settings with genuine data

- Use a dataset that *contains both* conditions you care about (trending and
  choppier periods); the [25-bar sample](sample-bars.md) shows mechanics, not
  edge cases.
- If a strategy threshold is per-bar-interval (e.g. "3 consecutive closes"),
  make sure your bars match the interval the strategy assumes — daily vs
  hourly data changes what "consecutive" means.
- Convert broker/venue CSV exports to the [required format](bar-file-format.md)
  before replaying; verify a couple of rows against the venue's published
  values before trusting a long backtest.

## Updating documentation when replay behaviour changes

Any change to the CLI surface, the trace format, the entry-counting rule, or
the config path used by replay must be reflected in this document, in
[`README.md`](../README.md), and — per [AGENTS.md](../AGENTS.md) — with an
entry appended to [`log.md`](log.md) in the same pull request; see
[Bundle Update Guide](bundle-update-guide.md).

[^readme]: README.md quick-start and configuration sections

[^replayrs]: `src/replay.rs` module doc: "Replay is read-only by design"

[^mainrs]: `src/main.rs`: strict argv matching and replay config path
