---
inclusion: always
---
# Project structure

```
crates/cs-core      domain types (no I/O)       crates/cs-store   SQLite, event log
crates/cs-proxy     LLM proxy + enforcement     crates/cs-daemon  binary, control API
crates/cs-otlp      OTLP receiver (later)       apps/desktop      Electron main/preload/renderer
packages/api-types  generated TS types          adapters/<runtime> per-agent setup
schemas/            JSON Schemas                python/           replay harness (later)
docs/adr/           numbered ADRs               docs/ROADMAP.md   feature order
```
- Crate prefix `cs-` follows the working name; rename in one commit if the name changes.
- One ADR per architecture decision (Status, Context, Decision, Consequences).
- Tests live next to code (Rust `#[cfg(test)]`, TS `*.test.ts`).
