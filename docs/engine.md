---
type: Reference
title: Trading Engine
description: The bar-to-signal-to-position loop, its last_signal semantics, and the failure-retry invariant.
tags: [engine, architecture, execution]
status: stable
sources:
  - id: engine-src
    resource: /src/engine.rs
    title: Engine module source (impl + tests)
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

The generic `Engine<S, B>` drives one [`Strategy`](strategy-consecutive-closes.md)
against one [`Broker`](execution-layer.md). Per bar: ask the strategy, then
move the broker to the position implied by the returned signal.[^engine-src]

| API | Meaning |
|---|---|
| `Engine::new(strategy, broker)` | Create with `last_signal = Flat` and no pending position. |
| `on_bar(&mut self, bar) -> Result<Signal, Error>` | Process one bar; see the flow below. |
| `last_signal() -> Signal` | Most recent signal **successfully executed** on the broker. |
| `pending_position() -> Option<Position>` | The position awaiting retry after a failed execution, if any. |

## Per-bar flow

1. If a previous execution left a pending position, retry it. On failure the
   error is returned and the bar is **not** fed to the strategy; on success the
   pending state is cleared.
2. Ask the strategy for its signal for `bar`.
3. Map the signal to a `Position` (`Flat → Flat`, `Long → Long`,
   `Short → Short`) and call `broker.set_position`.
4. On success, advance `last_signal` and return the signal. On failure, store
   the target as `pending` and return the execution error — `last_signal`
   deliberately does **not** advance.[^engine-src]

## Invariants

- **No drift.** Bars are not fed to the strategy until a previously failed
  execution succeeds, so strategy and broker state cannot fall out of step.
  The test suite pins this with a deliberately failing broker: bar 2 is
  *never* consumed while the retry of bar 1's position fails.[^engine-src]
- **`last_signal` is an executed-position truth, not "what the strategy last
  asked for".** It only advances on successful execution.
- Errors propagate as `Error::Strategy` (strategy rejected the bar) or
  `Error::Execution` (broker could not reach the target); see
  [Overview](overview.md) for the shared error type.

## Current consumers

- The no-args binary path constructs `Engine<ConsecutiveCloses, PaperBroker>`
  and reports readiness only.
- Replay constructs the same engine per run; since `PaperBroker` cannot fail,
  every replayed bar is consumed exactly once (see
  [Replay Workflow](replay-workflow.md)).
- A future live data source will feed real bars through this same loop — keep
  any new broker implementation's `set_position` idempotent-friendly, because
  the retry rule means a successful call may be repeated after a failed one.

[^engine-src]: `src/engine.rs`: impl docs and the `failed_execution_becomes_pending_and_blocks_later_bars` test
