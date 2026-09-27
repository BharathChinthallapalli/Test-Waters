# Roadmap — one feature spec at a time

Status: `active` (spec exists, in progress) · `next` · `queued` · `done`.
Write a spec only when a feature becomes active. Scope notes below, plus the detailed acceptance ideas in docs/backlog-notes.json, are inputs, not specs.

| # | Feature | Status | Scope (input for its future spec) |
|---|---|---|---|
| 01 | foundation | done | Workspaces, ADRs, hardened Electron shell, Rust→TS types, CI, OSS files |
| 02 | daemon-and-store | done | Loopback-only daemon, token-auth JSON-RPC control API, SQLite WAL, append-only hash-chained event log, content-addressed message blobs (capture off by default) |
| 03 | passthrough-proxy | active | Anthropic Messages proxy (plain + SSE), key never stored, recording off the critical path (bounded queue, drop+count), W3C traceparent, golden transparency tests, adapters/claude-code |
| 04 | log-integrity | queued | Root Ed25519 key in OS keychain, signed C2SP checkpoints, `verify-log` CLI |
| 05 | budgets | queued | Policy schema + tamper check, versioned price table, reserve/settle ledger with idempotency keys and startup reconciliation, 429 + Retry-After, streaming cap |
| 06 | rate-limits | queued | GCRA RPM/TPM/concurrency per scope, informational RateLimit headers, loop detection v0 |
| 07 | release-v0.1 | queued | Daemon + CLI release, checksummed installers, quick start under 5 minutes |
| 08 | cockpit | queued | Electron dailies timeline, call detail, health, budget/policy editor (taste skill) |
| 09 | ingest | queued | OTLP receiver + semconv normalization, exporters for OpenClaw/Hermes/Claude Code/Codex, MCP 2026-07-28 proxy (no confused deputy) |
| 10 | workers-and-identity | queued | ACP host, worker registry lifecycle, spec digest, hire record, wrap attestation (in-toto + DSSE), task board, approvals |
| 11 | casting | queued | Capacity-aware dispatch (load, quota, context headroom, skill, cost), handoff before context rot, recorded decisions |
| 12 | sides | queued | Token-budgeted context packs, recorded injection, eval set before claiming benefit |
| 13 | souls-and-continuity | queued | Portable souls, global + ephemeral memory from the event log, local and deletable |
| 14 | maintenance | queued | Adapter health, runtime doctor with confirmation, retention, backup/restore |
| 15 | reshoot | queued | Fork-from-step replay: determinism design doc first, snapshot + log |
| 16 | blame | queued | Signed worker commits, diff → span → wrap attestation |
