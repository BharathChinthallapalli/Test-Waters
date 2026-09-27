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

## 2026-09-26 — 01 foundation, task 6 (Add open-source files) and owner decisions

Owner decisions recorded (2026-09-26):
- Licence **MIT** (issue #4): ADR 0008 Accepted; `license = "MIT"` in
  `[workspace.package]` (inherited by all four crates) and in the three
  `package.json` files; the existing `LICENSE` stays. `deny.toml` now checks the
  workspace's own crates against the allow list too (no `private.ignore`).
- Repository stays **private** (issue #28, option 1): requirement 4.2 holds by
  convention until publication (`docs/ci.md`); `SECURITY.md` uses a
  "private contact request" issue until private vulnerability reporting can
  be enabled at publication. This is the recorded deviation from 6.1's
  "private vulnerability reporting".
- "Decide what's best" for the rest of #15: Biome row closed (ADR 0004);
  first proxy provider Anthropic (roadmap 03), OpenAI Responses stays in #5;
  "no retries or circuit breaker in passthrough" recorded as ADR 0010.
  `DECISIONS.md` now names the ADR next to each taken decision.

Task 6 files: `README.md`, `CONTRIBUTING.md` (one task per PR, gates, licence
of contributions, no LGPL/GPL copying from #11), `SECURITY.md`,
`CODE_OF_CONDUCT.md` (Contributor Covenant 3.0 as published at
EthicalSource/contributor_covenant `7255a28`, with only its two placeholders
filled or removed), `AGENTS.md` (49 lines, points to `.kiro/steering`),
`PRIVACY.md` (nothing leaves the machine by default; telemetry only ever
opt-in), `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.yml`,
`.github/pull_request_template.md`.

Checked: every relative link in the new docs resolves; issue forms have the
required `name`/`description`/`body`, names over 3 characters and unique ids;
`cargo metadata` reports MIT for all crates; all gates pass.

Sources:
- https://github.com/EthicalSource/contributor_covenant/blob/7255a28d23d5bc296de2e4e4e9bb5ee1126f1345/content/version/3/0/code_of_conduct.md
- GitHub docs (github/docs `main`): syntax for issue forms, form schema,
  configuring issue templates (`config.yml`), creating a pull request template.

Follow-up: the security and conduct contact is an issue-based request for a
private channel; add a dedicated contact address when the owner wants one.

## 2026-09-26 — 01 foundation, task 7 (Checkpoint: fresh clone passes every gate)

Fresh clone of `main` at `e4eb7f4` from GitHub into an empty directory, with
its own `CARGO_TARGET_DIR`. Toolchain: rustc 1.98.1 (from
`rust-toolchain.toml`), Node 22.22.2, pnpm 12.6.0, cargo-deny 0.20.2.

| Gate | Result |
|---|---|
| `rustup toolchain install` | ok |
| `cargo build --workspace --locked` (req 1.1) | ok |
| `cargo fmt --all --check` | ok |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | ok |
| `cargo test --workspace --locked` | ok, 5 tests |
| `cargo deny check` | advisories, bans, licenses, sources ok |
| `pnpm install --frozen-lockfile` (req 1.3) | ok |
| `pnpm biome check .` | ok, 25 files |
| `pnpm -r typecheck` | ok (desktop, api-types) |
| `pnpm -r test` | ok, 17 tests |
| `pnpm audit` | no known vulnerabilities |
| `pnpm check-types` (req 3.2) | ok |

The working tree was still clean after all gates (no generated drift). GitHub
Actions on the same commit (push to `main`, run 7) passed every job including
**CI passed** (req 4.1).

Desktop app built from the fresh clone and launched under Xvfb (`--no-sandbox`
because the container runs as root): it renders `app://renderer/index.html`;
the hardened web preferences, CSP header, blocked navigation and
`window.open`, frozen empty `window.callsheet`, denied permissions and 404s for
missing, escaping and malformed paths all hold.

Feature 01 is done; `docs/ROADMAP.md` marks it `done`. Known gaps carried
forward, by owner decision: requirement 4.2 (merge blocking) holds by convention
while the repository is private (#28), and SECURITY.md uses a private contact
request instead of private vulnerability reporting (6.1) until publication.
Next: feature 02 (daemon-and-store) starts with `requirements.md` for owner
approval.

## 2026-09-26 — 02 daemon-and-store: requirements drafted

Feature 02 is now `active`. `.kiro/specs/02-daemon-and-store/requirements.md` covers the
loopback daemon, the authenticated control API, the daemon-owned SQLite store, the
append-only hash-chained event log, content storage (off by default) and content erasure.
Per `workflow.md`, `design.md` waits for the owner's approval of the requirements.

Owner decision (issue #7): deleting erases stored content and keeps event hashes; recorded
as decision 18 in `DECISIONS.md`, to become an ADR in the design.

Inputs used: ADRs 0001–0003, 0006, 0007, 0009; backlog CS-101–CS-103; issues #7, #10
(Windows token ACL, `127.0.0.1` not `localhost`) and #21 (drop-and-count queue, for 03).
Differences from the backlog: content addresses are HMAC-SHA-256 with a per-install key
(ADR 0006), not plain SHA-256; the event log never updates or deletes rows even when
content is erased (a new event records the erasure).

### 02 requirements amended after the owner's review on PR #32

- R2.4: any request carrying an `Origin` header is rejected (no allowlist to configure).
- R2.9 (new): token rotation invalidates the old token at once; clients re-read the token
  file once after a 401 (ADR 0003).
- R4.3/R4.5: the global commit order is gap-free, so verification catches removals anywhere
  except at its very end; R4.6 (new) documents that tail truncation is only detectable from
  feature 04's signed checkpoints.
- R6.1/R6.2: erasure lists the other runs sharing the content first, erases only after
  confirmation, and has a dry run.
- Left for design.md: timestamp wire format (no `u64` nanoseconds over JSON), backup via
  `VACUUM INTO` before migrations, request size and timeout limits, stale discovery file
  (pid and start time, check the instance lock), and exercising capture-enabled paths on CI
  without a keychain (injectable secret store in tests).

## 2026-09-26 — 02 daemon-and-store: design drafted

`.kiro/specs/02-daemon-and-store/design.md` and ADR 0011 (erase content, keep hashes) are
ready for owner approval; `tasks.md` waits for it.

Choices worth knowing:
- Hand-written JSON-RPC over axum 0.8.9 instead of jsonrpsee.
- One writer thread for SQLite, with triggers that forbid UPDATE/DELETE on `events`.
- `etcetera` instead of `directories`, which pulls MPL-2.0 `option-ext`.
- std `File::try_lock` instead of `fs4`.
- keyring-core with per-platform stores and its `mock` store in tests.
- Unix-millisecond timestamps, safe under 2^53.
- `VACUUM INTO` backup before migrations.

The resolved tree is 178 crates. `deny.toml` will need `BSD-3-Clause` (subtle) and `Zlib`
(foldhash).

### 02 design amended after the review on #36 (issue #41)

- Erasure takes an exclusive read gate, so `wal_checkpoint(TRUNCATE)` can't be blocked by our
  own readers. It deletes leftover migration backups and reports success only when every copy
  is gone. Otherwise it returns 1004 "erasure pending", keeps a durable `erasure_pending`
  flag, retries every 30 s and at startup, and `health` shows it.
- Migration backups are deleted after the migrated database passes `integrity_check` and a
  full verify at startup.
- The append API rejects floats and integers outside ±(2^53 − 1); tests pin it.
- Verification runs in 10 000-event chunks, each a short read under the shared gate.
- There's a clear message and docs for Linux without Secret Service (WSL, servers).
- JSON-RPC batches follow section 6: an empty array gets one -32600 error, notifications
  get no entries, and an all-notification batch gets HTTP 204.

## 2026-09-26 — desktop: renderer session stays on the machine (issue #33)

The renderer made a network request of its own: Chromium's spellchecker was on for the
session and tried to download a dictionary from a Google CDN.

- `webPreferences.spellcheck` is `false`, and `session.setSpellCheckerEnabled(false)` turns it
  off for the whole session (the window flag alone left the session spellchecker on).
- `keepSessionOnMachine` (`apps/desktop/src/main/network.ts`) cancels every session request
  whose URL isn't `app://`.

Checked in the running app (local run as root, so `--no-sandbox`):

| | Before | After |
|---|---|---|
| Session spellchecker | `true` | `false` |
| Request to an off-machine URL | `ERR_TUNNEL_CONNECTION_FAILED` (it tried to leave) | `ERR_BLOCKED_BY_CLIENT` |
| `app://renderer/index.html` | 200 | 200, title "Callsheet" |

## 2026-09-26 — 02 daemon-and-store: design approved, shared foundation (task 0)

The owner approved the design (#42 merged) and chose to implement it with parallel workers
in waves; `tasks.md` records the order.

- The resolved tree grows to about 190 crates. `cargo deny` needed exactly the two licences
  the design predicted: BSD-3-Clause (subtle, and matchit via axum) and Zlib (foldhash).
- keyring-core 1.0.0 has no `mock` feature: `keyring_core::mock` is always built.
- zbus-secret-service-keyring-store uses `rt-tokio-crypto-rust`, so no OpenSSL.
- ts-rs maps `i64`/`u64` to `bigint`; wire integers that stay below 2^53 carry
  `#[ts(type = "number")]`.
- `cs-daemon` gained a library target, so modules land before `main` uses them without
  dead-code warnings.
- Blocked: the default port. `www.iana.org` is denied by this environment's network policy,
  so the registry couldn't be checked (task 0.5).

## 2026-09-27 — render check after wave 2; spellcheck dictionary download found and stopped

A run of `main` after #51 and #52, with the real binaries.

Daemon (`cs-daemon --data-dir $TMP`, default `127.0.0.1:0`):
- The data dir is `700`; `daemon.json`, `control-token` and `daemon.lock` are `600`.
- `version` with the token returns 200.
- No token gives 401. An `Origin` header or a `localhost` Host gives 403.
- A batch with one notification and one unknown method returns two entries, with -32601 for the unknown method.
- A second instance is refused and names the pid, exit 1.
- `--listen 0.0.0.0:1` exits 2 and creates no directory.
- SIGTERM exits 0 and removes `daemon.json`.
- The token is absent from the log.

Desktop app: renders `app://renderer/index.html` ("Callsheet").

**Leak found:** routed through a local proxy that logs and refuses every host, the app made one request on every start, to
`https://redirector.gvt1.com/edgedl/chrome/dict/en-us-10-1.bdic`. That is the spellchecker's Hunspell dictionary for
the system language.
- The #33 fix turned the spellchecker off but did not stop the download.
- The download goes through a loader that `webRequest` doesn't see, so the filter didn't catch it either.
- Also calling `session.setSpellCheckerLanguages([])` stops it: `main` made 1 request on each of 2 of 2 runs; the fix made 0 on 4 of 4.
- Follow-up: a CI check that launches the app behind a logging proxy and fails on any outbound request would catch regressions like this. CI has no display yet, so for now this is a manual check recorded here.

## 2026-09-27 — 02 daemon-and-store: tasks 1–12 merged

All twelve units of feature 02 are merged: #45–#52, #54, #55, #59 and #60. Each passed the 8 gates
locally. The notes below come from each PR's "Notes for progress.md" and "How it was verified".

- **1 event-hash (#46)**
  - serde_json holds `-0`, `1.0`, `1e3` and integers above `u64::MAX` as floats, so bodies with them are rejected as "not an integer". `JSON.stringify` never emits them for integers.
  - Nesting is capped at 64. serde_json parses at most 127 levels, so the first draft's cap of 128 accepted bodies that could never be read back (found in review).
  - canon-json 0.2.1's vectors leave out upstream's `333333333.33333329`: serde_json without `float_roundtrip` parses it to a neighbouring double. Event hashes are unaffected, since bodies have no floats.
  - A nested `crates/cs-core/tests/rfc8785/biome.json` (`"root": false`) stops Biome reformatting the vectors.
- **2 win-acl (#49)**
  - Protected DACL with one owner ACE, passed in `SECURITY_ATTRIBUTES` at creation. Directories get `D:P(A;OICI;FA;;;<SID>)`, files `D:P(A;;FA;;;<SID>)`: OI/CI do nothing on a file, although design.md writes OICI for both.
  - On Windows, SQLite's `-wal`/`-shm` inherit the directory's DACL, not the database file's. They are owner-only only inside an owner-only directory.
  - A follow-up commit sets the owner too (`O:<SID>`); without it an elevated process's files are owned by `BUILTIN\Administrators`.
  - New CI job "Rust on Windows (owner-only files)" on `windows-2025`; since #52 it runs all cs-store and cs-daemon tests. This is the workspace's first `#[allow(unsafe_code)]` (the lint is `deny`, not `forbid`).
  - A local `x86_64-pc-windows-gnu` build needs MinGW for bundled SQLite; compiling one module through a `#[path]` crate is a stand-in.
- **3 secrets (#48)**
  - The key is 64 lowercase hex characters via `set_password`/`get_password`, never `set_secret`. The Windows store keeps passwords as UTF-16 and secrets as raw bytes; KDE Wallet takes UTF-8 only.
  - `content_key()` must run under `spawn_blocking`: zbus's blocking API starts its own tokio runtime and panics inside an async task. It can wait on an unlock prompt, so fetch the key once.
  - "No Secret Service" arrives as a connection `NotFound` (no bus), `ServiceUnknown` (no provider) or `NoResult` (WSL, no `default` collection). It is recognised by walking the error's source chain.
  - A malformed stored key is reported and left in place; replacing it would orphan every blob.
  - Fixed task 0's macOS build: `apple-native-keyring-store` hits its own `compile_error!` without the `keychain` feature. No CI job builds for macOS.
- **4 rpc-http (#47)**
  - Layers, outermost first: 64 KiB body limit (413), 10 s timeout (408), Host/Origin (403), bearer (401), dispatch. They wrap the fallback too.
  - Build `HttpConfig::for_listener(&listener)` from the bound port, never the configured one, which may be 0.
  - A timed-out request drops the dispatch future at its current `.await`, and a batch shares one 10 s budget. Work that must finish goes to the writer thread.
  - Numeric ids outside ±(2^53 − 1), or with fractions, get -32600 with `id: null`. A batch over 16 entries gets one -32600 and nothing runs.
  - `axum::serve` 0.8.9 builds hyper's connection builder without a timer, so hyper's 30 s header-read timeout is silently dropped (fixed in #52).
- **5 schema (#50)**
  - Create `callsheet.db` owner-only first and never pass `SQLITE_OPEN_CREATE`. On Unix, SQLite 3.53.2 copies the database file's mode to `-wal`, `-journal` and `-shm`.
  - Check `user_version` on a read-only connection first. Closing a read-write connection checkpoints a newer daemon's hot WAL into the main file.
  - Startup order is `open_writer`, `migrate`, `open_reader`: a read-only connection can't switch to WAL.
  - `VACUUM INTO` accepts a pre-created empty target, so backups are owner-only from creation. They go through `backup-v<from>-next.db` and a rename.
  - The bundled build sets `SQLITE_USE_URI`, so relative paths get `./` to stop `file:` names being parsed as URIs.
- **6 docs (#45)**
  - SECURITY.md and PRIVACY.md say that before feature 04 a same-user process that can write the database can edit events and recompute hashes undetected.
  - PRIVACY.md says WAL truncation and backup deletion free disk space without overwriting it; R6.3 says content is overwritten "in the write-ahead log".
- **7 writer (#51)**
  - rusqlite 0.40.2 has `u64` `ToSql`/`FromSql` only behind `fallible_uint` (off), so positions are read as `i64` and converted with checks.
  - Capture changes are ticketed and the last request wins. Disabling never waits on the keychain, and no content is stored once a disable is requested. Fixed after review: disabling hung behind an unlock prompt and the pending append stored content anyway.
  - With capture already on at open, the key loads on the first append with content. After a failed load, such appends fail at once for 10 s, so a burst causes one prompt.
  - A key may be generated only while `event_content` and `blobs` are both empty, so a key lost after capture can't be replaced without a recovery design.
  - Provisional per-event caps: 64 content items, 8 MiB of content, 256 KiB of canonical body.
- **8 daemon-proc (#52)**
  - `cs_daemon::serve` replaces `axum::serve`: hyper `http1::Builder` with `TokioTimer` and a 10 s `header_read_timeout`, plus `GracefulShutdown`. The timeout closes without a response and also closes idle keep-alive connections.
  - Discovery probes must take a *shared* lock. With an exclusive probe, a client mistakes another client's probe for a live daemon.
  - SIGHUP is not handled on purpose: a handler overrides `nohup`'s inherited ignore. Only the third signal during shutdown forces exit 1, because terminals can deliver one Ctrl+C twice.
  - A console close gets 2 s of drain; Windows kills the process 5 s after `CTRL_CLOSE_EVENT`.
  - Default listen is `127.0.0.1:0`, provisional until task 0.5 (the IANA port check); clients read the port from `daemon.json`.
- **9 verify (#54)**
  - Reads 10 000-event chunks under the shared gate. A stored body must equal its own canonical JSON, because serde_json keeps the last of duplicated keys.
  - A missing blob counts as erased if a later erasure lists it and is in the referencing run or names that run in `affectedRuns`. The design says "same run", which would flag shared runs.
  - Truncating the global end with run heads set back, and editing a run's newest event with its hash recomputed, both pass until feature 04 (a test pins the first).
  - About 5 µs per event in release, so the 10 s request timeout covers roughly 2M events.
- **10 erase (#55)**
  - One writer command that takes the read gate itself with `blocking_write`. `Store::exclude_readers` is test-only, because holding it across an erase deadlocks.
  - `planId` includes `contentItems`, so replaying a used plan is refused with 1002.
  - `content.erased` is reserved: `Store::append` refuses it, and erase appends it only when a blob was deleted.
  - A marker stored in the db, the WAL and a `VACUUM INTO` backup was found 0 times after erasing and after both retry paths.
- **11 wire (#60)**
  - Dropping a tokio runtime waits for every `spawn_blocking` task with no limit. `main` calls `shutdown_timeout(1 s)` so a stuck keychain call can't keep the process alive. Reproduce by pointing `DBUS_SESSION_BUS_ADDRESS` at a socket that accepts and never answers.
  - The backup check runs whenever a `backup-v*.db` exists at startup. An `integrity_check` failure refuses to start; a verify problem starts with a warning, keeps the backup and repeats at every start.
  - A signal while the store opens exits 0 before listening. The lock handle is leaked so a restarted daemon can't migrate alongside the abandoned open.
  - Params are by name only, with `deny_unknown_fields`. `Discovery` moved to `cs-core::control` with a generated TS type.
  - New CI job "TypeScript client against the daemon"; a missing binary fails the test rather than skipping it.
  - node:test: a `setTimeout` from `timers/promises` inside `Promise.race` keeps the run alive; pass `{ ref: false }`.
- **12 desktop status screen (#59)**
  - Main calls the daemon over `node:http` (`agent: false`, explicit Host, no Origin, 2 s timeout, 64 KiB cap). A logging proxy saw 0 requests. The token never leaves `src/main/daemon.ts`.
  - Node can't probe the `daemon.lock` file lock. Liveness uses `process.kill(pid, 0)`, boot time from `os.uptime()` and, on Linux, `/proc/<pid>/stat` (zombie state, start time).
  - The renderer is compiled by `tsconfig.renderer.json` into `dist/renderer`; main serves `dist/renderer`, not `src/renderer`.
  - Electron 44: `execCommand("copy")` works on a user gesture with every permission denied; `navigator.clipboard` would need `clipboard-sanitized-write`.
  - New `.kiro/steering/ui.md` (always included) records the owner's UI rule and checkable standards.
  - UI review: Electron under Xvfb with `--remote-debugging-port`, `scripts/fake-daemon.ts`, `scripts/screenshot.ts`. Kill only the Electron binary; killing `xvfb-run` orphans Xvfb.

Settled in the closing commit: design.md now records the erasure rule as built (#54's; the comment in `erase.rs` that
said "any run" is corrected), the plan ID with `contentItems`, the Windows descriptors with `O:<SID>` and no OICI on
files, the backup-check policy, the startup-signal and runtime-shutdown behaviour, the `events.verify` time limit and
the desktop app's discovery deviation. R6.3 now says the write-ahead log is truncated, not overwritten. PRIVACY.md
covers the kept backup, Windows programs holding files open and the desktop app's network behaviour; SECURITY.md
states the desktop discovery gap.

### Open follow-ups

- Owner: choose the default port (task 0.5, #52).
- Docs: revisit the hash-chain wording in feature 04 and name the CLI commands in feature 07 (#45).
- CI: a macOS build check (#48); a launch behind a logging proxy once CI has a display (#59). Untested: the Windows signal path (#52) and a signal during a slow startup (#60).
- `Store::close` leaves about 50 KB of `-wal` after a graceful stop; close the reader first or checkpoint (#60).
- `events.verify` shares the 10 s timeout (about 2M events); needs a longer timeout or background verification (#54).
- Indexes: `events.kind` for the erasure pass (#54); `event_content.address` (#55).
- Daemon identity: a live-pid check in Rust needs `unsafe` (#52). Writing the boot id and start ticks into `daemon.json` would remove the desktop's clock slack (#59).
- Desktop discovery gaps (#59): pid reuse on macOS and Windows; on macOS a daemon slower than 10 s to publish shows as not running; treat a readable `daemon.lock` on Windows as no daemon, after checking on Windows.
- Windows: an existing `--data-dir` keeps its own ACEs, so decide whether to refuse or warn (#49).
- Store: revisit the per-event caps with feature 03 traffic and design lost-key recovery (#51). A pre-existing `callsheet.db`'s mode isn't checked, and a migration that rebuilds `runs` or `events` must handle `foreign_keys=ON` (#50).
- HTTP (#47): rate-limit the rejection logs; consider requiring `Content-Type: application/json`; check the RFC references written from memory.
- Key zeroing is best effort; `zeroize` would need a direct dependency (#48).
- Desktop: show `~` for home paths; replace Electron's default menu (#59). There is no config file yet; the listen address comes only from `--listen` (#52).

## 2026-09-27 — 02 checkpoint (task 13): render check on `main` after #59 and #60

A run of `main` at 82adca3 with the real binaries (`cs-daemon` and the desktop app under Xvfb, `--no-sandbox` for the
root container only). The app was routed through a local proxy that logs and refuses every host.

- **Before the daemon starts:** the status screen shows "Daemon not running" with the exact `cs-daemon --data-dir …`
  command and a Copy button.
- **Daemon running:** within one refresh (3 s) it shows "Daemon running", with the address, version 0.1.0, schema
  version 1, uptime, content capture Off with its note, and 0 events. Light and dark both checked.
- **After SIGTERM:** the daemon exits 0 and the screen returns to "Daemon not running". At 420 px wide the command and
  the data directory wrap with no sideways scroll.
- **Network:** the logging proxy saw no requests during the whole run.
- **Logs:** the daemon log contains no token and no `Authorization` value.
- The curl run against the daemon (every method, 401, 403, batches, token rotation) is in #60's body. The status
  screen's 21 states, each in both themes, were captured and reviewed on #59.
- Before merging #60 on top of #59 (both touch `pnpm-lock.yaml`), the combined tree passed Biome, typecheck and the
  desktop tests (118).
