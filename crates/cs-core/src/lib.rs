//! Domain types for Callsheet. This crate performs no I/O.
//!
//! Types that cross the control API derive `ts_rs::TS` in test builds only;
//! `pnpm gen-types` writes them to `packages/api-types` (ADR 0009).

pub mod control;
pub mod event;
pub mod llm;
pub mod rpc;
