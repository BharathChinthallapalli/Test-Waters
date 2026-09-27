//! SQLite store and append-only event log, owned by the daemon (ADR 0002).
//!
//! Each module's doc comment states its contract and which feature 02 unit
//! (`.kiro/specs/02-daemon-and-store/tasks.md`) implements it.

pub mod content;
pub mod db;
pub mod erase;
pub mod fsperm;
pub mod migrate;
pub mod secrets;
pub mod verify;
pub mod writer;

pub use content::{ContentKey, content_address};
pub use writer::{AppendEvent, AppendedEvent, InvalidEvent, Store, StoreError, StoreOpenError};
