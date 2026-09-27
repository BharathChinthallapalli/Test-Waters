//! R1.6: the token never reaches the logs. The real router and server run
//! in-process with the daemon's JSON subscriber writing into a buffer, and
//! requests carry the right token, a wrong one, and a half-sent request.
//!
//! Its own test binary, so no other test's subscriber or callsite cache is in play.

mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_daemon::http::{HttpConfig, router};
use cs_daemon::instance::prepare_data_dir;
use cs_daemon::logging;
use cs_daemon::serve::{self, ServeConfig};
use cs_daemon::token::{ControlToken, TOKEN_FILE_NAME};
use support::{call_version, exchange};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::level_filters::LevelFilter;

/// Collects everything the subscriber writes.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// A current-thread runtime: the server's tasks run on this thread, where the
// subscriber is the default.
#[tokio::test]
async fn token_and_header_values_never_appear_in_the_logs() {
    let captured = Captured::default();
    let writer = captured.clone();
    let _guard =
        tracing::subscriber::set_default(logging::subscriber(LevelFilter::TRACE, move || {
            writer.clone()
        }));

    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data");
    prepare_data_dir(&dir).unwrap();
    let token = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(
        HttpConfig::for_listener(&listener).unwrap(),
        token.clone(),
        Arc::new(support::TestHandler::default()),
    );
    let config = ServeConfig {
        header_read_timeout: Duration::from_millis(200),
        drain_timeout: Duration::from_secs(1),
    };
    let server = serve::spawn(listener, app, config);
    let secret = std::fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap();
    let wrong = "f".repeat(64);

    assert_eq!(call_version(addr, Some(&secret)).await, 200);
    assert_eq!(call_version(addr, Some(&wrong)).await, 401);
    // A request cut off inside the Authorization header, which times out.
    let mut half = TcpStream::connect(addr).await.unwrap();
    half.write_all(
        format!("POST /rpc HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {secret}").as_bytes(),
    )
    .await
    .unwrap();
    let mut rest = Vec::new();
    let _ = half.read_to_end(&mut rest).await;
    // The same token after rotation.
    token.rotate().unwrap();
    let rotated = std::fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap();
    assert_eq!(call_version(addr, Some(&secret)).await, 401);
    assert_eq!(call_version(addr, Some(&rotated)).await, 200);
    let origin = exchange(
        addr,
        &support::post(addr, Some(&rotated), support::VERSION_CALL).replace(
            "Content-Type",
            "Origin: http://origin-value.example\r\nContent-Type",
        ),
    )
    .await;
    assert_eq!(support::status(&origin), 403);
    server.stop().await;

    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    // The capture works: the rejections and the timeout were logged.
    assert!(logs.contains("missing or wrong bearer token"), "{logs}");
    assert!(logs.contains("Origin present"), "{logs}");
    assert!(logs.contains("header"), "{logs}");
    assert!(
        logs.contains(&addr.ip().to_string()),
        "peer not logged: {logs}"
    );
    for value in [
        secret.as_str(),
        rotated.as_str(),
        wrong.as_str(),
        "origin-value.example",
        "Bearer",
        "version",
    ] {
        assert!(!logs.contains(value), "log contains {value}: {logs}");
    }
}
