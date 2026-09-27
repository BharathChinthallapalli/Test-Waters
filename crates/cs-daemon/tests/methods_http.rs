//! The daemon's real method table (`Methods`, backed by a store and the token
//! file) behind the real HTTP stack and server, over a loopback socket.

mod support;

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use cs_daemon::http::{HttpConfig, router};
use cs_daemon::instance::prepare_data_dir;
use cs_daemon::methods::Methods;
use cs_daemon::serve::{self, ServeConfig, Server};
use cs_daemon::token::{ControlToken, TOKEN_FILE_NAME};
use cs_store::secrets::InMemorySecretStore;
use cs_store::{AppendEvent, Store};
use serde_json::{Value, json};
use support::{exchange, post, status};
use tokio::net::TcpListener;

struct Daemon {
    _root: tempfile::TempDir,
    dir: std::path::PathBuf,
    addr: SocketAddr,
    server: Server,
    store: Arc<Store>,
}

async fn start() -> Daemon {
    let root = tempfile::tempdir().unwrap();
    let dir = prepare_data_dir(&root.path().join("data")).unwrap();
    let store = Arc::new(Store::open(&dir, Arc::new(InMemorySecretStore::new([5; 32]))).unwrap());
    let token = Arc::new(ControlToken::load_or_create(&dir).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let methods = Methods::new(Arc::clone(&store), Arc::clone(&token), Instant::now());
    let app = router(
        HttpConfig::for_listener(&listener).unwrap(),
        token,
        Arc::new(methods),
    );
    let server = serve::spawn(listener, app, ServeConfig::default());
    Daemon {
        _root: root,
        dir,
        addr,
        server,
        store,
    }
}

fn token_in(dir: &Path) -> String {
    fs::read_to_string(dir.join(TOKEN_FILE_NAME)).unwrap()
}

/// Sends `body` and returns the status and the parsed body (`Null` if empty).
async fn call(addr: SocketAddr, token: &str, body: &str) -> (u16, Value) {
    let response = exchange(addr, &post(addr, Some(token), body)).await;
    let payload = response.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap()
    };
    (status(&response), value)
}

#[tokio::test]
async fn token_rotate_over_http_rejects_the_old_token_at_once() {
    let daemon = start().await;
    let old = token_in(&daemon.dir);
    let rotate = r#"{"jsonrpc":"2.0","method":"token.rotate","id":1}"#;
    let health = r#"{"jsonrpc":"2.0","method":"health","id":2}"#;

    let (code, body) = call(daemon.addr, &old, rotate).await;
    assert_eq!(code, 200);
    assert_eq!(body, json!({ "jsonrpc": "2.0", "result": {}, "id": 1 }));

    let (code, _) = call(daemon.addr, &old, health).await;
    assert_eq!(code, 401, "the old token still works");
    // What a client does after a 401: re-read the file once and retry.
    let new = token_in(&daemon.dir);
    assert_ne!(new, old);
    let (code, body) = call(daemon.addr, &new, health).await;
    assert_eq!(code, 200);
    assert_eq!(body["result"]["status"], "ok");

    daemon.server.stop().await;
    daemon.store.close().await.unwrap();
}

#[tokio::test]
async fn every_method_answers_over_http() {
    let daemon = start().await;
    let token = token_in(&daemon.dir);
    let request = |method: &str, params: Value| {
        json!({ "jsonrpc": "2.0", "method": method, "params": params, "id": method }).to_string()
    };

    for (method, params, expected) in [
        (
            "version",
            json!({}),
            json!({ "daemonVersion": env!("CARGO_PKG_VERSION") }),
        ),
        (
            "settings.get",
            json!({}),
            json!({ "captureContent": false }),
        ),
        (
            "settings.setCaptureContent",
            json!({ "enabled": true }),
            json!({ "captureContent": true }),
        ),
        (
            "events.verify",
            json!({}),
            json!({ "ok": true, "eventsChecked": 0, "erasedEvents": 0 }),
        ),
    ] {
        let (code, body) = call(daemon.addr, &token, &request(method, params)).await;
        assert_eq!(code, 200, "{method}");
        assert_eq!(body["result"], expected, "{method}");
        assert_eq!(body["id"], method);
    }

    let (_, body) = call(
        daemon.addr,
        &token,
        &request("content.erasePlan", json!({ "runId": "nope" })),
    )
    .await;
    assert_eq!(body["error"]["code"], 1003);
    let (_, body) = call(daemon.addr, &token, &request("no.such", json!({}))).await;
    assert_eq!(body["error"]["code"], -32601);
    let (_, body) = call(daemon.addr, &token, &request("health", json!([1]))).await;
    assert_eq!(body["error"]["code"], -32602);

    let (code, body) = call(daemon.addr, &token, &request("health", json!({}))).await;
    assert_eq!(code, 200);
    let health = &body["result"];
    assert_eq!(health["captureContent"], true);
    assert_eq!(health["lastGlobalPosition"], 0);
    assert_eq!(health["erasurePending"], false);
    assert_eq!(
        health["schemaVersion"],
        cs_store::migrate::CURRENT_SCHEMA_VERSION
    );

    daemon.server.stop().await;
    daemon.store.close().await.unwrap();
}

#[tokio::test]
async fn a_batch_mixes_calls_and_notifications() {
    let daemon = start().await;
    let token = token_in(&daemon.dir);
    let batch = json!([
        { "jsonrpc": "2.0", "method": "version", "id": 1 },
        { "jsonrpc": "2.0", "method": "settings.setCaptureContent", "params": { "enabled": true } },
        { "jsonrpc": "2.0", "method": "settings.get", "id": 2 },
        { "jsonrpc": "2.0", "method": "nope", "id": 3 },
    ])
    .to_string();

    let (code, body) = call(daemon.addr, &token, &batch).await;

    assert_eq!(code, 200);
    assert_eq!(
        body,
        json!([
            { "jsonrpc": "2.0", "result": { "daemonVersion": env!("CARGO_PKG_VERSION") }, "id": 1 },
            // The notification ran, in order, before this call.
            { "jsonrpc": "2.0", "result": { "captureContent": true }, "id": 2 },
            { "jsonrpc": "2.0", "error": { "code": -32601, "message": "Method not found" }, "id": 3 },
        ])
    );

    daemon.server.stop().await;
    daemon.store.close().await.unwrap();
}

#[tokio::test]
async fn calls_list_answers_over_http() {
    let daemon = start().await;
    let token = token_in(&daemon.dir);
    let request = |params: Value| {
        json!({ "jsonrpc": "2.0", "method": "calls.list", "params": params, "id": 1 }).to_string()
    };

    let (code, body) = call(daemon.addr, &token, &request(json!({}))).await;
    assert_eq!(code, 200);
    assert_eq!(body["result"], json!({ "calls": [] }));

    let call_body = json!({
        "provider": "anthropic", "method": "POST", "path": "/v1/messages", "status": 200,
        "outcome": "completed", "streamed": false, "startedAtMs": 1_790_000_000_000_u64,
        "durationMs": 5, "requestBytes": 10, "responseBytes": 20,
        "rateLimitHeaders": {}, "traceId": "0af7651916cd43dd8448eb211c80319c",
    });
    for (kind, body) in [
        ("llm.call", call_body.clone()),
        ("test.event", json!({})),
        ("llm.call", call_body.clone()),
    ] {
        daemon
            .store
            .append(AppendEvent {
                run_id: "cc-session".to_owned(),
                kind: kind.to_owned(),
                ts_ms: 1_790_000_000_000,
                body,
                content: Vec::new(),
            })
            .await
            .unwrap();
    }

    let (code, body) = call(daemon.addr, &token, &request(json!({ "limit": 1 }))).await;
    assert_eq!(code, 200);
    assert_eq!(
        body["result"],
        json!({
            "calls": [{ "globalPos": 3, "runId": "cc-session", "call": call_body }],
            "nextBefore": 3,
        })
    );
    let (_, body) = call(daemon.addr, &token, &request(json!({ "before": 3 }))).await;
    assert_eq!(body["result"]["calls"][0]["globalPos"], 1);
    assert_eq!(body["result"]["calls"].as_array().map(Vec::len), Some(1));
    assert!(body["result"].get("nextBefore").is_none());

    let (code, body) = call(daemon.addr, &token, &request(json!({ "limit": 0 }))).await;
    assert_eq!(code, 200);
    assert_eq!(
        body["error"],
        json!({ "code": -32602, "message": "Invalid params" })
    );

    daemon.server.stop().await;
    daemon.store.close().await.unwrap();
}
