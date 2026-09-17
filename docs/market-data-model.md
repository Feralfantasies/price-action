---
type: Reference
title: Market Data Model
description: The Bar (validated OHLCV) and BarSeries rolling window types that all strategies receive.
tags: [market-data, architecture]
status: draft
sources:
  - id: market-src
    resource: /src/market.rs
    title: Market module source (impl + tests)
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

`market.rs` holds the raw price data types. Everything downstream — strategies,
the engine, [replay](replay-workflow.md) — receives `Bar`s.[^market-src]

## `Bar` — one OHLCV bar

Construction is only through `Bar::new(timestamp, open, high, low, close, volume) ->
Result<Bar, Error>`:

- **Fields are private** and exposed via read-only getters (`timestamp()`,
  `open()`, `high()`, `low()`, `close()`, `volume()`), so the validation cannot
  be bypassed by direct construction.
- **Finiteness is enforced:** any NaN or infinite price/volume is rejected with
  `Error::MarketData`. This matters because Rust's `f64` parsing accepts
  `"inf"`, so a naive parse path would otherwise let non-finite values in.
- **Inverted highs/lows are silently normalised** to `low <= high` (swapped,
  not rejected) — defensive handling for malformed upstream data.

Derived helpers:

| Method | Meaning |
|---|---|
| `is_bullish()` | `close >= open` (a doji counts as both). |
| `is_bearish()` | `close <= open`. |
| `range()` | `high - low`, the full bar range. |
| `body()` | `abs(close - open)`, always non-negative. |

Timestamps are `SystemTime`; in [bar files](bar-file-format.md) they appear as
Unix seconds. The `BarSeries`/CSV layer uses checked/clamped conversions and
documents clamping to 0 for pre-epoch timestamps — never wrapping.

## `BarSeries` — rolling window

A fixed-capacity, **oldest-first** window of the most recent bars:

| API | Meaning |
|---|---|
| `BarSeries::new(capacity)` | Empty series; `capacity == 0` is treated as `1`. Configured by `series_capacity` (default 500) — see [Configuration](configuration.md). |
| `push(bar)` | Append, evicting the oldest bar when at capacity. |
| `last()` / `len()` / `is_empty()` | Standard window queries. |
| `iter()` / `IntoIterator for &BarSeries` | Iterate **oldest first**. |

Note: as of this writing the shipping strategy (`ConsecutiveCloses`) keeps its
own previous-close state and does not consume a `BarSeries`; the type exists as
the rolling-window primitive for window-based strategies (e.g. higher-timeframe
confirmation or bar-count rules). If you wire one in, keep the oldest-first
iteration order — code that assumes newest-first will be subtly wrong on the
oldest element after evictions.

[^market-src]: `src/market.rs`: `Bar`/`BarSeries` impls and their test modules (finiteness rejection, high/low normalisation, capacity eviction)
