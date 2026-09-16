---
type: Data Sample
title: Sample Bar File (synthetic)
description: The bundled 25-bar synthetic demo dataset at samples/sample-bars.csv — shaped like real trades, not exchange prices.
tags: [sample-data, csv, replay]
status: stable
resource: /samples/sample-bars.csv
sources:
  - id: sample-csv
    resource: /samples/sample-bars.csv
    title: The committed sample file (25 data rows)
  - id: readme
    resource: /README.md
    title: README quick-start description of the sample
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

`samples/sample-bars.csv` is the demo dataset used throughout the
[README.md](../README.md) and [Replay Workflow](replay-workflow.md). It is a
[bar file](bar-file-format.md) with **25 data rows**.[^sample-csv]

## Characteristics

- **Synthetic.** Shaped like real trades (an up-leg, a pullback, then a
  recovery), but **not actual exchange prices**. Do not cite it as a reference
  for real market behaviour or verify financial claims against it.
- **Cadence:** timestamps start at Unix `1725458400` and step by
  **900 seconds** (fifteen-minute bars), matching the file's internal
  consistency checks in the test workflow.[^readme]
- **Price range:** opens around `211.98`, peaking just over `215` mid-file, then
  pulling back — enough structure to demonstrate both entry types at low
  thresholds and no entries at high ones.

## Known output (defaults)

With the compiled defaults (`consecutive-closes`, threshold 3):

```text
result: signal=Flat entries=2 of 25 bars - paper execution only, no orders placed
```

Two `(entry)` marks appear in the trace — one on the rising leg and one after
the pullback. If a code or data change alters this line for this unmodified
file and defaults, treat it as a regression to investigate *and* refresh both
this document and the README's expected output.

## How to use it

1. Smoke test: `cargo run -- replay samples/sample-bars.csv` — see the full
   walkthrough in [Replay Workflow](replay-workflow.md).
2. A/B testing settings against it via env vars (e.g. threshold 1 vs 5) is the
   canonical example throughout the README.
3. It is committed to the repository and mounted as-is in Docker examples; do
   not edit it for experiments — copy it, or generate your own file from
   genuine history.

## Replacing or extending the sample

Any change to `samples/sample-bars.csv` (rows, cadence, values) requires:

- updating this concept (counts, timestamp step, price shape, known-output line);
- updating every expected-output block in [README.md](../README.md) and the
  [Replay Workflow](replay-workflow.md) that cites it;
- re-verifying with `cargo test` plus a manual replay run — see
  [Bundle Update Guide](bundle-update-guide.md).

[^sample-csv]: `samples/sample-bars.csv` as committed

[^readme]: README quick-start, step 2 (25 fifteen-minute bars, 900-second increments)
