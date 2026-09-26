# Privacy

Callsheet is local-first. **Nothing leaves your machine by default.** This file
is the contract; if the code ever disagrees with it, the code is the bug.

## What Callsheet sends

- **No telemetry.** Callsheet sends no usage data, crash reports or analytics.
  If telemetry is ever added it will be **opt-in**: off until you turn it on,
  described here first, and switchable off at any time.
- **Your own model traffic.** When you route an agent through the Callsheet
  proxy, the proxy forwards that agent's requests to the provider you
  configured, exactly as the agent would have sent them. That is your traffic
  to your provider, not data sent to the Callsheet project.
- **No other network calls** are made on your behalf unless a feature you
  enable says so here.

## What Callsheet stores (on your machine)

- **Metadata about each call by default:** model, token counts, latency,
  status, trace and span ids, and cost ([ADR 0006](docs/adr/0006-capture-modes.md)).
- **Message content only if you turn content capture on.** It is off by
  default; see [Content capture and your keychain](#content-capture-and-your-keychain).
- **Never:** API keys or auth headers. Callsheet does not log, store or echo
  them.
- **Signing keys** stay in your operating system's keychain and are used only
  by the local daemon ([ADR 0007](docs/adr/0007-identity-and-log-integrity.md)).

Everything is stored locally and stays under your control. You can erase a
run's message content; see [Erasing a run's content](#erasing-a-runs-content).

The daemon and store that hold this data are being built in feature 02. The
sections below describe how Callsheet behaves once that feature ships.

## Content capture and your keychain

- **Off by default.** While content capture is off, Callsheet stores no message
  content and no reference to it: no content address and no hash of it.
  Turning capture off stops new content being stored; content stored while it
  was on stays until you erase it.
- **Each message is stored once.** When you turn capture on, each distinct
  message is stored once in Callsheet's local database. It is addressed by an
  HMAC-SHA-256 computed with a secret key unique to your install, so someone
  holding your log can't confirm a guessed message by hashing it
  ([ADR 0006](docs/adr/0006-capture-modes.md)). The key is used only for these
  addresses; Callsheet does not encrypt the stored content itself.
- **The key lives in your operating system's keychain:** macOS Keychain,
  Windows Credential Manager, or a Secret Service provider on Linux.
- **No keychain, no capture.** If no keychain is available, Callsheet refuses
  to turn content capture on and tells you why. It never falls back to keeping
  the key in a file. On Linux without a Secret Service provider it says:
  "No Secret Service keychain was found (common on WSL and servers), so content
  capture stays off."
- In feature 02 no model traffic passes through Callsheet yet, so there is
  nothing to capture; the proxy arrives in feature 03.

## The event log

Callsheet records what happens as events in an append-only log
([ADR 0007](docs/adr/0007-identity-and-log-integrity.md)). Events never contain
message content; with capture on, they refer to it only by its content address.

Each run's events are hash-chained, and every event records its place in the
daemon's global commit order. Verification therefore detects an event that was
edited, reordered or inserted, or removed from the middle of the log. It
**cannot detect removal of the most recent events** (the end of the global
commit order) until signed checkpoints exist (feature 04), because a hash chain
has no outside anchor to compare against. For the same reason, until then
someone who can write the database can change events and recompute every hash
after them without verification noticing ([SECURITY.md](SECURITY.md)).

## Erasing a run's content

Erasing removes a run's stored message content and keeps the log verifiable
([ADR 0011](docs/adr/0011-erase-content-keep-hashes.md)). In feature 02 it is a
control-API call (`content.erasePlan`, then `content.erase`); there is no
command-line or desktop control for it yet.

What Callsheet does:

1. **Lists the other affected runs first.** The same message can be stored
   once for several runs. Callsheet lists every other run that refers to any of
   the content being erased, with the number of content items, and erases
   nothing until you confirm. A dry run shows the same list and erases
   nothing. If the list has changed since you saw it, the erasure is refused,
   so nothing is erased that you weren't shown.
2. **Removes every stored copy**, including the copies those other runs share.
   They lose that content too.
3. **Overwrites the content in the database file** rather than only marking
   the space free (SQLite `secure_delete` writes zeros over it), then truncates
   the write-ahead log, which can still hold earlier copies, to zero bytes.
4. **Removes Callsheet's own migration backups.** Before a schema upgrade
   Callsheet makes a backup copy of the database, and deletes it once the
   upgraded database has started cleanly. Erasure deletes any that remain.
5. **Reports success only once every copy is gone.** If the write-ahead log
   can't be truncated or a backup can't be removed yet, Callsheet reports
   "erasure pending" instead, and retries every 30 seconds and at the next
   start until it succeeds. The daemon's health report shows the erasure as
   pending meanwhile.
6. **Keeps every event and hash**, and records a `content.erased` event listing
   what was erased and which runs were affected. Verification still passes and
   reports the affected events as "content erased".

What remains after an erasure, by design:

- The fact that the events happened, their metadata (listed under
  [What Callsheet stores](#what-callsheet-stores-on-your-machine)), and the
  content addresses. What was said is gone. The addresses can't be turned back
  into content; only someone holding your install's content key could use one
  to confirm a guess.

What is outside Callsheet's control:

- Copies left by the storage device, for example by SSD wear levelling.
- Copies left by the filesystem, for example in snapshots or its journal.
- Disk space released without being overwritten. Truncating the write-ahead
  log and deleting a backup file free their space, and the old bytes can stay
  on the disk until that space is reused.
- Operating-system backups, such as Time Machine or File History.
- Backups or exports you made before the erasure.

## Status

Callsheet is pre-alpha. The daemon, proxy and store that these commitments
apply to are still being built (features 02–05); this document states the
rules they are built to.
