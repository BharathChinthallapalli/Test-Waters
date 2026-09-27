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
| Desktop makes no outbound requests | Builds the desktop app and `cs-daemon`, then `node scripts/egress-check.ts` under Xvfb, with logging proxies and strace (see [below](#the-egress-check)) |
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
R2.9; feature 03, requirement 7) starts a mock Anthropic API on loopback
(`node:http`), then the built `cs-daemon` binary with a temporary `--data-dir`,
the default listen address (port 0) and `--proxy-upstream` pointing at the mock.
It reads `daemon.json` (including `proxyAddress`, which must match the saved
`proxy-port`) and the token, calls `health` and `version` typed with the
generated types, sends one `POST /v1/messages` through the proxy with a fake
`x-api-key` and reads it back with `calls.list` (run, model, usage, outcome)
and `health.proxy.callsRecorded`, checks 401 for a missing or wrong token,
rotates the token (the old one gets 401, a client re-reads the file once and
succeeds), then sends `SIGTERM` and checks exit status 0 and that `daemon.json`
is gone. It uses only Node built-ins, and nothing leaves the machine: the
proxy's upstream is the loopback mock.

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
what they try to reach in two independent ways:

- **Logging proxies** name the destination of every request that honours a
  proxy setting. Three run on `127.0.0.1`
  (`apps/desktop/scripts/logging-proxy.ts`), one per way out, so each request
  is attributed to what sent it. Each answers every request with 403 and
  forwards nothing. It records the method and the destination (`host:port`,
  from the CONNECT target or the absolute URL) of the request line, never the
  path, query, headers or body. Bytes that aren't a request line count too, as
  "(unparsed)".
- **A syscall trace** catches what ignores the proxies. Every process under
  test runs under `strace -f`, which records each `connect`, `sendto`,
  `sendmsg` and `sendmmsg` of the process and of every process it starts
  (`apps/desktop/scripts/syscall-trace.ts`), each message of a `sendmmsg`
  included. It fails on a socket call to an IPv4 or IPv6 address that isn't
  local: local means `127.0.0.0/8`, `::1`, their IPv4-mapped forms
  (`::ffff:127.x.x.x`) and the unspecified addresses `0.0.0.0` and `::`, which
  Linux connects to this host. It also fails on any call to port 53 (a DNS
  query, even to Ubuntu's local stub resolver on `127.0.0.53`), on a
  connection to a local resolver's socket (systemd-resolved, Avahi), and on
  any address family other than IP, UNIX, netlink and `AF_UNSPEC` (which
  disconnects a socket). strace runs with `-s 0`, so no data is ever printed,
  only addresses.

`apps/desktop/scripts/egress-check.ts` then runs, in order:

1. **The canary** (`scripts/egress-canary.ts`): a small Electron main process
   started with exactly the app's switches and environment (below), which
   tries every way out the check claims to see:
   - a main-process `fetch`, which must reach the proxy named by
     `HTTPS_PROXY`;
   - `net.fetch` in the default session and `fetch` in a separate session
     partition, which must reach the `--proxy-server` proxy;
   - a request to a link-local address, which must reach the same proxy;
   - a raw TCP connection from the main process, a direct connection from
     Chromium's network service (a session set to `mode: "direct"`) and a DNS
     lookup, which the syscall trace must show.

   Host names end in `.invalid` and addresses are in `192.0.2.0/24`
   (TEST-NET-1), so nothing can succeed. If a backstop misses its canary, the
   check fails: an Electron or Node change has disabled it, and the rest of the
   check would prove nothing. `NODE_USE_ENV_PROXY` in particular is marked
   "Stability: 1.1 - Active development" in Node 24's docs.
2. **First start**: `scripts/fake-daemon.ts` on loopback, then the built
   desktop app (Electron, under Xvfb) with a fresh profile (`--user-data-dir`
   in a temporary directory).
3. **Restart**: the real `cs-daemon` built by the job, with a temporary
   `--data-dir`, then the app again with the same profile, pointed at that
   daemon, so the real pairing is exercised. The daemon runs behind its own
   proxy and under strace too.
4. **Idle start**: `cs-daemon` alone with a new `--data-dir` and no client.

The app and the canary are routed three ways:

- Chromium's network stack through `--proxy-server=127.0.0.1:<port>`, with
  `--proxy-bypass-list=<-loopback>`. That special rule removes Chromium's
  implicit bypass rules (`localhost`, `*.localhost`, `[::1]`, `127.0.0.1/8`,
  `169.254/16` and `[FE80::]/10`, Chromium's `net/docs/proxy.md`), so loopback
  and link-local requests, cloud metadata addresses included, go to the proxy
  as well.
- Node in the main process through `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY`
  with `NODE_USE_ENV_PROXY=1`, which makes Node's `fetch` and global HTTP
  agent use them. `NO_PROXY` is empty, so a client that honours these
  variables would send even loopback requests to the proxy. The app's daemon
  client (`agent: false`, `127.0.0.1` only) connects directly and must not
  appear.
- Everything, whatever it honours, through the syscall trace.

For each app run the check reads the page over the DevTools protocol, as
`scripts/screenshot.ts` does: the `app://renderer` page must open with the title
"Callsheet" and reach its daemon ("Daemon running"). The debugging port is
`--remote-debugging-port=0` on loopback; Chromium writes the port it chose to
the profile's `DevToolsActivePort` file. Every DevTools step gives up after 2 s
and the whole wait after 20 s. The app is then left idle for 20 s and stopped
with SIGTERM. Each daemon run must publish `daemon.json` and exit with status 0
on SIGTERM; alone, it is left idle for 10 s first.

The job fails if a proxy saw a request, if the trace shows a socket call beyond
this machine, if the canary missed anything, or if a positive check failed. It
prints each finding with its count, the phase and the process ("desktop app:
Chromium", "desktop app: main-process Node", "cs-daemon", or the traced thread's
name), so an app failure and a daemon failure are told apart, and writes the
same tally to the job summary. It uploads nothing. Output with the spellchecker
fix (`setSpellCheckerLanguages([])`) removed:

```text
Outbound requests at the proxies (every one refused with 403, none forwarded): 2
  FAIL  desktop app: Chromium (--proxy-server), first start: redirector.gvt1.com:443 x1
  FAIL  desktop app: Chromium (--proxy-server), restart: redirector.gvt1.com:443 x1
Socket calls beyond this machine in the syscall trace: 0
desktop app: FAIL (2 proxied requests, 0 traced socket calls, 0 other problems)
cs-daemon: PASS (0 proxied requests, 0 traced socket calls, 0 other problems)
egress check: PASS (0 proxied requests, 0 traced socket calls, 0 other problems)
```

And with a DNS lookup and an HTTPS request with its own agent added to the
main process, neither of which uses a proxy. The UDP connections to port 0
came from the same thread right after the lookup, to the two addresses
`example.com` resolved to; connecting a UDP socket sends no packet, but they
are reported with the rest:

```text
Socket calls beyond this machine in the syscall trace: 8
  FAIL  desktop app (strace), first start: connect UDP 8.8.8.8:53 by thread "libuv-worker" (non-loopback address) x1
  FAIL  desktop app (strace), first start: connect UDP 172.66.147.243:0 by thread "libuv-worker" (non-loopback address) x1
  FAIL  desktop app (strace), first start: connect UDP 104.20.23.154:0 by thread "libuv-worker" (non-loopback address) x1
  FAIL  desktop app (strace), first start: connect TCP 93.184.215.14:443 by thread "electron" (non-loopback address) x1
  …the same four for restart…
desktop app: FAIL (0 proxied requests, 8 traced socket calls, 0 other problems)
```

`--allow host:port` (repeatable) reports a destination at the proxies as
allowed instead of failing. It exists for a later test that routes a user's
model request to a provider on purpose. The idle-start check in CI passes none.

**What it proves:** on Linux (`ubuntu-24.04`), during startup and 20 s of idle,
with and without an existing profile, and against both a stand-in and the real
daemon, no process of the app or of `cs-daemon` connects or sends to an
address beyond this machine, looks up a name through DNS, or makes an HTTP
request through a proxy; and the canary shows each of those backstops would
have seen it.

**What it doesn't prove:**

- **Other platforms.** It runs only on Linux. Windows and macOS start
  differently (on macOS, for one, the spellchecker is the OS's own, while Windows
  and Linux download Hunspell dictionaries "from a Google CDN by default",
  Electron's spellchecker guide), so they remain uncovered until a job there can
  launch the app.
- **Traffic that no traced system call starts.** A request handed to another
  process over D-Bus or another local socket (for example a name lookup through
  systemd-resolved's D-Bus interface), and I/O submitted through `io_uring`,
  make no `connect` or `send` call in the traced processes. A `send` or `write`
  on an already connected socket isn't traced, but its `connect` is.
- **The daemon's HTTP client.** `cs-daemon` has no outbound client today:
  reqwest is in `cs-proxy`, which the daemon doesn't depend on yet (it will
  from task 8). Today the daemon legs prove that its idle start and its
  pairing with the app make no connections. Once the proxy is wired in, they
  also cover reqwest, which honours `HTTPS_PROXY` (cs-proxy turns proxies off
  only for a loopback upstream), and anything it connects directly shows in
  the trace.
- **Anything after the user acts.** The app is left idle; no button is pressed
  and no network feature is turned on.

Run it locally on Linux with Xvfb and strace, from the repository root:

```sh
pnpm install --frozen-lockfile
pnpm -C apps/desktop exec install-electron
pnpm -C apps/desktop build
cargo build -p cs-daemon --locked
cd apps/desktop
xvfb-run -a -s "-screen 0 1280x800x24" node scripts/egress-check.ts
```

Options: `--daemon-bin <path>` (default `CS_DAEMON_BIN`, then
`target/debug/cs-daemon`), `--idle-seconds` (20), `--daemon-idle-seconds` (10),
`--electron-arg <switch>` and `--trace-as-user <user>`. Where Chromium's
sandbox can't run, such as a container running as root, add
`--electron-arg=--no-sandbox` locally; CI never passes it. Where Chromium uses
the setuid sandbox, as on Ubuntu 24.04, pass `--trace-as-user "$(id -un)"`,
which needs sudo without a password. Ctrl-C stops every process under test
(SIGTERM, then SIGKILL; one that survives both isn't waited for) and removes
the temporary files. The proxy and trace parsing have their own tests
in `pnpm -r test` (`scripts/logging-proxy.test.ts`,
`scripts/syscall-trace.test.ts`).

### Why strace, and why it runs as root in CI

The job keeps Chromium's sandbox on. On Ubuntu 24.04 that is the setuid
sandbox: AppArmor restricts unprivileged user namespaces, so Chromium's
namespace sandbox can't start, and the job makes Electron's `chrome-sandbox`
helper root-owned with mode 4755 (Chromium's
`docs/security/apparmor-userns-restrictions.md`, option 3). Under a tracer
without privileges, "setuid and setgid programs are executed without effective
privileges" (strace.1), and Chromium then aborts with "The setuid sandbox is not
running as root". So in CI, `--trace-as-user` runs strace as root through `sudo`
and starts each traced process as the job's user with `strace -u`, the option
strace documents for running setuid programs correctly under tracing. Nothing
under test runs as root. Lifting the AppArmor restriction instead would weaken
the runner for a test, and `--no-sandbox` would test a configuration users
don't run.

A network namespace with only loopback (`unshare -n`) was the alternative. It
would stop egress but not report it: a lookup or a connection would just fail,
and the check would pass without saying what tried to leave. strace names the
thread, the address and the phase.

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
- The egress job uses the Xvfb already on the `ubuntu-24.04` image and installs
  strace with `apt-get install strace` from the runner's Ubuntu archive, by
  package name; no third-party action. Electron 44 has no
  install script (so pnpm's build-script settings and
  `ELECTRON_SKIP_BINARY_DOWNLOAD` play no part) and downloads its binary on first
  use; the job runs its `install-electron` command first, which checks the
  download against the checksums shipped in the `electron` package.
- Ubuntu 24.04 restricts unprivileged user namespaces with AppArmor, so
  Chromium's namespace sandbox can't start there and Electron aborts. The
  egress job makes Electron's `chrome-sandbox` helper root-owned with mode 4755,
  Chromium's setuid sandbox, instead of passing `--no-sandbox`: the app runs
  sandboxed, as users run it. strace then runs as root through `sudo` and
  starts the processes under test as the job's user
  ([why](#why-strace-and-why-it-runs-as-root-in-ci)).

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
  - Node 24 built-in proxy support (`NODE_USE_ENV_PROXY`, global agent only,
    "Stability: 1.1 - Active development"):
    https://github.com/nodejs/node/blob/v24.x/doc/api/http.md#built-in-proxy-support
  - Xvfb on the runner image (`xvfb` in the apt package list; strace isn't):
    https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md
  - strace 6.8 (`-f`, `-s`, `-yy`, `-Y`, `-u`, `-I`, exit status, setuid
    programs under tracing):
    https://github.com/strace/strace/blob/v6.8/doc/strace.1.in
  - sudo (`--preserve-env`, `--non-interactive`, exit status):
    https://github.com/sudo-project/sudo/blob/main/docs/sudo.man.in
  - `.invalid` (RFC 6761) and TEST-NET-1 (RFC 5737) for the canary's targets.
