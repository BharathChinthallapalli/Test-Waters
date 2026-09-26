//! Forward-only migrations tracked with `PRAGMA user_version` (R3.2, R3.3).
//!
//! Owned by unit `schema`: schema version 1 from the design, including the
//! append-only triggers; refusal to open a newer schema; the owner-only
//! `VACUUM INTO backup-v<from>.db` before migrating an existing database; and the
//! function that deletes those backups once a migrated startup has verified.
