---
type: Playbook
title: Development Workflow
description: Toolchain, strict lint policy (pedantic + panic-prevention set), verification commands, MSRV notes, and project layout for anyone changing the code.
tags: [development, tooling, lints, testing]
status: draft
sources:
  - id: toml
    resource: /Cargo.toml
    title: Package manifest + workspace clippy lint policy
  - id: code-quality
    resource: /README.md
    title: README "Code quality" section
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

The project is strict about what counts as passing code.[^toml][^code-quality]

## Toolchain

- [Rust](https://rustup.rs) via `rustup` is the **only** build requirement.
  Dependencies are intentionally minimal: `serde`, `toml`, `thiserror`.
- The crate targets **edition 2021**; test code has exercised the std APIs at
  that MSRV (e.g. `is_none_or` on `Option` and checked duration arithmetic) —
  when adding code, keep it MSRV-safe: if you reach for a newer std API, check
  the CI Rust image or pin behind a feature rather than assuming latest stable.

## Lint policy (`[workspace.lints.clippy]`)

- **`pedantic` is deny** — instances are fixed in code rather than allowed.
- **Nursery lints are warn**, not deny (experimental; they drift between clippy releases).
- **Panic-prevention restriction lints are deny:** `unwrap_used`, `expect_used`,
  `indexing_slicing`, `unreachable`, `unimplemented`, `todo`, `string_slice`,
  `panic_in_result_fn`, `panic`, `exit`. Production code therefore contains no
  `.unwrap()`/`.expect()`, unchecked indexing, or panicking constructs —
  errors propagate as `Result`s (see the shared `Error` type in [Overview](overview.md)).
- **Exception:** those panic lints are relaxed **in tests only** (`allow-unwrap-in-tests`,
  `allow-expect-in-tests`, `allow-panic-in-tests`, `allow-indexing-slicing-in-tests`),
  so test helpers may build values directly.
- `arithmetic_side_effects` and `as_conversions` are intentionally **warn**, not
  deny: they fire on safe, bounded arithmetic (date offsets, counters) where
  checked math adds noise without safety.[^toml]

## Verification commands (run these before pushing)

```sh
cargo fmt --check        # formatting (CI fails on diffs)
cargo clippy --all-targets -- -D warnings
cargo test               # full suite: lib + bin targets
```

Plus the behaviour checks your change affects — for anything touching replay,
config, or the CSV path, run a smoke matrix:

```sh
cargo run                       # no-args readiness (validates full config incl. live rules)
PRICE_ACTION_MODE=live cargo run                  # fails fast: live mode requires a non-empty broker URL
PRICE_ACTION_MODE=live \
  PRICE_ACTION_BROKER_URL=https://example.invalid cargo run   # then fails on "live trading is not implemented yet"
cargo run -- replay             # usage error, exit 1
cargo run -- replay a b         # rejected: extra arg
cargo run -- bogus              # rejected: unknown command
cargo run -- replay samples/sample-bars.csv   # expected output per Replay Workflow
```

## CI & release blast radius

Every push runs the full [CI pipeline](container-image-and-release.md)
(fmt, clippy `-D warnings`, tests, musl static build assertion, cargo-audit,
Trivy incl. the built scratch image), and `main` triggers automatic container
release to GHCR — **a red or broken push can publish a bad image.** See
[Container Image & Release](container-image-and-release.md).

## Repository layout

| Path | Purpose |
|---|---|
| `src/` | Crate (pipeline modules listed in [Overview](overview.md)). |
| `samples/sample-bars.csv` | Synthetic demo dataset — see [Sample Bar File](sample-bars.md); do not repurpose for real data. |
| `price-action.example.toml` | Reference config; copy to `price-action.toml` or set `PRICE_ACTION_CONFIG`. |
| `.github/workflows/ci.yml`, `release.yml` | CI and container release pipelines. |
| `Dockerfile` / `.dockerignore` | scratch-friendly image build (keep the static + rustls rules!). |
| `docs/` | This OKF knowledge bundle — **keep it current with your changes** ([Bundle Update Guide](bundle-update-guide.md)). |
| `AGENTS.md` (repo root) | Mandatory agent rules for this repo. |

## Commit & PR conventions in this org

Follow the workspace `AGENTS.md` golden rules: branch + worktree, every commit
carries the AI-disclosure trailer, disclosure body on the PR, human review and
merge only. This repository additionally requires the documentation updates
described in [Bundle Update Guide](bundle-update-guide.md) to land **in the same
PR** as the behaviour change they describe.

[^toml]: `Cargo.toml` workspace lint table with per-lint rationale comments

[^code-quality]: README "Code quality" section (lint policy summary and verification commands)
