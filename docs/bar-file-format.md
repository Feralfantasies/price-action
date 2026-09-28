---
type: Reference
title: OHLCV Bar File Format
description: The CSV format replay consumes and live sessions persist — header, column schema, Unix-second timestamps, validation rules, and lossless price round-trips.
tags: [csv, data-format, replay, live]
status: draft
sources:
  - id: csv-src
    resource: /src/csv.rs
    title: CSV module source (load/save, validation, tests)
  - id: sample-csv
    resource: /samples/sample-bars.csv
    title: Bundled sample bar file
  - id: livers
    resource: /src/live.rs
    title: Live session source (persist_bars / session_csv_path)
generated: { by: pi-agent/qwen3.8-max, at: 2026-09-28T02:25:00Z }
---

Bar files are the data format consumed by `replay <bars.csv>` (see
[Replay Workflow](replay-workflow.md)) and produced/consumed by `src/csv.rs`.[^csv-src]

A [live session](live-market-data-session.md) **writes this same format**: on
shutdown it persists every bar it consumed to
`<live_csv_dir>/live-<SYMBOL>-<UTC stamp>.csv`, so the session can be re-replayed
offline and reproduces exactly (both paths share one pipeline). One consequence
worth knowing: a live-session CSV is a faithful record of the bars *that session
saw*, gaps included — a `[feed gap: …]` annotation exists only in the live
report's trace lines, not in the CSV, because a file has no way to know what
never arrived.[^livers]

## Schema

```text
timestamp,open,high,low,close,volume
1725458400,211.98,212.97,211.59,212.50,45731000
1725459300,212.50,213.41,212.22,213.10,46462000
```

| Column | Meaning | Format |
|---|---|---|
| `timestamp` | Bar start | Unix **seconds** (unsigned integer), e.g. `1725458400`. |
| `open` / `high` / `low` / `close` | OHLC prices | Finite decimal numbers; any precision is accepted and round-tripped exactly (see below). |
| `volume` | Quantity traded in the bar window | Finite decimal number. |

Rules:

- **The first line must be exactly** `timestamp,open,high,low,close,volume`
  (trimmed). Any other header is an error rather than a silent schema mismatch.[^csv-src]
- One row per bar, in **chronological order**. Shuffled rows are meaningless to
  consecutive-close strategies — nothing detects or reorders them for you.
- Whitespace around fields is tolerated; empty lines are skipped.
- Bars carry no symbol and no interval: the file describes one series for one
  configured `symbol` at one assumed bar interval (see [Configuration](configuration.md)).

## Validation on load

Errors are `Error::MarketData`. **Row-level** errors carry the 1-based line
number so bad rows are locatable; file-level errors name the problem without
a line number:[^csv-src]

| Bad input | Error shape |
|---|---|
| Wrong header | `line 1: unexpected CSV header <actual> (expected \`timestamp,open,high,low,close,volume\`)` |
| Row with ≠ 6 fields | `line N: expected 6 comma-separated fields, found M` |
| Non-numeric field | `line N: invalid <column> value '<raw>' (expected a finite number)` |
| `"inf"`/`"NaN"` style values | Same line-N "finite number" error — explicit finiteness check, because Rust's `f64` parsing accepts `inf`. |
| Header only, no data rows | `bar file contains no data lines` (no line number) |
| Missing/unreadable file | `cannot read bar file <path>: <io error>` (no line number) |

## Writing / round-trip guarantees

`save_bars` writes the header plus one row per bar using Rust's default
`Display` format for `f64`, which is a **bit-exact lossless round-trip** for any
finite value — including prices with many decimal places that a fixed
`"{:.2}"` would silently truncate.[^csv-src] The test suite pins this: it saves
bars with 8–9 decimal digits, reloads them, and asserts `to_bits()` equality on
every field, then replays the file end-to-end to prove behaviour survives.

Timestamps convert via checked/clamped arithmetic: pre-epoch times clamp to
`0`, overflows clamp to `i64::MAX` — they never wrap.

## Creating files from other sources

Broker/venue exports rarely match this schema directly. The README shows a
GNU-date one-liner converting `Date,Open,High,Low,Close,Volume` (ISO dates)
into Unix seconds; verify a couple of converted rows against the venue's
published values before trusting a long backtest
(see [Replay Workflow](replay-workflow.md), "Hygiene").

## Conformance with OKF data sources

When adding persistent datasets or generated fixtures, keep them as plain CSV
under `samples/` (or similar) and document each file here plus in the repo's
[README.md](../README.md) — synthetic-vs-real status must always be explicit,
as it is for the [sample bar file](sample-bars.md).

[^csv-src]: `src/csv.rs`: `HEADER`, `load_bars`/`save_bars` and the test module (header rejection, short rows, non-numeric + inf rejection, bit-exact round-trip)

[^livers]: `src/live.rs`: `persist_bars` / `session_csv_path` (writes `csv::save_bars` output into `live_csv_dir`) and the re-replay reconciliation test
