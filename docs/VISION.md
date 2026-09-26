# Vision — Callsheet (working name, unconfirmed)

## The problem
Developers now run several AI coding agents (Claude Code, Codex, Hermes, OpenClaw, pi, Ralph
loops). Each has its own UI, limits, logs and memory. Nobody sees across them, nobody decides
which agent should take which task, costs are invisible until the bill, and long sessions
degrade as context rots.

## The idea: ephemeral hiring for AI agents
A local, open-source desktop control plane that **hires AI workers per task and releases them
when done**.

- Paperclip hires fixed staff: a CEO agent, a CTO agent. Here nobody is on payroll.
- You register adapters (harnesses or APIs). For each task, Callsheet casts whichever worker
  has capacity, quota, context headroom and the right skills *right now*, starts a fresh
  session, injects a precise context pack, and releases the worker at wrap.
- Memory and identity (souls) belong to the project, not to any worker. Like Hermes, it grows
  with you; unlike Hermes, it is independent of any one runtime.
- Every worker is registered and deregistered, cryptographically identified, and every action
  lands in a tamper-evident log.

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
2. **Zero-code capture**: proxy + open standards (ACP, OTLP, MCP). Competing replay tools need
   an SDK in your code; this needs a base-URL change.
3. **Live capacity data**: the proxy sees real usage and 429s for every worker, which no
   agent-side skill can see. Casting runs on facts, not guesses.
4. **Verifiable provenance**: worker identity + signed log + signed commits.

Individually, every feature exists somewhere (see RESEARCH.md). The combination and the
neutrality are the product.

## Two capture modes
- **Passive**: LLM proxy + OTLP ingest. Works with any agent. Replay covers LLM calls only.
- **Hosted**: Callsheet is the ACP client and owns file and terminal operations. Enables full
  delegation, recording of side effects, and full replay.

## Stack and why each language
- **Rust**: daemon, proxy, store, identity — speed, safety, always-on.
- **TypeScript + Electron**: the cockpit UI.
- **Python** (only from the replay feature): eval and replay harness, where the ecosystem is.

## Non-goals
Own model or agent runtime, chat channels, fixed org charts, multi-user, cloud hosting.

## Open-source approach
Personal OSS project, no employer anything. Nothing leaves the machine by default. Ship the
daemon + CLI first (v0.1 after budgets and rate limits), desktop UI after. Adapters are the
ecosystem outsiders can contribute to.
