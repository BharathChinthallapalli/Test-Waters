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

## 2026-09-26 — 01 foundation, task 3 (Build the hardened desktop shell)

Done: `apps/desktop` (Electron 44.4.5, TypeScript 7.0.2, `@types/node` 24).
- `src/main/window.ts`: one pure `createWindowOptions()`; contextIsolation,
  sandbox, webSecurity on; nodeIntegration (all three), webviewTag,
  experimentalFeatures, allowRunningInsecureContent, navigateOnDragDrop off.
- `src/main/navigation.ts`: every `will-navigate` prevented and every
  `window.open` denied, applied to all web contents via `web-contents-created`.
- Renderer served from a custom `app://renderer/` scheme (no `file://`, no
  `bypassCSP` privilege) with the CSP as a response header; the same policy is
  in the page's `<meta>` tag, and a test keeps them identical. Path traversal
  and unknown file types return 404. All permission requests are denied.
- `src/preload/index.cts`: CommonJS (sandboxed preloads cannot use ESM),
  exposes a frozen, empty, typed `window.callsheet` (`CallsheetApi`).
- Unit tests use Node's built-in runner (`node --test`, native type stripping in
  Node 22.22): 13 tests, no test-framework dependency. They import no Electron
  runtime code, so CI can run them without the Electron binary.

Rendering check (Playwright `_electron` under Xvfb, driver kept outside the
repo): window renders; `window.callsheet` is a frozen empty object; `require`
and `process` are undefined in the page; inline and remote scripts are blocked;
`eval` and `new Function` throw `EvalError` from page code; navigation and
`window.open` are blocked; the served page carries the CSP header; a missing
file and an encoded `..` path return 404. The container runs as root, so
Electron was launched with `--no-sandbox` for this check: the app's
`sandbox: true` renderer mode was exercised, the OS-level Chromium sandbox was
not.

Gates: `pnpm biome check .`, `pnpm -r typecheck`, `pnpm -r test` and the Rust
gates pass. New dependencies are MIT, ISC or Apache-2.0; `pnpm audit` found no
known vulnerabilities.

Sources (Electron docs at tag `v44.4.5`, commit `694f45852a0f`):
- https://github.com/electron/electron/blob/v44.4.5/docs/tutorial/security.md
  (items 3, 4, 5, 6, 7, 13, 14, 18)
- https://github.com/electron/electron/blob/v44.4.5/docs/tutorial/esm.md
- https://github.com/electron/electron/blob/v44.4.5/docs/tutorial/sandbox.md
- https://github.com/electron/electron/blob/v44.4.5/docs/api/protocol.md
- https://github.com/electron/electron/blob/v44.4.5/docs/api/structures/web-preferences.md
- https://github.com/nodejs/node/blob/v22.22.2/doc/api/typescript.md and
  `doc/api/test.md`

Follow-ups:
- Pin Node (Electron's installer requires `>= 22.12.0`) in CI with task 5.
- The compiled preload keeps TypeScript's CommonJS `exports` marker; it runs
  fine in the sandboxed preload (verified above), but switch to a bundler if
  the preload ever needs more than `require("electron")`.
- Packaging (fuses, code signing, `asar`) belongs to the release feature.

## 2026-09-26 — Imported planning docs

Added `docs/README.md`, `docs/VISION.md`, `docs/RESEARCH.md` and
`docs/DECISIONS.md` verbatim from the owner's planning archive
(`callsheet-complete.zip`, commit `eca1392`). The archive's other files were
older copies of files already here, so the repository versions were kept.

Needs an owner look (the imported text was not edited):
- `DECISIONS.md` lists "Lint for TS: Biome or ESLint" as open, but task 1.3
  already uses Biome and ADR 0004 records it as Accepted. Either close that row
  or supersede ADR 0004.
- `DECISIONS.md` lists "Type generator: ts-rs or schemars" as an owner
  decision; task 4.1 depends on it.
- The "Taken" list says the decisions are "to be recorded as ADRs"; they now
  are (ADRs 0001–0007).

## 2026-09-26 — 01 foundation, task 4 (Generate shared types)

Done: ADR 0009 (ts-rs, owner-approved after a side-by-side comparison with
schemars → json-schema-to-typescript). `cs-core` gains `control::VersionResult`
(the `version` method's result, camelCase on the wire) with a serde round-trip
test. ts-rs is a dev-dependency used through `cfg_attr(test, …)`;
`.cargo/config.toml` points exports at `packages/api-types/src/generated`.
`pnpm gen-types` regenerates; `pnpm check-types` fails when the generated files
differ from git. `packages/api-types` re-exports the generated types and
typechecks.

Checked: `check-types` passes on a clean tree, fails when a Rust field is added
without regenerating, and a stray file in the generated folder is removed by
regeneration.

Patterns:
- Wire types derive `ts_rs::TS` only in test builds:
  `#[cfg_attr(test, derive(ts_rs::TS), ts(export))]`.
- Never edit `packages/api-types/src/generated`; Biome skips it.
- Integers that can exceed 2^53 go on the wire as strings.

Follow-up: CI (task 5) runs `pnpm check-types` in the types job.

## 2026-09-26 — Issue #14 (desktop permissions and malformed URLs)

- `src/main/permissions.ts`: `denyAllPermissions()` denies permission requests,
  permission checks and device (HID/serial/USB) permissions; 3 unit tests.
- `resolveRendererFile()` returns null for malformed percent-escapes instead of
  throwing; 2 URLs added to the tests.
- Running app, before vs after: the page saw `Notification.permission`,
  `geolocation` and `clipboard-read` as **granted** before and **denied** after;
  `app://renderer/%` and `app://renderer/%E0%A4%A` errored out before and return
  404 after.
- Source: https://github.com/electron/electron/blob/v44.4.5/docs/api/session.md
  (`setPermissionCheckHandler`, `setDevicePermissionHandler`).

## 2026-09-26 — 01 foundation, task 5 (Add CI quality gates)

Done: `.github/workflows/ci.yml` with jobs Rust (fmt, clippy, test), Rust
dependencies (cargo-deny), TypeScript (Biome, typecheck, test, `pnpm audit`),
Generated types are current (`gen-types --check`), and **CI passed**, an
always-run job that fails unless every other job succeeded. `deny.toml` allows
MIT, Apache-2.0 and Unicode-3.0, denies yanked crates, wildcards and unknown
registries or git sources. `.node-version` pins Node 22.22.2. `docs/ci.md`
lists the gates and the ruleset steps.

Hardening from issue #9: `permissions: {}` at the top with `contents: read`
per job; `persist-credentials: false`; every action pinned by full SHA
(including `actions/*`); no caches (`package-manager-cache: false`);
cargo-deny is the prebuilt 0.20.2 release binary checked against a pinned
SHA-256 (first run built it from source, ~3 minutes; review on PR #27), not its
Docker action, whose image ships Rust 1.85, older than the pinned 1.98.

Checked locally: `actionlint` 1.7.12 reports nothing; `cargo deny check` passes
and fails when `Unicode-3.0` is removed from the allow list; the **CI passed**
logic succeeds only when every result is `success` (a `skipped`, `failure` or
`cancelled` result fails it).

Needs the owner (repository setting): create the `main` ruleset requiring
**CI passed** (steps in `docs/ci.md`). Until then CI reports but does not block.

Sources:
- `action.yml` of actions/checkout v7.0.1 (`3d3c42e5`), actions/setup-node
  v7.0.0 (`82076278`), pnpm/action-setup v6.1.0 (`ea17c68d`)
- https://github.com/EmbarkStudios/cargo-deny/tree/0.20.2/docs/src/checks
- rustup CHANGELOG (`rustup toolchain install` with no arguments installs the
  active toolchain)
- GitHub docs (github/docs `main`): available rules for rulesets, creating
  rulesets for a repository

`docs/ci.md` notes that `pnpm audit` and cargo-deny read live advisory
databases, so `main` can turn red without a code change.

Follow-ups: Dependabot/Renovate and zizmor were left out by owner choice; the
SHA pins and the Electron update policy (#8) need one of them later.

## 2026-09-26 — Docs fixes from issues #15 and #11

- ADR 0005: MCP Streamable HTTP request headers (`MCP-Protocol-Version`,
  required `Mcp-Method`, `Mcp-Name` for `tools/call`/`resources/read`/
  `prompts/get`; `HeaderMismatchError` -32020 with HTTP 400), re-verified at the
  pinned tag `5f5440bb`.
- `docs/README.md` points at the first unticked task instead of "task 1".
- `docs/backlog-notes.json`: project renamed to Callsheet, `BB-*` ids to
  `CS-*` (including `dependsOn`), `bb-*` crates to `cs-*`,
  `blackbox.cost.*` to `callsheet.cost.*`; `CONTEXT.md` and `tasks.json`
  references now point at `.kiro/steering/` and `.kiro/specs/<feature>/tasks.md`;
  every story has a `roadmapFeature`. Checked: all 30 stories are identical to
  the previous file apart from these renames.

Still open for the owner (#15): the Biome row in `DECISIONS.md` (ADR 0004 is
Accepted), the first proxy provider (with #5), and whether "no circuit breaker
or retries in passthrough" gets its own ADR. From #11: the "no code copied from
LGPL PI-Desktop" rule (CS-006) belongs in CONTRIBUTING (task 6.1).
