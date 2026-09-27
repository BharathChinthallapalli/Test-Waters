//! The proxy's listener, in-process (`cs_daemon::proxy::start`, served by
//! `cs_daemon::serve::spawn_proxy`): unlike the control API it has no request
//! timeout, and like it, it has a header-read timeout.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cs_daemon::http::REQUEST_TIMEOUT;
use cs_daemon::proxy::{self, RunningProxy};
use cs_daemon::serve::ServeConfig;
use cs_proxy::Upstream;
use cs_store::Store;
use cs_store::secrets::InMemorySecretStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Short, so the header-read test is fast; the no-timeout test shows it only
/// bounds reading a request head, not the response.
const HEADER_READ_TIMEOUT: Duration = Duration::from_millis(500);

/// Any wait in a test fails after this.
const LIMIT: Duration = Duration::from_secs(30);

/// The proxy on `127.0.0.1:0` in front of `upstream`, with a store in a
/// temporary directory.
async fn start_proxy(upstream: &str) -> (tempfile::TempDir, RunningProxy) {
    let root = tempfile::tempdir().unwrap();
    let dir = cs_daemon::instance::prepare_data_dir(&root.path().join("data")).unwrap();
    let store = Arc::new(Store::open(&dir, Arc::new(InMemorySecretStore::new([5; 32]))).unwrap());
    let (listener, address) = proxy::bind(&dir, Some("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap();
    let running = proxy::start(
        listener,
        address,
        Upstream::parse(upstream).unwrap(),
        1,
        store,
        ServeConfig {
            header_read_timeout: HEADER_READ_TIMEOUT,
            drain_timeout: Duration::from_secs(1),
        },
    )
    .unwrap();
    (root, running)
}

/// An upstream that reads one request, waits `head_delay` before its response
/// head, then sends two chunks a second apart.
async fn slow_upstream(head_delay: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "the proxy closed the upstream connection");
            request.extend_from_slice(&chunk[..read]);
        }
        tokio::time::sleep(head_delay).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ntransfer-encoding: chunked\r\n\
                  connection: close\r\n\r\n5\r\nfirst\r\n",
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        stream.write_all(b"6\r\nsecond\r\n0\r\n\r\n").await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn a_response_slower_than_the_control_api_request_timeout_still_arrives() {
    // The control API answers 408 once REQUEST_TIMEOUT passes without a response
    // head (tower-http's `TimeoutLayer` bounds the response future). A
    // non-streamed model call can take longer than that before its head.
    let upstream = slow_upstream(REQUEST_TIMEOUT + Duration::from_secs(1)).await;
    let (_root, proxy) = start_proxy(&upstream).await;
    let started = Instant::now();

    let mut client = TcpStream::connect(proxy.address).await.unwrap();
    client
        .write_all(
            format!(
                "GET /v1/models HTTP/1.1\r\nhost: {}\r\nconnection: close\r\n\r\n",
                proxy.address
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(LIMIT, client.read_to_end(&mut response))
        .await
        .expect("no response")
        .unwrap();

    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    assert!(
        response.contains("first") && response.contains("second"),
        "{response}"
    );
    assert!(started.elapsed() > REQUEST_TIMEOUT + Duration::from_secs(1));
    proxy.server.stop().await;
}

#[tokio::test]
async fn a_client_that_never_finishes_its_head_is_disconnected() {
    let (_root, proxy) = start_proxy("http://127.0.0.1:9").await;
    let started = Instant::now();

    let mut client = TcpStream::connect(proxy.address).await.unwrap();
    client
        .write_all(b"POST /v1/messages HTTP/1.1\r\nhost: 127.0.0.1")
        .await
        .unwrap();
    let mut response = Vec::new();
    // A reset counts as a close too.
    let _ = tokio::time::timeout(LIMIT, client.read_to_end(&mut response))
        .await
        .expect("the proxy kept a half-sent request open");

    assert!(
        response.is_empty(),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert!(started.elapsed() >= HEADER_READ_TIMEOUT, "closed too early");
    proxy.server.stop().await;
}
