//! Golden transparency tests for the proxy service (design, requirements 1, 2,
//! 5 and 8; "Testing"): a real mock upstream and the proxy on loopback, driven
//! by a raw HTTP/1.1 client.
//!
//! `ResponseObserver` belongs to task 2, so these tests assert bytes, status,
//! outcome and timing, never usage or other parsed fields.

mod common;

use std::io;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use common::*;
use cs_core::llm::CallOutcome;
use cs_proxy::forward::MAX_REQUEST_BYTES;
use tokio::sync::mpsc;

const DATE: &str = "Sun, 27 Sep 2026 14:00:00 GMT";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// A mock whose every response body is a channel; each new channel's sender
/// arrives on the returned receiver.
async fn streaming_upstream(
    status: u16,
    headers: &'static [(&'static str, &'static str)],
) -> (
    MockUpstream,
    mpsc::UnboundedReceiver<mpsc::Sender<io::Result<Bytes>>>,
) {
    let (senders_tx, senders_rx) = mpsc::unbounded_channel();
    let upstream = MockUpstream::start(move |_| {
        let (tx, body) = channel_body();
        senders_tx.send(tx).unwrap();
        response(status, headers, body)
    })
    .await;
    (upstream, senders_rx)
}

/// A mock answering every request with `200` and a small JSON body.
async fn ok_upstream() -> MockUpstream {
    MockUpstream::start(|_| {
        response(
            200,
            &[
                ("content-type", "application/json"),
                ("content-length", "2"),
            ],
            full("{}"),
        )
    })
    .await
}

/// Reads chunks until the body so far ends with `want`, returning it.
async fn read_until(client: &mut RawClient, want: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    while !body.ends_with(want) {
        let chunk = tokio::time::timeout(WAIT, client.next_chunk())
            .await
            .expect("timed out waiting for the next chunk")
            .expect("the body ended early");
        body.extend_from_slice(&chunk);
    }
    body
}

// ------------------------------------------------------------ transparency

#[tokio::test]
async fn a_json_call_passes_byte_for_byte_both_ways() {
    let response_body = br#"{"id":"msg_01","type":"message","role":"assistant","content":[{"type":"text","text":"hi"}],"model":"claude-x","stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":1}}"#;
    let length = response_body.len().to_string();
    let response_headers: Vec<(&str, String)> = vec![
        ("date", DATE.into()),
        ("content-type", "application/json".into()),
        ("content-length", length.clone()),
        ("request-id", "req_json".into()),
        ("anthropic-ratelimit-requests-remaining", "99".into()),
        ("anthropic-organization-id", "org-1".into()),
        ("x-should-retry", "false".into()),
        ("set-cookie", "a=1".into()),
        ("set-cookie", "b=2".into()),
    ];
    let mock_headers = response_headers.clone();
    let upstream = MockUpstream::start(move |_| {
        let pairs: Vec<(&str, &str)> = mock_headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
        response(200, &pairs, full(&response_body[..]))
    })
    .await;
    let proxy = TestProxy::start(&upstream.base(), false).await;

    // Odd spacing and key order: the upstream must see exactly these bytes.
    let body = br#"{"model": "claude-x",  "max_tokens":1,"system":[{"type":"text","text":"a"}],"messages":[{"role":"user","content":"hi"}]}"#;
    let kept: &[(&str, &str)] = &[
        ("content-type", "application/json"),
        ("accept", "application/json"),
        ("anthropic-version", "2023-06-01"),
        ("anthropic-beta", "a,b,oauth-2025-04-20"),
        ("anthropic-dangerous-direct-browser-access", "false"),
        ("x-api-key", "sk-ant-json-test"),
        ("user-agent", "claude-cli/2.1.300 (external, cli)"),
        ("x-claude-code-session-id", "s-json"),
        ("x-multi", "one"),
        ("x-multi", "two"),
        (
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        ),
    ];
    let stripped: &[(&str, &str)] = &[
        ("accept-encoding", "gzip, deflate, br"),
        ("connection", "keep-alive, x-hop"),
        ("x-hop", "1"),
        ("keep-alive", "timeout=5"),
        ("x-callsheet-run", "json-run"),
    ];
    let all: Vec<(&str, &str)> = kept.iter().chain(stripped).copied().collect();
    let request = request_bytes("POST", "/v1/messages?beta=true", &proxy.host(), &all, body);
    let before = now_ms();
    let (head, client_body) = RawClient::exchange(proxy.addr, &request).await;

    // Upstream side.
    let received = upstream.received();
    assert_eq!(received.len(), 1);
    let got = &received[0];
    assert_eq!(got.method, "POST");
    assert_eq!(got.target, "/v1/messages?beta=true");
    assert_eq!(&got.body[..], &body[..]);
    let content_length = body.len().to_string();
    let upstream_host = upstream.addr.to_string();
    let mut expected: Vec<(&str, &str)> = kept.to_vec();
    expected.push(("host", &upstream_host));
    expected.push(("content-length", &content_length));
    assert_eq!(sorted_map(&got.headers), sorted_pairs(&expected));

    // Client side.
    assert_eq!(head.status, 200);
    let pairs: Vec<(&str, &str)> = response_headers
        .iter()
        .map(|(n, v)| (*n, v.as_str()))
        .collect();
    assert_eq!(head.sorted(), sorted_pairs(&pairs));
    assert_eq!(client_body, response_body);

    // Record.
    let calls = proxy.sink.wait_for(1).await;
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.run_id, "json-run");
    assert!(call.content.is_empty(), "capture is off");
    let record = &call.record;
    assert_eq!(record.provider, "anthropic");
    assert_eq!(record.method, "POST");
    assert_eq!(record.path, "/v1/messages");
    assert_eq!(record.status, 200);
    assert_eq!(record.outcome, CallOutcome::Completed);
    assert_eq!(record.request_bytes, body.len() as u64);
    assert_eq!(record.response_bytes, response_body.len() as u64);
    assert_eq!(record.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(
        record.user_agent.as_deref(),
        Some("claude-cli/2.1.300 (external, cli)")
    );
    assert_eq!(record.content_truncated, None);
    assert!(record.started_at_ms >= before && record.started_at_ms <= now_ms());
    assert!(record.ttfb_ms.unwrap() <= record.duration_ms);
}

#[tokio::test]
async fn an_sse_stream_passes_unchanged_and_as_it_arrives() {
    const HEADERS: &[(&str, &str)] = &[
        ("date", DATE),
        ("content-type", "text/event-stream; charset=utf-8"),
        ("cache-control", "no-cache"),
        ("request-id", "req_sse"),
        ("anthropic-ratelimit-tokens-remaining", "1000"),
    ];
    let events: [&[u8]; 5] = [
        b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-x\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
        b"event: ping\ndata: {\"type\": \"ping\"}\n\n",
        b"event: callsheet_future_event\ndata: {\"type\":\"callsheet_future_event\",\"x\":[1,2]}\n\n",
        b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"h\xc3\xa9\"}}\n\n",
        b"event: error\ndata: {\"type\": \"error\", \"error\": {\"type\": \"overloaded_error\", \"message\": \"Overloaded\"}}\n\n",
    ];
    let (upstream, mut senders) = streaming_upstream(200, HEADERS).await;
    let proxy = TestProxy::start(&upstream.base(), false).await;

    let mut client = RawClient::connect(proxy.addr).await;
    let body = br#"{"model":"claude-x","stream":true}"#;
    let headers = [
        ("content-type", "application/json"),
        ("anthropic-version", "2023-06-01"),
    ];
    client
        .send(&request_bytes(
            "POST",
            "/v1/messages?beta=true",
            &proxy.host(),
            &headers,
            body,
        ))
        .await
        .unwrap();
    let upstream_tx = tokio::time::timeout(WAIT, senders.recv())
        .await
        .unwrap()
        .unwrap();
    upstream_tx
        .send(Ok(Bytes::from_static(events[0])))
        .await
        .unwrap();

    let head = client.read_head().await;
    assert_eq!(head.status, 200);
    let mut expected_headers = HEADERS.to_vec();
    expected_headers.push(("transfer-encoding", "chunked"));
    assert_eq!(head.sorted(), sorted_pairs(&expected_headers));

    // The first event reaches the client while the upstream still holds the rest.
    let mut received = read_until(&mut client, events[0]).await;
    assert_eq!(received, events[0]);
    let released = Instant::now();
    tokio::time::sleep(Duration::from_millis(150)).await;
    for event in &events[1..] {
        upstream_tx
            .send(Ok(Bytes::from_static(event)))
            .await
            .unwrap();
    }
    drop(upstream_tx);
    while let Some(chunk) = client.next_chunk().await {
        received.extend_from_slice(&chunk);
    }
    assert!(released.elapsed() >= Duration::from_millis(150));
    assert_eq!(received, events.concat());

    let calls = proxy.sink.wait_for(1).await;
    let record = &calls[0].record;
    assert_eq!(record.status, 200);
    // `completed` or, once the observer reports the `error` event,
    // `upstreamError`; the rule itself is unit-tested in `forward`.
    assert!(
        matches!(
            record.outcome,
            CallOutcome::Completed | CallOutcome::UpstreamError
        ),
        "{:?}",
        record.outcome
    );
    assert_eq!(record.response_bytes, events.concat().len() as u64);
    let ttfb = record.ttfb_ms.unwrap();
    assert!(
        ttfb + 150 <= record.duration_ms,
        "ttfb {ttfb} ms, duration {} ms",
        record.duration_ms
    );
    assert_eq!(calls[0].run_id, DEFAULT_RUN);
}

#[tokio::test]
async fn a_429_passes_unchanged_and_is_an_upstream_error() {
    const BODY: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"},"request_id":"req_429"}"#;
    let length = BODY.len().to_string();
    let headers: Vec<(&'static str, String)> = vec![
        ("date", DATE.into()),
        ("content-type", "application/json".into()),
        ("content-length", length),
        ("retry-after", "30".into()),
        ("x-should-retry", "true".into()),
        ("request-id", "req_429".into()),
        ("anthropic-ratelimit-tokens-remaining", "0".into()),
    ];
    let mock_headers = headers.clone();
    let upstream = MockUpstream::start(move |_| {
        let pairs: Vec<(&str, &str)> = mock_headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
        response(429, &pairs, full(BODY))
    })
    .await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let request = request_bytes(
        "POST",
        "/v1/messages",
        &proxy.host(),
        &[("content-type", "application/json")],
        b"{}",
    );
    let (head, body) = RawClient::exchange(proxy.addr, &request).await;
    assert_eq!(head.status, 429);
    let pairs: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
    assert_eq!(head.sorted(), sorted_pairs(&pairs));
    assert_eq!(body, BODY.as_bytes());

    let record = &proxy.sink.wait_for(1).await[0].record;
    assert_eq!(record.status, 429);
    assert_eq!(record.outcome, CallOutcome::UpstreamError);
}

#[tokio::test]
async fn a_redirect_reaches_the_client_and_is_not_followed() {
    let upstream = MockUpstream::start(|_| {
        response(
            307,
            &[
                ("location", "http://127.0.0.1:9/v1/elsewhere"),
                ("content-length", "0"),
            ],
            full(""),
        )
    })
    .await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let request = request_bytes("GET", "/v1/models", &proxy.host(), &[], b"");
    let (head, body) = RawClient::exchange(proxy.addr, &request).await;
    assert_eq!(head.status, 307);
    assert_eq!(
        head.get("location"),
        Some(&b"http://127.0.0.1:9/v1/elsewhere"[..])
    );
    assert!(body.is_empty());
    assert_eq!(upstream.received().len(), 1);
    assert_eq!(proxy.sink.wait_for(1).await[0].record.status, 307);
}

// ------------------------------------------------------------ guard

#[tokio::test]
async fn origin_or_a_foreign_host_gets_403() {
    let upstream = ok_upstream().await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let port = proxy.addr.port();

    let origin = request_bytes(
        "POST",
        "/v1/messages",
        &proxy.host(),
        &[("origin", "https://evil.example")],
        b"{}",
    );
    let wrong_hosts = [
        "evil.example".to_owned(),
        format!("evil.example:{port}"),
        format!("127.0.0.1:{}", port.wrapping_add(1)),
        format!("0.0.0.0:{port}"),
        format!("[::1]:{port}"),
    ];
    let mut refused = vec![origin];
    for host in &wrong_hosts {
        refused.push(request_bytes("POST", "/v1/messages", host, &[], b"{}"));
    }
    for request in refused {
        let (head, body) = RawClient::exchange(proxy.addr, &request).await;
        assert_eq!(head.status, 403, "{}", String::from_utf8_lossy(&request));
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "permission_error");
    }
    assert!(
        upstream.received().is_empty(),
        "nothing reached the upstream"
    );

    // Both loopback names with the right port are allowed.
    for host in [format!("127.0.0.1:{port}"), format!("LOCALHOST:{port}")] {
        let request = request_bytes("POST", "/v1/messages", &host, &[], b"{}");
        let (head, _) = RawClient::exchange(proxy.addr, &request).await;
        assert_eq!(head.status, 200, "{host}");
    }
    assert_eq!(upstream.received().len(), 2);
    assert_eq!(
        proxy.sink.wait_for(2).await.len(),
        2,
        "refused requests aren't recorded"
    );
}

// ------------------------------------------------------------ body cap

#[tokio::test]
async fn a_declared_body_over_32_mib_gets_413_before_it_is_read() {
    let upstream = ok_upstream().await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let length = (MAX_REQUEST_BYTES + 1).to_string();
    let request = request_bytes(
        "POST",
        "/v1/messages",
        &proxy.host(),
        &[("content-length", &length)],
        b"",
    );
    let (head, body) = RawClient::exchange(proxy.addr, &request).await;
    assert_eq!(head.status, 413);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["type"], "request_too_large");
    assert!(upstream.received().is_empty());
}

#[tokio::test]
async fn a_streamed_body_of_32_mib_plus_one_byte_gets_413() {
    let upstream = ok_upstream().await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let mut client = RawClient::connect(proxy.addr).await;
    let head = request_bytes(
        "POST",
        "/v1/messages",
        &proxy.host(),
        &[("transfer-encoding", "chunked")],
        b"",
    );
    client.send(&head).await.unwrap();
    // 32 MiB in 1 MiB chunks, then the one byte too many and the last chunk.
    let mib = vec![b'x'; 1024 * 1024];
    let mut chunk = format!("{:x}\r\n", mib.len()).into_bytes();
    chunk.extend_from_slice(&mib);
    chunk.extend_from_slice(b"\r\n");
    for _ in 0..MAX_REQUEST_BYTES / mib.len() {
        client.send(&chunk).await.unwrap();
    }
    // The proxy may answer and close as soon as it has read the extra byte.
    let _ = client.send(b"1\r\ny\r\n0\r\n\r\n").await;
    let head = client.read_head().await;
    assert_eq!(head.status, 413);
    let body: serde_json::Value = serde_json::from_slice(&client.read_body(&head).await).unwrap();
    assert_eq!(body["error"]["type"], "request_too_large");
    assert!(upstream.received().is_empty());
}

#[tokio::test]
async fn a_body_of_exactly_32_mib_is_forwarded() {
    let upstream = ok_upstream().await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let body = vec![b'z'; MAX_REQUEST_BYTES];
    let request = request_bytes("POST", "/v1/messages", &proxy.host(), &[], &body);
    let (head, _) = RawClient::exchange(proxy.addr, &request).await;
    assert_eq!(head.status, 200);
    assert_eq!(upstream.received()[0].body.len(), MAX_REQUEST_BYTES);
}

// ------------------------------------------------------------ run grouping

#[tokio::test]
async fn the_run_comes_from_x_callsheet_run_then_the_session_then_the_default() {
    let upstream = ok_upstream().await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let cases: [&[(&str, &str)]; 3] = [
        &[
            ("x-callsheet-run", "my-run"),
            ("x-claude-code-session-id", "s1"),
        ],
        &[("x-claude-code-session-id", "s1")],
        &[],
    ];
    for headers in cases {
        let request = request_bytes("POST", "/v1/messages", &proxy.host(), headers, b"{}");
        let (head, _) = RawClient::exchange(proxy.addr, &request).await;
        assert_eq!(head.status, 200);
    }
    let runs: Vec<String> = proxy
        .sink
        .wait_for(3)
        .await
        .into_iter()
        .map(|call| call.run_id)
        .collect();
    assert_eq!(runs, ["my-run", "cc-s1", DEFAULT_RUN]);

    let received = upstream.received();
    assert!(
        received[0].headers.get("x-callsheet-run").is_none(),
        "stripped"
    );
    for request in &received[..2] {
        assert_eq!(
            request.headers.get("x-claude-code-session-id").unwrap(),
            "s1",
            "the session header is forwarded"
        );
    }
}

// ------------------------------------------------------------ endings

#[tokio::test]
async fn a_client_that_leaves_mid_stream_cancels_the_upstream_call() {
    const HEADERS: &[(&str, &str)] = &[("content-type", "text/event-stream")];
    let first: &[u8] = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n";
    let (upstream, mut senders) = streaming_upstream(200, HEADERS).await;
    let proxy = TestProxy::start(&upstream.base(), true).await;

    let mut client = RawClient::connect(proxy.addr).await;
    client
        .send(&request_bytes(
            "POST",
            "/v1/messages",
            &proxy.host(),
            &[],
            b"{\"stream\":true}",
        ))
        .await
        .unwrap();
    let upstream_tx = tokio::time::timeout(WAIT, senders.recv())
        .await
        .unwrap()
        .unwrap();
    upstream_tx
        .send(Ok(Bytes::from_static(first)))
        .await
        .unwrap();
    let head = client.read_head().await;
    assert_eq!(head.status, 200);
    assert_eq!(read_until(&mut client, first).await, first);
    drop(client);

    // The upstream keeps streaming until it notices its connection is gone.
    let closed = tokio::time::timeout(WAIT, async {
        loop {
            let ping = Bytes::from_static(b"event: ping\ndata: {\"type\": \"ping\"}\n\n");
            if upstream_tx.send(Ok(ping)).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(closed.is_ok(), "the upstream connection was not closed");

    let calls = proxy.sink.wait_for(1).await;
    assert_eq!(calls.len(), 1, "recorded once");
    let record = &calls[0].record;
    assert_eq!(record.outcome, CallOutcome::ClientCancelled);
    assert_eq!(record.status, 200);
    // Capture is on: what the client was sent before it left.
    assert_eq!(calls[0].content[0], b"{\"stream\":true}");
    assert!(calls[0].content[1].starts_with(first));
}

#[tokio::test]
async fn a_client_that_leaves_before_the_response_head_cancels_the_call() {
    // An upstream that reads the request, never answers, and reports when its
    // connection closes.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    let (got_request_tx, got_request) = tokio::sync::oneshot::channel();
    let (closed_tx, closed) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = silent.accept().await.unwrap();
        let mut got_request_tx = Some(got_request_tx);
        let mut buf = [0u8; 4096];
        loop {
            if stream.readable().await.is_err() {
                break;
            }
            match stream.try_read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(tx) = got_request_tx.take() {
                        let _ = tx.send(());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }
        let _ = closed_tx.send(());
    });
    let proxy = TestProxy::start(&format!("http://{silent_addr}"), false).await;

    let client = RawClient::connect(proxy.addr).await;
    client
        .send(&request_bytes(
            "POST",
            "/v1/messages",
            &proxy.host(),
            &[],
            b"{}",
        ))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, got_request)
        .await
        .unwrap()
        .unwrap();
    drop(client);
    tokio::time::timeout(WAIT, closed)
        .await
        .expect("the upstream connection was not closed")
        .unwrap();

    let calls = proxy.sink.wait_for(1).await;
    let record = &calls[0].record;
    assert_eq!(record.outcome, CallOutcome::ClientCancelled);
    assert_eq!(record.status, 0, "the client got no response");
    assert_eq!(record.ttfb_ms, None);
}

#[tokio::test]
async fn an_upstream_failure_mid_body_is_incomplete() {
    const HEADERS: &[(&str, &str)] = &[("content-type", "text/event-stream")];
    let first: &[u8] = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n";
    let (upstream, mut senders) = streaming_upstream(200, HEADERS).await;
    let proxy = TestProxy::start(&upstream.base(), false).await;

    let mut client = RawClient::connect(proxy.addr).await;
    client
        .send(&request_bytes(
            "POST",
            "/v1/messages",
            &proxy.host(),
            &[],
            b"{}",
        ))
        .await
        .unwrap();
    let upstream_tx = tokio::time::timeout(WAIT, senders.recv())
        .await
        .unwrap()
        .unwrap();
    upstream_tx
        .send(Ok(Bytes::from_static(first)))
        .await
        .unwrap();
    let head = client.read_head().await;
    assert_eq!(read_until(&mut client, first).await, first);
    upstream_tx
        .send(Err(io::Error::other("connection reset")))
        .await
        .unwrap();
    // The client's body ends after the bytes that did arrive.
    assert!(head.is_chunked());
    assert_eq!(client.next_chunk().await, None);

    let record = &proxy.sink.wait_for(1).await[0].record;
    assert_eq!(record.outcome, CallOutcome::Incomplete);
    assert_eq!(record.response_bytes, first.len() as u64);
}

#[tokio::test]
async fn an_unreachable_upstream_gets_502_in_the_anthropic_shape() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = closed.local_addr().unwrap();
    drop(closed);
    let proxy = TestProxy::start(&format!("http://{dead}"), false).await;
    let request = request_bytes(
        "POST",
        "/v1/messages",
        &proxy.host(),
        &[
            ("x-api-key", "sk-ant-unreachable"),
            ("authorization", "Bearer tok-unreachable"),
        ],
        b"{}",
    );
    let (head, body) = RawClient::exchange(proxy.addr, &request).await;
    assert_eq!(head.status, 502);
    assert_eq!(head.get("content-type"), Some(&b"application/json"[..]));
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains("sk-ant-unreachable") && !text.contains("tok-unreachable"));
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["type"], "error");
    assert_eq!(json["error"]["type"], "api_error");
    let message = json["error"]["message"].as_str().unwrap();
    assert!(message.contains(&dead.to_string()), "{message}");

    let record = &proxy.sink.wait_for(1).await[0].record;
    assert_eq!(record.outcome, CallOutcome::UpstreamUnreachable);
    assert_eq!(record.status, 502);
    assert_eq!(record.ttfb_ms, None);
}

// ------------------------------------------------------------ capture

#[tokio::test]
async fn capture_on_keeps_request_and_response_bodies() {
    let upstream =
        MockUpstream::start(|_| response(200, &[("content-length", "10")], full("0123456789")))
            .await;
    let proxy = TestProxy::start(&upstream.base(), true).await;
    let request = request_bytes("POST", "/v1/messages", &proxy.host(), &[], b"{\"q\":1}");
    RawClient::exchange(proxy.addr, &request).await;
    let call = &proxy.sink.wait_for(1).await[0];
    assert_eq!(
        call.content,
        vec![b"{\"q\":1}".to_vec(), b"0123456789".to_vec()]
    );
    assert_eq!(call.record.content_truncated, None);
}

#[tokio::test]
async fn capture_off_keeps_nothing() {
    let upstream =
        MockUpstream::start(|_| response(200, &[("content-length", "10")], full("0123456789")))
            .await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let request = request_bytes("POST", "/v1/messages", &proxy.host(), &[], b"{\"q\":1}");
    RawClient::exchange(proxy.addr, &request).await;
    let call = &proxy.sink.wait_for(1).await[0];
    assert!(call.content.is_empty());
    assert_eq!(call.record.content_truncated, None);
}

#[tokio::test]
async fn capture_over_the_store_cap_keeps_nothing_and_says_so() {
    let big = Bytes::from(vec![b'r'; cs_store::writer::MAX_CONTENT_BYTES]);
    let length = big.len().to_string();
    let upstream = MockUpstream::start(move |_| {
        response(200, &[("content-length", &length)], full(big.clone()))
    })
    .await;
    let proxy = TestProxy::start(&upstream.base(), true).await;
    let request = request_bytes("POST", "/v1/messages", &proxy.host(), &[], b"{}");
    let (head, body) = RawClient::exchange(proxy.addr, &request).await;
    assert_eq!(head.status, 200);
    assert_eq!(
        body.len(),
        cs_store::writer::MAX_CONTENT_BYTES,
        "the client still gets it all"
    );
    let call = &proxy.sink.wait_for(1).await[0];
    assert!(call.content.is_empty());
    assert_eq!(call.record.content_truncated, Some(true));
}

// ------------------------------------------------------------ not recorded

#[tokio::test]
async fn head_api_hello_is_forwarded_but_not_recorded() {
    let upstream = MockUpstream::start(|request| {
        let length = if request.method == "HEAD" { "5" } else { "2" };
        response(
            200,
            &[("content-length", length)],
            full(if request.method == "HEAD" { "" } else { "{}" }),
        )
    })
    .await;
    let proxy = TestProxy::start(&upstream.base(), false).await;

    let mut client = RawClient::connect(proxy.addr).await;
    client
        .send(&request_bytes(
            "HEAD",
            "/api/hello",
            &proxy.host(),
            &[],
            b"",
        ))
        .await
        .unwrap();
    let head = client.read_head().await;
    assert_eq!(head.status, 200);
    assert_eq!(
        head.content_length(),
        Some(5),
        "the HEAD answer's length passes"
    );

    // A recorded call after it: once it shows up, the HEAD would have too.
    let request = request_bytes("GET", "/v1/models?limit=1000", &proxy.host(), &[], b"");
    RawClient::exchange(proxy.addr, &request).await;
    let calls = proxy.sink.wait_for(1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let calls_later = proxy.sink.calls();
    assert_eq!(calls_later.len(), 1);
    assert_eq!(calls[0].record.path, "/v1/models");
    assert_eq!(calls[0].record.outcome, CallOutcome::Completed);

    let received = upstream.received();
    assert_eq!(received[0].method, "HEAD");
    assert_eq!(received[0].target, "/api/hello");
    assert_eq!(received[1].target, "/v1/models?limit=1000");
}

#[tokio::test]
async fn a_path_the_url_parser_would_change_is_refused() {
    let upstream = ok_upstream().await;
    let proxy = TestProxy::start(&upstream.base(), false).await;
    let request = request_bytes("POST", "/v1/../admin", &proxy.host(), &[], b"{}");
    let (head, body) = RawClient::exchange(proxy.addr, &request).await;
    assert_eq!(head.status, 400);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(upstream.received().is_empty());

    // Only percent-encoded, not changed in meaning: forwarded.
    let request = request_bytes("GET", "/v1/models?after_id='a'", &proxy.host(), &[], b"");
    let (head, _) = RawClient::exchange(proxy.addr, &request).await;
    assert_eq!(head.status, 200);
    assert_eq!(upstream.received()[0].target, "/v1/models?after_id=%27a%27");
}
