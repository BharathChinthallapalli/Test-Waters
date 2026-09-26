# Progress log

## Codebase patterns
- (sessions add reusable patterns here)
- Crates inherit metadata with `{key}.workspace = true` and opt into shared lints with
  `[lints] workspace = true`; lints are not inherited implicitly.
- `rust-version` in `[workspace.package]` tracks the channel pinned in `rust-toolchain.toml`.
- Biome reads `.gitignore` (`vcs.useIgnoreFile`) and formats JSON with 2-space indentation to
  match existing files. Run `pnpm exec biome format --write <file>` to fix format errors.
- pnpm 12 records its own version under `packageManagerDependencies` in `pnpm-lock.yaml`.

---

## 2026-09-26 — 01 foundation, task 1 (Set up workspaces)

Done: root `Cargo.toml` (resolver 3, `workspace.package`, `workspace.lints`), stub crates
cs-core, cs-store, cs-proxy (libs) and cs-daemon (only binary), each with one test;
`rust-toolchain.toml` (1.98.1, rustfmt + clippy, minimal profile); `.gitignore`;
`pnpm-workspace.yaml` (apps/*, packages/*); root `package.json` (pnpm 12.6.0, Biome 2.5.14,
`check`/`format`/`typecheck` scripts); `biome.json` from `biome init`.

Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (4 tests), `pnpm biome check .`, `pnpm -r typecheck` (no packages yet)
all pass. `cargo deny check` not run: deny.toml and cargo-deny arrive with task 5.2.

Sources (official docs; the doc sites were blocked by this session's network proxy, so the same
content was read through Context7 and the projects' GitHub doc sources):
- https://doc.rust-lang.org/cargo/reference/workspaces.html
- https://doc.rust-lang.org/cargo/reference/manifest.html#the-lints-section
- https://rust-lang.github.io/rustup/overrides.html
- https://pnpm.io/workspaces · https://pnpm.io/package_json · https://pnpm.io/settings
- https://biomejs.dev/reference/configuration/
- Versions: https://static.rust-lang.org/dist/channel-rust-stable.toml (1.98.1),
  npm registry dist-tags for pnpm (12.6.0) and @biomejs/biome (2.5.14).

Decisions made in this task:
- `unsafe_code = "deny"` (not `forbid`) so a future crate can allow it locally with a
  written justification; clippy `all` warns and CI's `-D warnings` makes it fatal.
- `docs/backlog-notes.json` was reformatted by Biome (whitespace only; `jq -S` output is
  identical) so `biome check .` passes on the existing tree.

Follow-ups (not in this task's scope):
- Owner decision: the repo already has an MIT `LICENSE` from its initial commit, but
  requirement 6.4 says no LICENSE file until ADR 0008 is Accepted. Resolve in task 2.2/6.3.
- `docs/backlog-notes.json` still uses the old "Blackbox" / `bb-*` names; the spec and
  steering use Callsheet / `cs-*`, which this task followed.
- Pin the Node.js version (e.g. `devEngines.runtime` or CI `setup-node`) with task 3 or 5.

## 2026-09-26 — 01 foundation, task 2 (Record architecture decisions)

Done: `docs/adr/template.md` and ADRs 0001–0007 (Accepted) plus 0008 licence
(Proposed, owner decision). Each has Status, Context, Decision, Consequences
and Sources; 0005 pins exact versions and commits.

Findings that change later work:
- The OTel GenAI conventions moved out of `semantic-conventions` (v1.44.0) into
  `open-telemetry/semantic-conventions-genai`, which has no tags. ADR 0005 pins
  its commit `e57c543b`; re-pin when upstream tags a release.
- MCP 2026-07-28 is stateless (no `initialize`, version in `_meta`,
  `server/discover`). Feature 09 must be designed for that shape.
- A C2SP checkpoint's root hash is an RFC 6962 Merkle tree root, not a hash-chain
  head. ADR 0007 keeps the per-event chain and adds a Merkle tree over event
  hashes for checkpoints; feature 04 designs the details.
- ACP wire compatibility is the negotiated `protocolVersion` (1), not the schema
  or crate version.

Source access: the protocol sites (w3.org, ietf.org, modelcontextprotocol.io,
c2sp.org, opentelemetry.io) were blocked by this session's proxy. Specs were
read from their Git sources at the pinned tags or commits (git clone over
https://github.com works), and JSON-RPC, SQLite and Biome docs through Context7.

Follow-ups:
- Owner decision: ADR 0008 licence (MIT, Apache-2.0 or dual) and the existing
  MIT `LICENSE` file. Task 6.3 stays blocked on it.
- Owner review: ADRs 0001–0007 are marked Accepted because they restate
  decisions already in `.kiro/steering/`; the specific pins in 0005 and the
  Merkle-tree consequence in 0007 are new detail worth a look.

### PR #3 review changes (task 2)

- ADR 0005: Trace Context now pins Level 1 from the `level-1` branch (commit
  `6f387678`), `sampled` flag only; the `main` branch is the Level 3 draft.
- ADR 0007: event hashing fully fixed (one chain per run, hash input = JCS of
  the event without `event_hash`, hex encoding, all-zero genesis, Merkle leaves
  in daemon commit order). Separate root key (DSSE only) and checkpoint key
  (signed notes only); hire records bind the worker public key.
- ADR 0006: content blobs addressed by HMAC-SHA-256 with a per-install key;
  no content reference at all when capture is off.
- ADR 0003: exact `Host` value, `Authorization: Bearer`, token held only by the
  Electron main process.
- ADR 0008 links issue #4 (licence decision).
