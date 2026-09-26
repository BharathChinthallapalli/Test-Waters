---
inclusion: always
---
# Product: Callsheet (working name, unconfirmed)

Local-first, open-source desktop control plane that **hires AI workers per task and releases
them when done**. Paperclip hires fixed staff; here nobody is on payroll. You register adapters
(Claude Code, Codex, Hermes, any API) and each task is cast to whichever worker has the
capacity, quota, context headroom and skills right now, with a fresh session and a precise
context pack. Memory and identity belong to the project, not to the worker.

## Edge (why this exists)
- Neutral dispatcher outside every agent, not one agent acting as boss.
- Zero-code capture: proxy + open standards (ACP, OTLP, MCP), no SDK in user code.
- The proxy sees real usage and 429s for every worker; casting uses that live data.
- Every worker has a cryptographic identity; every action lands in a tamper-evident log.

## Glossary (use these names in code and UI)
Adapter · Worker (ephemeral session) · Casting (dispatcher) · Sides (context pack) ·
Soul (portable role) · Continuity (memory) · Wrap (release) · Dailies (observability) ·
Reshoot (replay) · Spec digest · Hire record · Wrap attestation

## Non-goals
Own model or agent runtime, chat channels, fixed org charts, multi-user, cloud hosting.

## Open-source principles
- Personal OSS project: no employer code, data, domain or infrastructure.
- Nothing leaves the machine by default; PRIVACY.md is the contract.
- Ship daemon + CLI first; desktop UI follows.
- Licence and final name are owner decisions; never pick them in a session.

Feature order: docs/ROADMAP.md.
