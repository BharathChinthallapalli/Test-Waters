//! The model-call proxy wired into the built `cs-daemon` binary (feature 03,
//! task 8): `--proxy-upstream` pointing at a mock upstream on loopback, calls
//! through the proxy listed by `calls.list` and counted by `health.proxy`, the
//! saved port reused across restarts, a call in flight at SIGTERM drained into
//! the store, and no credential or body in the logs (at `trace`) or the data
//! directory.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use cs_core::control::HealthResult;
use cs_core::llm::{CallOutcome, CallsListResult, Usage};
use cs_daemon::instance::{DISCOVERY_FILE_NAME, Discovery, read_discovery};
use cs_daemon::proxy::{PORT_BAND, PORT_FILE_NAME};
use cs_daemon::token::TOKEN_FILE_NAME;
use serde_json::Value;

const DAEMON: &str = env!("CARGO_BIN_EXE_cs-daemon");

/// Generous: CI machines can be slow to start a process.
const LIMIT: Duration = Duration::from_secs(30);

const API_KEY: &str = "sk-ant-api03-WIRETESTKEY-0123456789";
const BEARER: &str = "WIRETESTBEARER-abcdef0123456789";
/// In every request body, and must never reach a log or the data directory.
const REQUEST_MARKER: &str = "request-body-marker-7f3a";
/// In every response body, likewise.
const RESPONSE_MARKER: &str = "response-body-marker-9c1e";

// ---------------------------------------------------------------- mock upstream

/// A request as the mock upstream received it.
#[derive(Debug, Clone)]
struct Received {
    target: String,
    /// Lower-case names.
    headers: BTreeMap<String, String>,
}

/// An HTTP/1.1 upstream on `127.0.0.1:0`, one thread per connection, one
/// request per connection. A body with `"stream":true` gets SSE (with
/// `x-test-slow`, 400 ms between events); anything else gets a JSON message.
struct MockUpstream {
    addr: SocketAddr,
    received: Arc<Mutex<Vec<Received>>>,
}

impl MockUpstream {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&received);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    let _ = answer(stream, &log);
                });
            }
        });
        Self { addr, received }
    }

    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    #[cfg(unix)]
    fn wait_for_requests(&self, count: usize) {
        let started = Instant::now();
        while self.received().len() < count {
            assert!(started.elapsed() < LIMIT, "the upstream got no request");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn answer(stream: TcpStream, log: &Mutex<Vec<Received>>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let target = line.split(' ').nth(1).unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let streamed = String::from_utf8_lossy(&body).contains("\"stream\":true");
    let slow = headers.contains_key("x-test-slow");
    log.lock().unwrap().push(Received { target, headers });

    let mut stream = stream;
    if streamed {
        stream.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nrequest-id: req_stream\r\n\
              anthropic-ratelimit-requests-remaining: 49\r\ntransfer-encoding: chunked\r\n\
              connection: close\r\n\r\n",
        )?;
        for event in sse_events() {
            write!(stream, "{:x}\r\n{event}\r\n", event.len())?;
            stream.flush()?;
            if slow {
                std::thread::sleep(Duration::from_millis(400));
            }
        }
        stream.write_all(b"0\r\n\r\n")?;
    } else {
        let json = format!(
            r#"{{"id":"msg_json","type":"message","role":"assistant","model":"claude-wire-json","content":[{{"type":"text","text":"{RESPONSE_MARKER}"}}],"stop_reason":"end_turn","usage":{{"input_tokens":21,"output_tokens":8}}}}"#
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nrequest-id: req_json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{json}",
            json.len()
        )?;
    }
    stream.flush()
}

fn sse_events() -> Vec<String> {
    vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_sse\",\"model\":\"claude-wire-sse\",\"usage\":{\"input_tokens\":12,\"output_tokens\":1,\"cache_creation_input_tokens\":3,\"cache_read_input_tokens\":4}}}\n\n".to_owned(),
        "event: ping\ndata: {\"type\": \"ping\"}\n\n".to_owned(),
        format!("event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{RESPONSE_MARKER}\"}}}}\n\n"),
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":12,\"output_tokens\":7,\"cache_creation_input_tokens\":3,\"cache_read_input_tokens\":4}}\n\n".to_owned(),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_owned(),
    ]
}

// ---------------------------------------------------------------- proxy client

/// A response from the proxy: status, lower-case headers and the decoded body.
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

/// Sends `POST /v1/messages?beta=true` to the proxy with `headers` and `body`,
/// `Connection: close`, and reads the whole response.
fn call_proxy(proxy: SocketAddrV4, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut stream = TcpStream::connect(proxy).unwrap();
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    let mut request = format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\nhost: {proxy}\r\ncontent-type: application/json\r\n\
         anthropic-version: 2023-06-01\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    parse_reply(&raw)
}

fn parse_reply(raw: &[u8]) -> Reply {
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("no response head");
    let head = String::from_utf8_lossy(&raw[..end]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap();
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let mut rest = &raw[end + 4..];
    let body = if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
        let mut body = Vec::new();
        loop {
            let size_end = rest
                .windows(2)
                .position(|window| window == b"\r\n")
                .expect("chunk size line");
            let size =
                usize::from_str_radix(std::str::from_utf8(&rest[..size_end]).unwrap().trim(), 16)
                    .unwrap();
            rest = &rest[size_end + 2..];
            if size == 0 {
                break;
            }
            body.extend_from_slice(&rest[..size]);
            rest = &rest[size + 2..];
        }
        body
    } else {
        rest.to_vec()
    };
    Reply {
        status,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

fn streamed_body() -> String {
    format!(
        r#"{{"model":"claude-wire-sse","max_tokens":16,"stream":true,"messages":[{{"role":"user","content":"{REQUEST_MARKER}"}}]}}"#
    )
}

fn json_body() -> String {
    format!(
        r#"{{"model":"claude-wire-json","max_tokens":16,"messages":[{{"role":"user","content":"{REQUEST_MARKER}"}}]}}"#
    )
}

// ---------------------------------------------------------------- daemon

/// Held by every test in this file, so they run one at a time: one test picks
/// a free port and passes it with `--proxy-listen`, another expects a restart
/// to get its saved port back, and neither may lose that port to a daemon or a
/// listener started by a test running alongside. (Test binaries themselves run
/// one after another.)
fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    // A failed test poisons it; the next one still runs.
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn temp_data_dir() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data");
    (root, dir)
}

fn daemon(data_dir: &Path) -> Command {
    let mut command = Command::new(DAEMON);
    command
        .arg("--data-dir")
        .arg(data_dir)
        // Every log line there is, so the credential checks see them all.
        .env("CALLSHEET_LOG", "trace")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// A running daemon, killed if a test fails before it stops.
struct Running {
    child: Option<Child>,
    discovery: Discovery,
}

impl Running {
    fn spawn(command: &mut Command, data_dir: &Path) -> Self {
        let mut child = command.spawn().unwrap();
        let started = Instant::now();
        loop {
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
            assert!(started.elapsed() < LIMIT, "daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn proxy(&self) -> SocketAddrV4 {
        self.discovery.proxy_address.expect("no proxyAddress")
    }

    /// Calls a control-API method with the token from the token file.
    fn rpc(&self, data_dir: &Path, method: &str, params: Value) -> Value {
        let token = fs::read_to_string(data_dir.join(TOKEN_FILE_NAME)).unwrap();
        let addr: SocketAddr = self.discovery.address.into();
        let body =
            serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params, "id": 1 })
                .to_string();
        let request = support::post(addr, Some(&token), &body);
        let response = rt().block_on(support::exchange(addr, &request));
        assert_eq!(support::status(&response), 200, "{response}");
        let (_, payload) = response.split_once("\r\n\r\n").unwrap();
        let reply: Value = serde_json::from_str(payload).unwrap();
        assert!(reply.get("error").is_none(), "{reply}");
        reply["result"].clone()
    }

    fn health(&self, data_dir: &Path) -> HealthResult {
        serde_json::from_value(self.rpc(data_dir, "health", serde_json::json!({}))).unwrap()
    }

    fn calls(&self, data_dir: &Path) -> CallsListResult {
        serde_json::from_value(self.rpc(data_dir, "calls.list", serde_json::json!({}))).unwrap()
    }

    /// Waits until `health.proxy.callsRecorded` reaches `count`.
    fn wait_recorded(&self, data_dir: &Path, count: u64) -> HealthResult {
        let started = Instant::now();
        loop {
            let health = self.health(data_dir);
            if health.proxy.as_ref().unwrap().calls_recorded >= count {
                return health;
            }
            assert!(started.elapsed() < LIMIT, "only {health:?} recorded");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    fn signal(&self, signal: &str) {
        let pid = self.child.as_ref().unwrap().id();
        let sent = Command::new("kill")
            .args([&format!("-{signal}"), &pid.to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
    }

    /// Waits for the process to exit by itself.
    #[cfg(unix)]
    fn wait(mut self) -> (std::process::ExitStatus, Output) {
        let mut child = self.child.take().unwrap();
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return (status, child.wait_with_output().unwrap());
            }
            if started.elapsed() > LIMIT {
                let _ = child.kill();
                panic!("daemon did not exit within {LIMIT:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn kill(mut self) -> Output {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait_with_output().unwrap()
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

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_no_secret_or_body(what: &str, text: &[u8]) {
    let text = String::from_utf8_lossy(text);
    for needle in [
        API_KEY,
        BEARER,
        "WIRETESTKEY",
        "WIRETESTBEARER",
        REQUEST_MARKER,
        RESPONSE_MARKER,
    ] {
        assert!(!text.contains(needle), "{what} contains {needle:?}");
    }
}

/// Every file in the data directory, as bytes (the database, its WAL, the
/// token, `daemon.json`...).
fn data_dir_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

// ---------------------------------------------------------------- tests

/// Starts a daemon in `dir` in front of `upstream`, sends a streamed call (API
/// key, `x-callsheet-run: wire-run`) and a non-streamed one (bearer token,
/// Claude Code session `sess-42`) through its proxy, and checks what the
/// upstream received, `health.proxy` and `calls.list`.
fn start_and_send_two_calls(upstream: &MockUpstream, dir: &Path) -> (Running, SocketAddrV4) {
    let first = Running::spawn(
        daemon(dir).args(["--proxy-upstream", &upstream.base()]),
        dir,
    );

    // daemon.json names the proxy on the port saved in the data directory.
    let proxy = first.proxy();
    assert_eq!(*proxy.ip(), std::net::Ipv4Addr::LOCALHOST);
    assert!(PORT_BAND.contains(&proxy.port()), "{proxy}");
    assert_eq!(
        fs::read_to_string(dir.join(PORT_FILE_NAME)).unwrap().trim(),
        proxy.port().to_string()
    );

    // A streamed call in a named run, with an API key.
    let streamed = call_proxy(
        proxy,
        &[("x-api-key", API_KEY), ("x-callsheet-run", "wire-run")],
        &streamed_body(),
    );
    assert_eq!(streamed.status, 200);
    assert_eq!(
        streamed.headers.get("content-type").map(String::as_str),
        Some("text/event-stream")
    );
    assert_eq!(streamed.body, sse_events().concat(), "SSE passed unchanged");

    // A non-streamed call from a Claude Code session, with a bearer token.
    let json = call_proxy(
        proxy,
        &[
            ("authorization", &format!("Bearer {BEARER}")),
            ("x-claude-code-session-id", "sess-42"),
        ],
        &json_body(),
    );
    assert_eq!(json.status, 200);
    assert!(json.body.contains(RESPONSE_MARKER));

    // The credentials reached the upstream unchanged; the run header did not.
    let received = upstream.received();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].target, "/v1/messages?beta=true");
    assert_eq!(
        received[0].headers.get("x-api-key").map(String::as_str),
        Some(API_KEY)
    );
    assert!(!received[0].headers.contains_key("x-callsheet-run"));
    assert_eq!(
        received[1].headers.get("authorization").map(String::as_str),
        Some(format!("Bearer {BEARER}").as_str())
    );
    assert_eq!(
        received[1]
            .headers
            .get("x-claude-code-session-id")
            .map(String::as_str),
        Some("sess-42")
    );

    // health.proxy counts both, and calls.list shows them newest first.
    let health = first.wait_recorded(dir, 2);
    let proxy_health = health.proxy.unwrap();
    assert_eq!(proxy_health.address, proxy);
    assert_eq!(proxy_health.calls_recorded, 2);
    assert_eq!(proxy_health.records_dropped, 0);

    let listed = first.calls(dir).calls;
    assert_eq!(listed.len(), 2, "{listed:?}");
    let (newest, oldest) = (&listed[0], &listed[1]);
    assert!(newest.global_pos > oldest.global_pos);

    assert_eq!(oldest.run_id, "wire-run");
    let call = &oldest.call;
    assert_eq!(
        (call.method.as_str(), call.path.as_str(), call.status),
        ("POST", "/v1/messages", 200)
    );
    assert_eq!(call.outcome, CallOutcome::Completed);
    assert!(call.streamed);
    assert_eq!(call.model.as_deref(), Some("claude-wire-sse"));
    assert_eq!(call.request_id.as_deref(), Some("req_stream"));
    assert_eq!(call.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(
        call.usage,
        Some(Usage {
            input_tokens: 12,
            output_tokens: 7,
            cache_creation_input_tokens: Some(3),
            cache_read_input_tokens: Some(4),
        })
    );
    assert_eq!(
        call.rate_limit_headers
            .get("anthropic-ratelimit-requests-remaining")
            .map(String::as_str),
        Some("49")
    );
    assert_eq!(call.content_truncated, None);

    assert_eq!(newest.run_id, "cc-sess-42");
    let call = &newest.call;
    assert_eq!(call.outcome, CallOutcome::Completed);
    assert!(!call.streamed);
    assert_eq!(call.model.as_deref(), Some("claude-wire-json"));
    assert_eq!(call.request_id.as_deref(), Some("req_json"));
    assert_eq!(
        call.usage,
        Some(Usage {
            input_tokens: 21,
            output_tokens: 8,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        })
    );
    (first, proxy)
}

#[test]
fn calls_through_the_proxy_are_listed_and_counted_without_credentials_in_logs() {
    let _serial = serial();
    let upstream = MockUpstream::start();
    let (_root, dir) = temp_data_dir();
    let (running, _proxy) = start_and_send_two_calls(&upstream, &dir);

    // Killed, so this also runs on Windows; the rows were written already.
    let logs = stderr(&running.kill());

    assert!(logs.contains("Callsheet daemon listening"), "{logs}");
    assert_no_secret_or_body("the daemon's log", logs.as_bytes());
    for (path, bytes) in data_dir_files(&dir) {
        assert_no_secret_or_body(&path.display().to_string(), &bytes);
    }
}

#[cfg(unix)]
#[test]
fn calls_at_sigterm_are_drained_and_the_next_start_reuses_the_port() {
    let _serial = serial();
    let upstream = MockUpstream::start();
    let (_root, dir) = temp_data_dir();
    let (first, proxy) = start_and_send_two_calls(&upstream, &dir);

    let first_started_at_ms = first.discovery.started_at_ms;
    // A call answered just before SIGTERM, and a slow stream still running when
    // SIGTERM arrives, in the default run.
    let quick = call_proxy(proxy, &[], &json_body());
    assert_eq!(quick.status, 200);
    let in_flight =
        std::thread::spawn(move || call_proxy(proxy, &[("x-test-slow", "1")], &streamed_body()));
    upstream.wait_for_requests(4);
    first.signal("TERM");

    let slow = in_flight.join().unwrap();
    assert_eq!(slow.status, 200);
    assert_eq!(
        slow.body,
        sse_events().concat(),
        "the stream was cut short by the shutdown"
    );
    let (status, output) = first.wait();
    let first_logs = stderr(&output);
    assert_eq!(status.code(), Some(0), "{first_logs}");
    // The recorder had written all four before the store closed.
    let drained = first_logs
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| line["fields"]["message"] == "shutdown: call recorder drained")
        .unwrap_or_else(|| panic!("no drain line: {first_logs}"));
    assert_eq!(drained["fields"]["calls_recorded"], 4, "{drained}");
    assert_eq!(drained["fields"]["records_dropped"], 0, "{drained}");
    assert!(!dir.join(DISCOVERY_FILE_NAME).exists());

    // The next start reuses the port, and both calls were written before exit.
    let second = Running::spawn(
        daemon(&dir).args(["--proxy-upstream", &upstream.base()]),
        &dir,
    );
    assert_eq!(second.proxy(), proxy);
    let listed = second.calls(&dir).calls;
    assert_eq!(listed.len(), 4, "{listed:?}");
    let default_run = format!("proxy-{first_started_at_ms}");
    assert_eq!(listed[0].run_id, default_run);
    assert!(listed[0].call.streamed);
    assert_eq!(listed[0].call.outcome, CallOutcome::Completed);
    assert_eq!(listed[1].run_id, default_run);
    assert!(!listed[1].call.streamed);
    // Counters start again with the process.
    assert_eq!(
        second.health(&dir).proxy.unwrap().calls_recorded,
        0,
        "health.proxy counts this process's calls"
    );

    second.signal("TERM");
    let (status, output) = second.wait();
    let second_logs = stderr(&output);
    assert_eq!(status.code(), Some(0), "{second_logs}");

    // No credential or body in the logs (at trace) or in any file.
    assert_no_secret_or_body("the first daemon's log", first_logs.as_bytes());
    assert_no_secret_or_body("the second daemon's log", second_logs.as_bytes());
    for (path, bytes) in data_dir_files(&dir) {
        assert_no_secret_or_body(&path.display().to_string(), &bytes);
    }
}

#[test]
fn a_taken_saved_port_stops_startup_naming_the_port_and_the_fix() {
    let _serial = serial();
    let (_root, dir) = temp_data_dir();
    cs_daemon::instance::prepare_data_dir(&dir).unwrap();
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    fs::write(dir.join(PORT_FILE_NAME), format!("{port}\n")).unwrap();

    let output = daemon(&dir).output().unwrap();

    let logs = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{logs}");
    let errors: Vec<&str> = logs
        .lines()
        .filter(|line| line.starts_with("cs-daemon:"))
        .collect();
    assert_eq!(errors.len(), 1, "{logs}");
    assert!(errors[0].contains(&format!("127.0.0.1:{port}")), "{logs}");
    assert!(errors[0].contains("--proxy-listen"), "{logs}");
    assert!(!dir.join(DISCOVERY_FILE_NAME).exists());
    assert_eq!(
        fs::read_to_string(dir.join(PORT_FILE_NAME)).unwrap(),
        format!("{port}\n")
    );
}

#[test]
fn a_corrupt_port_file_stops_startup_and_is_kept() {
    let _serial = serial();
    for contents in ["not a port", "0", "65536"] {
        let (_root, dir) = temp_data_dir();
        cs_daemon::instance::prepare_data_dir(&dir).unwrap();
        fs::write(dir.join(PORT_FILE_NAME), contents).unwrap();

        let output = daemon(&dir).output().unwrap();

        let logs = stderr(&output);
        assert_eq!(output.status.code(), Some(1), "{contents}: {logs}");
        assert!(logs.contains("does not hold a port number"), "{logs}");
        assert!(!dir.join(DISCOVERY_FILE_NAME).exists());
        assert_eq!(
            fs::read_to_string(dir.join(PORT_FILE_NAME)).unwrap(),
            contents
        );
    }
}

#[test]
fn proxy_listen_is_used_and_leaves_the_port_file_alone() {
    let _serial = serial();
    let (_root, dir) = temp_data_dir();
    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap().to_string();
    drop(free);

    let running = Running::spawn(daemon(&dir).args(["--proxy-listen", &address]), &dir);

    assert_eq!(running.proxy().to_string(), address);
    assert_eq!(
        running.health(&dir).proxy.unwrap().address.to_string(),
        address
    );
    assert!(!dir.join(PORT_FILE_NAME).exists());
    running.kill();
}

#[test]
fn bad_proxy_flags_exit_2_before_creating_anything() {
    let _serial = serial();
    for args in [
        ["--proxy-listen", "0.0.0.0:1"],
        ["--proxy-listen", "localhost:1"],
        ["--proxy-upstream", "http://example.com"],
        ["--proxy-upstream", "https://user:hunter2@example.com"],
    ] {
        let (_root, dir) = temp_data_dir();

        let output = daemon(&dir).args(args).output().unwrap();

        let message = stderr(&output);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {message}");
        assert_eq!(message.lines().count(), 1, "{args:?}: {message}");
        assert!(message.contains(args[0]), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
        assert!(!dir.exists(), "{args:?}: the data directory was created");
    }
}
