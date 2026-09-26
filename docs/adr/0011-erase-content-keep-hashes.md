# 0011. Erasing content keeps event hashes

- Status: Accepted (owner decision on issue #7, 2026-09-26)
- Date: 2026-09-26

## Context

The event log is append-only and hash-chained (ADR 0007). Later features promise that memory
is "local and deletable" (roadmap 13) and add retention (roadmap 14), and PRIVACY.md promises
users control over their data. If personal content sat inside a hashed preimage, deleting it
would break verification. Because feature 02 fixes the chain format, this must be settled
before its first migration.

Issue #7 lists what the relevant specifications and SQLite allow:
- Merkle proofs never contain leaf data (RFC 9162), so data can go while proofs still verify.
- C2SP tlog-tiles allows pruning only under a written policy.
- A plain SQLite `DELETE` doesn't erase bytes; `secure_delete` and a truncating WAL
  checkpoint are needed.

The owner chose between erasing content only, also pruning whole events, and no deletion in
feature 02.

## Decision

**Deleting erases stored content and keeps every event and hash.**

- Events never contain message content; they refer to it by its keyed content address
  (ADR 0006).
- Erasing a run's content removes every stored copy of that content, including copies shared
  with other runs. The shared runs are listed first, and erasure needs confirmation (feature
  02, requirement 6).
- The bytes are overwritten, not just freed: SQLite `secure_delete` is on, and the WAL is
  truncated after the erasure.
- A new `content.erased` event records the erasure. Verification still passes and reports the
  affected events as "content erased".
- Whole events are never pruned. At 32 bytes of hash per event, that costs little. If pruning
  is ever needed, it will follow C2SP tlog-tiles with a recorded minimum index and a written
  policy, in a new ADR.

## Consequences

- The chain and future checkpoints (feature 04) stay verifiable forever, including across
  erasures.
- What was said is gone after an erasure, but the fact that events happened remains. That
  metadata is kept by design, and PRIVACY.md says so.
- Files Callsheet itself creates never keep erased content: its own migration backups are
  deleted once a migrated database has started cleanly, and erasure removes any that remain.
  An erasure reports success only after the WAL has been truncated as well.
- Copies left by the storage device, the filesystem, or backups and exports the user made
  before the erasure (including feature 14's) are outside Callsheet's control; PRIVACY.md
  says so. Feature 14's backups use `VACUUM INTO`, so they carry no deleted residue.
- Metadata fields must never become content in disguise (for example tool arguments or file
  paths in event bodies); feature 03's design has to check this for each field it records.

## Sources

- Issue #7 (owner research: RFC 9162 §2.1.1 and §2.1.4, C2SP tlog-tiles "Pruning", SQLite
  `lang_vacuum`, `pragma`, `wal` pages).
- SQLite 3.53.2 source as bundled by rusqlite 0.40.2: secure-delete overwrite and
  `SQLITE_CHECKPOINT_TRUNCATE` ("truncates the log file to zero bytes").
- ADR 0006 (keyed content addresses), ADR 0007 (event hashing).
