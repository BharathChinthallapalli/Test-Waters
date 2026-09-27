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
| Desktop makes no outbound requests | Builds the desktop app and `cs-daemon`, then `node scripts/egress-check.ts` under Xvfb (see [below](#the-egress-check)) |
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

The egress check needs a display and the Electron binary; [its section](#the-egress-check)
says how to run it locally.

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

## The egress check

PRIVACY.md promises that nothing leaves the machine by default. A unit test of
the app's settings can't show that: after #33 turned the spellchecker off, the
app still downloaded a dictionary on every start through a loader that
`webRequest` doesn't see (`docs/progress.md`, 2026-09-27). The job **Desktop
makes no outbound requests** (issue #56) starts the real binaries and watches
what they try to reach. `apps/desktop/scripts/egress-check.ts`:

1. Starts three logging proxies on `127.0.0.1`
   (`apps/desktop/scripts/logging-proxy.ts`). Each answers every request with
   403 and forwards nothing. It records the method and the destination
   (`host:port`, from the CONNECT target or the absolute URL) of the request
   line, never the path, query, headers or body. Bytes that aren't a request
   line count too, as "(unparsed)".
2. Starts `scripts/fake-daemon.ts` on loopback, so the app has a daemon to talk to.
3. Starts the built desktop app (Electron, under Xvfb) with a fresh profile
   (`--user-data-dir` in a temporary directory), then again with the same
   profile, as **first start** and **restart**. Each run is routed two ways:
   - Chromium's network stack through `--proxy-server=127.0.0.1:<port>`;
   - Node in the main process through `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY`
     with `NODE_USE_ENV_PROXY=1`, which is what makes Node's `fetch` and global
     HTTP agent use them.

   `NO_PROXY` is empty, so a client that honours these variables would send
   even loopback requests to the proxy. The app's daemon client (`agent: false`,
   `127.0.0.1` only) connects directly and must not appear.
4. For each run, checks over the DevTools protocol, as `scripts/screenshot.ts`
   does, that the `app://renderer` page opened with the title "Callsheet" and
   reached the daemon ("Daemon running"). The debugging port is
   `--remote-debugging-port=0` on loopback; Chromium writes the port it chose
   to the profile's `DevToolsActivePort` file. It then leaves the app idle for
   20 s and stops it with SIGTERM.
5. Starts `cs-daemon` alone with a temporary `--data-dir` and the proxy
   variables pointing at the third proxy, checks that it publishes
   `daemon.json`, leaves it idle for 10 s and checks that SIGTERM stops it with
   exit status 0.

The job fails if any proxy saw a request, or if a positive check failed. It
prints each destination with its count, the phase and the process: "desktop
app: Chromium", "desktop app: main-process Node" or "cs-daemon", so an app
failure and a daemon failure are told apart. It uploads nothing. Output with
the spellchecker fix (`setSpellCheckerLanguages([])`) removed:

```text
Outbound requests (every one refused with 403, none forwarded): 2
  FAIL  desktop app: Chromium (--proxy-server), first start: redirector.gvt1.com:443 x1
  FAIL  desktop app: Chromium (--proxy-server), restart: redirector.gvt1.com:443 x1
desktop app: FAIL (2 outbound requests, 0 other problems)
cs-daemon: PASS (0 outbound requests, 0 other problems)
```

`--allow host:port` (repeatable) reports a destination as allowed instead of
failing. It exists for a later test that routes a user's model request to a
provider on purpose. The idle-start check in CI passes none.

**What it proves:** on Linux (`ubuntu-24.04`), during startup and 20 s of idle,
with and without an existing profile, neither Chromium's HTTP, HTTPS and
WebSocket requests nor Node's `fetch` and global-agent requests in the main
process leave the app, and the idle daemon makes no request through the proxy
variables that its HTTP client honours.

**What it doesn't prove:**

- **Other platforms.** It runs only on Linux. Windows and macOS start
  differently (on macOS, for one, the spellchecker is the OS's own, while Windows
  and Linux download Hunspell dictionaries "from a Google CDN by default",
  Electron's spellchecker guide), so they remain uncovered until a job there can
  launch the app.
- **Traffic that isn't an HTTP request through a proxy.** Electron's
  `--proxy-server` "only affects requests with HTTP protocol, including HTTPS and
  WebSocket requests" (Electron 44 command-line switches). Chromium defers name
  resolution to an HTTP proxy for the requests it sends there ("name resolution
  is always deferred to the proxy", `net/docs/proxy.md`), but its proxy
  documentation says nothing about DNS prefetching, WebRTC or other UDP, so a
  DNS lookup or a UDP packet would not be seen. Chromium also never proxies
  `localhost`, `127.0.0.1/8`, `[::1]` or link-local addresses (its implicit
  bypass rules, unless `<-loopback>` is set), so loopback traffic isn't seen
  either; that is intended here.
- **Node clients that ignore the proxy variables.** A main-process request made
  with its own agent (as the daemon client does, on purpose) or a raw socket
  connects directly; only `fetch` and the global agent are covered. The daemon
  leg covers what its HTTP client (reqwest with `system-proxy`) sends through
  the variables, not a connection made another way.
- **Anything after the user acts.** The app is left idle; no button is pressed
  and no network feature is turned on.

Run it locally on Linux with Xvfb, from the repository root:

```sh
pnpm install --frozen-lockfile
pnpm -C apps/desktop exec install-electron
pnpm -C apps/desktop build
cargo build -p cs-daemon --locked
cd apps/desktop
xvfb-run -a -s "-screen 0 1280x800x24" node scripts/egress-check.ts
```

Options: `--daemon-bin <path>` (default `CS_DAEMON_BIN`, then
`target/debug/cs-daemon`), `--idle-seconds` (20), `--daemon-idle-seconds` (10)
and `--electron-arg <switch>`. Where Chromium's sandbox can't run, such as a
container running as root, add `--electron-arg=--no-sandbox` locally; CI never
passes it. The proxy code has its own tests in `pnpm -r test`
(`scripts/logging-proxy.test.ts`).

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
- The egress job installs Xvfb with `apt-get install xvfb` from the runner's
  Ubuntu archive, by package name; no third-party action. Electron 44 has no
  install script (so pnpm's build-script settings and
  `ELECTRON_SKIP_BINARY_DOWNLOAD` play no part) and downloads its binary on first
  use; the job runs its `install-electron` command first, which checks the
  download against the checksums shipped in the `electron` package.
- Ubuntu 24.04 restricts unprivileged user namespaces with AppArmor, so
  Chromium's namespace sandbox can't start there and Electron aborts. The
  egress job makes Electron's `chrome-sandbox` helper root-owned with mode 4755,
  Chromium's setuid sandbox, instead of passing `--no-sandbox`: the app runs
  sandboxed, as users run it.

## Sources

- Required status checks and rulesets:
  https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets
  and `creating-rulesets-for-a-repository` (read from github/docs `main`).
- Action inputs read from each action's `action.yml` at the pinned commit.
- Windows runner labels (`windows-2025`, `windows-latest`):
  https://github.com/actions/runner-images/blob/main/README.md
- Egress check:
  - Electron 44 switches (`--proxy-server` covers HTTP, HTTPS and WebSocket):
    https://github.com/electron/electron/blob/v44.4.5/docs/api/command-line-switches.md
  - Electron spellchecker (macOS native, Hunspell from a Google CDN elsewhere):
    https://github.com/electron/electron/blob/v44.4.5/docs/tutorial/spellchecker.md
  - Electron binary download and `install-electron`:
    https://github.com/electron/electron/blob/v44.4.5/docs/tutorial/installation.md
    and `install.js`/`index.js` in the `electron@44.4.5` package
  - Chromium proxy resolution, implicit bypass rules, name resolution:
    https://github.com/chromium/chromium/blob/main/net/docs/proxy.md
  - `DevToolsActivePort` for `--remote-debugging-port=0` (Chromium 152, the
    version in Electron 44.4.5):
    https://github.com/chromium/chromium/blob/152.0.7977.130/content/browser/devtools/devtools_http_handler.cc
  - Chromium on Ubuntu 23.10+ (AppArmor and user namespaces) and the setuid
    sandbox helper:
    https://github.com/chromium/chromium/blob/main/docs/security/apparmor-userns-restrictions.md
    and https://github.com/chromium/chromium/blob/main/docs/linux/suid_sandbox_development.md
  - Node 24 built-in proxy support (`NODE_USE_ENV_PROXY`, global agent only):
    https://github.com/nodejs/node/blob/v24.x/doc/api/http.md#built-in-proxy-support
