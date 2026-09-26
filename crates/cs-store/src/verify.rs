//! Chain and global-order verification (R4.5, R4.6).
//!
//! Owned by unit `verify`: scans `events` by `global_pos` in chunks of 10 000, each
//! a short read under the shared read gate, and reports the first problem as a
//! `cs_core::control::VerifyProblem`. Erased content is reported, not a failure.
