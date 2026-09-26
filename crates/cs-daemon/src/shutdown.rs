//! Graceful shutdown (R1.3).
//!
//! Owned by unit `daemon-proc`: Ctrl+C and SIGTERM (console close on Windows) stop
//! new requests, drain the writer, remove `daemon.json`, release the lock and exit 0.
