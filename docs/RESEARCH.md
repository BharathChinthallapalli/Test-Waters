# Research notes (September 2026)

All findings were gathered from the projects' own repos, official specs and docs.
Re-verify anything version-sensitive before building on it.

## 1. Reference projects
| Project | What it is | Take | Avoid |
|---|---|---|---|
| [PI-Desktop](https://github.com/vastsa/PI-Desktop) | Electron + Rust + TS desktop app for the pi agent (LGPL-3.0) | Rust host owns SQLite; numbered ADRs; Cargo + pnpm workspaces; Biome | Rust host as stdio child (dies with UI); hand-mirrored Rust/TS constants; copying LGPL code |
| [Paperclip](https://github.com/paperclipai/paperclip) | Node/React "company" of agents: org chart, budgets, audit (MIT) | `adapters/*` per agent; checksummed install script; ROADMAP | Fixed hiring model; Postgres/server scale |
| [Obot](https://github.com/obot-platform/obot) | Go org-wide AI gateway + MCP governance | Token usage from streams, models.dev prices, daily limits → 429 | Competing on gateway features |
| [Ralph](https://github.com/snarktank/ralph) | ~110-line loop: fresh agent context per iteration, prd.json + progress log | Files as memory; one task per iteration | Runs agents with permission checks disabled |
| [Ponytail](https://github.com/DietrichGebert/ponytail) | One behaviour plugin shipped to ~20 agents | One manifest per agent; benchmarks folder | — |
| [OpenClaw](https://github.com/openclaw/openclaw) | Personal agent gateway (MIT) | `diagnostics-otel` (OTLP export), `acpx` (hosts Claude/Codex over ACP), gateway doctor, privacy statement | Being a fourth cockpit for its users |
| [Hermes Agent](https://github.com/NousResearch/hermes-agent) | Self-improving agent with memory and SOUL.md | `acp_adapter/` (Hermes as ACP agent), optional OTLP extra, souls | Hermes Desktop already exists (also Electron) |

## 2. Protocols
| Protocol | Status | Role | Notes |
|---|---|---|---|
| OTLP | Stable | Telemetry ingest | Ports 4317 gRPC / 4318 HTTP |
| OTel GenAI semconv ([repo](https://github.com/open-telemetry/semantic-conventions-genai)) | Development, own repo since mid-2026 | Span vocabulary | **No cost attribute** → use own namespace. **Prompt/response content is Opt-In** → replay content must come from the proxy. Pin a commit. |
| MCP 2026-07-28 ([spec](https://github.com/modelcontextprotocol/modelcontextprotocol)) | Current | Tool interception | Stateless (no initialize/session); version + capabilities in `_meta` per request; required `Mcp-Method` / `Mcp-Name` headers; `traceparent` in `_meta` |
| ACP ([repo](https://github.com/agentclientprotocol/agent-client-protocol)) | Active | Hosting agents | Client implements `fs/*` and `terminal/*`; `session/fork` exists; Rust schema crate |
| W3C Trace Context | Stable | Joining spans | proxy ↔ agent ↔ MCP |
| Retry-After (RFC 9110) + IETF RateLimit headers | RFC + draft-11 | 429 responses | Draft is informational only |
| JSON-RPC 2.0 | Stable | Daemon ↔ UI | Same codec as MCP and ACP |
| A2A v1.0 | Stable | Agent-to-agent | Not needed yet |

## 3. Competitive landscape (be honest in the README)
- **Cockpits**: groundctl (multi-agent tracking + dashboard, reads Claude Code transcripts),
  OpenClaw Studio, Koda, AgentDashboard.
- **Replay**: agent-vcr (fork from any frame, git rollback, dashboard), replayd, Mimic,
  AgentReplay; LangGraph has checkpoint time travel for its own agents. **Replay is not a
  moat by itself.** These need SDK/code integration; Callsheet does not.
- **Delegation**: codex-jr-engineer, opencode-for-claude-code (Claude as boss delegating),
  codex-fleetui (Codex worker pool), uai (context strategy per provider).
- **Gateway/budgets**: Obot at org scale; Paperclip budgets per agent.

## 4. Identity and audit
- A hash alone is not an identity. Use three: **spec digest** (what was hired), **instance id +
  ephemeral Ed25519 key** (which hire), **root-signed hire record** (authorised).
- Standards to use: [in-toto Statement v1](https://github.com/in-toto/attestation) + DSSE for
  wrap attestations; [C2SP tlog-checkpoint / signed-note](https://c2sp.org/tlog-checkpoint) for
  log checkpoints; RFC 8785 canonical JSON for hashing; git SSH commit signing for blame.
- IETF agent-identity work (AIMS reusing SPIFFE/OAuth, AIP with delegation chains, PEDIGREE
  with scope-narrowing hops) is **individual drafts**. Adopt the principle "sub-agents never
  get more capability than their parent"; don't depend on the drafts.
- Honest limit: tamper-evident, not tamper-proof against local root compromise.

## 5. System-design mappings (from the project's system-design references)
| Pattern | Applied as |
|---|---|
| Event sourcing (designed for determinism) | Append-only, per-run sequenced event log; UI tables are projections |
| Exchange sequencer / critical path split | Enforcement synchronous; recording async, bounded, drop-and-count |
| Payments: idempotency + reconciliation | Budget reserve/settle with idempotency keys and startup reconciliation |
| Redis RDB + AOF, LangGraph checkpoints | Replay = snapshot + log |
| Token bucket family | GCRA rate limiting |
| Gateway circuit breaker | **Rejected** in passthrough: it changes agent-visible behaviour |
| MCP confused deputy | Per-client consent in the MCP proxy |

Build-your-own-X reading (background only): Tokio mini-Redis tutorial (before the daemon),
Write yourself a Git / ugit (content-addressed snapshots), Writing a Linux Debugger (stepping),
Build your own shell in Rust (ACP terminal host).

## 6. Naming research
- Good OSS names are references whose trait *is* the method: Ralph (naive persistence),
  Ponytail (lazy senior dev), Paperclip (the maximizer thought experiment), Hermes (messenger).
- Two different models both suggested "Clothespin": proof of anchoring and typicality bias.
- Taken or colliding: Argus, Panoptes, Magpie, Switchboard, Patchcord (AI messenger),
  Ground Control (groundctl), Pennyworth (AI companion), Sheepdog (storage system).
- **Callsheet**: fits the ephemeral-hiring story end to end; crates.io free, npm plain name
  taken (use a scope); no AI collision found. GitHub org, domain, trademark not yet checked.

## 7. Spec format
Kiro: `.kiro/steering/` (product, tech, structure always included; custom files with
inclusion modes) and `.kiro/specs/<feature>/requirements.md` (EARS), `design.md`, `tasks.md`.
Kiro's "Run all tasks" runs independent tasks in parallel waves; run tasks individually here.
Docs: https://kiro.dev/docs/specs/ and https://kiro.dev/docs/steering/
