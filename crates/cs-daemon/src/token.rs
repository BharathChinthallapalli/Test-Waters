//! The control-API token (R2.2, R2.3, R2.9).
//!
//! Owned by unit `daemon-proc`: 32 random bytes (`getrandom::fill`) stored as 64
//! lowercase hex characters, and nothing else, in the owner-only `control-token`
//! file in the data directory. The first start creates it with
//! `cs_store::fsperm::write_owner_only_atomic`; later starts read it. A file that
//! isn't exactly 64 lowercase hex characters is an error and is never replaced
//! silently: the user may have clients configured with it.
//!
//! In memory the token sits behind a lock and is compared with
//! [`crate::http::tokens_match`] (constant time after a length check).
//! [`ControlToken::rotate`] writes a new token to a temporary owner-only file,
//! renames it over `control-token`, then swaps the in-memory value; from that
//! moment the old token gets 401. Neither `Debug` nor any error message shows a
//! token.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};

use cs_store::fsperm;

use crate::http::{TokenVerifier, tokens_match};

/// The token file's name inside the data directory.
pub const TOKEN_FILE_NAME: &str = "control-token";

/// Random bytes in a token (256 bits, R2.2).
const TOKEN_BYTES: usize = 32;

/// Length of the token as it is stored and sent: hex, two characters per byte.
pub const TOKEN_LEN: usize = TOKEN_BYTES * 2;

/// A token as sent in `Authorization: Bearer`: 64 lowercase hex characters.
type Encoded = [u8; TOKEN_LEN];

/// Why the token could not be loaded, created or rotated. Never contains a token.
#[derive(Debug)]
pub enum TokenError {
    /// Reading or writing the file failed.
    Io { path: PathBuf, source: io::Error },
    /// The file exists but isn't 64 lowercase hex characters.
    Malformed { path: PathBuf },
    /// The operating system's random number generator failed.
    Random(getrandom::Error),
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "cannot use {}: {source}", path.display()),
            Self::Malformed { path } => write!(
                f,
                "{} is not a Callsheet token (expected {TOKEN_LEN} lowercase hex \
                 characters); fix or delete it",
                path.display()
            ),
            Self::Random(source) => write!(f, "cannot generate a token: {source}"),
        }
    }
}

/// The message already includes the underlying error.
impl std::error::Error for TokenError {}

/// The control token of a running daemon, and its file.
pub struct ControlToken {
    path: PathBuf,
    current: RwLock<Encoded>,
    /// Serialises rotations, so the file and the memory always end up with the
    /// same token.
    rotating: Mutex<()>,
}

impl fmt::Debug for ControlToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlToken")
            .field("path", &self.path)
            .field("current", &"<redacted>")
            .finish()
    }
}

impl ControlToken {
    /// Reads `control-token` in `data_dir`, or creates it with a new random token
    /// when there is none.
    pub fn load_or_create(data_dir: &Path) -> Result<Self, TokenError> {
        let path = data_dir.join(TOKEN_FILE_NAME);
        let token = match fs::read(&path) {
            Ok(bytes) => {
                parse(&bytes).ok_or_else(|| TokenError::Malformed { path: path.clone() })?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let token = generate()?;
                write(&path, &token)?;
                token
            }
            Err(source) => return Err(TokenError::Io { path, source }),
        };
        Ok(Self {
            path,
            current: RwLock::new(token),
            rotating: Mutex::new(()),
        })
    }

    /// The token file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Replaces the token (R2.9): the new one is written to the file first, then
    /// takes effect in memory. If writing fails, the old token stays valid in both.
    ///
    /// Blocks on file I/O; from async code run it with
    /// `tokio::task::spawn_blocking`, which also lets it finish when the request
    /// that asked for it is dropped (see `crate::rpc::Handler`).
    pub fn rotate(&self) -> Result<(), TokenError> {
        let _rotating = self.rotating.lock().unwrap_or_else(PoisonError::into_inner);
        let token = generate()?;
        write(&self.path, &token)?;
        // A poisoned lock still holds a whole token: writes replace it in one step.
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = token;
        Ok(())
    }
}

impl TokenVerifier for ControlToken {
    fn verify(&self, presented: &[u8]) -> bool {
        let current = self.current.read().unwrap_or_else(PoisonError::into_inner);
        tokens_match(current.as_slice(), presented)
    }
}

fn generate() -> Result<Encoded, TokenError> {
    let mut random = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut random).map_err(TokenError::Random)?;
    Ok(encode(random))
}

/// Lowercase hex, two characters per byte.
fn encode(bytes: [u8; TOKEN_BYTES]) -> Encoded {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut token = [0u8; TOKEN_LEN];
    for (pair, byte) in token.as_chunks_mut::<2>().0.iter_mut().zip(bytes) {
        pair[0] = DIGITS[usize::from(byte >> 4)];
        pair[1] = DIGITS[usize::from(byte & 0x0f)];
    }
    token
}

fn write(path: &Path, token: &Encoded) -> Result<(), TokenError> {
    fsperm::write_owner_only_atomic(path, token).map_err(|source| TokenError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// The token in a file's contents: exactly 64 lowercase hex characters.
fn parse(bytes: &[u8]) -> Option<Encoded> {
    let token: Encoded = bytes.try_into().ok()?;
    token
        .iter()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        .then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_64_lowercase_hex_and_differ() {
        let first = generate().unwrap();
        let second = generate().unwrap();

        assert!(parse(&first).is_some());
        assert_ne!(first, second);
    }

    #[test]
    fn encode_is_lowercase_hex() {
        let bytes: [u8; TOKEN_BYTES] = std::array::from_fn(|i| (i as u8).wrapping_mul(37));

        assert_eq!(encode(bytes).as_slice(), hex::encode(bytes).as_bytes());
    }

    #[test]
    fn parse_accepts_only_exactly_64_lowercase_hex() {
        let good = "0123456789abcdef".repeat(4);
        assert!(parse(good.as_bytes()).is_some());

        for bad in [
            good.to_uppercase(),
            format!("{good}\n"),
            good[1..].to_owned(),
            format!("{}g", &good[1..]),
            String::new(),
        ] {
            assert!(parse(bad.as_bytes()).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn debug_and_errors_hide_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let token = ControlToken::load_or_create(dir.path()).unwrap();
        let secret = fs::read_to_string(token.path()).unwrap();

        assert!(!format!("{token:?}").contains(&secret));

        fs::write(token.path(), format!("{secret}x")).unwrap();
        let error = ControlToken::load_or_create(dir.path()).unwrap_err();
        assert!(matches!(error, TokenError::Malformed { .. }));
        assert!(!error.to_string().contains(&secret), "{error}");
    }
}
