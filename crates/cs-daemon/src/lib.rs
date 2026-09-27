//! Callsheet daemon internals. `main.rs` is the only binary; the modules live in
//! this library so each can be tested on its own.
//!
//! Each module's doc comment states its contract and which feature 02 unit
//! (`.kiro/specs/02-daemon-and-store/tasks.md`) implements it.

pub mod config;
pub mod http;
pub mod instance;
pub mod logging;
pub mod methods;
pub mod rpc;
pub mod serve;
pub mod shutdown;
pub mod token;
