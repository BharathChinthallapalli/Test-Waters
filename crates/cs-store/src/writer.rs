//! The single writer thread and event append (R4.1–R4.4).
//!
//! Owned by unit `writer`: a dedicated thread fed by a bounded `tokio::sync::mpsc`
//! queue with a `oneshot` reply per command; `append` assigns `seq` and
//! `global_pos`, hashes with `cs_core::event`, and writes `event_content` in the
//! same transaction; the shared read gate (`tokio::sync::RwLock`); the
//! `capture_content` setting (default false).
