# 0002. The daemon is the only process that opens the SQLite database

- Status: Accepted
- Date: 2026-09-26

## Context

Callsheet stores calls, settings, budgets and an append-only event log
locally. The event log is hash-chained ([ADR 0007](0007-identity-and-log-integrity.md)),
so every append must see the previous event's hash and the per-run sequence
number without races. Budget reservations (feature 05) likewise need a single
serialisation point.

If the desktop app, the CLI and the daemon all opened the database file,
each would need its own migrations, locking discipline and integrity rules,
and any of them could write an event that breaks the chain.

## Decision

SQLite, in WAL journal mode, is owned by `cs-daemon`. No other process opens
the database file. The desktop app, the CLI and adapters read and change data
only through the control API ([ADR 0003](0003-json-rpc-control-api.md)).
Schema migrations are forward-only and run by the daemon at startup.

## Consequences

- One writer means event sequence numbers, hash chaining and budget
  reservations are serialised in one place, which keeps the log verifiable.
- WAL mode lets the daemon serve reads while it writes.
- Every read the UI needs must exist as a control-API method; there is no
  "just query the file" shortcut, even for debugging.
- Backup and restore (feature 14) go through the daemon rather than by copying
  a live database file.
- The WAL and shared-memory side files are local state and are ignored in git.

## Sources

- SQLite write-ahead logging: https://sqlite.org/wal.html (read via the
  Context7 index of sqlite.org documentation; sqlite.org itself was not
  reachable from the authoring environment). Feature 02 re-reads this page
  before choosing WAL checkpoint settings.
