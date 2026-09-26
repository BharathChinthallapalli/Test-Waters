//! Opening the database (feature 02 design "Store").
//!
//! Owned by unit `schema`: the writer connection and the read-only connection, each
//! with `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON` and
//! `secure_delete=ON`. The database file is created owner-only through
//! [`crate::fsperm`] before SQLite opens it; SQLite gives its `-wal` and `-shm`
//! files the same permissions as the database file.
