//! The built `cs-daemon` binary, with a temporary `--data-dir`: argument checks,
//! single instance, files and modes, token reuse, logs and signals.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use cs_daemon::instance::{DISCOVERY_FILE_NAME, Discovery, read_discovery};
use cs_daemon::token::TOKEN_FILE_NAME;
use support::call_version;

const DAEMON: &str = env!("CARGO_BIN_EXE_cs-daemon");

/// Generous: CI machines can be slow to start a process.
const STARTUP_LIMIT: Duration = Duration::from_secs(30);

fn daemon(data_dir: &Path) -> Command {
    let mut command = Command::new(DAEMON);
    command
        .arg("--data-dir")
        .arg(data_dir)
        .env_remove("CALLSHEET_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn temp_data_dir() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("nested").join("data");
    (root, dir)
}

/// A running daemon, killed if a test fails before stopping it.
struct Running {
    child: Option<Child>,
    discovery: Discovery,
}

impl Running {
    fn start(data_dir: &Path) -> Self {
        let mut child = daemon(data_dir).spawn().unwrap();
        let started = Instant::now();
        loop {
            // The pid check skips a `daemon.json` left by a killed daemon, which
            // Windows clients can't tell apart (see `cs_daemon::instance`).
            if let Some(discovery) = read_discovery(data_dir)
                .unwrap()
                .filter(|discovery| discovery.pid == child.id())
            {
                return Self {
                    child: Some(child),
                    discovery,
                };
            }
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                panic!(
                    "daemon exited with {status}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert!(started.elapsed() < STARTUP_LIMIT, "daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// Kills the process (no graceful shutdown) and returns its output.
    fn kill(mut self) -> Output {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait_with_output().unwrap()
    }

    /// Waits for the process to exit by itself, within a limit.
    #[cfg(unix)]
    fn wait(mut self, limit: Duration) -> (std::process::ExitStatus, Output) {
        let mut child = self.child.take().unwrap();
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return (status, child.wait_with_output().unwrap());
            }
            if started.elapsed() > limit {
                let _ = child.kill();
                panic!("daemon did not exit within {limit:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn run_to_end(command: &mut Command) -> Output {
    command.output().unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn non_loopback_listen_addresses_exit_2_before_creating_anything() {
    for address in [
        "0.0.0.0:1",
        "[::1]:1",
        "::1",
        "127.0.0.2:1",
        "localhost:1",
        "garbage",
        "127.0.0.1",
    ] {
        let (_root, dir) = temp_data_dir();

        let output = run_to_end(daemon(&dir).args(["--listen", address]));

        let message = stderr(&output);
        assert_eq!(output.status.code(), Some(2), "{address}: {message}");
        assert_eq!(message.lines().count(), 1, "{address}: {message}");
        assert!(message.contains("--listen"), "{address}: {message}");
        assert!(!dir.exists(), "{address}: the data directory was created");
        assert!(!dir.parent().unwrap().exists(), "{address}");
    }
}

#[test]
fn unknown_arguments_exit_2() {
    let (_root, dir) = temp_data_dir();

    let output = run_to_end(daemon(&dir).arg("--port"));

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!dir.exists());
}

#[test]
fn help_exits_0_with_usage() {
    let output = run_to_end(Command::new(DAEMON).arg("--help"));

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--listen"));
}

#[test]
fn invalid_log_level_exits_2_without_repeating_it() {
    let (_root, dir) = temp_data_dir();

    let output = run_to_end(daemon(&dir).env("CALLSHEET_LOG", "loud-value"));

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("CALLSHEET_LOG"));
    assert!(!stderr(&output).contains("loud-value"));
}

#[test]
fn serves_version_with_the_token_and_401_without() {
    let (_root, dir) = temp_data_dir();
    let daemon = Running::start(&dir);
    let token = fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap();
    let addr = daemon.discovery.address.into();

    assert_eq!(daemon.discovery.pid, daemon.pid());
    assert_ne!(daemon.discovery.address.port(), 0);
    let (ok, missing, wrong) = rt().block_on(async {
        (
            call_version(addr, Some(&token)).await,
            call_version(addr, None).await,
            call_version(addr, Some(&"0".repeat(64))).await,
        )
    });

    assert_eq!((ok, missing, wrong), (200, 401, 401));
    let output = daemon.kill();
    let logs = stderr(&output);
    assert!(logs.contains("Callsheet daemon listening"), "{logs}");
    assert!(logs.contains("missing or wrong bearer token"), "{logs}");
    assert!(!logs.contains(&token), "the token is in the logs");
}

#[test]
fn second_instance_is_refused_naming_the_running_pid() {
    let (_root, dir) = temp_data_dir();
    let first = Running::start(&dir);

    let output = run_to_end(&mut daemon(&dir));

    let message = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{message}");
    assert!(
        message.contains(&format!("pid {}", first.pid())),
        "{message}"
    );
    assert_eq!(message.lines().count(), 1, "{message}");
    // The running daemon is untouched.
    assert_eq!(read_discovery(&dir).unwrap(), Some(first.discovery.clone()));
}

#[test]
fn token_is_reused_after_a_restart_even_after_a_crash() {
    let (_root, dir) = temp_data_dir();
    let first = Running::start(&dir);
    let token = fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap();
    first.kill();
    // A crash leaves daemon.json behind, but no lock holder. Windows may take a
    // moment to release the lock of a killed process.
    assert!(dir.join(DISCOVERY_FILE_NAME).exists());
    let started = Instant::now();
    while read_discovery(&dir).unwrap().is_some() {
        assert!(
            started.elapsed() < STARTUP_LIMIT,
            "the lock was never released"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let second = Running::start(&dir);

    assert_eq!(
        fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap(),
        token
    );
    let addr = second.discovery.address.into();
    assert_eq!(rt().block_on(call_version(addr, Some(&token))), 200);
}

#[test]
fn malformed_token_file_stops_startup_and_is_kept() {
    let (_root, dir) = temp_data_dir();
    cs_daemon::instance::prepare_data_dir(&dir).unwrap();
    fs::write(dir.join(TOKEN_FILE_NAME), "not a token").unwrap();

    let output = run_to_end(&mut daemon(&dir));

    let message = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{message}");
    assert!(message.contains(TOKEN_FILE_NAME), "{message}");
    assert!(!message.contains("not a token"), "{message}");
    assert_eq!(
        fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap(),
        "not a token"
    );
    assert!(!dir.join(DISCOVERY_FILE_NAME).exists());
}

#[test]
fn listen_address_in_use_stops_startup() {
    let (_root, dir) = temp_data_dir();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();

    let output = run_to_end(daemon(&dir).args(["--listen", &address]));

    let message = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{message}");
    assert!(message.contains(&address), "{message}");
    assert!(!dir.join(DISCOVERY_FILE_NAME).exists());
}

#[cfg(unix)]
#[test]
fn unix_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let (_root, dir) = temp_data_dir();
    let _daemon = Running::start(&dir);
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;

    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(dir.parent().unwrap()), 0o700);
    for name in [
        DISCOVERY_FILE_NAME,
        TOKEN_FILE_NAME,
        cs_daemon::instance::LOCK_FILE_NAME,
    ] {
        assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
    }
}

#[cfg(unix)]
fn signal_stops_gracefully(signal: &str) {
    let (_root, dir) = temp_data_dir();
    let daemon = Running::start(&dir);
    let token = fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap();
    let addr: std::net::SocketAddr = daemon.discovery.address.into();
    assert_eq!(rt().block_on(call_version(addr, Some(&token))), 200);

    let sent = Command::new("kill")
        .args([&format!("-{signal}"), &daemon.pid().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let (status, output) = daemon.wait(Duration::from_secs(20));

    let logs = stderr(&output);
    assert_eq!(status.code(), Some(0), "{signal}: {logs}");
    assert!(logs.contains(&format!("SIG{signal}")), "{logs}");
    assert!(logs.contains("Callsheet daemon stopped"), "{logs}");
    assert!(!dir.join(DISCOVERY_FILE_NAME).exists());
    assert!(dir.join(cs_daemon::instance::LOCK_FILE_NAME).exists());
    assert_eq!(read_discovery(&dir).unwrap(), None);
    assert!(std::net::TcpStream::connect(addr).is_err());
    assert!(!logs.contains(&token));
}

#[cfg(unix)]
#[test]
fn sigterm_exits_0_and_removes_daemon_json() {
    signal_stops_gracefully("TERM");
}

#[cfg(unix)]
#[test]
fn sigint_exits_0_and_removes_daemon_json() {
    signal_stops_gracefully("INT");
}
