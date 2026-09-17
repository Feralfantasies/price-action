# AGENTS.md — Working Rules for This Repository

This repository is `price-action`: automated price-action trading in Rust,
currently **paper-only** (no live execution path exists). It follows the
workspace-wide standard that governs every repository here: all changes land
on a branch, are committed with the mandatory AI-disclosure trailer
(`Authored-by` / `Human-Review:` / `AI-Disclosure`), are pushed and reviewed
via a Pull Request, are merged only by humans, and never contain secrets. This
file adds repository-specific rules that take precedence for matters internal
to this repo.

## Read the documentation FIRST

**Before making ANY change to this repository, read the knowledge bundle in
[`docs/`](docs/index.md).** It is an Open Knowledge Format (OKF v0.2) bundle —
start at [`docs/index.md`](docs/index.md), then at minimum:

1. [Overview](docs/overview.md) — what the project is, its pipeline, and its
   current scope (paper-only, no market-data adapters).
2. [Configuration](docs/configuration.md) — the three-layer precedence model
   and every setting.
3. [Replay Workflow](docs/replay-workflow.md) — how strategy behaviour is
   verified against historic bars; replay is read-only by design.

Then read the concept documents relevant to your change (engine, strategy,
bar format, container/CI, development workflow). **Do not guess current
behaviour from memory or from stale README snippets — the bundle plus the
source of record (`src/*.rs`, `Cargo.toml`, workflows) is authoritative.** If
the bundle and the code disagree, treat that as an incident: re-verify against
the code and fix the docs in the same PR.

## Keep the documentation current

**Every pull request that changes documented behaviour MUST update the
affected concepts in `docs/` and append to [`docs/log.md`](docs/log.md) in the
same commit stack.** "Documented behaviour" means any of: CLI surface or
argv handling, configuration keys/env vars, strategy rules, bar/CSV format or
validation, engine invariants (retry rule, `last_signal`), broker contract,
container image constraints, CI/release pipeline behaviour, lint policy, or
sample data.

The mapping of changes to required doc updates and the frontmatter maintenance
rules (what to bump, how attribution footnotes work, when `verified` may be
set) are defined in the [Bundle Update Guide](docs/bundle-update-guide.md).
A PR that changes behaviour but not the docs it covers is incomplete — leave a
review comment to yourself: *did this change touch a documented surface?*

## Hard rules (repository-specific)

- **No live trading claims.** `mode = "live"` remains configuration-only until
  a real broker implementation lands in its own reviewed PR; never describe
  replays or signals as profitable, validated, or recommendations — this is
  educational software and the [disclaimer](docs/disclaimer-risk.md) is load-
  bearing (keep `README.md`'s top disclaimer and the bundle consistent).
- **Scratch image constraints.** The container is `FROM scratch`: keep binaries
  statically linked (`x86_64-unknown-linux-musl`) and TLS on **`rustls` with
  bundled roots, never OpenSSL/`native-tls`** ([Container Image & Release](docs/container-image-and-release.md)).
  Any new network-capable dependency is subject to this before review.
- **Strict lint policy.** `clippy::pedantic` and the panic-prevention lints
  are deny in non-test code; no `.unwrap()`/`.expect()`, unchecked indexing, or
  panics outside `#[cfg(test)]` ([Development Workflow](docs/development-workflow.md)).
- **Secrets.** Never commit tokens, keys, or credentials — including in `docs/`,
  in tests, or in commit messages. Broker credentials belong in the environment
  or mounted secrets only; `Config`'s redacting `Debug` impl exists for a
  reason, and it is not a license to print configs in logs.
- **Sample data.** Do not replace [`samples/sample-bars.csv`](docs/sample-bars.md)
  with real market data without a provenance note (source + license); synthetic
  demo data stays labelled synthetic everywhere.

## Verify before pushing

Run the full pass from the [Development Workflow](docs/development-workflow.md):

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

plus the smoke matrix for any change touching CLI, config, CSV, or replay —
and a real `cargo run -- replay samples/sample-bars.csv` when anything in that
path moves (its output is documented; if it changed on purpose, update the docs).
If CI cannot be green, say exactly why in the PR body rather than implying it.
