//! Manual end-to-end check: the proxy on a fixed loopback port in front of a
//! mock upstream that streams SSE events 500 ms apart, for `curl -N`.
//!
//! ```sh
//! CS_PROXY_E2E_PORT=47821 CS_PROXY_E2E_SECS=20 \
//!   cargo test -p cs-proxy --test e2e_serve -- --ignored --nocapture
//! curl -sN --noproxy '*' http://127.0.0.1:47821/v1/messages?beta=true \
//!   -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
//!   -d '{"stream":true}'
//! ```
//!
//! When the time is up it prints each recorded call as JSON.

mod common;

use std::time::Duration;

use bytes::Bytes;
use common::*;

const EVENTS: [&str; 6] = [
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_e2e\",\"model\":\"claude-e2e\",\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n",
    "event: ping\ndata: {\"type\": \"ping\"}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
];

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[tokio::test]
#[ignore = "manual end-to-end run for curl; serves for CS_PROXY_E2E_SECS"]
async fn serve_for_curl() {
    let port: u16 = env_or("CS_PROXY_E2E_PORT", 47821);
    let seconds: u64 = env_or("CS_PROXY_E2E_SECS", 20);
    let upstream = MockUpstream::start(|_| {
        let (tx, body) = channel_body();
        tokio::spawn(async move {
            for event in EVENTS {
                if tx
                    .send(Ok(Bytes::from_static(event.as_bytes())))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
        response(
            200,
            &[
                ("content-type", "text/event-stream"),
                ("request-id", "req_e2e"),
            ],
            body,
        )
    })
    .await;
    let proxy = TestProxy::start_on(&format!("127.0.0.1:{port}"), &upstream.base(), false).await;
    println!("mock upstream on {}", upstream.addr);
    println!("proxy on http://{} for {seconds} s", proxy.addr);
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    for call in proxy.sink.calls() {
        println!(
            "recorded run={} {}",
            call.run_id,
            serde_json::to_string(&call.record).unwrap()
        );
    }
}
