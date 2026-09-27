# Vision — Callsheet (working name, unconfirmed)

## The problem
Developers now run several AI coding agents (Claude Code, Codex, Hermes, OpenClaw, pi, Ralph
loops). Each has its own UI, limits, logs and memory. Nobody sees across them, nobody decides
which agent should take which task, costs are invisible until the bill, and long sessions
degrade as context rots.

## The idea: ephemeral hiring for AI agents
A local desktop control plane, intended for public open-source release, that
**hires AI workers per task and releases them when done**. The current
repository is private and the worker lifecycle remains a proposed capability.

- Paperclip hires fixed staff: a CEO agent, a CTO agent. Here nobody is on payroll.
- You register adapters (harnesses or APIs). For each task, Callsheet casts whichever worker
  has capacity, quota, context headroom and the right skills *right now*, starts a fresh
  session, injects a precise context pack, and releases the worker at wrap.
- Memory and identity (souls) belong to the project, not to any worker. Like Hermes, it grows
  with you; unlike Hermes, it is independent of any one runtime.
- In hosted mode, Callsheet aims to register workers and sign daemon-observed
  records. Signatures authenticate the daemon's claim about retained events;
  they do not independently prove every action performed by an external worker.

## The film-set metaphor (also the code vocabulary)
| Film set | Callsheet |
|---|---|
| Call sheet | Today's tasks and who is hired for each |
| Casting | Dispatcher: load, quota, context headroom, skill, cost |
| Sides | Token-budgeted context pack for one task |
| The character | Soul: the role persists, the worker changes |
| Continuity | Global memory + ephemeral per-task memory |
| Wrap | Record, update memory, release the worker, destroy its key |
| Dailies / reshoots | Observability / fork-from-step replay |

## The six pillars
Observability · Control (budgets, rate limits, loop detection) · Monitoring · Delegation +
Tasks · Maintenance · Replay/Blame. One recorder (append-only event log) underneath all of them.

## Why it can win (the edge)
1. **Neutral dispatcher** outside every agent, not one agent acting as boss.
2. **Capture without an SDK for supported integrations**: a configured proxy
   sees model calls routed through it; OTLP and ACP add only the information
   their runtimes actually expose. Changing a base URL can alter client features.
3. **Observed usage signals**: for routed calls, the proxy can see provider
   usage and rate-limit responses when provided. Subscription headroom,
   task quality, and actions outside the proxy require other evidence.
4. **Verifiable integrity of retained records**: future signed checkpoints and
   commits can show that recorded data changed; they do not prove that every
   action was captured or that an untrusted worker performed it.

Individually, every feature exists somewhere (see RESEARCH.md). The combination and the
neutrality are the product.

## Two capture modes
- **Passive**: LLM proxy + optional OTLP ingest, for agents whose model
  traffic or telemetry can be routed to Callsheet. It cannot record or gate
  unrelated file and terminal actions. Records can be dropped under pressure;
  any replay is limited to captured data and requires content capture.
- **Hosted**: Callsheet acts as an ACP client. It can record and gate operations
  mediated through its advertised capabilities. Direct actions outside those
  methods still need a separate observation or enforcement mechanism.

## Stack and why each language
- **Rust**: daemon, proxy, store, identity — speed, safety, always-on.
- **TypeScript + Electron**: the cockpit UI.
- **Python** (only from the replay feature): eval and replay harness, where the ecosystem is.

## Non-goals
Own model or agent runtime, chat channels, fixed org charts, multi-user, cloud hosting.

## Open-source approach
Personal MIT-licensed project, currently private, with a public open-source
release intended. No employer code. No Callsheet telemetry by default; model
requests explicitly routed through the proxy still go to the configured
provider. Ship a useful daemon + CLI workflow before claiming the wider
control-plane vision. Adapters can be contributed after publication.
