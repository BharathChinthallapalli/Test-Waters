//! Single instance and discovery (R1.5, R2.7).
//!
//! Owned by unit `daemon-proc`: `daemon.lock` held with `File::try_lock` for the
//! life of the process; `daemon.json` (`pid`, `startedAtMs`, `address`,
//! `schemaVersion`) written owner-only after the listener is bound, removed on
//! graceful shutdown.
