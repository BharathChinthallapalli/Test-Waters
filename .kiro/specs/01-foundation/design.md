# Design — 01 foundation

## Overview
Two workspaces at the repo root (Cargo, pnpm), four stub crates, one hardened Electron app,
one generated-types package, a CI workflow, ADRs and OSS files. Nothing talks to anything yet;
the value is that every later feature inherits working gates and settled decisions.

## Layout
See steering/structure.md. This feature creates: `Cargo.toml`, `rust-toolchain.toml`,
`crates/cs-{core,store,proxy,daemon}`, `pnpm-workspace.yaml`, `biome.json`, `apps/desktop`,
`packages/api-types`, `.github/workflows/ci.yml`, `deny.toml`, `docs/adr/*`, OSS files.

## Components

### Rust workspace
- `resolver = "3"`, shared `[workspace.package]` (edition, rust-version, repository).
- `[workspace.lints]` sets clippy and rustc lints once; crates opt in with `lints.workspace = true`.
- cs-daemon is the only binary; others are libraries.

### Desktop shell (apps/desktop)
```
main/index.ts      create window with hardened webPreferences; block navigation + window.open
preload/index.ts   contextBridge.exposeInMainWorld('callsheet', {}) — typed, empty
renderer/          static placeholder page, CSP via <meta> and response header
```
Window options live in one exported function so a unit test can assert every flag.

### Type generation
Research candidates from official docs (e.g. ts-rs; schemars → JSON Schema → TS), pick one,
record it in an ADR. A `gen-types` script writes packages/api-types; CI runs it then
`git diff --exit-code packages/api-types`.

### CI
One workflow, jobs: rust (fmt, clippy, test, deny), ts (biome, typecheck, test), types
(regenerate + diff). `permissions: contents: read` at top level. Actions pinned by SHA with the
version in a trailing comment.

## Decisions (ADRs 0001–0008)
0001 daemon not sidecar · 0002 SQLite owned by daemon · 0003 JSON-RPC 2.0 + token ·
0004 Biome · 0005 protocol pins · 0006 passive vs hosted capture · 0007 identity and log
integrity · 0008 licence (Proposed, owner decides).

## Error handling
Not applicable beyond build failures; CI must surface the failing gate by job name.

## Testing strategy
- Rust: each stub crate has one compile-and-run test so `cargo test` exercises the workspace.
- Desktop: unit test for window options and navigation handlers.
- Types: CI diff check.
- Acceptance: fresh clone → install → all gates green, done by hand once and noted in progress.md.
