//! Erasing content while keeping every event and hash (R6, ADR 0011).
//!
//! Owned by unit `erase`: `plan` and `plan_id`; `erase` under the exclusive read
//! gate (delete blobs, append `content.erased`, set `erasure_pending`, then
//! `wal_checkpoint(TRUNCATE)` and backup removal); error 1004 and the 30-second
//! retry while an erasure is pending.
