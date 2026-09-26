---
inclusion: always
---
# Tech stack and constraints

- Rust (daemon, proxy, store, identity), TypeScript + Electron (desktop), Python only from the
  replay feature onward (uv, ruff).
- Daemon runs independently of the UI, binds 127.0.0.1 only, control API = JSON-RPC 2.0 over
  localhost HTTP with a per-install token.
- SQLite (WAL) owned only by the daemon. Event log is append-only, sequenced per run,
  hash-chained (SHA-256 over RFC 8785 canonical JSON), with signed C2SP checkpoints.
- TS types are generated from Rust. Never hand-mirror types or constants.
- Electron: contextIsolation, sandbox, no nodeIntegration, strict CSP, narrow preload API.
- Tooling: rustfmt, clippy -D warnings, cargo-deny, Biome, tsc --noEmit.

## Protocol pins
OTLP (stable) · OTel GenAI semconv (pinned commit; cost under `callsheet.cost.*`, never
`gen_ai.*`; content capture is opt-in, so replay content comes from the proxy) ·
MCP 2026-07-28 · ACP · W3C Trace Context · 429 with Retry-After (IETF RateLimit headers
draft-11 informational only) · in-toto Statement v1 + DSSE · C2SP tlog-checkpoint.
Agent-identity IETF documents (AIMS, AIP, PEDIGREE) are individual drafts: watch, don't depend.

## Security (non-negotiable, even on localhost)
- Never log, store or echo API keys or auth headers; redaction has a test.
- Private keys live only in the daemon (root key in the OS keychain); workers never hold keys.
- Tamper-evident, not tamper-proof: say so in docs.
- Policy file outside agent-writable workspaces; out-of-band edits rejected.
- New dependency: check licence and advisories first. Never invent crypto.
