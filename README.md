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

**Pre-alpha. Nothing is usable yet.** Feature 01 (foundation) is in progress:
workspaces, architecture decisions, a hardened desktop shell, generated types
and CI. The daemon, proxy and CLI arrive in features 02–07.

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
pnpm --filter @callsheet/desktop start   # opens the desktop shell
```

All quality gates and how CI runs them: [docs/ci.md](docs/ci.md).

## Contributing, security and privacy

- [CONTRIBUTING.md](CONTRIBUTING.md): how to work on a task
- [SECURITY.md](SECURITY.md): how to report a vulnerability
- [PRIVACY.md](PRIVACY.md): what Callsheet stores and sends
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

## Licence

[MIT](LICENSE)
