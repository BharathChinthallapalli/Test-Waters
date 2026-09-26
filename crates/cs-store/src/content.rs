//! Content-addressed message storage, off by default (R5.1, R5.2).
//!
//! Owned by unit `writer`: `put_content` stores bytes under
//! `hex(HMAC-SHA-256(content key, bytes))` with `INSERT OR IGNORE`, so identical
//! content is stored once. Not called while capture is off.
