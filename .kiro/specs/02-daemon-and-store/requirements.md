# Requirements — 02 daemon-and-store

## Introduction
Build the always-on local core that every later feature writes through: a daemon that listens
on the loopback address only, an authenticated JSON-RPC control API, a SQLite store owned by
the daemon, an append-only hash-chained event log, and content-addressed message storage that
is off by default. No model traffic is proxied yet (feature 03) and nothing is signed yet
(feature 04); this feature fixes the formats those features build on.

Decisions this spec relies on: ADR 0001 (daemon, not sidecar), 0002 (SQLite owned by the
daemon), 0003 (control API), 0006 (capture modes, keyed content addresses), 0007 (event
hashing), and the owner's decision on issue #7 (2026-09-26): **deleting erases stored content
and keeps event hashes**, so the chain always verifies. That decision gets its own ADR in
this feature's design.

## Requirement 1: Daemon process and loopback binding
**User story:** As a user, I want the daemon reachable only from my own machine, so that nothing
on my network can talk to it.

1. WHEN the daemon starts THE SYSTEM SHALL listen for the control API on `127.0.0.1` only, on a configurable port.
2. IF the configured listen address is not a loopback IPv4 address THEN THE SYSTEM SHALL exit with a non-zero status and an error message before opening any socket.
3. WHEN the daemon receives SIGINT or SIGTERM (Ctrl+C or a console close event on Windows) THE SYSTEM SHALL stop accepting requests, finish or flush pending writes, and exit with status 0.
4. THE SYSTEM SHALL keep its configuration and data in the operating system's per-user application directories and SHALL create them readable and writable by the owning user only.
5. IF another daemon is already running with the same data directory THEN THE SYSTEM SHALL refuse to start and name the running instance.
6. THE SYSTEM SHALL write structured logs, and SHALL never write the control-API token or any `Authorization` header value to them (tested).

## Requirement 2: Authenticated control API
**User story:** As the desktop app or the CLI, I want to call the daemon over a small typed API,
so that only clients of the same user can control it.

1. THE SYSTEM SHALL accept JSON-RPC 2.0 requests over HTTP POST and answer with JSON-RPC 2.0 responses.
2. WHEN the daemon starts for the first time THE SYSTEM SHALL generate a random token of at least 256 bits and store it in a file readable only by the owning user: mode `0600` on Unix, and a protected access-control list granting only the current user on Windows.
3. IF a request's `Authorization: Bearer` token is missing or wrong THEN THE SYSTEM SHALL answer with HTTP 401 and SHALL compare tokens in constant time.
4. IF a request carries an `Origin` header, or its `Host` header is not exactly `127.0.0.1:<bound port>`, THEN THE SYSTEM SHALL answer with HTTP 403. (Callsheet's clients, the CLI and the Electron main process, send no `Origin`; a browser-context client would be added by exact origin in a later ADR.)
5. THE SYSTEM SHALL provide the methods `health` and `version`; `version` SHALL return the daemon's version.
6. IF a request is not valid JSON, not a valid JSON-RPC request, or names an unknown method THEN THE SYSTEM SHALL answer with the matching JSON-RPC 2.0 error (-32700, -32600, -32601).
7. WHEN the daemon is listening THE SYSTEM SHALL write a discovery file, readable only by the owning user, that tells local clients the address to connect to; clients SHALL connect to `127.0.0.1`, never to `localhost`.
8. THE SYSTEM SHALL define every method's parameters, results and errors as Rust types in `cs-core` and generate their TypeScript types into `packages/api-types` (ADR 0009); a TypeScript client test SHALL call `health` and `version` using those types.
9. WHEN the user rotates the token THE SYSTEM SHALL write a new token and SHALL reject the old one from that moment on; IF a client receives HTTP 401 THEN it SHALL re-read the token file once before reporting the failure (ADR 0003).

## Requirement 3: SQLite store owned by the daemon
**User story:** As a maintainer, I want one process to own the database, so that the event log
stays consistent.

1. THE SYSTEM SHALL store its data in one SQLite database that only the daemon opens, in write-ahead-log mode (tested).
2. WHEN the daemon starts THE SYSTEM SHALL apply forward-only schema migrations, and running them again SHALL change nothing (tested).
3. IF the database schema is newer than the daemon understands THEN THE SYSTEM SHALL refuse to start instead of modifying it.
4. THE SYSTEM SHALL keep settings in the store, including `capture_content`, which SHALL default to `false`.

## Requirement 4: Append-only, hash-chained event log
**User story:** As a user, I want a record of what happened that can't be changed without it
showing, so that I can trust it later.

1. THE SYSTEM SHALL only ever insert event rows; no code path SHALL update or delete an event row (tested).
2. THE SYSTEM SHALL give each run its own chain, and SHALL number each run's events from 1 with no gaps and no repeats, including under concurrent appends (tested).
3. THE SYSTEM SHALL compute each event's `prev_hash` and `event_hash` exactly as ADR 0007 specifies (RFC 8785 canonical JSON of the event without `event_hash`, lowercase hex, 64 zeros for a run's first event), and SHALL record each event's position in the daemon's global commit order, numbered from 1 with no gaps.
4. THE SYSTEM SHALL never put message content in an event; an event SHALL refer to content only by its content address, and SHALL carry no content reference at all while content capture is off.
5. THE SYSTEM SHALL provide a verification that recomputes every chain and the global commit order, and reports the first event that was edited, reordered or inserted, or removed from anywhere except the end of the global commit order (tested for each case).
6. THE SYSTEM SHALL state in SECURITY.md and PRIVACY.md that removing the most recent events (the end of the global commit order) cannot be detected until signed checkpoints exist (feature 04), because a hash chain has no outside anchor.
7. THE SYSTEM SHALL check its RFC 8785 canonicalization against published test vectors before events are written with it.

## Requirement 5: Content storage, off by default
**User story:** As a user, I want message content stored only if I ask for it, once, and under
a key only my machine holds.

1. WHILE `capture_content` is `false` THE SYSTEM SHALL store no message content and no content address.
2. WHEN `capture_content` is `true` THE SYSTEM SHALL store each distinct message once, addressed by HMAC-SHA-256 under a per-install secret (ADR 0006); identical content in different calls SHALL be stored once (tested).
3. THE SYSTEM SHALL keep the per-install content secret in the operating system's keychain.
4. IF the keychain is unavailable THEN THE SYSTEM SHALL refuse to enable content capture and SHALL report why; it SHALL NOT fall back to storing the secret in a file.

## Requirement 6: Deleting content (issue #7)
**User story:** As a user, I want "delete" to actually remove my content, without breaking the
log's integrity.

1. WHEN the user asks to erase the content of a run THE SYSTEM SHALL first list every other run that refers to any of the same content, and SHALL erase only after the user confirms; a dry run SHALL show the same list and erase nothing.
2. WHEN the user confirms an erasure THE SYSTEM SHALL remove every stored copy of that content, including copies shared with the listed runs.
3. WHEN content is erased THE SYSTEM SHALL overwrite it in the database file and in the write-ahead log rather than only marking the space free, and SHALL remove any separate content files.
4. WHEN content is erased THE SYSTEM SHALL append an event recording the erasure and SHALL leave every existing event and hash unchanged; verification SHALL still pass and SHALL report the affected events as "content erased" (tested).
5. THE SYSTEM SHALL document in PRIVACY.md exactly what erasing does, including that copies left by the storage device or filesystem are outside Callsheet's control.

## Requirement 7: Checks
**User story:** As a maintainer, I want this feature held to the same gates as everything else.

1. THE SYSTEM SHALL pass every quality gate in `docs/ci.md` on Linux in CI.
2. WHERE behaviour differs by platform (file permissions, keychain, signals) THE SYSTEM SHALL have a test for each supported platform, and SHALL state in the design which platforms CI runs them on.
