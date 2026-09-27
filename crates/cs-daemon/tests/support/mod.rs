//! Helpers shared by the daemon's integration tests: raw HTTP/1.1 over a loopback
//! socket, so the tests need no HTTP client.

#![allow(dead_code)] // Each test file uses a different subset.

use std::net::SocketAddr;
use std::time::Duration;

use cs_daemon::rpc::{Handler, RpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Answers `version` like the daemon's bootstrap handler, and `slow` after 300 ms.
pub struct TestHandler;

impl Handler for TestHandler {
    async fn call(&self, method: &str, _params: Option<Value>) -> Result<Value, RpcError> {
        match method {
            "version" => Ok(json!({ "daemonVersion": "test" })),
            "slow" => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(json!("slow"))
            }
            _ => Err(RpcError::method_not_found()),
        }
    }
}

pub const VERSION_CALL: &str = r#"{"jsonrpc":"2.0","method":"version","id":1}"#;

/// Fails instead of hanging CI if the server keeps a connection open.
pub const EXCHANGE_LIMIT: Duration = Duration::from_secs(10);

/// A `POST /rpc` with `Connection: close`; `token` adds `Authorization: Bearer`.
pub fn post(addr: SocketAddr, token: Option<&str>, body: &str) -> String {
    let authorization = token.map_or_else(String::new, |token| {
        format!("Authorization: Bearer {token}\r\n")
    });
    format!(
        "POST /rpc HTTP/1.1\r\nHost: {addr}\r\n{authorization}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Sends `raw_request` and reads until the server closes the connection.
pub async fn exchange(addr: SocketAddr, raw_request: &str) -> String {
    let round_trip = async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(raw_request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        // A reset after the response counts as the end, like a close.
        let _ = stream.read_to_end(&mut response).await;
        String::from_utf8(response).unwrap()
    };
    tokio::time::timeout(EXCHANGE_LIMIT, round_trip)
        .await
        .expect("the server kept the connection open")
}

/// The status code of a raw HTTP/1.1 response, or 0 for no response.
pub fn status(response: &str) -> u16 {
    response
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

/// `POST /rpc` calling `version`, returning the status code.
pub async fn call_version(addr: SocketAddr, token: Option<&str>) -> u16 {
    status(&exchange(addr, &post(addr, token, VERSION_CALL)).await)
}
