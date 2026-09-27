//! Data directory, single instance and discovery (R1.4, R1.5, R2.7).
//!
//! Owned by unit `daemon-proc`.
//!
//! **Data directory.** [`prepare_data_dir`] creates it owner-only with
//! `cs_store::fsperm`. Everything secret lives in it (the token, the lock, the
//! database), and on Windows SQLite's `-wal`/`-shm` files get their ACL from it
//! (`cs_store::db` module docs), so its permissions matter:
//! - Unix: the directory must have no group or other permission bits and be
//!   owned by the daemon's effective uid (`rustix::process::geteuid`), which
//!   owns every file the daemon creates, or the daemon refuses to start. The
//!   mode is checked first, then the owner; neither check creates anything.
//! - Windows: the directory must be owned by the current user and have a
//!   protected DACL whose allow ACEs are all for that user
//!   (`cs_store::fsperm::owner_only_problem`), or the daemon refuses to start.
//!
//! A directory that fails is never repaired (chmod-ed, or given a new DACL):
//! - Callsheet itself only ever creates it through `fsperm`, so a failing
//!   directory was made or changed by someone else, and whatever it holds may
//!   already carry other users' ACEs or owners, which re-applying the parent's
//!   permissions doesn't remove;
//! - Windows checks access when a handle is opened and a handle keeps the
//!   access it was granted (Microsoft Learn, "Requesting Access Rights to an
//!   Object"; MS-FSA 2.1.5.1.2.1), so another account's open handle outlives a
//!   new DACL;
//! - refusing is what Unix does too. The message names the directory and the fix.
//!
//! **Single instance.** `daemon.lock` is created owner-only and held with
//! `std::fs::File::try_lock` (an exclusive `flock` on Unix, `LockFileEx` over the
//! whole file on Windows; Rust 1.98.1 `library/std/src/sys/fs/{unix,windows}.rs`)
//! for the life of the process. The holder writes its pid into it. The lock file is
//! never deleted: deleting it would let two daemons lock two different files.
//!
//! **Discovery.** Once the listeners are bound, `daemon.json`
//! (`{ "pid", "startedAtMs", "address", "schemaVersion", "proxyAddress" }`, the
//! last one absent when no proxy runs) is written owner-only
//! and atomically, and it is removed on graceful shutdown. A crash leaves it
//! behind; the next daemon removes it right after taking the lock.
//!
//! Clients use [`read_discovery`]. It returns the record only when:
//! - `daemon.lock` is exclusively locked by someone else. The probe is a *shared*
//!   `try_lock_shared`: probing clients never block each other, so a client can't
//!   mistake another client's probe for a live daemon, while the daemon's
//!   exclusive lock makes every probe fail;
//! - `daemon.json` exists and its address is `127.0.0.1` with a non-zero port;
//! - on Unix, the pid written into `daemon.lock` equals the one in `daemon.json`.
//!
//! **Residual gaps.** This makes a stale `daemon.json` unlikely to receive the
//! token, not impossible:
//! - Unix: between a new daemon's `try_lock` and its writing its own pid into
//!   `daemon.lock`, the lock file still holds the crashed daemon's pid, which
//!   matches the old `daemon.json`. A client probing in that window (two system
//!   calls long) gets the old address.
//! - Windows: an exclusive `LockFileEx` lock "denies all other processes both read
//!   and write access" to the file (Microsoft Learn, `LockFileEx`, Remarks), so
//!   clients can't read the pid in `daemon.lock` and only the lock is checked.
//!   The window runs from the new daemon's lock to its removal of the old
//!   `daemon.json`. Also, Windows may keep a killed process's lock "depending upon
//!   available system resources" (same page) for a moment after the process is
//!   gone, and during that time the old `daemon.json` passes the check.
//! - Follow-up: checking that the pid is alive (and is a `cs-daemon`) would close
//!   both, but needs `libc` / Win32 calls with `unsafe`, which is denied here.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cs_store::fsperm;

/// Held by the running daemon.
pub const LOCK_FILE_NAME: &str = "daemon.lock";

/// Where clients find the running daemon.
pub const DISCOVERY_FILE_NAME: &str = "daemon.json";

/// How often, and how long apart, [`Instance::acquire`] retries a held lock
/// before giving up: [`read_discovery`] takes a shared lock for a moment to see if
/// the file is locked, and a daemon starting at that moment must not mistake the
/// client for another daemon.
const LOCK_ATTEMPTS: u32 = 25;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

/// The contents of `daemon.json`, shared with clients through `cs-core` and its
/// generated TypeScript type (ADR 0009).
pub use cs_core::control::Discovery;

/// Why the data directory or the instance lock can't be used. The messages name
/// the path involved and never a file's contents.
#[derive(Debug)]
pub enum InstanceError {
    /// Creating the data directory failed.
    DataDir { path: PathBuf, source: io::Error },
    /// The data directory exists, but reading its path, metadata or (Windows)
    /// security descriptor to check it failed.
    DataDirCheck { path: PathBuf, source: io::Error },
    /// The data directory path exists and is not a directory.
    NotADirectory { path: PathBuf },
    /// Unix: the data directory has group or other permission bits.
    DataDirNotOwnerOnly { path: PathBuf, mode: u32 },
    /// Unix: the data directory is owned by `owner`, not by `current`, the
    /// daemon's effective uid.
    DataDirNotOwned {
        path: PathBuf,
        owner: u32,
        current: u32,
    },
    /// Windows: the data directory's owner or DACL is not owner-only.
    DataDirAclNotOwnerOnly {
        path: PathBuf,
        problem: fsperm::AclProblem,
    },
    /// Opening, locking or writing `daemon.lock` failed.
    Lock { path: PathBuf, source: io::Error },
    /// Another daemon holds the lock (R1.5). `pid` is from `daemon.json`, or on
    /// Unix from `daemon.lock`, when either could be read.
    AlreadyRunning { data_dir: PathBuf, pid: Option<u32> },
    /// Writing or removing `daemon.json` failed.
    Discovery { path: PathBuf, source: io::Error },
}

impl fmt::Display for InstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DataDir { path, source } => {
                write!(
                    f,
                    "cannot create data directory {}: {source}",
                    path.display()
                )
            }
            Self::DataDirCheck { path, source } => {
                write!(
                    f,
                    "cannot check data directory {}: {source}",
                    path.display()
                )
            }
            Self::NotADirectory { path } => {
                write!(f, "data directory {} is not a directory", path.display())
            }
            Self::DataDirNotOwnerOnly { path, mode } => write!(
                f,
                "data directory {} has mode {mode:04o}, but it must not be accessible to \
                 group or others; run `chmod 700` on it or pass another --data-dir",
                path.display()
            ),
            Self::DataDirNotOwned {
                path,
                owner,
                current,
            } => write!(
                f,
                "data directory {} is owned by uid {owner}, but Callsheet runs as uid {current}; \
                 pass another --data-dir",
                path.display()
            ),
            Self::DataDirAclNotOwnerOnly { path, problem } => write!(
                f,
                "data directory {} {problem}, but only you may have access to it; move it aside \
                 (its database and token stay in the old location) so Callsheet re-creates it \
                 owner-only, or pass another --data-dir",
                path.display()
            ),
            Self::Lock { path, source } => write!(f, "cannot lock {}: {source}", path.display()),
            Self::AlreadyRunning { data_dir, pid } => {
                let pid = pid.map_or_else(|| "unknown pid".to_owned(), |pid| format!("pid {pid}"));
                write!(
                    f,
                    "another Callsheet daemon ({pid}) is already running with data directory {}",
                    data_dir.display()
                )
            }
            Self::Discovery { path, source } => {
                write!(f, "cannot write {}: {source}", path.display())
            }
        }
    }
}

/// The message already includes the underlying error.
impl std::error::Error for InstanceError {}

/// Creates the data directory owner-only if it is missing, checks it (see the
/// module docs), and returns the path to use from then on. On Unix that is the
/// path resolved once with `fs::canonicalize`, so a symlink swapped in after the
/// check can't redirect later file operations; on Windows it is `path` as given.
pub fn prepare_data_dir(path: &Path) -> Result<PathBuf, InstanceError> {
    prepare_data_dir_for(path, &Account::current())
}

/// [`prepare_data_dir`], with the account the directory must belong to given,
/// so tests can check a refusal without another account to own the directory.
fn prepare_data_dir_for(path: &Path, account: &Account) -> Result<PathBuf, InstanceError> {
    if let Err(source) = fsperm::create_dir_all_owner_only(path) {
        // Creating fails with "already exists" when the path is a file.
        return Err(match fs::metadata(path) {
            Ok(metadata) if !metadata.is_dir() => InstanceError::NotADirectory {
                path: path.to_path_buf(),
            },
            _ => InstanceError::DataDir {
                path: path.to_path_buf(),
                source,
            },
        });
    }
    let check_error = |source| InstanceError::DataDirCheck {
        path: path.to_path_buf(),
        source,
    };
    let resolved = resolve(path).map_err(check_error)?;
    let metadata = fs::metadata(&resolved).map_err(check_error)?;
    if !metadata.is_dir() {
        return Err(InstanceError::NotADirectory {
            path: path.to_path_buf(),
        });
    }
    check_owner_only(&resolved, &metadata, account)?;
    Ok(resolved)
}

/// The account the data directory must belong to.
struct Account {
    /// Unix: the effective uid. "The owner (user ID) of the new file is set to
    /// the effective user ID of the process" (Linux man-pages, open(2),
    /// `O_CREAT`), so it owns every file the daemon creates. On Windows,
    /// `fsperm` compares against the current user's SID itself.
    #[cfg(unix)]
    uid: u32,
}

impl Account {
    fn current() -> Self {
        Self {
            #[cfg(unix)]
            uid: rustix::process::geteuid().as_raw(),
        }
    }
}

#[cfg(unix)]
fn resolve(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

/// `fs::canonicalize` on Windows returns a `\\?\` path; `fsperm` already makes
/// paths absolute and verbatim itself.
#[cfg(not(unix))]
fn resolve(path: &Path) -> io::Result<PathBuf> {
    Ok(path.to_path_buf())
}

/// The mode first, then the owner, from `metadata` alone.
#[cfg(unix)]
fn check_owner_only(
    path: &Path,
    metadata: &fs::Metadata,
    account: &Account,
) -> Result<(), InstanceError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(InstanceError::DataDirNotOwnerOnly {
            path: path.to_path_buf(),
            mode,
        });
    }
    if metadata.uid() != account.uid {
        return Err(InstanceError::DataDirNotOwned {
            path: path.to_path_buf(),
            owner: metadata.uid(),
            current: account.uid,
        });
    }
    Ok(())
}

#[cfg(windows)]
fn check_owner_only(
    path: &Path,
    _metadata: &fs::Metadata,
    _account: &Account,
) -> Result<(), InstanceError> {
    match fsperm::owner_only_problem(path) {
        Ok(None) => Ok(()),
        Ok(Some(problem)) => Err(InstanceError::DataDirAclNotOwnerOnly {
            path: path.to_path_buf(),
            problem,
        }),
        Err(source) => Err(InstanceError::DataDirCheck {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// `fsperm` can't create the directory on other platforms, so this is never
/// reached.
#[cfg(not(any(unix, windows)))]
fn check_owner_only(
    _path: &Path,
    _metadata: &fs::Metadata,
    _account: &Account,
) -> Result<(), InstanceError> {
    Ok(())
}

/// The running daemon's hold on its data directory: the locked `daemon.lock` and,
/// once [`published`](Self::publish), `daemon.json`.
#[derive(Debug)]
pub struct Instance {
    data_dir: PathBuf,
    lock: File,
    started_at_ms: u64,
}

impl Instance {
    /// Takes `daemon.lock` in `data_dir` (which must exist, see
    /// [`prepare_data_dir`]), writes this process's pid into it and removes a
    /// `daemon.json` left by a daemon that crashed.
    pub fn acquire(data_dir: &Path) -> Result<Self, InstanceError> {
        let path = data_dir.join(LOCK_FILE_NAME);
        let lock_error = |source| InstanceError::Lock {
            path: path.clone(),
            source,
        };
        let lock = open_lock_file(&path).map_err(lock_error)?;
        let mut attempt = 1;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if attempt < LOCK_ATTEMPTS => {
                    attempt += 1;
                    std::thread::sleep(LOCK_RETRY_DELAY);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(InstanceError::AlreadyRunning {
                        data_dir: data_dir.to_path_buf(),
                        pid: running_pid(data_dir),
                    });
                }
                Err(TryLockError::Error(source)) => return Err(lock_error(source)),
            }
        }
        write_pid(&lock).map_err(lock_error)?;

        let discovery = data_dir.join(DISCOVERY_FILE_NAME);
        remove_if_present(&discovery).map_err(|source| InstanceError::Discovery {
            path: discovery,
            source,
        })?;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            lock,
            started_at_ms: now_ms(),
        })
    }

    /// When this daemon started, in Unix milliseconds: `startedAtMs` in
    /// `daemon.json`.
    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    /// Writes `daemon.json` for a control API bound to `address` and, when it
    /// runs, a proxy bound to `proxy_address`, owner-only and atomically,
    /// replacing an earlier one.
    pub fn publish(
        &self,
        address: SocketAddrV4,
        proxy_address: Option<SocketAddrV4>,
    ) -> Result<Discovery, InstanceError> {
        let discovery = Discovery {
            pid: std::process::id(),
            started_at_ms: self.started_at_ms,
            address,
            schema_version: cs_store::migrate::CURRENT_SCHEMA_VERSION,
            proxy_address,
        };
        let path = self.data_dir.join(DISCOVERY_FILE_NAME);
        let discovery_error = |source| InstanceError::Discovery {
            path: path.clone(),
            source,
        };
        let json = serde_json::to_vec(&discovery).map_err(|e| discovery_error(e.into()))?;
        fsperm::write_owner_only_atomic(&path, &json).map_err(discovery_error)?;
        Ok(discovery)
    }

    /// Graceful shutdown: removes `daemon.json`, then releases the lock. The lock
    /// is released even when the removal fails.
    pub fn close(self) -> Result<(), InstanceError> {
        let path = self.data_dir.join(DISCOVERY_FILE_NAME);
        let removed = remove_if_present(&path);
        let unlocked = self.lock.unlock();
        removed.map_err(|source| InstanceError::Discovery { path, source })?;
        unlocked.map_err(|source| InstanceError::Lock {
            path: self.data_dir.join(LOCK_FILE_NAME),
            source,
        })
    }
}

/// Unlocks explicitly rather than leaving it to closing the file: Windows unlocks
/// on close too, but "the time it takes ... depends upon available system
/// resources" (Microsoft Learn, `LockFileEx`, Remarks), and a restarting daemon
/// would find the lock still held. After [`Instance::close`] this unlocks a second
/// time, which fails harmlessly.
impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

/// Opens `daemon.lock` for writing, creating it owner-only if it is missing.
fn open_lock_file(path: &Path) -> io::Result<File> {
    match fsperm::create_new_owner_only(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            OpenOptions::new().write(true).open(path)
        }
        Err(error) => Err(error),
    }
}

fn write_pid(mut lock: &File) -> io::Result<()> {
    lock.set_len(0)?;
    lock.write_all(std::process::id().to_string().as_bytes())
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The pid of the daemon holding the lock, for the "already running" message.
fn running_pid(data_dir: &Path) -> Option<u32> {
    read_discovery_file(data_dir)
        .ok()
        .flatten()
        .map(|discovery| discovery.pid)
        .or_else(|| lock_holder_pid(&data_dir.join(LOCK_FILE_NAME)))
}

/// The pid written into `daemon.lock`. Always `None` on Windows while the lock is
/// held (see the module docs).
fn lock_holder_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_discovery_file(data_dir: &Path) -> io::Result<Option<Discovery>> {
    let bytes = match fs::read(data_dir.join(DISCOVERY_FILE_NAME)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "daemon.json is malformed"))
}

/// For clients: the running daemon's discovery record, or `None` when no daemon
/// is running with `data_dir` (or one is still starting).
///
/// `None` unless `daemon.lock` is exclusively locked by another open file (a live
/// daemon, possibly in this process), `daemon.json` exists and names
/// `127.0.0.1:<non-zero port>`, and on Unix the pid in `daemon.lock` equals the one
/// in `daemon.json`. Only then may a client send the token to the address. See the
/// module docs for the gaps that remain.
///
/// To test the lock this takes a shared lock for a moment when it is free; a
/// daemon starting at that moment retries ([`Instance::acquire`]).
pub fn read_discovery(data_dir: &Path) -> io::Result<Option<Discovery>> {
    let lock_path = data_dir.join(LOCK_FILE_NAME);
    let lock = match File::open(&lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // Shared: another client's probe doesn't block this one (module docs).
    match lock.try_lock_shared() {
        Ok(()) => {
            lock.unlock()?;
            return Ok(None);
        }
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(error)) => return Err(error),
    }
    let Some(discovery) = read_discovery_file(data_dir)? else {
        return Ok(None);
    };
    if *discovery.address.ip() != Ipv4Addr::LOCALHOST || discovery.address.port() == 0 {
        return Ok(None);
    }
    if cfg!(unix) && lock_holder_pid(&lock_path) != Some(discovery.pid) {
        return Ok(None);
    }
    Ok(Some(discovery))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_camel_case_with_the_address_as_a_string() {
        let discovery = Discovery {
            pid: 42,
            started_at_ms: 1_700_000_000_000,
            address: "127.0.0.1:4100".parse().unwrap(),
            schema_version: 1,
            proxy_address: None,
        };

        let json = serde_json::to_value(&discovery).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "pid": 42,
                "startedAtMs": 1_700_000_000_000_u64,
                "address": "127.0.0.1:4100",
                "schemaVersion": 1,
            })
        );
        assert_eq!(
            serde_json::from_value::<Discovery>(json).unwrap(),
            discovery
        );

        let with_proxy = Discovery {
            proxy_address: Some("127.0.0.1:4200".parse().unwrap()),
            ..discovery
        };
        let json = serde_json::to_value(&with_proxy).unwrap();
        assert_eq!(json["proxyAddress"], "127.0.0.1:4200");
        assert_eq!(
            serde_json::from_value::<Discovery>(json).unwrap(),
            with_proxy
        );
    }

    #[test]
    fn publish_writes_the_proxy_address_and_the_start_time() {
        let root = tempfile::tempdir().unwrap();
        let dir = prepare_data_dir(&root.path().join("data")).unwrap();
        let instance = Instance::acquire(&dir).unwrap();
        let proxy = "127.0.0.1:4200".parse().unwrap();

        let published = instance
            .publish("127.0.0.1:4100".parse().unwrap(), Some(proxy))
            .unwrap();

        assert_eq!(published.proxy_address, Some(proxy));
        assert_eq!(published.started_at_ms, instance.started_at_ms());
        let on_disk: Discovery =
            serde_json::from_slice(&fs::read(dir.join(DISCOVERY_FILE_NAME)).unwrap()).unwrap();
        assert_eq!(on_disk, published);
        instance.close().unwrap();
    }

    #[test]
    fn discovery_readers_ignore_unknown_fields() {
        let json =
            r#"{"pid":1,"startedAtMs":2,"address":"127.0.0.1:3","schemaVersion":4,"later":true}"#;

        let discovery: Discovery = serde_json::from_str(json).unwrap();

        assert_eq!(discovery.schema_version, 4);
    }

    #[test]
    fn already_running_names_the_pid_or_says_unknown() {
        let known = InstanceError::AlreadyRunning {
            data_dir: PathBuf::from("d"),
            pid: Some(4242),
        };
        let unknown = InstanceError::AlreadyRunning {
            data_dir: PathBuf::from("d"),
            pid: None,
        };

        assert!(known.to_string().contains("pid 4242"), "{known}");
        assert!(unknown.to_string().contains("unknown pid"), "{unknown}");
    }

    /// An account that doesn't own the directories this process creates.
    #[cfg(unix)]
    fn another_account() -> Account {
        Account {
            uid: Account::current().uid ^ 1,
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_a_directory_of_another_account_is_refused_and_left_alone() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let dir = prepare_data_dir(&root.path().join("data")).unwrap();
        let before = fs::metadata(&dir).unwrap();
        let expected = another_account();

        let error = prepare_data_dir_for(&dir, &expected).unwrap_err();

        match &error {
            InstanceError::DataDirNotOwned { owner, current, .. } => {
                assert_eq!(*owner, before.uid());
                assert_eq!(*current, expected.uid);
            }
            other => panic!("{other}"),
        }
        let message = error.to_string();
        assert!(
            message.contains(&dir.display().to_string())
                && message.contains(&format!("uid {}", before.uid()))
                && message.contains("--data-dir")
                && !message.contains('\n'),
            "{message}"
        );
        // Nothing was created in it or changed.
        let after = fs::metadata(&dir).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(after.uid(), before.uid());
        assert_eq!(after.permissions().mode(), before.permissions().mode());
    }

    #[cfg(unix)]
    #[test]
    fn unix_the_mode_is_checked_before_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = prepare_data_dir(&root.path().join("data")).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).unwrap();

        let error = prepare_data_dir_for(&dir, &another_account()).unwrap_err();

        assert!(
            matches!(
                error,
                InstanceError::DataDirNotOwnerOnly { mode: 0o750, .. }
            ),
            "{error}"
        );
    }

    /// The effective uid is the one that owns what this process creates.
    #[cfg(unix)]
    #[test]
    fn unix_the_current_account_owns_the_files_this_process_creates() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();

        assert_eq!(
            Account::current().uid,
            fs::metadata(root.path()).unwrap().uid()
        );
    }

    #[test]
    fn check_failures_say_check_not_create() {
        let error = InstanceError::DataDirCheck {
            path: PathBuf::from("data"),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };

        assert!(
            error
                .to_string()
                .starts_with("cannot check data directory data: "),
            "{error}"
        );
    }

    #[test]
    fn acl_message_is_one_line_naming_the_directory_and_the_fix() {
        let error = InstanceError::DataDirAclNotOwnerOnly {
            path: PathBuf::from("data"),
            problem: fsperm::AclProblem::OthersAllowed,
        };

        let message = error.to_string();

        assert!(
            message.starts_with("data directory data grants access to other accounts")
                && message
                    .contains("move it aside (its database and token stay in the old location)")
                && message.contains("--data-dir")
                && !message.contains('\n'),
            "{message}"
        );
    }
}
