//! Event hashing (ADR 0007, feature 02 design "Event hashing").
//!
//! Owned by feature 02 unit `event-hash`: the hashed-object type, `ZERO_HASH`,
//! `event_hash()` over RFC 8785 canonical JSON, and the number check that keeps
//! floats and integers outside ±(2^53 − 1) off the hash path.
