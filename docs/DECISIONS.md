# Decisions

## Taken (to be recorded as ADRs in 01-foundation)
1. Rust daemon runs independently of the UI; not a stdio sidecar.
2. SQLite (WAL) owned only by the daemon.
3. Control API: JSON-RPC 2.0 over localhost HTTP, per-install token, loopback only.
4. TS types generated from Rust; never hand-mirrored.
5. Event log append-only and hash-chained from day one; signed C2SP checkpoints.
6. Two capture modes: passive (proxy + OTLP) and hosted (ACP).
7. Ephemeral workers only: fresh session per task, released at wrap.
8. Workers never hold keys; the daemon signs on their behalf.
9. Daemon + CLI ships first (v0.1); desktop UI after.
10. One spec at a time, Kiro format, approval between requirements, design and tasks.
11. No circuit breaker or retries in passthrough mode.
12. Python only from the replay feature.
13. TS types generated with ts-rs (owner-approved 2026-09-26; ADR 0009).

## Open (owner decisions — sessions must not guess)
| Decision | Options | Blocks |
|---|---|---|
| Project name | Callsheet (proposed) or other | Crate prefix, README, repo going public |
| Licence | MIT or Apache-2.0 (patent grant) | LICENSE file (ADR 0008) |
| Lint for TS | Biome (proposed, Rust-based) or ESLint | 01-foundation task 1.3 |
| First provider for the proxy | Anthropic (proposed) | Feature 03 |
| Price source | Official provider pages or models.dev | Feature 05 |
| OpenClaw as delegation target | Via its gateway, or observe-only | Feature 10 |

## Risks
- **Scope**: six pillars, solo builder. Mitigation: strict roadmap order; v0.1 before orchestration.
- **Crowded market**: every single feature exists elsewhere. Mitigation: lead with neutrality and zero-code capture, not with any one feature.
- **Replay determinism**: model non-determinism and side effects. Mitigation: hosted mode + design doc before code.
- **Spec churn**: GenAI semconv and agent-identity drafts are moving. Mitigation: pin versions, normalize at ingest.
- **Trust**: the tool records prompts. Mitigation: local-only, PRIVACY.md, redaction tests, deletable memory.
