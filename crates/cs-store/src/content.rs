//! Content-addressed message storage, off by default (R5.1, R5.2, ADR 0006).
//!
//! Owned by unit `writer`: [`put_content`] stores bytes under
//! [`content_address`], `hex(HMAC-SHA-256(content key, bytes))`, with
//! `INSERT OR IGNORE`, so identical content is stored once. The writer calls it
//! only while capture is on; with capture off nothing here runs and events carry
//! no `content` field.
//!
//! The address is keyed with the per-install content key (ADR 0006): a plain
//! SHA-256 would let anyone holding the log confirm a guessed message. HMAC comes
//! from `hmac` 0.13.0 over `sha2` 0.11.0 (RustCrypto; `hmac-0.13.0/src/lib.rs`,
//! `block_api.rs`, `utils.rs`).
//!
//! [`ContentKey`] holds the key in memory. It zeroes its bytes when dropped and
//! never prints them; like `crate::secrets`, this is best effort, since safe Rust
//! can't stop the compiler from leaving copies.

use std::fmt;

use hmac::digest::common::KeySizeUser;
use hmac::digest::typenum::Unsigned;
use hmac::{Hmac, KeyInit, Mac};
use rusqlite::{Connection, params};
use sha2::Sha256;

use crate::secrets::KEY_LEN;

/// Length of a content address: 64 lowercase hex characters.
pub const ADDRESS_LEN: usize = 64;

type HmacSha256 = Hmac<Sha256>;

// The content key fits in HMAC-SHA-256's key block (its hash block, 64 bytes).
const _: () = assert!(KEY_LEN <= <<HmacSha256 as KeySizeUser>::KeySize as Unsigned>::USIZE);

/// The per-install content key, held in memory while capture is on.
///
/// Zeroed on drop; `Debug` never shows it.
pub struct ContentKey([u8; KEY_LEN]);

impl ContentKey {
    /// Takes the key out of `bytes`, which is zeroed.
    pub fn take(bytes: &mut [u8; KEY_LEN]) -> Self {
        let key = Self(*bytes);
        wipe(bytes);
        key
    }
}

impl Drop for ContentKey {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl fmt::Debug for ContentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContentKey(..)")
    }
}

/// `hex(HMAC-SHA-256(key, bytes))`: 64 lowercase hex characters.
pub fn content_address(key: &ContentKey, bytes: &[u8]) -> String {
    // HMAC pads a key shorter than the hash's block with zeros (RFC 2104
    // section 2; `get_der_key` in hmac 0.13.0 `utils.rs`), so this block-sized
    // key gives the same MAC as the 32-byte key. Passing a full block uses
    // `KeyInit::new`, which takes a key of exactly that size and can't fail.
    let mut block = hmac::digest::Key::<HmacSha256>::default();
    block[..KEY_LEN].copy_from_slice(&key.0);
    let mut mac = <HmacSha256 as KeyInit>::new(&block);
    wipe(&mut block);
    mac.update(bytes);
    hex::encode(mac.finalize().into_bytes())
}

/// Stores `bytes` under their content address unless a blob with that address
/// exists already, and returns the address.
///
/// Runs inside the caller's transaction, so the blob commits (or not) together
/// with the event that refers to it.
pub fn put_content(conn: &Connection, key: &ContentKey, bytes: &[u8]) -> rusqlite::Result<String> {
    let address = content_address(key, bytes);
    conn.prepare_cached("INSERT OR IGNORE INTO blobs (address, bytes) VALUES (?1, ?2)")?
        .execute(params![address, bytes])?;
    Ok(address)
}

/// Whether any blob is stored.
pub fn has_blobs(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS (SELECT 1 FROM blobs)", [], |row| row.get(0))
}

/// Whether content was ever stored under the current key: any blob, or any
/// event that refers to a content address. Erasure removes blobs but keeps
/// `event_content`, so this stays true after every blob is erased.
pub fn content_ever_stored(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM event_content) OR EXISTS (SELECT 1 FROM blobs)",
        [],
        |row| row.get(0),
    )
}

/// How many blobs are stored.
pub fn blob_count(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row("SELECT count(*) FROM blobs", [], |row| {
        let count: i64 = row.get(0)?;
        u64::try_from(count).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, count))
    })
}

fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(bytes: [u8; KEY_LEN]) -> ContentKey {
        ContentKey::take(&mut { bytes })
    }

    fn sequential_key() -> ContentKey {
        let mut bytes = [0u8; KEY_LEN];
        for (byte, value) in bytes.iter_mut().zip(0u8..) {
            *byte = value;
        }
        key(bytes)
    }

    fn blobs_table() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE blobs (address TEXT PRIMARY KEY, bytes BLOB NOT NULL);")
            .unwrap();
        conn
    }

    /// RFC 4231 test case 2 (key "Jefe"). HMAC zero-pads short keys, so "Jefe"
    /// followed by 28 zero bytes is the same key.
    #[test]
    fn matches_rfc_4231_test_case_2() {
        let mut bytes = [0u8; KEY_LEN];
        bytes[..4].copy_from_slice(b"Jefe");

        assert_eq!(
            content_address(&key(bytes), b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    /// Pinned with Python's `hmac` module:
    /// `hmac.new(bytes(range(32)), b'hello content', hashlib.sha256).hexdigest()`.
    #[test]
    fn matches_an_independent_hmac_with_a_full_length_key() {
        let address = content_address(&sequential_key(), b"hello content");

        assert_eq!(
            address,
            "a6772aa135ae74111d1c03961fa961279bf612143010fb0052b9590ee884eddd"
        );
        assert_eq!(address.len(), ADDRESS_LEN);
    }

    #[test]
    fn the_address_depends_on_the_key() {
        let other = key([7; KEY_LEN]);

        assert_ne!(
            content_address(&sequential_key(), b"same"),
            content_address(&other, b"same")
        );
    }

    #[test]
    fn take_zeroes_the_source_and_debug_hides_the_key() {
        let mut bytes = [0xAB; KEY_LEN];
        let key = ContentKey::take(&mut bytes);

        assert_eq!(bytes, [0; KEY_LEN]);
        assert_eq!(format!("{key:?}"), "ContentKey(..)");
    }

    #[test]
    fn identical_content_is_stored_once() {
        let conn = blobs_table();
        let key = sequential_key();

        let first = put_content(&conn, &key, b"a message").unwrap();
        let second = put_content(&conn, &key, b"a message").unwrap();
        let other = put_content(&conn, &key, b"another message").unwrap();

        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(blob_count(&conn).unwrap(), 2);
        assert!(has_blobs(&conn).unwrap());
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT bytes FROM blobs WHERE address = ?1",
                [&first],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, b"a message");
    }

    #[test]
    fn empty_content_has_an_address_too() {
        let conn = blobs_table();
        assert!(!has_blobs(&conn).unwrap());

        let address = put_content(&conn, &sequential_key(), b"").unwrap();

        assert_eq!(address.len(), ADDRESS_LEN);
        assert_eq!(blob_count(&conn).unwrap(), 1);
    }
}
