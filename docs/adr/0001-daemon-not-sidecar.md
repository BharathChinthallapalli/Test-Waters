# 0001. Run Callsheet as a standalone daemon, not an Electron sidecar

- Status: Accepted
- Date: 2026-09-26

## Context

Callsheet proxies and records every call that AI workers make, enforces
budgets and rate limits, and holds the install's private keys. Agents run in
terminals, editors and CI, whether or not the desktop app is open.

Two shapes were considered:

- **Sidecar:** the Electron main process spawns the backend and stops it when
  the window closes.
- **Daemon:** `cs-daemon` is an independent process that the CLI and the
  desktop app both connect to.

A sidecar ties recording and enforcement to the UI's lifecycle. Closing the
window would stop the proxy, so agent traffic would fail or silently go
unrecorded. The roadmap also ships daemon + CLI (feature 07) before the
desktop cockpit (feature 08), so the backend must work with no UI at all.

## Decision

`cs-daemon` is the only long-running backend process. It runs independently of
the desktop app, binds to 127.0.0.1 only, and is reached through the control
API in [ADR 0003](0003-json-rpc-control-api.md). The desktop app and the CLI
are clients; neither starts, owns or embeds the daemon's state.

## Consequences

- Recording and enforcement keep working when the desktop app is closed or
  crashes, and a UI crash cannot corrupt the store.
- The daemon is the single owner of the database ([ADR 0002](0002-sqlite-owned-by-daemon.md))
  and of private keys ([ADR 0007](0007-identity-and-log-integrity.md)).
- Clients need a discovery step (address and token) and must handle a daemon
  that is not running or runs a different version; the control API exposes a
  `version` method for this (feature 02).
- Starting the daemon at login, and upgrading it, become release concerns
  (feature 07) rather than something Electron handles implicitly.

## Sources

No external specification is pinned by this decision. It restates the
constraint in `.kiro/steering/tech.md` ("Daemon runs independently of the UI,
binds 127.0.0.1 only").
