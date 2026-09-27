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
