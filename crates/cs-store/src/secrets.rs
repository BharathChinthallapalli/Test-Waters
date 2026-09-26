//! The per-install content key, kept in the OS keychain (R5.3, R5.4).
//!
//! Owned by unit `secrets`: the `SecretStore` trait, the keyring-core store
//! (service `callsheet`, user `content-key`, generated on first use), an in-memory
//! store for tests, and the "keychain unavailable" error (code 1001) with the
//! Linux-without-Secret-Service message from the design. Never a key file.
