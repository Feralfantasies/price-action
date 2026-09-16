---
type: Reference
title: Container Image & Release
description: The FROM-scratch container image, its static-link and rustls constraints, CI jobs, and how releases land on GHCR with git tags.
tags: [container, docker, ci, release, scratch]
status: stable
sources:
  - id: dockerfile
    resource: /Dockerfile
    title: Multi-stage Dockerfile (musl builder + scratch runtime)
  - id: ci-yml
    resource: .github/workflows/ci.yml
    title: CI workflow
  - id: release-yml
    resource: .github/workflows/release.yml
    title: Container build & release workflow
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

The project is built to run **`FROM scratch`** — the final image contains only
a statically linked binary; the runtime stage sets `[ENTRYPOINT ["/price-action"]]`.[^dockerfile] Two rules
keep the project self-contained enough for that, and they are **binding
constraints on every dependency choice**:

1. **Statically linked.** The binary is cross-built for
   `x86_64-unknown-linux-musl`, because `scratch` ships no libc. The builder
   stage installs `musl-tools`, caches dependencies by building stub sources
   against the real `Cargo.toml`/`Cargo.lock`, then builds the real code.[^dockerfile]
2. **`rustls`, never OpenSSL.** Any crate doing TLS/HTTPS must be pure-Rust
   `rustls` with bundled `webpki` roots — `scratch` has no OpenSSL and no CA
   certificate store. When adding an HTTP client, disable default features and
   opt into the rustls variant:

   ```toml
   reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
   ```

   Never pull in `native-tls`/`openssl` — it would break the scratch image at
   runtime, which local `docker build` will not catch for you (the binary just
   fails on first TLS use).

## Configuration at runtime

Nothing is baked into the image. All [configuration](configuration.md) is
supplied at runtime via `PRICE_ACTION_*` environment variables and/or a TOML
file mounted at the path named by `PRICE_ACTION_CONFIG`. A working example
(bar replay):

```sh
docker build -t price-action .
docker run --rm \
  -v "$PWD/aapl-daily.csv:/data/bars.csv:ro" \
  -e PRICE_ACTION_SYMBOL=MSFT \
  -e PRICE_ACTION_MODE=paper \
  price-action replay /data/bars.csv
```

## CI (`.github/workflows/ci.yml`)

Runs on pushes; jobs are gated by a `group` with `cancel-in-progress`, and all
jobs use read-only contents permissions.[^ci-yml]

| Job | What it enforces |
|---|---|
| Format | `cargo fmt --check`. |
| Clippy | `cargo clippy --all-targets -D warnings` (pedantic + panic-prevention lints — see [Development Workflow](development-workflow.md)). |
| Tests | `cargo test`. |
| Build (musl static) | Builds for `x86_64-unknown-linux-musl` and asserts the binary is statically linked. |
| Security Audit | `cargo-audit` against `Cargo.lock`. |
| Trivy + Container | Builds the image and runs Trivy scans, including against the built scratch image. |

## Release (`.github/workflows/release.yml`)

Triggered by green CI of `main` (`workflow_run`) plus manual dispatch; jobs run
sequentially (`create-tag` → `build-and-push` → `publish-tag` →
`create-release`):[^release-yml]

- **Compute Semantic Version Tag** — derives a semver tag (dry-runnable).
- **Build and Push Container** — builds the image for
  `x86_64-unknown-linux-musl`, scans, and pushes to
  `ghcr.io/feralfantasies/price-action` under the computed tag. References use
  a lowercase image name (GHCR requirement; this was fixed in PR #5) and both
  semver + `latest` tags are published per the README.
- **Publish Git Tag** — pushes the computed tag to the git remote using the
  workflow's git identity.
- **Create GitHub Release** — publishes a release named after the tag with the
  image reference, so `docker pull ghcr.io/feralfantasies/price-action:latest`
  (or a pinned semver) works out of the box.

Operational notes for agents:

- Do not hand-push tags or releases; the workflow owns both (`main`-triggered).
- The container is part of CI's blast radius: **any dependency change must keep
  both static linking and `rustls` intact**, otherwise the scratch image breaks
  in production before anyone notices locally.

[^dockerfile]: `Dockerfile` as committed (builder + scratch stages)

[^ci-yml]: `.github/workflows/ci.yml`: job list and permissions

[^release-yml]: `.github/workflows/release.yml`: workflow_run trigger, four-job pipeline
