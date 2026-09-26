# Continuous integration

`.github/workflows/ci.yml` runs on every pull request and on every push to
`main`. It runs the quality gates from `.kiro/steering/workflow.md`:

| Job | Gates |
|---|---|
| Rust (fmt, clippy, test) | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` |
| Rust on Windows (owner-only files) | `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test -p cs-store --locked fsperm` on `windows-2025` |
| Rust dependencies (cargo-deny) | `cargo deny check` (licences, advisories, bans, sources; see `deny.toml`) |
| TypeScript (Biome, typecheck, test, audit) | `pnpm biome check .`, `pnpm -r typecheck`, `pnpm -r test`, `pnpm audit` |
| Generated types are current | `node scripts/gen-types.ts --check` (ADR 0009) |
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
```

`pnpm audit` and the advisory part of `cargo deny check` read live advisory
databases, so `main` can turn red with no code change when a new advisory is
published. That is the gate working, not a flake: update or replace the
affected dependency, or record a reasoned exception.

## Windows

The other jobs run on Linux (`ubuntu-24.04`). Behaviour that differs by
platform (file permissions, locks, signals; requirement 7.2 of feature 02) also
needs a test on Windows, so **Rust on Windows** runs on the pinned
`windows-2025` image, with the same checkout and toolchain steps as the Linux
Rust job. For now it runs clippy for `cs-store` and the owner-only file tests
(`fsperm`: a protected DACL that grants only the current user). Later feature 02
tasks extend it with the lock and signal tests for `cs-store` and `cs-daemon`.
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
