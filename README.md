# Callsheet

*Working name.* A local-first, open-source desktop control plane that hires AI
coding workers per task and releases them when the task is done.

You register adapters (Claude Code, Codex, Hermes, any model API). For each task,
Callsheet casts whichever worker has the capacity, quota, context headroom and
skills right now, starts a fresh session with a precise context pack, records
everything in a tamper-evident log, and releases the worker at wrap. Memory and
identity belong to the project, not to the worker. Nothing leaves your machine
by default.

See [docs/VISION.md](docs/VISION.md) for the idea and
[docs/ROADMAP.md](docs/ROADMAP.md) for the build order.

## Status

**Pre-alpha. Nothing is usable yet.** Feature 01 (foundation) and feature 02
(daemon and store) are done: a loopback-only daemon with a token-protected
control API, an append-only hash-chained event log in SQLite, and a desktop
status screen that shows whether the daemon is running. The proxy that
records model traffic arrives in feature 03, and the CLI later.

## Repository layout

| Path | What |
|---|---|
| `crates/cs-core`, `cs-store`, `cs-proxy`, `cs-daemon` | Rust workspace (daemon is the only binary) |
| `apps/desktop` | Electron desktop app (hardened shell) |
| `packages/api-types` | TypeScript types generated from Rust |
| `docs/adr/` | Architecture decisions |
| `.kiro/steering/`, `.kiro/specs/` | Project rules and the active feature spec |

## Development

Requirements: [rustup](https://rustup.rs/) (it installs the Rust version in
`rust-toolchain.toml`), Node.js 22.22.2 (`.node-version`) and pnpm 12.6.0
(`packageManager` in `package.json`).

```sh
pnpm install --frozen-lockfile
cargo build --workspace
pnpm -r test && cargo test --workspace
cargo run -p cs-daemon                   # starts the daemon; Ctrl+C stops it
pnpm --filter @callsheet/desktop start   # opens the desktop app's status screen
```

All quality gates and how CI runs them: [docs/ci.md](docs/ci.md).

Platform note: content capture (off by default) needs an operating-system
keychain: macOS Keychain, Windows Credential Manager, or a Secret Service
provider on Linux. Without one, Callsheet keeps content capture off and never
falls back to keeping its content key in a file. On Linux without a Secret
Service provider it says: "No Secret Service keychain was found (common on WSL
and servers), so content capture stays off." Details: [PRIVACY.md](PRIVACY.md).

## Contributing, security and privacy

- [CONTRIBUTING.md](CONTRIBUTING.md): how to work on a task
- [SECURITY.md](SECURITY.md): how to report a vulnerability
- [PRIVACY.md](PRIVACY.md): what Callsheet stores and sends
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

## Licence

[MIT](LICENSE)
