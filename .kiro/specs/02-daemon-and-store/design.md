# Design — 02 daemon-and-store

## Overview
`cs-daemon` becomes a long-running process with one HTTP listener on `127.0.0.1` that serves
a small JSON-RPC 2.0 control API. It owns one SQLite database through `cs-store`. `cs-store`
holds the event log, settings and content blobs, and all writes go through a single writer
thread. `cs-core` holds every wire type and exports them to TypeScript (ADR 0009). No model
traffic, signing or Merkle tree yet; the formats here are what features 03 and 04 build on.

```
client (CLI, Electron main)
  │  POST /rpc  Authorization: Bearer <token>   Host: 127.0.0.1:<port>   (no Origin)
  ▼
cs-daemon ── body limit → timeout → Host/Origin check (403) → token check (401) → dispatch
  │                                                                                   │
  └── lock file, discovery file, token file, signals                  cs-store (writer thread)
                                                                        │
                                                          SQLite (WAL) + OS keychain secret
```

## Crates and dependencies
Versions are the latest on crates.io on 2026-09-26; each was read at its release tag.

| Crate | Version | Licence | Used for |
|---|---|---|---|
| tokio | 1.53.1 | MIT | runtime, TCP, signals, channels |
| axum | 0.8.9 | MIT | HTTP routing, `DefaultBodyLimit` (default 2 MB, set lower) |
| tower-http | 0.7.1 | MIT | `TimeoutLayer::with_status_code` (`::new` is deprecated) |
| rusqlite | 0.40.2 | MIT | SQLite, `bundled` feature → SQLite 3.53.2 |
| canon-json | 0.2.1 | MIT OR Apache-2.0 | RFC 8785 canonical JSON as a `serde_json` formatter |
| sha2 / hmac | 0.11.0 / 0.13.0 | MIT OR Apache-2.0 | SHA-256 event hashes, HMAC-SHA-256 content addresses |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | token and secret generation |
| subtle | 2.6.1 | BSD-3-Clause | constant-time token comparison |
| keyring-core | 1.0.0 | MIT OR Apache-2.0 | OS keychain API; its `mock` store in tests |
| apple-native / windows-native / zbus-secret-service keyring stores | 1.0.2 / 1.1.0 / 1.0.1 | per crate, checked by `cargo deny` | the platform keychains |
| etcetera | 0.11.0 | MIT OR Apache-2.0 | per-user directories (`choose_native_strategy`) |
| tracing / tracing-subscriber | 0.1.44 / 0.3.23 | MIT | structured logs |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | owner-only ACLs (Windows only) |
| hex, serde, serde_json | current | MIT OR Apache-2.0 | encoding |

Deliberately not used:
- `jsonrpsee`: two methods plus a few more don't justify it, and hand-written types in
  `cs-core` export cleanly with ts-rs.
- `directories`: its `dirs-sys` pulls `option-ext` (MPL-2.0); `etcetera` does the same job
  without it.
- `fs4`: `std::fs::File::try_lock` (stable since Rust 1.89) was checked on 1.98.1.
- A migrations crate: `PRAGMA user_version` plus an ordered list of SQL strings is enough.

`deny.toml` gains `BSD-3-Clause` (subtle) and `Zlib` (foldhash, via rusqlite's hashlink), both
permissive and compatible with MIT. The full tree resolves to 178 crates; everything else is
MIT, Apache-2.0, MIT/Apache dual, Unicode-3.0, BSL-1.0 or Unlicense OR MIT.

## Components

### Paths and permissions (R1.4)
`etcetera::app_strategy::choose_native_strategy` gives the per-user data and config
directories: Apple conventions on macOS, Windows on Windows, XDG elsewhere. A `--data-dir`
flag overrides it for tests. Everything Callsheet creates there is owner-only:
- **Unix:** directories `0700`, files `0600`, set at creation with
  `DirBuilderExt::mode` / `OpenOptionsExt::mode`, never chmod afterwards (no race).
- **Windows:** created with a protected DACL that grants only the current user's SID
  (`D:P(A;OICI;FA;;;<user SID>)`), built with `ConvertStringSecurityDescriptorToSecurityDescriptorW`
  and passed in `SECURITY_ATTRIBUTES` to `CreateFileW` / `CreateDirectoryW`, so there is no
  window where inherited permissions apply. The SID comes from `GetTokenInformation(TokenUser)`.
  Never a NULL DACL.

### Single instance and discovery (R1.5, R2.7)
- `daemon.lock`: opened and held with `File::try_lock` for the life of the process. If it's
  already locked, the daemon exits non-zero, naming the pid recorded in `daemon.json`.
- `daemon.json`, written after the listener is bound, owner-only:
  `{ "pid", "startedAtMs", "address": "127.0.0.1:<port>", "schemaVersion" }`. It's removed
  on graceful shutdown.
- Clients read `daemon.json` and send the token only if `daemon.lock` is currently locked,
  which means a daemon is alive, and the pid matches. A stale file left by a crash, where
  another process may have taken the port, therefore never receives the token.

### Configuration and binding (R1.1, R1.2)
The listen address comes from `--listen <ip:port>` or the config file, default
`127.0.0.1:<default port>`. The port is chosen in task 1 after checking the IANA registry;
tests use port `0`. The address is parsed first; anything other than `127.0.0.1` exits with
status 2 and a message before any socket exists. `127.0.0.1` rather than `::1` or
`localhost` keeps one address for clients (#10).

### Token (R2.2, R2.3, R2.9)
- 32 random bytes from `getrandom`, stored as 64 hex characters in `control-token`
  (owner-only).
- Kept in memory behind a lock. It's compared with `subtle::ConstantTimeEq` after a length
  check; the length is public.
- **Rotation:** the `token.rotate` method, authenticated with the current token, writes the
  new token to a temporary owner-only file, renames it over `control-token`, then swaps the
  in-memory value. Requests with the old token get 401 from then on.
- After a 401, clients re-read the file once and retry once.

### HTTP layer (R2.1, R2.3, R2.4, local limits)
One route: `POST /rpc`. Layers, outermost first:
1. `DefaultBodyLimit::max(64 KiB)`.
2. `TimeoutLayer::with_status_code(408, 10 s)`. Header-read timeouts come from the server
   builder; task 3 confirms the hyper setting in use.
3. Host/Origin: 403 if `Origin` is present, or if `Host` is not exactly `127.0.0.1:<port>`.
4. Token: 401 if `Authorization: Bearer` is missing or wrong.
5. JSON-RPC dispatch.

Logs never include header values. Auth failures log the fact and the peer, nothing else.

### JSON-RPC (R2.1, R2.5, R2.6, R2.8)
The envelope types are in `cs-core::rpc` (`Request`, `Response`, `ErrorObject`) and every
params and result type is in `cs-core::control`, all exported with ts-rs.
- **Standard errors:** -32700 parse, -32600 invalid request, -32601 method not found, -32602
  invalid params.
- **Callsheet errors** use codes outside the reserved range (ADR 0003): 1001 keychain
  unavailable, 1002 erase plan out of date, 1003 unknown run.
- **Batches** up to 16 requests are processed in order.

| Method | Result |
|---|---|
| `health` | `{ status: "ok", uptimeMs, schemaVersion, captureContent, lastGlobalPosition }` |
| `version` | `{ daemonVersion }` (the existing `VersionResult`) |
| `token.rotate` | `{}` |
| `settings.get` / `settings.setCaptureContent` | `{ captureContent }`; enabling fails with 1001 without a keychain |
| `events.verify` | `{ ok, eventsChecked, erasedEvents, firstProblem? }` |
| `content.erasePlan` | `{ planId, runId, sharedWithRuns[], contentItems }` (dry run, R6.1) |
| `content.erase` | `{ erasedItems, affectedRuns[], walTruncated }` given `runId` and `planId` |

No method appends events in 02; the proxy does that from feature 03. Tests append through
the `cs-store` API.

### Store (R3)
- **Connections:** one writer connection on a dedicated thread, fed by a bounded
  `tokio::sync::mpsc` queue with a `oneshot` reply per command. Every write is serialised
  there, which is what makes sequence numbers gap-free under concurrent callers. Verification
  and `health` use a separate read-only connection.
- **Pragmas** on every connection: `journal_mode=WAL`, `synchronous=FULL` (the log is the
  product, so durability wins over throughput), `foreign_keys=ON`, `secure_delete=ON`.
  `secure_delete` overwrites deleted content with zeros, which erasure relies on.
- **Migrations:** an ordered list of SQL strings, tracked with `PRAGMA user_version`.
  - A newer `user_version` than the daemon knows → refuse to start (R3.3).
  - Pending migrations on an existing database → first `VACUUM INTO backup-v<from>.db`
    (owner-only). That backup contains no free-page residue.
  - Each migration runs in its own transaction.

Schema version 1:

```sql
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE runs (run_id TEXT PRIMARY KEY, created_ms INTEGER NOT NULL,
                   last_seq INTEGER NOT NULL, last_hash TEXT NOT NULL);
CREATE TABLE events (
  global_pos INTEGER PRIMARY KEY,            -- 1, 2, 3 … assigned by the writer, no gaps
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  seq INTEGER NOT NULL,                      -- 1, 2, 3 … per run
  kind TEXT NOT NULL,
  ts_ms INTEGER NOT NULL,                    -- Unix milliseconds (< 2^53, safe as a JSON number)
  body TEXT NOT NULL,                        -- canonical JSON, no floats, content only by address
  prev_hash TEXT NOT NULL,
  event_hash TEXT NOT NULL,
  UNIQUE (run_id, seq));
CREATE TRIGGER events_no_update BEFORE UPDATE ON events BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER events_no_delete BEFORE DELETE ON events BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TABLE blobs (address TEXT PRIMARY KEY, bytes BLOB NOT NULL);
CREATE TABLE event_content (global_pos INTEGER NOT NULL REFERENCES events(global_pos),
                            address TEXT NOT NULL, PRIMARY KEY (global_pos, address));
```

The triggers enforce R4.1 in the database as well as in code. `event_content` is an index
of which events point at which content, written in the same transaction as the event. It
makes dry runs and "content erased" reports cheap.

### Event hashing (R4.2, R4.3, R4.7)
The hashed object is:

```json
{"body":{…},"globalPos":7,"kind":"…","prevHash":"<64 hex>","runId":"…","seq":3,"tsMs":1790000000000}
```

`event_hash = hex(SHA-256(canon_json(object)))`. `prevHash` is 64 zeros for `seq = 1`,
otherwise the previous event's `event_hash`. This is exactly ADR 0007. `globalPos` is
included, so moving an event in the global order also breaks its hash. Before any event
code runs, a test feeds the RFC 8785 test vectors (`input/` → `output/`, from the
cyberphone/json-canonicalization set that canon-json ships under `testdata/`) through our
serialiser. No floats appear in event bodies at all.

### Verification (R4.5, R4.6)
Verification scans `events` by `global_pos` and reports the first of:
- a gap or repeat in `global_pos`;
- a gap or repeat in `seq` within a run;
- a `prev_hash` mismatch;
- an `event_hash` mismatch (field edited);
- `runs.last_*` disagreeing with the last event.

Content references are checked against `blobs`. A missing blob is `contentErased` if a later
`content.erased` event in the same run lists it, and a problem otherwise. Removing events
from the end of the global order leaves no trace, and SECURITY.md and PRIVACY.md say so
until feature 04's checkpoints exist.

### Content and secrets (R5)
- `trait SecretStore { fn content_key(&self) -> Result<[u8; 32], KeychainUnavailable>; }`.
  The production store keeps the key in a keyring-core entry (service `callsheet`,
  user `content-key`), generating it on first use. Tests inject keyring-core's `mock` store
  or an in-memory implementation, so capture-enabled paths run on any CI machine.
- Enabling capture first calls `content_key()`. If that fails, capture stays off and the
  method returns error 1001 with the store's reason. No key file fallback.
- `put_content(bytes)` → `address = hex(HMAC-SHA-256(key, bytes))`, then
  `INSERT OR IGNORE INTO blobs`. The same content is stored once. While capture is off,
  `put_content` isn't called and events carry no `content` field.

### Erasure (R6)
- **Plan:** `plan(run_id)` collects the addresses referenced by that run's events, the other
  runs referencing any of them, and
  `plan_id = hex(SHA-256(canon_json({runId, addresses, sharedWithRuns})))`.
- **Erase:** `erase(run_id, plan_id)` recomputes the plan and rejects with 1002 if the ID
  differs, so nothing is erased that the user wasn't shown. Otherwise, in one transaction:
  1. `DELETE FROM blobs WHERE address IN (…)` (overwritten because `secure_delete=ON`);
  2. append a `content.erased` event to the run, listing the addresses and affected runs.
- **After commit:** `PRAGMA wal_checkpoint(TRUNCATE)`. Success truncates the WAL to zero
  bytes; if it reports busy, retry briefly and return `walTruncated: false` so callers know.
- **Test:** after erasing, the database file and the `-wal` file are searched for the
  content's bytes and must not contain them.
- In 02 content lives only in the database, so there are no separate content files to
  remove (R6.3).

### Logging (R1.6)
`tracing-subscriber` writes JSON to stderr. There's no request/response body or header
logging anywhere. A test sends requests carrying a known token, then asserts the captured
log output never contains it.

### Shutdown (R1.3)
`tokio::signal` handles `ctrl_c` and Unix `SignalKind::terminate()`, plus Windows
`ctrl_c` and `ctrl_close`. Shutdown then runs in order:
1. axum's `with_graceful_shutdown` stops new requests;
2. the writer queue is drained and closed;
3. `daemon.json` is removed and the lock released;
4. the process exits 0.

## Decisions (ADRs)
- **ADR 0011** — erasing content keeps event hashes (owner decision on #7), written with
  this design.
- ADRs 0001–0003, 0006, 0007 and 0009 apply unchanged.

## Error handling
`cs-store` and `cs-daemon` use typed errors that map to JSON-RPC errors at one place, the
dispatcher. Startup errors (bad address, lock held, newer schema, unreadable token) print one
line, name the file or value involved without revealing secrets, and exit non-zero.
Nothing panics on untrusted input: request bodies, headers or files.

## Testing strategy
- **Unit (cs-store):**
  - migrations are idempotent;
  - a newer schema is refused;
  - triggers block UPDATE and DELETE;
  - gap-free `seq` and `global_pos` with 8 concurrent writers × 1 000 events;
  - verification catches each tamper case: edit, reorder, insert, middle removal;
  - end-of-log truncation is *not* caught (asserted, documenting R4.6);
  - dedup;
  - erasure: plan/confirm, stale plan rejected, bytes absent from `.db` and `-wal`, verify
    still ok with `contentErased`;
  - RFC 8785 vectors.
- **Unit (cs-daemon):**
  - non-loopback addresses rejected;
  - 401, 403 (Origin; wrong Host) and 408 paths;
  - body limit;
  - token rotation;
  - lock held → refuse;
  - owner-only modes on Unix;
  - token absent from logs.
- **Integration (TypeScript):** a `node --test` file in `packages/api-types` starts the
  daemon binary with a temporary `--data-dir` and port 0. It reads `daemon.json` and the
  token, calls `health` and `version` with the generated types, and checks 401 with a wrong
  token and a successful call after `token.rotate` plus a re-read.
- **Platforms (R7.2):**
  - Linux CI runs everything.
  - A new Windows CI job runs the ACL, lock and signal tests for `cs-store` and `cs-daemon`.
  - macOS shares the Unix permission code with Linux; the real keychain is exercised by hand
    once per release, since CI uses the mock store everywhere.
  - The new job is added to **CI passed**.
- **Render check:** the desktop app is not wired to the daemon in 02; a manual `curl` run
  against the running daemon is recorded in `progress.md` instead.

## Sources (read 2026-09-26)
- axum `axum-v0.8.9`: `axum-core/src/extract/default_body_limit.rs`,
  `axum/src/serve/mod.rs` (graceful shutdown).
- tower-http `tower-http-0.7.1`: `tower-http/src/timeout/service.rs`.
- rusqlite `v0.40.2`: `Cargo.toml` features, `libsqlite3-sys/sqlite3/sqlite3.h`
  (`SQLITE_VERSION "3.53.2"`), `sqlite3.c` (secure-delete overwrite, `SQLITE_CHECKPOINT_TRUNCATE`
  "truncates the log file to zero bytes", `VACUUM INTO`); also the sqlite.org pages quoted in #7.
- canon-json `v0.2.1` README and `testdata/`; serde_jcs `v0.2.0` compared.
- keyring-rs `v4.2.0` README and `Cargo.toml` (store crates); keyring-core `1.0.0` (`mock`).
- etcetera `0.11.0` README (`choose_native_strategy`).
- tokio `tokio-1.53.1`: `tokio/src/signal/unix.rs`, `windows.rs`.
- Rust std `File::try_lock`: probed on rustc 1.98.1 (second handle gets `WouldBlock`).
- Microsoft Learn: "Creating a DACL", "Security Descriptor String Format", and SDDL
  protected-DACL (`D:P`) semantics.
- Dependency licences from `cargo metadata` over the resolved tree.
- Owner inputs: review on #32, issues #7, #10, #21, #22.
