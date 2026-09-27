# Continuous integration

`.github/workflows/ci.yml` runs on every pull request and on every push to
`main`. It runs the quality gates from `.kiro/steering/workflow.md`:

| Job | Gates |
|---|---|
| Rust (fmt, clippy, test) | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` |
| Rust on Windows (cs-store, cs-daemon) | `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test -p cs-store -p cs-daemon --locked` on `windows-2025` |
| Rust dependencies (cargo-deny) | `cargo deny check` (licences, advisories, bans, sources; see `deny.toml`) |
| TypeScript (Biome, typecheck, test, audit) | `pnpm biome check .`, `pnpm -r typecheck`, `pnpm -r test`, `pnpm audit` |
| Generated types are current | `node scripts/gen-types.ts --check` (ADR 0009) |
| TypeScript client against the daemon | `cargo build -p cs-daemon --locked`, then `pnpm -C packages/api-types test:integration` |
| **CI passed** | Always runs; fails unless every job above succeeded |

Run the same gates locally before pushing:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
pnpm biome check .
pnpm -r typecheck
pnpm -r test
pnpm check-types
cargo build -p cs-daemon --locked
pnpm -C packages/api-types test:integration
```

## The daemon client test

`packages/api-types/test/daemon.integration.test.ts` (feature 02, R2.8 and
R2.9) starts the built `cs-daemon` binary with a temporary `--data-dir` and the
default listen address (port 0), reads `daemon.json` and the token, calls
`health` and `version` typed with the generated types, checks 401 for a missing
or wrong token, rotates the token (the old one gets 401, a client re-reads the
file once and succeeds), then sends `SIGTERM` and checks exit status 0 and that
`daemon.json` is gone. It uses only Node built-ins.

It needs the binary, so it is not part of `pnpm -r test` (the `ts` job has no
Rust toolchain) and has its own script instead. It fails, never skips, when the
binary is missing. The binary is `target/debug/cs-daemon[.exe]` at the
repository root unless `CS_DAEMON_BIN` names another path (for example with a
custom `CARGO_TARGET_DIR`). Its test files are type-checked by `pnpm -r
typecheck` through `packages/api-types/tsconfig.test.json`.

`pnpm audit` and the advisory part of `cargo deny check` read live advisory
databases, so `main` can turn red with no code change when a new advisory is
published. That is the gate working, not a flake: update or replace the
affected dependency, or record a reasoned exception.

## Windows

The other jobs run on Linux (`ubuntu-24.04`). Behaviour that differs by
platform (file permissions, locks, signals; requirement 7.2 of feature 02) also
needs a test on Windows, so **Rust on Windows** runs on the pinned
`windows-2025` image, with the same checkout and toolchain steps as the Linux
Rust job. It runs clippy for the whole workspace and every test of `cs-store` and
`cs-daemon`, among them:

- owner-only files (`fsperm`: owned by the current user, with a protected DACL
  that grants only that user);
- the instance lock (`daemon.lock`, a second instance is refused and named) and
  discovery (`daemon.json` is trusted only while the lock is held);
- the token file, rotation, the header-read timeout and graceful connection
  draining, and the built `cs-daemon` binary (exit status 2 for a non-loopback
  `--listen`, a second instance refused, token reused after a crash).

Not covered on Windows: delivering Ctrl+C or a console close event to the
daemon. Sending one (`GenerateConsoleCtrlEvent`) needs Win32 FFI, and `unsafe`
is denied outside `cs-store::fsperm`. The shutdown sequence after the signal
is the same code on every platform and is tested on Linux with `SIGINT` and
`SIGTERM`. The Unix-only tests (file modes, a data directory open to others,
the pid check in `daemon.lock`, signals) are compiled out on Windows.
**CI passed** fails unless this job succeeded too.

There is no full local equivalent on Linux: `cargo clippy --target
x86_64-pc-windows-gnu` needs a MinGW C compiler to build rusqlite's bundled
SQLite, so the CI job is the authoritative check.

## Blocking merges (repository setting)

A workflow reports failures but cannot block a merge by itself. Merging is
blocked only when a ruleset on `main` requires the check. Required checks pass
on `success`, `skipped` or `neutral`, so require only **CI passed**: it runs
even when another job is skipped and fails if any job did not succeed.

**While the repository is private, this is not available.** On GitHub Free,
rulesets and branch protection need a public repository (or GitHub Pro). The
owner chose to stay private for now (issue #28), so until publication the rule
is a convention: merge only when **CI passed** is green. Set up the ruleset
below as part of making the repository public.

This is a GitHub setting, not a file, so a repository admin sets it once:

1. Open the repository on GitHub and click **Settings**.
2. In the left sidebar, under "Code and automation", click **Rules**, then
   **Rulesets**, then **New ruleset** → **New branch ruleset**.
3. Name it `main`, set **Enforcement status** to **Active**, and under
   **Target branches** add the default branch.
4. Enable **Require a pull request before merging**.
5. Enable **Require status checks to pass before merging**, and add the check
   **CI passed** (source: GitHub Actions).
6. Enable **Block force pushes**, then **Create**.

To confirm it works, open a pull request that breaks one gate (for example an
unformatted Rust file): the **CI passed** check fails and the merge button stays
blocked.

## Workflow rules

- The workflow starts with `permissions: {}`; each job grants only
  `contents: read`.
- `actions/checkout` runs with `persist-credentials: false`; no job pushes.
- Every action, including GitHub's own, is pinned to a full commit SHA with the
  version in a trailing comment. Update the SHA and the comment together.
- Node comes from `.node-version`; pnpm comes from `packageManager` in the root
  `package.json`; Rust comes from `rust-toolchain.toml`.
- No caches: every run starts from a clean install.
- cargo-deny is the prebuilt release binary, verified against a SHA-256 pinned
  in the workflow; update the version and the hash together.

## Sources

- Required status checks and rulesets:
  https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets
  and `creating-rulesets-for-a-repository` (read from github/docs `main`).
- Action inputs read from each action's `action.yml` at the pinned commit.
- Windows runner labels (`windows-2025`, `windows-latest`):
  https://github.com/actions/runner-images/blob/main/README.md
