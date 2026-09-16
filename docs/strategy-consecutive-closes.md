---
type: Reference
title: Consecutive Closes Strategy
description: The only shipping strategy — long after N consecutive higher closes, short after N consecutive lower closes, else flat.
tags: [strategy, signals]
status: stable
sources:
  - id: strategy-src
    resource: /src/strategy.rs
    title: Strategy module source (impl + tests)
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

The `Strategy` trait is the interface every strategy implements:[^strategy-src]

| Member | Meaning |
|---|---|
| `fn name(&self) -> &'static str` | Human-readable name used in logs/reports (for this strategy: `"consecutive-closes"`). |
| `fn on_bar(&mut self, &Bar) -> Result<Signal, Error>` | Feed the next bar; return the current signal. |

Signals are one of `Flat` (hold no position), `Long`, or `Short`.

## Rule (the only shipping strategy)

`ConsecutiveCloses { threshold }`: go **long** after `threshold` consecutive
higher closes, **short** after `threshold` consecutive lower closes, otherwise
**flat**.[^strategy-src]

State machine per bar (only **close prices** are used — OHLC shape and volume
are ignored by this strategy):

1. No previous close seen yet → reset, signal `Flat`.
2. Close strictly greater than previous → increment the positive run; signal is
   `Long` exactly when the run reaches `threshold`.
3. Close strictly lower than previous → increment the negative run (mirrored);
   signal is `Short` exactly when the magnitude reaches `threshold`.
4. Close **equal** to previous → the run resets to 0 and the signal is `Flat`;
   a flat bar interrupts any accumulating streak.

Implementation notes:

- The run is tracked as one signed counter (positive = uptrend, negative =
  downtrend) with saturating arithmetic; `threshold < 1` in the constructor is
  clamped to `1`, while the config layer already rejects zero for this value.
- Comparison is strict (`>` / `<`): an equal close neither extends nor flips
  the run — it aborts it (rule 4 above).

## Parameter

| Knob | Config key / env var | Default | Doc |
|---|---|---|---|
| `threshold` (consecutive closes required) | `consecutive_closes_threshold` / `PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD` | `3` | [Configuration](configuration.md) |

Use the [Replay Workflow](replay-workflow.md) to see how changing the threshold
moves entries on historic data — it is the intended tuning loop for this
strategy.

## Worked example (threshold 3, closes only)

```text
closes:  212.50 → 213.10 → 213.90 → 214.55   run: +1 → +2 → +3 → Long (entry here)
next close falls to 214.20                    run resets → Flat
```

The [sample bar file](sample-bars.md) produces exactly this pattern: with the
default threshold-3 it yields `entries=2 of 25 bars` (one Long leg, then a
later Short-style recovery structure is not reached — the second entry comes
from the pullback/recovery shape; run replay to inspect).

## Adding a new strategy

1. Add the type and its `Strategy` impl in [`src/strategy.rs`](../src/strategy.rs)
   (or split out a module mirroring the pipeline layout in [Overview](overview.md)).
2. Wire it into whatever selects a strategy — currently replay and the no-args
   path both hard-code `ConsecutiveCloses::new(config.consecutive_closes_threshold)`;
   a selectable strategy will also need configuration surface for selecting it
   (see [Configuration](configuration.md)).
3. Cover at minimum: reaching the threshold in both directions, run reset on
   direction change or equal close, and the threshold-clamping edge. Follow the
   existing `feed`-helper test pattern in the source.
4. **Update this document** (rule, state machine, parameters, worked example)
   and the [Overview](overview.md) scope section if "only shipping strategy"
   stops being true — see [Bundle Update Guide](bundle-update-guide.md).

[^strategy-src]: `src/strategy.rs`: impl of `ConsecutiveCloses` and its test module (threshold reach, reset-on-direction-change, zero-threshold clamping)
