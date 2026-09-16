---
type: Reference
title: Disclaimer & Risk
description: Educational-purpose status of this software — not financial advice, substantial risk of loss, and the paper-only execution stance.
tags: [disclaimer, risk, compliance]
status: draft
sources:
  - id: disclaimer-md
    resource: /DISCLAIMER.md
    title: Full disclaimer document (governing text)
generated: { by: pi-agent/use_this, at: 2026-09-16T23:15:00Z }
---

**This software is for educational and research purposes only and is not
financial advice.**[^disclaimer-md] The authoritative text is
[`DISCLAIMER.md`](../DISCLAIMER.md); agents must preserve its position in any
docs, READMEs, or commits they change. Summary:

- **Not financial/investment/legal/tax advice.** Nothing in the repository —
  code, strategies, parameters, sample data, or commit messages — is a
  recommendation to buy or sell any instrument, nor an offer or solicitation
  to do so.[^disclaimer-md]
- **Substantial risk of loss**, including loss of entire invested capital.
  Automated trading adds risks on top (operational, model/strategy, data
  quality) beyond ordinary trading risk.
- **Sample data is synthetic.** The [sample bar file](sample-bars.md) is shaped
  like real trades but is not exchange data; no result shown in this bundle is
  evidence of prospective performance.
- **Paper-only by design, currently.** There is no live order path today — the
  only broker is the in-memory `PaperBroker`, and the binary refuses to run
  live mode even when configured (see [Overview](overview.md)). Any future live
  execution must land behind configuration gates, on a human-decided review of
  a PR, and must restate this disclaimer.
- **No liability.** The authors accept no liability for damages or trading
  losses.

## Practical consequences for agents working here

1. Never describe signals, backtests, or replays as "profitable", "validated
   edges", or recommendations — the correct framing is *what the configured
   strategy decided on this data*, nothing more.
2. Keep the README's top-of-file disclaimer and this bundle's positioning
   consistent; if you change one, check the other in the same PR.
3. No financial figures from real markets belong in committed datasets without
   a clear provenance note (source + license) — when in doubt, don't commit
   them; point replay users at public sources per the
   [Replay Workflow](replay-workflow.md) hygiene section.

[^disclaimer-md]: `DISCLAIMER.md` sections "Not financial advice" and "Trading involves substantial risk of loss"
