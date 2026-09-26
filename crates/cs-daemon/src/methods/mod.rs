//! Control-API method handlers (feature 02 design, method table).
//!
//! Owned by unit `wire`, which registers every method; units `verify` and `erase`
//! add their handlers in their own files.

pub mod core;
pub mod erase;
pub mod verify;
