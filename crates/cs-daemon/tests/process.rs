//! The daemon process's parts, in-process: data directory, lock and discovery,
//! token, and the server (header-read timeout, graceful shutdown, rotation).

mod support;

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cs_daemon::http::{HttpConfig, router};
use cs_daemon::instance::{
    DISCOVERY_FILE_NAME, Instance, InstanceError, LOCK_FILE_NAME, prepare_data_dir, read_discovery,
};
use cs_daemon::serve::{self, ServeConfig, Server};
use cs_daemon::token::{ControlToken, TOKEN_FILE_NAME, TokenError};
use support::{TestHandler, call_version, exchange, post, status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

fn data_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data");
    prepare_data_dir(&dir).unwrap();
    (root, dir)
}

fn token_in(dir: &Path) -> String {
    fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap()
}

async fn start(token: Arc<ControlToken>, config: ServeConfig) -> (SocketAddr, Server) {
    let (addr, server, _) = start_with_handler(token, config).await;
    (addr, server)
}

/// Also returns the notifier of the handler's `slow` method.
async fn start_with_handler(
    token: Arc<ControlToken>,
    config: ServeConfig,
) -> (SocketAddr, Server, Arc<Notify>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handler = TestHandler::default();
    let slow_started = handler.slow_started.clone();
    let app = router(
        HttpConfig::for_listener(&listener).unwrap(),
        token,
        Arc::new(handler),
    );
    (addr, serve::spawn(listener, app, config), slow_started)
}

// ---- data directory -------------------------------------------------------------

#[test]
fn data_dir_is_created_and_reused() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a").join("data");

    prepare_data_dir(&dir).unwrap();
    prepare_data_dir(&dir).unwrap();

    assert!(dir.is_dir());
}

#[test]
fn a_file_is_not_a_data_dir() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("data");
    fs::write(&file, b"").unwrap();

    let error = prepare_data_dir(&file).unwrap_err();

    assert!(
        matches!(error, InstanceError::NotADirectory { .. }),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn unix_data_dir_created_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data");

    prepare_data_dir(&dir).unwrap();

    let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

#[cfg(unix)]
#[test]
fn unix_data_dir_open_to_others_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o755, 0o750, 0o701, 0o770] {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();

        let error = prepare_data_dir(&dir).unwrap_err();

        assert!(
            matches!(error, InstanceError::DataDirNotOwnerOnly { .. }),
            "{mode:o}: {error}"
        );
        assert!(error.to_string().contains("chmod 700"), "{error}");
    }
}

/// Needs root, which can create a directory owned by someone else; skipped
/// otherwise (the uid comparison itself is a unit test in `instance.rs`).
#[cfg(unix)]
#[test]
fn unix_data_dir_owned_by_another_uid_is_refused() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    if fs::metadata(root.path()).unwrap().uid() != 0 {
        eprintln!("skipped: not running as root");
        return;
    }
    let dir = root.path().join("data");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::chown(&dir, Some(65534), Some(65534)).unwrap();

    let error = prepare_data_dir(&dir).unwrap_err();

    assert!(
        matches!(
            error,
            InstanceError::DataDirNotOwned {
                owner: 65534,
                current: 0,
                ..
            }
        ),
        "{error}"
    );
    // Nothing was created in it or changed.
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    assert_eq!(fs::metadata(&dir).unwrap().uid(), 65534);
}

/// The DACL as `icacls` prints it, to show that a refused directory is left as
/// it was.
#[cfg(windows)]
fn icacls(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("icacls")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(windows)]
#[test]
fn windows_data_dir_made_by_std_is_refused_and_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data");
    // Takes the parent's inheritable ACEs, and no protected DACL.
    fs::create_dir(&dir).unwrap();
    let before = icacls(&dir, &[]);

    let error = prepare_data_dir(&dir).unwrap_err();

    assert!(
        matches!(error, InstanceError::DataDirAclNotOwnerOnly { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("--data-dir"), "{error}");
    assert_eq!(icacls(&dir, &[]), before);
}

#[cfg(windows)]
#[test]
fn windows_data_dir_with_an_everyone_ace_is_refused_and_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data");
    prepare_data_dir(&dir).unwrap();
    // Everyone (S-1-1-0) may read; `*` marks a numeric SID (Microsoft Learn,
    // "icacls", Remarks).
    icacls(&dir, &["/grant", "*S-1-1-0:(OI)(CI)(R)"]);
    let before = icacls(&dir, &[]);

    let error = prepare_data_dir(&dir).unwrap_err();

    assert!(
        matches!(
            error,
            InstanceError::DataDirAclNotOwnerOnly {
                problem: cs_store::fsperm::AclProblem::OthersAllowed,
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(icacls(&dir, &[]), before);
}

// ---- lock and discovery ---------------------------------------------------------

#[test]
fn second_instance_is_refused_naming_the_pid() {
    let (_root, dir) = data_dir();
    let first = Instance::acquire(&dir).unwrap();
    first.publish("127.0.0.1:4100".parse().unwrap()).unwrap();

    let error = Instance::acquire(&dir).unwrap_err();

    let pid = std::process::id();
    assert!(
        matches!(error, InstanceError::AlreadyRunning { pid: Some(p), .. } if p == pid),
        "{error}"
    );
    assert!(error.to_string().contains(&format!("pid {pid}")), "{error}");
    first.close().unwrap();
}

#[test]
fn lock_is_released_on_close_and_daemon_json_removed() {
    let (_root, dir) = data_dir();
    let first = Instance::acquire(&dir).unwrap();
    first.publish("127.0.0.1:4100".parse().unwrap()).unwrap();
    assert!(dir.join(DISCOVERY_FILE_NAME).exists());

    first.close().unwrap();

    assert!(!dir.join(DISCOVERY_FILE_NAME).exists());
    assert!(dir.join(LOCK_FILE_NAME).exists());
    Instance::acquire(&dir).unwrap().close().unwrap();
}

#[test]
fn lock_is_released_when_the_instance_is_dropped() {
    let (_root, dir) = data_dir();
    drop(Instance::acquire(&dir).unwrap());

    Instance::acquire(&dir).unwrap().close().unwrap();
}

#[test]
fn read_discovery_returns_the_live_daemon() {
    let (_root, dir) = data_dir();
    let instance = Instance::acquire(&dir).unwrap();
    assert_eq!(read_discovery(&dir).unwrap(), None, "not published yet");

    let published = instance.publish("127.0.0.1:4100".parse().unwrap()).unwrap();

    let found = read_discovery(&dir).unwrap().unwrap();
    assert_eq!(found, published);
    assert_eq!(found.pid, std::process::id());
    assert_eq!(found.address.to_string(), "127.0.0.1:4100");
    assert_eq!(
        found.schema_version,
        cs_store::migrate::CURRENT_SCHEMA_VERSION
    );
    // Probing the lock doesn't take it from the daemon.
    assert!(Instance::acquire(&dir).is_err());
    instance.close().unwrap();
    assert_eq!(read_discovery(&dir).unwrap(), None);
}

#[test]
fn read_discovery_refuses_a_stale_file_without_a_lock_holder() {
    let (_root, dir) = data_dir();
    let instance = Instance::acquire(&dir).unwrap();
    instance.publish("127.0.0.1:4100".parse().unwrap()).unwrap();
    let stale = fs::read(dir.join(DISCOVERY_FILE_NAME)).unwrap();
    instance.close().unwrap();
    // What a crash leaves behind: the file, and an unlocked lock file.
    fs::write(dir.join(DISCOVERY_FILE_NAME), stale).unwrap();

    assert_eq!(read_discovery(&dir).unwrap(), None);
}

#[test]
fn read_discovery_without_any_files_is_none() {
    let (_root, dir) = data_dir();

    assert_eq!(read_discovery(&dir).unwrap(), None);
}

#[test]
fn a_new_daemon_removes_a_stale_discovery_file() {
    let (_root, dir) = data_dir();
    let stale = r#"{"pid":1,"startedAtMs":1,"address":"127.0.0.1:1","schemaVersion":1}"#;
    fs::write(dir.join(DISCOVERY_FILE_NAME), stale).unwrap();

    let instance = Instance::acquire(&dir).unwrap();

    assert!(!dir.join(DISCOVERY_FILE_NAME).exists());
    assert_eq!(read_discovery(&dir).unwrap(), None);
    instance.close().unwrap();
}

#[cfg(unix)]
#[test]
fn read_discovery_refuses_a_pid_other_than_the_lock_holders() {
    let (_root, dir) = data_dir();
    let instance = Instance::acquire(&dir).unwrap();
    // A stale file written by a previous daemon, while this one holds the lock.
    let other_pid = std::process::id() + 1;
    let stale = format!(
        r#"{{"pid":{other_pid},"startedAtMs":1,"address":"127.0.0.1:1","schemaVersion":1}}"#
    );
    fs::write(dir.join(DISCOVERY_FILE_NAME), stale).unwrap();

    assert_eq!(read_discovery(&dir).unwrap(), None);
    instance.close().unwrap();
}

#[cfg(unix)]
#[test]
fn unix_lock_and_discovery_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let (_root, dir) = data_dir();
    let instance = Instance::acquire(&dir).unwrap();
    instance.publish("127.0.0.1:4100".parse().unwrap()).unwrap();
    let token = ControlToken::load_or_create(&dir).unwrap();

    for name in [LOCK_FILE_NAME, DISCOVERY_FILE_NAME, TOKEN_FILE_NAME] {
        let mode = fs::metadata(dir.join(name)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{name}");
    }
    drop(token);
    instance.close().unwrap();
}

// ---- token ----------------------------------------------------------------------

#[test]
fn token_file_is_created_once_and_reused() {
    let (_root, dir) = data_dir();
    let first = ControlToken::load_or_create(&dir).unwrap();
    let written = token_in(&dir);
    drop(first);

    let second = ControlToken::load_or_create(&dir).unwrap();

    assert_eq!(written.len(), 64);
    assert!(
        written
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(token_in(&dir), written);
    use cs_daemon::http::TokenVerifier;
    assert!(second.verify(written.as_bytes()));
}

#[test]
fn malformed_token_file_is_an_error_and_left_alone() {
    let (_root, dir) = data_dir();
    let path = dir.join(TOKEN_FILE_NAME);
    for contents in [
        "",
        "not-a-token",
        &"A".repeat(64),
        &format!("{}\n", "a".repeat(64)),
    ] {
        fs::write(&path, contents).unwrap();

        let error = ControlToken::load_or_create(&dir).unwrap_err();

        assert!(matches!(error, TokenError::Malformed { .. }), "{error}");
        assert!(error.to_string().contains(TOKEN_FILE_NAME), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), contents);
    }
}

#[tokio::test]
async fn rotation_rejects_the_old_token_and_accepts_the_new_one() {
    let (_root, dir) = data_dir();
    let token = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    let (addr, server) = start(token.clone(), ServeConfig::default()).await;
    let old = token_in(&dir);
    assert_eq!(call_version(addr, Some(&old)).await, 200);

    token.rotate().unwrap();

    let new = token_in(&dir);
    assert_ne!(new, old);
    assert_eq!(call_version(addr, Some(&old)).await, 401);
    assert_eq!(call_version(addr, Some(&new)).await, 200);
    assert_eq!(call_version(addr, None).await, 401);
    // The rotated token is the one a restarted daemon uses.
    let reloaded = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    use cs_daemon::http::TokenVerifier;
    assert!(reloaded.verify(new.as_bytes()));
    assert!(!reloaded.verify(old.as_bytes()));
    server.stop().await;
}

// ---- server ---------------------------------------------------------------------

#[tokio::test]
async fn half_sent_headers_are_disconnected_after_the_header_read_timeout() {
    let (_root, dir) = data_dir();
    let token = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    let timeout = Duration::from_millis(200);
    let config = ServeConfig {
        header_read_timeout: timeout,
        drain_timeout: Duration::from_secs(1),
    };
    let (addr, server) = start(token, config).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let started = Instant::now();
    stream
        .write_all(b"POST /rpc HTTP/1.1\r\nHost: 127.0.0.1")
        .await
        .unwrap();
    let mut response = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
        .await
        .expect("the server kept a half-sent request open");
    let elapsed = started.elapsed();

    // Closed (or reset) without a response, and not before the timeout.
    assert!(read.is_err() || response.is_empty(), "{response:?}");
    assert!(
        elapsed >= timeout - Duration::from_millis(50),
        "{elapsed:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn the_header_read_timeout_does_not_cut_complete_requests() {
    let (_root, dir) = data_dir();
    let token = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    let config = ServeConfig {
        header_read_timeout: Duration::from_millis(200),
        drain_timeout: Duration::from_secs(1),
    };
    let (addr, server) = start(token, config).await;
    let slow = r#"{"jsonrpc":"2.0","method":"slow","id":1}"#;

    // The handler takes 300 ms, longer than the header-read timeout.
    let response = exchange(addr, &post(addr, Some(&token_in(&dir)), slow)).await;

    assert_eq!(status(&response), 200, "{response}");
    server.stop().await;
}

#[test]
fn daemon_timeouts_cover_the_request_timeout() {
    assert_eq!(
        ServeConfig::default(),
        ServeConfig {
            header_read_timeout: Duration::from_secs(10),
            drain_timeout: cs_daemon::http::REQUEST_TIMEOUT + Duration::from_secs(1),
        }
    );
}

#[tokio::test]
async fn graceful_stop_finishes_in_flight_requests_then_refuses_connections() {
    let (_root, dir) = data_dir();
    let token = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    let (addr, server, slow_started) = start_with_handler(token, ServeConfig::default()).await;
    let slow = r#"{"jsonrpc":"2.0","method":"slow","id":1}"#;
    let request = post(addr, Some(&token_in(&dir)), slow);

    let in_flight = tokio::spawn(async move { exchange(addr, &request).await });
    slow_started.notified().await;
    server.stop().await;

    let response = in_flight.await.unwrap();
    assert_eq!(status(&response), 200, "{response}");
}

#[tokio::test]
async fn graceful_stop_closes_idle_keep_alive_connections() {
    let (_root, dir) = data_dir();
    let token = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    let (addr, server) = start(token, ServeConfig::default()).await;
    let keep_alive = post(addr, Some(&token_in(&dir)), support::VERSION_CALL)
        .replace("Connection: close\r\n", "");
    let mut idle = TcpStream::connect(addr).await.unwrap();
    idle.write_all(keep_alive.as_bytes()).await.unwrap();
    let mut first = [0u8; 12];
    idle.read_exact(&mut first).await.unwrap();
    assert_eq!(&first, b"HTTP/1.1 200");

    let started = Instant::now();
    server.stop().await;

    // Well under the 10 s drain timeout: the idle connection was closed at once.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn requests_still_running_after_the_drain_timeout_are_aborted() {
    let (_root, dir) = data_dir();
    let token = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    let (addr, server, slow_started) = start_with_handler(token, ServeConfig::default()).await;
    let slow = r#"{"jsonrpc":"2.0","method":"slow","id":1}"#;
    let request = post(addr, Some(&token_in(&dir)), slow);

    let in_flight = tokio::spawn(async move { exchange(addr, &request).await });
    slow_started.notified().await;
    let started = Instant::now();
    server.stop_within(Duration::from_millis(50)).await;

    // The 300 ms handler was dropped, and its connection closed without an answer.
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(in_flight.await.unwrap(), "");
}

// ---- review regressions ---------------------------------------------------------

/// A stale `daemon.lock` + `daemon.json` pair, as a crash leaves them: the pid in
/// both files is the same, and nobody holds the lock.
fn stale_pair(dir: &Path) {
    let instance = Instance::acquire(dir).unwrap();
    instance.publish("127.0.0.1:4100".parse().unwrap()).unwrap();
    let json = fs::read(dir.join(DISCOVERY_FILE_NAME)).unwrap();
    instance.close().unwrap();
    fs::write(dir.join(DISCOVERY_FILE_NAME), json).unwrap();
}

#[test]
fn another_clients_probe_is_not_mistaken_for_a_live_daemon() {
    let (_root, dir) = data_dir();
    stale_pair(&dir);
    // Another client in the middle of its probe.
    let probe = fs::File::open(dir.join(LOCK_FILE_NAME)).unwrap();
    probe.try_lock_shared().unwrap();

    assert_eq!(read_discovery(&dir).unwrap(), None);
    probe.unlock().unwrap();
}

#[test]
fn a_daemon_starts_while_a_client_probes() {
    let (_root, dir) = data_dir();
    stale_pair(&dir);
    let probe = fs::File::open(dir.join(LOCK_FILE_NAME)).unwrap();
    probe.try_lock_shared().unwrap();
    let released = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        probe.unlock().unwrap();
    });

    // `acquire` retries while the probe holds its shared lock.
    let instance = Instance::acquire(&dir).unwrap();

    released.join().unwrap();
    instance.close().unwrap();
}

#[test]
fn read_discovery_refuses_addresses_other_than_127_0_0_1_with_a_port() {
    let (_root, dir) = data_dir();
    let instance = Instance::acquire(&dir).unwrap();
    let pid = std::process::id();
    for address in [
        "127.0.0.1:0",
        "127.0.0.2:4100",
        "0.0.0.0:4100",
        "10.0.0.1:4100",
    ] {
        let json =
            format!(r#"{{"pid":{pid},"startedAtMs":1,"address":"{address}","schemaVersion":1}}"#);
        fs::write(dir.join(DISCOVERY_FILE_NAME), json).unwrap();

        assert_eq!(read_discovery(&dir).unwrap(), None, "{address}");
    }
    instance.publish("127.0.0.1:4100".parse().unwrap()).unwrap();
    assert!(read_discovery(&dir).unwrap().is_some());
    instance.close().unwrap();
}

#[cfg(unix)]
#[test]
fn unix_data_dir_is_resolved_once_through_symlinks() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let link = root.path().join("link");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let resolved = prepare_data_dir(&link).unwrap();

    assert_eq!(resolved, fs::canonicalize(&real).unwrap());
}
