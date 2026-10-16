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
generated: { by: pi-agent/qwen3.8-max, at: 2026-10-16T15:30:00Z }
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
- **Execution is paper-only by design, currently.** There is no live order path
  today — the only broker is the in-memory `PaperBroker`, and the binary refuses
  to run live *mode* even when configured (see [Overview](overview.md)). Any
  future live execution must land behind configuration gates, on a
  human-decided review of a PR, and must restate this disclaimer.
- **Real data, fake money.** The `live` subcommand streams genuine real-time
  market data and paper-trades it against a funded fake balance — see
  [Live Market-Data Session](live-market-data-session.md). "Live" describes the
  **data source only**; execution stays in memory and no order can be placed.
  Its numbers are a priced simulation of what the configured strategy would
  have done at the configured fee rate, with no slippage, no partial fills, no
  venue minimums and no queue — they are **not** a track record, not evidence of
  prospective performance, and not a claim that the strategy works.
- **Leveraged numbers are an approximation.** A run with `max_leverage > 1`
  prices fees and P/L on the leveraged notional at the flat per-side bps rate
  and force-liquidates at a bar close when marked equity can no longer support
  the required margin — an approximation of Kraken's tiered fee schedule and
  maintenance-margin rules, not a model of them: no intra-bar liquidation, no
  funding rates, no tiered fees. See
  [Paper Trading Accounting](paper-trading-accounting.md).
- **Even the optional Kraken key pair can only read.** The one private call in
  the codebase is a single signed `GET /0/private/Balance` used to verify
  credentials (`kraken check-keys`, or at Kraken-source `live` startup); there
  is no order path for any key to reach. See
  [Kraken Market Data & Keys](kraken-market-data.md).
- **Data holes are reported, not hidden.** A live session annotates bars that
  follow missing windows/ticks or a feed interruption, because a gap can change
  what the strategy decided. Read those annotations before drawing any
  conclusion from a session's totals.
- **Delivered summaries carry the same framing.** A daily summary sent to
  Telegram ends with "paper account only — no orders were placed, not financial
  advice", precisely because it lands on a phone where the surrounding context
  is gone (see [Telegram Notifications](telegram-notifications.md)).
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
