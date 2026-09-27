//! The control API's HTTP stack over a real loopback socket: `cs_daemon::serve`
//! (the daemon's server), hyper's HTTP/1.1 parsing, the layers and dispatch,
//! driven with raw bytes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cs_daemon::http::{HttpConfig, StaticToken, router};
use cs_daemon::rpc::{Handler, RpcError};
use cs_daemon::serve::{self, ServeConfig};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOKEN: &str = "e2e-token-0123456789abcdef";

struct Version;

impl Handler for Version {
    async fn call(&self, method: &str, _params: Option<Value>) -> Result<Value, RpcError> {
        match method {
            "version" => Ok(json!({ "daemonVersion": "0.1.0" })),
            _ => Err(RpcError::method_not_found()),
        }
    }
}

/// Fails instead of hanging CI if a regression keeps the connection open.
const EXCHANGE_LIMIT: Duration = Duration::from_secs(5);

async fn exchange(addr: SocketAddr, raw_request: String) -> String {
    let round_trip = async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(raw_request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    };
    tokio::time::timeout(EXCHANGE_LIMIT, round_trip)
        .await
        .expect("the server kept the connection open")
}

fn post(addr: SocketAddr, authorization: &str, body: &str) -> String {
    format!(
        "POST /rpc HTTP/1.1\r\nHost: {addr}\r\n{authorization}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn whole_stack_over_a_loopback_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(
        HttpConfig::for_listener(&listener).unwrap(),
        Arc::new(StaticToken::new(TOKEN)),
        Arc::new(Version),
    );
    let server = serve::spawn(listener, app, ServeConfig::default());
    let bearer = format!("Authorization: Bearer {TOKEN}\r\n");
    let call = r#"{"jsonrpc":"2.0","method":"version","id":1}"#;

    let ok = exchange(addr, post(addr, &bearer, call)).await;
    let unauthorized = exchange(addr, post(addr, "", call)).await;
    let forbidden = exchange(
        addr,
        post(
            addr,
            &format!("{bearer}Origin: http://evil.example\r\n"),
            call,
        ),
    )
    .await;

    assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "{ok}");
    let body = ok.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(
        serde_json::from_str::<Value>(body).unwrap(),
        json!({ "jsonrpc": "2.0", "result": { "daemonVersion": "0.1.0" }, "id": 1 })
    );
    assert!(
        unauthorized.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "{unauthorized}"
    );
    assert!(
        forbidden.starts_with("HTTP/1.1 403 Forbidden\r\n"),
        "{forbidden}"
    );
    server.stop().await;
}
