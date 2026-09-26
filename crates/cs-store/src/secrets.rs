//! The per-install content key, kept in the OS keychain (R5.3, R5.4).
//!
//! Owned by unit `secrets`: the `SecretStore` trait, the keyring-core store
//! (service `callsheet`, user `content-key`, generated only when the caller says
//! no content exists yet), an in-memory
//! store for tests, and the "keychain unavailable" error (code 1001) with the
//! Linux-without-Secret-Service message from the design. Never a key file.
//!
//! - The key is 32 random bytes from [`getrandom::fill`], stored as 64 lowercase
//!   hex characters with `set_password`. A string round-trips identically on every
//!   store: the Windows store keeps passwords as UTF-16 but secrets as raw bytes, so
//!   the pair used to write must also be used to read, and KDE Wallet's Secret
//!   Service accepts only UTF-8 (zbus-secret-service-keyring-store 1.0.1 crate docs,
//!   "Usage with KDE Wallet").
//! - A stored key that isn't 64 hex characters is reported, never replaced:
//!   replacing it would orphan every blob addressed under the old key.
//! - Reasons never contain the key or any part of it.
//! - Buffers holding the key are zeroed before they are dropped, as far as safe
//!   Rust allows (best effort: the compiler may still leave copies).

use std::error::Error as StdError;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use keyring_core::{CredentialStore, Entry, Error as KeyringError};

/// The keychain service name of the content key.
pub const SERVICE: &str = "callsheet";
/// The keychain user (account) name of the content key.
pub const USER: &str = "content-key";
/// Length of the content key in bytes.
pub const KEY_LEN: usize = 32;

/// The reason reported on Linux when no Secret Service provider answers.
pub const NO_SECRET_SERVICE: &str = "No Secret Service keychain was found (common on WSL and servers), so content capture stays off.";

/// The reason reported on Linux when a Secret Service answers but has no
/// default keyring (collection) to hold the key.
pub const NO_DEFAULT_KEYRING: &str = "A Secret Service keychain answered but has no default keyring (common on WSL), so content capture stays off. Create or unlock a default keyring and try again.";

/// The reason reported when the key is missing but content may be stored under it.
pub const KEY_MISSING: &str = "The OS keychain has no Callsheet content key, but content was stored under one. A new key would make that content unreadable, so none was created and content capture stays off. If the keychain is locked, unlock it and try again.";

const HEX_LEN: usize = KEY_LEN * 2;

/// What [`SecretStore::content_key`] does when the keychain has no content key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfMissing {
    /// Create and store a new key. Only for a store that holds no content yet,
    /// so no blob can be addressed under a key that is replaced.
    Generate,
    /// Report [`KEY_MISSING`]. Some keychains hide entries while locked (a
    /// Secret Service provider may leave locked collections out of searches), so
    /// "no entry" doesn't prove the key never existed.
    Fail,
}

/// Where the content key comes from.
pub trait SecretStore: Send + Sync {
    /// Returns the content key. If the keychain has none, creates one only when
    /// `if_missing` is [`IfMissing::Generate`].
    ///
    /// This call blocks, and on Linux it drives the Secret Service over D-Bus
    /// through zbus's own runtime, which panics if entered from a thread that is
    /// already running async code. Call it from a blocking context, for example
    /// `tokio::task::spawn_blocking`, never directly from an async task.
    ///
    /// It may also wait for the user to answer a keychain unlock prompt, and each
    /// call talks to the keychain again. Call it once when capture is enabled and
    /// keep the key; a caller that must answer promptly bounds its own wait.
    fn content_key(&self, if_missing: IfMissing) -> Result<[u8; KEY_LEN], KeychainUnavailable>;
}

/// The OS keychain can't provide the content key, so content capture stays off.
///
/// Carries a reason for the user; never the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainUnavailable {
    reason: String,
}

impl KeychainUnavailable {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// Why the key is unavailable, for the user.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// The JSON-RPC error code for this error (ADR 0003).
    pub fn code(&self) -> i32 {
        cs_core::rpc::codes::KEYCHAIN_UNAVAILABLE
    }
}

impl fmt::Display for KeychainUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl StdError for KeychainUnavailable {}

type OpenStore = dyn Fn() -> keyring_core::Result<Arc<CredentialStore>> + Send + Sync;
type Explain = fn(&KeyringError) -> Option<&'static str>;

/// Serializes key creation across every store in the process, so two callers
/// can't each store a different key. Reads don't take it, so a caller never
/// waits behind another caller's unlock prompt just to read. The daemon's
/// single-instance lock (R1.5) keeps other Callsheet processes out.
static GENERATING: Mutex<()> = Mutex::new(());

/// The content key in a keyring-core credential store.
pub struct KeyringSecretStore {
    open: Box<OpenStore>,
    explain: Explain,
}

/// The content key in the OS keychain: the login keychain on macOS, the
/// Credential Manager on Windows, the Secret Service on other Unix systems.
///
/// Nothing is contacted until [`SecretStore::content_key`] is called, and each
/// call connects afresh, so a keychain started after the daemon is still found.
pub fn os_keychain() -> KeyringSecretStore {
    KeyringSecretStore::opening(platform::open, platform::explain)
}

impl KeyringSecretStore {
    /// The content key in `store`, for example `keyring_core::mock::Store`.
    pub fn new(store: Arc<CredentialStore>) -> Self {
        Self::opening(move || Ok(Arc::clone(&store)), |_| None)
    }

    /// Opens the store with `open` on every call; `explain` may replace the
    /// store's reason for an error with one written for the user.
    fn opening(
        open: impl Fn() -> keyring_core::Result<Arc<CredentialStore>> + Send + Sync + 'static,
        explain: Explain,
    ) -> Self {
        Self {
            open: Box::new(open),
            explain,
        }
    }

    fn entry(&self) -> keyring_core::Result<Entry> {
        (self.open)()?.build(SERVICE, USER, None)
    }

    /// Creates and stores a key unless another caller did so first.
    fn generate(&self, entry: &Entry) -> Result<[u8; KEY_LEN], KeychainUnavailable> {
        match read_key(entry) {
            Err(Failure::Store(KeyringError::NoEntry)) => {}
            other => return other.map_err(|failure| self.failure(failure)),
        }
        let mut key = generate_key()?;
        let stored = store_key(entry, &key);
        wipe(&mut key);
        stored.map_err(|err| self.unavailable(err))?;
        tracing::info!(
            service = SERVICE,
            user = USER,
            "stored a new content key in the OS keychain"
        );
        // Return what the keychain now holds, which is what later calls will see.
        read_key(entry).map_err(|failure| match failure {
            Failure::Store(KeyringError::NoEntry) => KeychainUnavailable::new(
                "The OS keychain accepted the new content key but then had no entry for it, \
                 so content capture stays off.",
            ),
            other => self.failure(other),
        })
    }

    fn failure(&self, failure: Failure) -> KeychainUnavailable {
        match failure {
            Failure::Store(err) => self.unavailable(err),
            Failure::Malformed => malformed(),
        }
    }

    fn unavailable(&self, err: KeyringError) -> KeychainUnavailable {
        match (self.explain)(&err) {
            Some(reason) => KeychainUnavailable::new(reason),
            None => KeychainUnavailable::new(format!(
                "The OS keychain could not provide the content key ({}), so content capture \
                 stays off.",
                describe(err)
            )),
        }
    }
}

impl SecretStore for KeyringSecretStore {
    fn content_key(&self, if_missing: IfMissing) -> Result<[u8; KEY_LEN], KeychainUnavailable> {
        let entry = self.entry().map_err(|err| self.unavailable(err))?;
        match read_key(&entry) {
            Err(Failure::Store(KeyringError::NoEntry)) => {}
            other => return other.map_err(|failure| self.failure(failure)),
        }
        if if_missing == IfMissing::Fail {
            return Err(KeychainUnavailable::new(KEY_MISSING));
        }
        // The guard protects no data, so a panic elsewhere can't leave it inconsistent.
        let _guard = GENERATING.lock().unwrap_or_else(PoisonError::into_inner);
        self.generate(&entry)
    }
}

impl fmt::Debug for KeyringSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringSecretStore").finish_non_exhaustive()
    }
}

/// Why reading the stored key failed.
enum Failure {
    Store(KeyringError),
    /// An entry exists but doesn't hold 64 hex characters.
    Malformed,
}

fn read_key(entry: &Entry) -> Result<[u8; KEY_LEN], Failure> {
    let mut stored = entry.get_password().map_err(Failure::Store)?;
    let decoded = decode_key(stored.as_bytes());
    wipe_string(&mut stored);
    decoded.ok_or(Failure::Malformed)
}

fn decode_key(hex_key: &[u8]) -> Option<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    match hex::decode_to_slice(hex_key, &mut key) {
        Ok(()) => Some(key),
        Err(_) => {
            wipe(&mut key);
            None
        }
    }
}

fn store_key(entry: &Entry, key: &[u8; KEY_LEN]) -> keyring_core::Result<()> {
    let mut hex_key = hex::encode(key);
    let result = entry.set_password(&hex_key);
    wipe_string(&mut hex_key);
    result
}

fn generate_key() -> Result<[u8; KEY_LEN], KeychainUnavailable> {
    let mut key = [0u8; KEY_LEN];
    getrandom::fill(&mut key).map_err(|err| {
        KeychainUnavailable::new(format!(
            "The system random number generator failed ({err}), so no content key was \
             created and content capture stays off."
        ))
    })?;
    Ok(key)
}

fn malformed() -> KeychainUnavailable {
    KeychainUnavailable::new(format!(
        "The OS keychain entry \"{USER}\" of service \"{SERVICE}\" does not hold a \
         {HEX_LEN}-character hex content key. It was left unchanged, because replacing it \
         would make stored content unreadable, so content capture stays off."
    ))
}

/// The store's own description of `err`, with any secret bytes it carries wiped.
fn describe(err: KeyringError) -> String {
    match err {
        // Both carry what the keychain returned, which may be (part of) the key.
        KeyringError::BadEncoding(mut bytes) => {
            wipe(&mut bytes);
            "the stored content key is not valid text".to_owned()
        }
        KeyringError::BadDataFormat(mut bytes, cause) => {
            wipe(&mut bytes);
            format!("the stored content key is malformed: {cause}")
        }
        // The store's text lists every matching item's internals.
        KeyringError::Ambiguous(entries) => format!(
            "{} keychain items match service \"{SERVICE}\" and user \"{USER}\"; keep only \
             the one that holds your content key",
            entries.len()
        ),
        other => other.to_string(),
    }
}

fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
}

fn wipe_string(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    wipe(&mut bytes);
}

/// A fixed content key, or a fixed failure, for tests of other units.
pub struct InMemorySecretStore {
    key: Result<[u8; KEY_LEN], KeychainUnavailable>,
}

impl InMemorySecretStore {
    /// Always returns `key`.
    pub fn new(key: [u8; KEY_LEN]) -> Self {
        Self { key: Ok(key) }
    }

    /// Always fails with `reason`.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            key: Err(KeychainUnavailable::new(reason)),
        }
    }
}

impl SecretStore for InMemorySecretStore {
    fn content_key(&self, _if_missing: IfMissing) -> Result<[u8; KEY_LEN], KeychainUnavailable> {
        self.key.clone()
    }
}

impl Drop for InMemorySecretStore {
    fn drop(&mut self) {
        if let Ok(key) = &mut self.key {
            wipe(key);
        }
    }
}

impl fmt::Debug for InMemorySecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemorySecretStore")
            .field("available", &self.key.is_ok())
            .finish_non_exhaustive()
    }
}

/// Recognizes "nobody answers for the Secret Service" in a zbus store error.
///
/// secret-service 5.2.0 maps a missing session bus to `Error::Unavailable`
/// only for some zbus errors (`util::handle_conn_error`); zbus 5.19 reports a
/// missing bus socket as `Error::Connection` wrapping an I/O error instead, and a
/// running bus without a provider answers `OpenSession` with the D-Bus error
/// `ServiceUnknown`. A provider without a `default` collection, as on WSL
/// (zbus-secret-service-keyring-store 1.0.1 crate docs, "Usage on Windows
/// Subsystem for Linux"), fails with `Error::NoResult`, which secret-service only
/// returns when a collection alias resolves to nothing; that one gets its own
/// reason, since a provider did answer. Neither crate is a direct dependency, so
/// the source chain is inspected through `std::error::Error`, matching message
/// texts checked against secret-service 5.2.0 and zbus 5.19: re-check them when
/// either is upgraded.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
mod secret_service {
    use std::error::Error as StdError;
    use std::io;
    use std::sync::Arc;

    use keyring_core::Error as KeyringError;

    /// Message of `secret_service::Error::Unavailable` in secret-service 5.2.0
    /// (`src/error.rs`).
    const UNAVAILABLE: &str = "no secret service provider or dbus session found";
    /// Message of `secret_service::Error::NoResult` in secret-service 5.2.0.
    const NO_RESULT: &str = "SS error: result not returned from SS API";
    /// D-Bus errors meaning no process owns or can be started for the bus name.
    const NO_OWNER: [&str; 2] = [
        "org.freedesktop.DBus.Error.ServiceUnknown",
        "org.freedesktop.DBus.Error.NameHasNoOwner",
    ];

    pub(super) fn is_unreachable(err: &KeyringError) -> bool {
        causes(err).any(means_unreachable)
    }

    /// A provider answered but has no `default` collection.
    pub(super) fn has_no_default_collection(err: &KeyringError) -> bool {
        causes(err).any(|cause| cause.to_string().contains(NO_RESULT))
    }

    /// The platform error in `err` and its sources. Only these two variants carry
    /// one; the variants that carry stored bytes are never inspected.
    fn causes(err: &KeyringError) -> impl Iterator<Item = &(dyn StdError + 'static)> {
        let platform: Option<&(dyn StdError + 'static)> = match err {
            KeyringError::PlatformFailure(cause) | KeyringError::NoStorageAccess(cause) => {
                Some(cause.as_ref())
            }
            _ => None,
        };
        std::iter::successors(platform, |&cause| cause.source())
    }

    fn means_unreachable(cause: &(dyn StdError + 'static)) -> bool {
        // No bus socket, or nothing listening on it.
        if matches!(
            io_error_kind(cause),
            Some(io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
        ) {
            return true;
        }
        let message = cause.to_string();
        std::iter::once(&UNAVAILABLE)
            .chain(NO_OWNER.iter())
            .any(|marker| message.contains(marker))
    }

    fn io_error_kind(cause: &(dyn StdError + 'static)) -> Option<io::ErrorKind> {
        cause
            .downcast_ref::<io::Error>()
            .or_else(|| cause.downcast_ref::<Arc<io::Error>>().map(AsRef::as_ref))
            .map(io::Error::kind)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::sync::Arc;

    use keyring_core::{CredentialStore, Error as KeyringError};

    /// Connects to the session bus and opens a Secret Service session; blocks.
    pub(super) fn open() -> keyring_core::Result<Arc<CredentialStore>> {
        let store: Arc<CredentialStore> = zbus_secret_service_keyring_store::Store::new()?;
        Ok(store)
    }

    pub(super) fn explain(err: &KeyringError) -> Option<&'static str> {
        use super::secret_service::{has_no_default_collection, is_unreachable};
        if is_unreachable(err) {
            Some(super::NO_SECRET_SERVICE)
        } else if has_no_default_collection(err) {
            Some(super::NO_DEFAULT_KEYRING)
        } else {
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::Arc;

    use keyring_core::{CredentialStore, Error as KeyringError};

    /// The user's login keychain.
    pub(super) fn open() -> keyring_core::Result<Arc<CredentialStore>> {
        let store: Arc<CredentialStore> = apple_native_keyring_store::keychain::Store::new()?;
        Ok(store)
    }

    pub(super) fn explain(_err: &KeyringError) -> Option<&'static str> {
        None
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::Arc;

    use keyring_core::{CredentialStore, Error as KeyringError};

    /// The user's Windows Credential Manager.
    pub(super) fn open() -> keyring_core::Result<Arc<CredentialStore>> {
        let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
        Ok(store)
    }

    pub(super) fn explain(_err: &KeyringError) -> Option<&'static str> {
        None
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::sync::Arc;

    use keyring_core::{CredentialStore, Error as KeyringError};

    pub(super) fn open() -> keyring_core::Result<Arc<CredentialStore>> {
        Err(KeyringError::NotSupportedByStore(
            "Callsheet has no OS keychain for this platform".to_owned(),
        ))
    }

    pub(super) fn explain(_err: &KeyringError) -> Option<&'static str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring_core::mock;
    use std::io;

    fn mock_store() -> Arc<mock::Store> {
        mock::Store::new().unwrap()
    }

    fn secret_store(store: &Arc<mock::Store>) -> KeyringSecretStore {
        KeyringSecretStore::new(store.clone())
    }

    fn entry(store: &Arc<mock::Store>) -> Entry {
        keyring_core::api::CredentialStoreApi::build(store.as_ref(), SERVICE, USER, None).unwrap()
    }

    fn fail_next_call(store: &Arc<mock::Store>, err: KeyringError) {
        let entry = entry(store);
        let cred: &mock::Cred = entry.as_any().downcast_ref().unwrap();
        cred.set_error(err);
    }

    fn platform_error(err: impl StdError + Send + Sync + 'static) -> KeyringError {
        KeyringError::PlatformFailure(Box::new(err))
    }

    #[test]
    fn first_call_generates_and_stores_a_hex_key() {
        let store = mock_store();

        let key = secret_store(&store)
            .content_key(IfMissing::Generate)
            .unwrap();

        assert_ne!(key, [0u8; KEY_LEN]);
        assert_eq!(entry(&store).get_password().unwrap(), hex::encode(key));
    }

    #[test]
    fn later_calls_return_the_same_key() {
        let store = mock_store();
        let secrets = secret_store(&store);

        let first = secrets.content_key(IfMissing::Generate).unwrap();

        assert_eq!(secrets.content_key(IfMissing::Generate).unwrap(), first);
        assert_eq!(
            secret_store(&store)
                .content_key(IfMissing::Generate)
                .unwrap(),
            first
        );
    }

    #[test]
    fn existing_key_is_returned_unchanged() {
        let store = mock_store();
        let existing = [0xa5u8; KEY_LEN];
        entry(&store).set_password(&hex::encode(existing)).unwrap();

        assert_eq!(
            secret_store(&store)
                .content_key(IfMissing::Generate)
                .unwrap(),
            existing
        );
        assert_eq!(entry(&store).get_password().unwrap(), hex::encode(existing));
    }

    #[test]
    fn existing_uppercase_hex_key_is_accepted() {
        let store = mock_store();
        let existing = [0xabu8; KEY_LEN];
        entry(&store)
            .set_password(&hex::encode_upper(existing))
            .unwrap();

        assert_eq!(
            secret_store(&store)
                .content_key(IfMissing::Generate)
                .unwrap(),
            existing
        );
    }

    #[test]
    fn malformed_stored_keys_are_reported_and_left_in_place() {
        let short = "ab".repeat(KEY_LEN - 1);
        let long = "ab".repeat(KEY_LEN + 1);
        let not_hex = format!("zz{}", "ab".repeat(KEY_LEN - 1));
        for stored in ["", short.as_str(), long.as_str(), not_hex.as_str()] {
            let store = mock_store();
            entry(&store).set_password(stored).unwrap();

            let err = secret_store(&store)
                .content_key(IfMissing::Generate)
                .unwrap_err();

            assert!(err.reason().contains("left unchanged"), "{err}");
            assert_eq!(err.code(), cs_core::rpc::codes::KEYCHAIN_UNAVAILABLE);
            assert_eq!(entry(&store).get_password().unwrap(), stored);
        }
    }

    #[test]
    fn non_text_stored_secret_is_reported_and_left_in_place() {
        let store = mock_store();
        let raw = [0xffu8; KEY_LEN];
        entry(&store).set_secret(&raw).unwrap();

        let err = secret_store(&store)
            .content_key(IfMissing::Generate)
            .unwrap_err();

        assert!(err.reason().contains("not valid text"), "{err}");
        assert_eq!(entry(&store).get_secret().unwrap(), raw);
    }

    #[test]
    fn reasons_never_contain_the_stored_value() {
        let store = mock_store();
        let almost = "c0ffee".repeat(10);
        entry(&store).set_password(&almost).unwrap();

        let err = secret_store(&store)
            .content_key(IfMissing::Generate)
            .unwrap_err();

        assert!(!err.reason().contains("c0ffee"), "{err}");
    }

    #[test]
    fn read_errors_keep_the_store_reason() {
        let store = mock_store();
        fail_next_call(
            &store,
            KeyringError::NoStorageAccess(Box::new(io::Error::other("keychain is locked"))),
        );

        let err = secret_store(&store)
            .content_key(IfMissing::Generate)
            .unwrap_err();

        assert!(err.reason().contains("keychain is locked"), "{err}");
        assert!(err.reason().contains("content capture stays off"), "{err}");
        assert_eq!(err.code(), cs_core::rpc::codes::KEYCHAIN_UNAVAILABLE);
    }

    #[test]
    fn write_errors_keep_the_store_reason() {
        // The mock fails only its next call, which would be the read, so a
        // store that refuses writes stands in for it here.
        let secrets = KeyringSecretStore::new(Arc::new(RefusesWrites));

        let err = secrets.content_key(IfMissing::Generate).unwrap_err();

        assert!(err.reason().contains("write refused"), "{err}");
        assert_eq!(err.code(), cs_core::rpc::codes::KEYCHAIN_UNAVAILABLE);
    }

    #[test]
    fn failing_to_open_the_store_is_unavailable() {
        let secrets = KeyringSecretStore::opening(
            || Err(platform_error(io::Error::other("no keychain daemon"))),
            |_| None,
        );

        let err = secrets.content_key(IfMissing::Generate).unwrap_err();

        assert!(err.reason().contains("no keychain daemon"), "{err}");
    }

    #[test]
    fn ambiguous_reason_omits_the_matching_items() {
        let store = mock_store();
        let other = mock_store();
        let err = describe(KeyringError::Ambiguous(vec![entry(&store), entry(&other)]));

        assert!(err.starts_with("2 keychain items match"), "{err}");
        assert!(!err.contains("Cred"), "{err}");
    }

    #[test]
    fn bad_data_format_reason_omits_the_bytes() {
        let err = describe(KeyringError::BadDataFormat(
            b"secret-bytes".to_vec(),
            Box::new(io::Error::other("bad padding")),
        ));

        assert!(err.contains("bad padding"), "{err}");
        assert!(!err.contains("secret"), "{err}");
    }

    /// Errors the zbus store gives when no Secret Service answers.
    fn secret_service_unreachable() -> Vec<KeyringError> {
        vec![
            platform_error(io::Error::from(io::ErrorKind::NotFound)),
            platform_error(io::Error::from(io::ErrorKind::ConnectionRefused)),
            platform_error(Wrapped(Arc::new(io::Error::from(io::ErrorKind::NotFound)))),
            platform_error(io::Error::other(
                "no secret service provider or dbus session found",
            )),
            platform_error(io::Error::other(
                "org.freedesktop.DBus.Error.ServiceUnknown: The name org.freedesktop.secrets \
                 was not provided by any .service files",
            )),
            KeyringError::NoStorageAccess(Box::new(io::Error::other(
                "org.freedesktop.DBus.Error.NameHasNoOwner: no owner",
            ))),
        ]
    }

    fn no_default_collection() -> KeyringError {
        KeyringError::NoStorageAccess(Box::new(io::Error::other(
            "SS error: result not returned from SS API",
        )))
    }

    #[test]
    fn a_provider_without_a_default_collection_is_not_unreachable() {
        let err = no_default_collection();
        assert!(!secret_service::is_unreachable(&err));
        assert!(secret_service::has_no_default_collection(&err));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn os_keychain_reports_a_missing_default_keyring() {
        let secrets =
            KeyringSecretStore::opening(|| Err(no_default_collection()), platform::explain);

        let err = secrets.content_key(IfMissing::Generate).unwrap_err();

        assert_eq!(err.reason(), NO_DEFAULT_KEYRING);
    }

    #[test]
    fn a_missing_key_is_not_replaced_unless_the_caller_allows_it() {
        let store = mock_store();

        let err = secret_store(&store)
            .content_key(IfMissing::Fail)
            .unwrap_err();

        assert_eq!(err.reason(), KEY_MISSING);
        assert_eq!(err.code(), cs_core::rpc::codes::KEYCHAIN_UNAVAILABLE);
        assert!(matches!(
            entry(&store).get_password(),
            Err(KeyringError::NoEntry)
        ));
    }

    #[test]
    fn an_existing_key_is_returned_whatever_the_caller_allows() {
        let store = mock_store();
        let existing = [0x5au8; KEY_LEN];
        entry(&store).set_password(&hex::encode(existing)).unwrap();

        assert_eq!(
            secret_store(&store).content_key(IfMissing::Fail).unwrap(),
            existing
        );
    }

    #[test]
    fn secret_service_unreachable_errors_are_recognized() {
        for err in secret_service_unreachable() {
            assert!(secret_service::is_unreachable(&err), "{err}");
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn os_keychain_reports_the_design_message_when_no_secret_service_answers() {
        for index in 0..secret_service_unreachable().len() {
            // Wired like os_keychain(), with an opener that fails the way zbus does.
            let secrets = KeyringSecretStore::opening(
                move || Err(secret_service_unreachable().swap_remove(index)),
                platform::explain,
            );

            let err = secrets.content_key(IfMissing::Generate).unwrap_err();

            assert_eq!(err.reason(), NO_SECRET_SERVICE, "case {index}");
            assert_eq!(err.code(), cs_core::rpc::codes::KEYCHAIN_UNAVAILABLE);
        }
    }

    #[test]
    fn other_secret_service_errors_keep_their_reason() {
        let reachable = [
            platform_error(io::Error::from(io::ErrorKind::PermissionDenied)),
            platform_error(io::Error::other(
                "org.freedesktop.DBus.Error.AccessDenied: not allowed",
            )),
            KeyringError::NoStorageAccess(Box::new(io::Error::other("SS Error: object locked"))),
            KeyringError::NoEntry,
        ];
        for err in reachable {
            assert!(!secret_service::is_unreachable(&err), "{err}");
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn os_keychain_keeps_other_secret_service_reasons() {
        let secrets = KeyringSecretStore::opening(
            || {
                Err(platform_error(io::Error::other(
                    "org.freedesktop.DBus.Error.AccessDenied: not allowed",
                )))
            },
            platform::explain,
        );

        let err = secrets.content_key(IfMissing::Generate).unwrap_err();

        assert!(err.reason().contains("AccessDenied: not allowed"), "{err}");
        assert!(err.reason().contains("content capture stays off"), "{err}");
    }

    #[test]
    fn in_memory_store_returns_its_key() {
        let key = [7u8; KEY_LEN];
        let secrets = InMemorySecretStore::new(key);

        assert_eq!(secrets.content_key(IfMissing::Generate).unwrap(), key);
        assert_eq!(secrets.content_key(IfMissing::Generate).unwrap(), key);
        assert!(!format!("{secrets:?}").contains('7'));
    }

    #[test]
    fn unavailable_in_memory_store_always_fails() {
        let secrets = InMemorySecretStore::unavailable("no keychain in this test");

        for _ in 0..2 {
            let err = secrets.content_key(IfMissing::Generate).unwrap_err();
            assert_eq!(err.reason(), "no keychain in this test");
            assert_eq!(err.to_string(), "no keychain in this test");
            assert_eq!(err.code(), cs_core::rpc::codes::KEYCHAIN_UNAVAILABLE);
        }
    }

    #[test]
    fn stores_are_shareable_trait_objects() {
        let stores: Vec<Arc<dyn SecretStore>> = vec![
            Arc::new(InMemorySecretStore::new([1; KEY_LEN])),
            Arc::new(secret_store(&mock_store())),
            Arc::new(os_keychain()),
        ];
        assert_eq!(stores.len(), 3);
    }

    /// A store with no entry that refuses every write.
    #[derive(Debug)]
    struct RefusesWrites;

    impl keyring_core::api::CredentialStoreApi for RefusesWrites {
        fn vendor(&self) -> String {
            "test".to_owned()
        }

        fn id(&self) -> String {
            "refuses-writes".to_owned()
        }

        fn build(
            &self,
            _service: &str,
            _user: &str,
            _modifiers: Option<&std::collections::HashMap<&str, &str>>,
        ) -> keyring_core::Result<Entry> {
            Ok(Entry::new_with_credential(Arc::new(RefusesWrites)))
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    impl keyring_core::api::CredentialApi for RefusesWrites {
        fn set_secret(&self, _secret: &[u8]) -> keyring_core::Result<()> {
            Err(KeyringError::NoStorageAccess(Box::new(io::Error::other(
                "write refused",
            ))))
        }

        fn get_secret(&self) -> keyring_core::Result<Vec<u8>> {
            Err(KeyringError::NoEntry)
        }

        fn delete_credential(&self) -> keyring_core::Result<()> {
            Err(KeyringError::NoEntry)
        }

        fn get_credential(&self) -> keyring_core::Result<Option<Arc<keyring_core::Credential>>> {
            Err(KeyringError::NoEntry)
        }

        fn get_specifiers(&self) -> Option<(String, String)> {
            None
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// An error whose cause is an `Arc<io::Error>`, as zbus's `Error::Connection` has.
    #[derive(Debug)]
    struct Wrapped(Arc<io::Error>);

    impl fmt::Display for Wrapped {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("failed to connect to address")
        }
    }

    impl StdError for Wrapped {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.0)
        }
    }
}
