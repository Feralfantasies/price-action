---
type: Reference
title: Execution Layer
description: The Broker trait, Position enum, and the in-memory PaperBroker — still the only broker implementation, used by replay and live sessions alike.
tags: [execution, broker, architecture]
status: draft
sources:
  - id: execution-src
    resource: /src/execution.rs
    title: Execution module source (impl + tests)
  - id: livers
    resource: /src/live.rs
    title: Live session source (reuses ReplaySession, hence PaperBroker)
generated: { by: pi-agent/qwen3.8-max, at: 2026-09-28T02:25:00Z }
---

`execution.rs` defines the seam between decisions and order placement.[^execution-src]

## `Position`

The state a broker should be holding: `Flat` (default), `Long`, or `Short`.
Signals map 1:1 onto positions in the [engine](engine.md) (`Flat → Flat`,
`Long → Long`, `Short → Short`).

## `Broker` trait

| Member | Meaning |
|---|---|
| `fn set_position(&mut self, target: Position) -> Result<(), Error>` | Move the account to `target`; return `Error::Execution` when the backend cannot reach it. |

Design constraint imposed by the [engine's retry rule](engine.md): a successful
call may be repeated after an earlier failed one (the engine retries any
pending position before consuming the next bar), so **broker implementations
must behave sensibly under repeated `set_position` calls for the same target**.

## `PaperBroker`

In-memory placeholder: it only tracks its intended
position, starting `Flat`, and always succeeds. It never places orders, models
no costs, and keeps no fills history. It is what makes both data paths safe by
design — replay always runs on a `PaperBroker` regardless of configured mode
(see [Replay Workflow](replay-workflow.md)), and a
[live session](live-market-data-session.md) runs the *same* `ReplaySession`, so
it inherits the same `PaperBroker`. Streaming real market data therefore adds
no order path: the feed is data-in only, and there is still no way for this
program to place a trade anywhere.[^livers]

| API | Meaning |
|---|---|
| `PaperBroker::new()` | Flat broker (same as `Default`). |
| `position() -> Position` | The position currently held. |

## Real brokers: not implemented yet

Live trading (`mode = "live"`) is a validation-only path today — see
[Overview](overview.md), [Configuration](configuration.md). When a real venue
adapter lands it:

1. must implement `Broker` (and keep the idempotency-friendliness note above);
2. will need TLS — and therefore **`rustls`, never OpenSSL** — because the
   container is `FROM scratch`; see [Container Image & Release](container-image-and-release.md) for
   the exact dependency rule;
3. must respect the layered configuration (`broker_url`, `broker_api_key`) and
   keep secrets out of the repo (env or mounted secret, never committed);
4. must be accompanied by docs updates here (this file, [Overview](overview.md)
   scope, [Development Workflow](development-workflow.md) if it changes what CI
   can run) — see [Bundle Update Guide](bundle-update-guide.md).

[^execution-src]: `src/execution.rs` module and test: paper broker tracks position through set_position calls

[^livers]: `src/live.rs`: `SessionState::new` builds a `ReplaySession`, which constructs `Engine<ConsecutiveCloses, PaperBroker>`; `src/feed.rs` has no execution surface at all (no `Broker` import, no send path other than auth/subscribe frames)
