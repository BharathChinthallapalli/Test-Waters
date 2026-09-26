# Decisions

## Taken (ADR in brackets where one records it)
1. Rust daemon runs independently of the UI; not a stdio sidecar (ADR 0001).
2. SQLite (WAL) owned only by the daemon (ADR 0002).
3. Control API: JSON-RPC 2.0 over localhost HTTP, per-install token, loopback only (ADR 0003).
4. TS types generated from Rust; never hand-mirrored.
5. Event log append-only and hash-chained from day one; signed C2SP checkpoints (ADR 0007).
6. Two capture modes: passive (proxy + OTLP) and hosted (ACP) (ADR 0006).
7. Ephemeral workers only: fresh session per task, released at wrap.
8. Workers never hold keys; the daemon signs on their behalf.
9. Daemon + CLI ships first (v0.1); desktop UI after.
10. One spec at a time, Kiro format, approval between requirements, design and tasks.
11. No circuit breaker or retries in passthrough mode (ADR 0010).
12. Python only from the replay feature.
13. TS types generated with ts-rs (owner-approved 2026-09-26; ADR 0009).
14. Licence: MIT (owner decision 2026-09-26; ADR 0008, issue #4).
15. Lint and format for TS: Biome (ADR 0004).
16. First provider for the proxy: Anthropic Messages (roadmap feature 03). An
    OpenAI Responses proxy for Codex is tracked in issue #5.
17. The repository stays private for now (owner decision 2026-09-26, issue #28).
    On GitHub Free a private repository has no rulesets, so requirement 4.2
    (CI blocks merge) holds by convention until it goes public, and private
    vulnerability reporting starts at publication (see `docs/ci.md`,
    `SECURITY.md`).
18. Deleting erases stored content and keeps event hashes, so the chain and checkpoints
    always verify; erased events show "content erased" (owner decision 2026-09-26,
    issue #7; ADR in feature 02's design).

## Open (owner decisions — sessions must not guess)
| Decision | Options | Blocks |
|---|---|---|
| Project name | Callsheet (proposed) or other | Crate prefix, README, repo going public |
| Price source | Official provider pages or models.dev | Feature 05 |
| OpenClaw as delegation target | Via its gateway, or observe-only | Feature 10 |

## Risks
- **Scope**: six pillars, solo builder. Mitigation: strict roadmap order; v0.1 before orchestration.
- **Crowded market**: every single feature exists elsewhere. Mitigation: lead with neutrality and zero-code capture, not with any one feature.
- **Replay determinism**: model non-determinism and side effects. Mitigation: hosted mode + design doc before code.
- **Spec churn**: GenAI semconv and agent-identity drafts are moving. Mitigation: pin versions, normalize at ingest.
- **Trust**: the tool records prompts. Mitigation: local-only, PRIVACY.md, redaction tests, deletable memory.
