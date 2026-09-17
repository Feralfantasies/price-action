---
type: Playbook
title: Bundle Update Guide
description: How to keep this OKF bundle in sync with the code — which documents a change must touch, and how frontmatter gets maintained.
tags: [documentation, maintenance, okf]
status: draft
sources:
  - id: spec
    resource: https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md
    title: OKF v0.2 specification (the format this bundle conforms to)
  - id: agents-md
    resource: /AGENTS.md
    title: Repository agent rules that mandate this process
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

This bundle is a [OKF v0.2](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md)
knowledge base for the repository.[^spec] It is **not optional documentation**:
[AGENTS.md](../AGENTS.md) requires any agent working in this repo to read it
before making changes and to keep it current with every change that alters
documented behaviour.[^agents-md] This page defines *how*.

## The rule

> **Every PR that changes documented behaviour (code, configuration surface,
> CLI, data formats, container/CI behaviour, sample data) MUST update the
> affected concepts in `docs/` and this bundle's [log](log.md) in the same
> pull request.**

Docs that drift from code are a defect: agents use this bundle as their primary
source of truth when working here. If you cannot tell which documents your
change affects, re-read this page's mapping table — or ask a human before
merging.

## Change → document mapping

| Your change touches… | You MUST update |
|---|---|
| CLI surface (argv, subcommands, exit behaviour) | [Replay Workflow](replay-workflow.md) invocation section, [Development Workflow](development-workflow.md) smoke matrix, `README.md` quick start. |
| Replay report shape or paper-accounting semantics | [Replay Workflow](replay-workflow.md) (expected output + guarantees), [Paper Trading Accounting](paper-trading-accounting.md) for any change to pricing rules/skips/roll-ups, and every expected-output block in `README.md` + [Sample Bar File](sample-bars.md) when the default run changes. |
| Strategy rules or a new strategy | [Consecutive Closes Strategy](strategy-consecutive-closes.md) (or a new concept per strategy), [Overview](overview.md) scope if "only shipping strategy" changes; new files also need an [index.md](index.md) entry. |
| Configuration knobs (add/remove/rename) | [Configuration](configuration.md) table, `price-action.example.toml`, `README.md` config section, and [Replay Workflow](replay-workflow.md) A/B examples if replay-visible. |
| Bar/CSV format or validation | [Bar File Format](bar-file-format.md); [Sample Bar File](sample-bars.md) if fixtures change; `README.md` data-creation notes. |
| Engine loop or its invariants (retry, `last_signal`) | [Trading Engine](engine.md), and [Execution Layer](execution-layer.md) for broker-contract implications. |
| Market data types (`Bar`, `BarSeries`) | [Market Data Model](market-data-model.md). |
| Execution layer / new broker or dependencies | [Execution Layer](execution-layer.md); **if any TLS/HTTP dependency: the rustls-vs-OpenSSL rule in [Container Image & Release](container-image-and-release.md) applies**; [Overview](overview.md) scope for "paper only" accuracy. |
| Container, image, CI or release pipeline | [Container Image & Release](container-image-and-release.md). |
| Lint policy, toolchain or test conventions | [Development Workflow](development-workflow.md). |
| Sample data (rows, cadence, values) | [Sample Bar File](sample-bars.md), every expected-output block in `README.md` and [Replay Workflow](replay-workflow.md), re-verified with a real replay run. |
| Legal/risk positioning | [Disclaimer & Risk](disclaimer-risk.md) **and** the `README.md` top disclaimer (keep them consistent; the governing text is `DISCLAIMER.md`). |

## Editing a concept

1. **Body first, structure preserved.** Keep conventional headings (`# Schema`,
   `# Examples`, `# Computation`) where they fit — structural markdown over
   prose helps both readers and retrieval agents.
2. **Frontmatter maintenance rules:**
   - Bump `generated.at` to the edit time (ISO 8601 UTC, e.g.
     `2026-09-16T23:15:00Z`) and keep `generated.by` as the acting agent in
     the org actor convention (`<harness>-agent/<model>`).
   - Keep `sources[].resource` pointing at artifacts that still exist and still
     support the claims; add a source when you start relying on a new file or
     external document. **Per-claim attribution uses footnotes whose label is
     exactly a `sources[].id`** — if your footnote label doesn't match an id,
     the attribution link is broken.
   - `verified`: **do not add or extend it unless a specific named human has
     actually reviewed the content.** A human review of the enclosing PR counts;
     write `{ by: human:<github-id>, at: <date> }`. (Workspace golden rule: never
     claim a human review that has not happened.)
   - `status`: new/uncertain concepts start `draft`; flip to `stable` when they
     pass human review; use `deprecated` for retired concepts **instead of
     deleting them** so links and history survive.
   - `stale_after`: an absolute instant after which the content is stale by
     definition. Leave it unset unless there is a concrete known expiry (a
     scheduled policy, an EOL date); do not invent relative TTLs.
3. **Cross-links.** Link related concepts with standard markdown links
   (relative within `docs/`, absolute `/...` if you prefer bundle-root paths —
   either is OKF-valid; be consistent per file). A link to a concept that does
   not exist yet is permitted ("not-yet-written knowledge") but should be
   resolved in the same PR where feasible.
4. **Index.** New or removed files get an entry (or removal) in
   [index.md](index.md), grouped under the right heading, with a short
   description matching the concept's `description`.
5. **Log.** Append an entry to [log.md](log.md): newest first, ISO date
   heading, prose sentence naming the changed concept and why (and the PR link
   when it exists).

## Adding a new concept

1. Name the file kebab-case after the thing described (e.g.
   `strategy-mean-reversion.md`), in this directory for flat top-level topics
   or under a subdirectory with its own `index.md` for families of concepts
   (mirroring the spec's progressive-disclosure pattern).
2. Frontmatter minimum: `type` (a descriptive value — existing examples:
   `Reference`, `Playbook`), plus `title`, `description`, `tags`, and
   `generated`. Bind a `resource`/`sources` entry to the code it documents.
3. Link it from at least one existing concept (usually [Overview](overview.md)
   or the relevant sibling) and add its index.md entry in the same commit.

## Verifying before committing docs

- Every non-reserved `.md` file has a parseable YAML frontmatter with a
  non-empty `type` (`index.md`/`log.md` are the reserved exceptions).
- Every footnote label `[^id]` resolves to a `sources[].id` in the same file,
  and every `sources[].resource` path exists.
- All bundled relative links resolve (open each or run a quick link check).
- Where code behaviour is cited, cite it the way this bundle does: file-level
  source entries + tests named where relevant — claims should be re-verifiable
  by re-running the commands in [Development Workflow](development-workflow.md).

## House conventions used in this bundle

- Concept types in use: `Reference` (descriptive docs of code/formats),
  `Playbook` (procedural how-tos, incl. this guide and the replay workflow),
  `Data Sample` (the committed fixture). New types must be self-explanatory
  strings; there is no registry.
- One concept per concern; split rather than bloat (an oversized doc is a
  retrieval failure for future agents).
- No secrets, tokens, or internal infrastructure details may ever appear in
  this bundle (workspace golden rule) — the disclaimer and rustls rules are
  fine as policy statements.

[^spec]: OKF v0.2 SPEC.md (GoogleCloudPlatform/knowledge-catalog): bundle structure, frontmatter families, reserved filenames, per-claim attribution

[^agents-md]: Root `AGENTS.md` "Read the documentation FIRST" / "Keep the documentation current" sections
