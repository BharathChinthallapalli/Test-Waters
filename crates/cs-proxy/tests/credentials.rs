//! Credentials reach the upstream and nothing else (design, requirement 2):
//! `x-api-key`, `Authorization: Bearer` and both together (`apiKeyHelper`
//! [GWC]) are forwarded unchanged, and appear in no `PendingCall` (serialized
//! and searched, capture on) and in no tracing output.
//!
//! Tracing output is captured by a subscriber that writes every event and span
//! field, at every level, into a string. The test runs on a current-thread
//! runtime so every task, the proxy's included, logs through it.

mod common;

use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use cs_core::llm::CallOutcome;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

const API_KEY: &str = "sk-ant-api03-SECRETKEYVALUE-0123456789";
const TOKEN: &str = "SECRETBEARERTOKEN-abcdef";

/// Writes every event and span, with all fields, into `out`.
#[derive(Clone, Default)]
struct CaptureLog {
    out: Arc<Mutex<String>>,
    next_id: Arc<AtomicU64>,
}

struct Fields<'a>(&'a mut String);

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}

impl Subscriber for CaptureLog {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        let mut out = self.out.lock().unwrap();
        let _ = write!(out, "span {}", attributes.metadata().name());
        attributes.record(&mut Fields(&mut out));
        out.push('\n');
        Id::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _: &Id, values: &Record<'_>) {
        let mut out = self.out.lock().unwrap();
        values.record(&mut Fields(&mut out));
        out.push('\n');
    }

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut out = self.out.lock().unwrap();
        let metadata = event.metadata();
        let _ = write!(out, "{} {}", metadata.level(), metadata.target());
        event.record(&mut Fields(&mut out));
        out.push('\n');
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

fn assert_secret_free(what: &str, text: &str) {
    for secret in [API_KEY, TOKEN, "SECRETKEYVALUE", "SECRETBEARERTOKEN"] {
        assert!(
            !text.contains(secret),
            "{what} contains a credential:\n{text}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn credentials_reach_the_upstream_and_nowhere_else() {
    let log = CaptureLog::default();
    let _guard = tracing::subscriber::set_default(log.clone());

    let upstream = MockUpstream::start(|_| {
        response(
            200,
            &[
                ("content-type", "application/json"),
                ("content-length", "2"),
            ],
            full("{}"),
        )
    })
    .await;
    // Capture on, so the stored content is searched too.
    let proxy = TestProxy::start(&upstream.base(), true).await;
    let bearer = format!("Bearer {TOKEN}");
    let forms: [&[(&str, &str)]; 3] = [
        &[("x-api-key", API_KEY)],
        &[("authorization", &bearer)],
        &[("x-api-key", API_KEY), ("authorization", &bearer)],
    ];
    for form in forms {
        let mut headers = vec![("anthropic-version", "2023-06-01")];
        headers.extend_from_slice(form);
        let request = request_bytes(
            "POST",
            "/v1/messages",
            &proxy.host(),
            &headers,
            b"{\"q\":1}",
        );
        let (head, _) = RawClient::exchange(proxy.addr, &request).await;
        assert_eq!(head.status, 200);
    }

    // The same credentials against a dead upstream, for the error path's log.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = closed.local_addr().unwrap();
    drop(closed);
    let unreachable = TestProxy::start(&format!("http://{dead}"), true).await;
    let request = request_bytes("POST", "/v1/messages", &unreachable.host(), forms[2], b"{}");
    let (head, body) = RawClient::exchange(unreachable.addr, &request).await;
    assert_eq!(head.status, 502);
    assert_secret_free("the 502 body", &String::from_utf8_lossy(&body));

    // The upstream got every credential unchanged.
    let received = upstream.received();
    assert_eq!(received.len(), 3);
    assert_eq!(received[0].headers.get("x-api-key").unwrap(), API_KEY);
    assert!(received[0].headers.get("authorization").is_none());
    assert_eq!(
        received[1].headers.get("authorization").unwrap(),
        bearer.as_str()
    );
    assert!(received[1].headers.get("x-api-key").is_none());
    assert_eq!(received[2].headers.get("x-api-key").unwrap(), API_KEY);
    assert_eq!(
        received[2].headers.get("authorization").unwrap(),
        bearer.as_str()
    );

    // No record holds them.
    let mut calls = proxy.sink.wait_for(3).await;
    calls.extend(unreachable.sink.wait_for(1).await);
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[3].record.outcome, CallOutcome::UpstreamUnreachable);
    for call in &calls {
        let mut text = serde_json::to_string(&call.record).unwrap();
        text.push_str(&call.run_id);
        assert_eq!(call.content.len(), 2, "capture was on");
        for item in &call.content {
            text.push_str(&String::from_utf8_lossy(item));
        }
        assert_secret_free("a PendingCall", &text);
    }

    // Nor does the log, which did capture the proxy's own events.
    let output = log.out.lock().unwrap().clone();
    assert!(
        output.contains("proxied call"),
        "log capture is working:\n{output}"
    );
    assert!(output.contains("provider unreachable"), "{output}");
    assert_secret_free("the tracing output", &output);
}
