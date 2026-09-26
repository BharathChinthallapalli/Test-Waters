//! Command line and configuration (R1.1, R1.2).
//!
//! Owned by unit `daemon-proc`: `--listen <ip:port>` and `--data-dir`; the address
//! is parsed first and anything but `127.0.0.1` exits with status 2 before any
//! socket exists; per-user directories from `etcetera::choose_native_strategy`.
//! The default port waits on an owner decision (see `tasks.md`, task 0).
