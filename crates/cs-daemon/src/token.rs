//! The control-API token (R2.2, R2.3, R2.9).
//!
//! Owned by unit `daemon-proc`: 32 random bytes stored as 64 hex characters in the
//! owner-only `control-token` file, kept in memory behind a lock, compared in
//! constant time after a length check, and rotated by writing a new file with
//! `cs_store::fsperm::write_owner_only_atomic` before swapping the in-memory value.
