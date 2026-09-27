//! Peak memory of [`ResponseObserver`] on hostile bodies, measured with a
//! counting global allocator. The allocator is the only reason this is its own
//! test binary: it counts nothing outside this file's tests.
//!
//! The observer's caps (`crates/cs-proxy/src/observe.rs`): a non-streamed body
//! is buffered up to 4 MiB; an SSE line and an event's data up to 1 MiB each.
//! Parsing must add little on top: the bound asserted is twice the bytes the
//! observer may buffer.

// A counting allocator needs `unsafe impl GlobalAlloc`; every method only
// forwards to `System` with the caller's arguments (see SAFETY notes).
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use cs_proxy::ResponseObserver;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue, StatusCode};

const MIB: usize = 1 << 20;
const JSON_CAP: usize = 4 * MIB;
const SSE_LINE_CAP: usize = MIB;
const SSE_DATA_CAP: usize = MIB;

/// Counts live bytes allocated by the thread that turned counting on, so
/// tests running in parallel don't disturb each other.
struct CountingAllocator;

#[derive(Clone, Copy)]
struct Counts {
    on: bool,
    live: isize,
    peak: isize,
}

thread_local! {
    // `const` and no destructor: reading it never allocates.
    static COUNTS: Cell<Counts> = const { Cell::new(Counts { on: false, live: 0, peak: 0 }) };
}

fn record(delta: isize) {
    let _ = COUNTS.try_with(|counts| {
        let mut current = counts.get();
        if current.on {
            current.live += delta;
            current.peak = current.peak.max(current.live);
            counts.set(current);
        }
    });
}

// SAFETY: every method passes its arguments unchanged to `System`, which
// upholds the `GlobalAlloc` contract; `record` never allocates or panics.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size() as isize);
        // SAFETY: the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size() as isize);
        // SAFETY: the caller upholds `alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record(-(layout.size() as isize));
        // SAFETY: the caller upholds `dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Counted as a new block before the old one is freed, as a moving
        // realloc briefly holds both.
        record(new_size as isize);
        // SAFETY: the caller upholds `realloc`'s contract.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        record(-(layout.size() as isize));
        new
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Peak bytes allocated by `run` above what was live when it started.
fn peak_bytes(run: impl FnOnce()) -> usize {
    COUNTS.with(|counts| {
        counts.set(Counts {
            on: true,
            live: 0,
            peak: 0,
        });
    });
    run();
    let counts = COUNTS.with(|counts| {
        counts.replace(Counts {
            on: false,
            live: 0,
            peak: 0,
        })
    });
    counts.peak.max(0) as usize
}

fn headers(content_type: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers
}

/// `prefix`, then `unit` repeated, then `suffix`, at most `limit` bytes.
fn padded(prefix: &str, unit: &str, separator: &str, suffix: &str, limit: usize) -> Vec<u8> {
    let mut body = prefix.as_bytes().to_vec();
    let mut first = true;
    loop {
        let extra = unit.len() + if first { 0 } else { separator.len() };
        if body.len() + extra + suffix.len() > limit {
            break;
        }
        if !first {
            body.extend_from_slice(separator.as_bytes());
        }
        body.extend_from_slice(unit.as_bytes());
        first = false;
    }
    body.extend_from_slice(suffix.as_bytes());
    body
}

/// Feeds `body` in 64 KiB chunks; returns the peak and what was observed.
fn measure(headers: &HeaderMap, body: &[u8]) -> (usize, cs_proxy::Observed) {
    let mut observed = None;
    let peak = peak_bytes(|| {
        let mut observer = ResponseObserver::new(StatusCode::OK, headers);
        for chunk in body.chunks(64 * 1024) {
            observer.feed(chunk);
        }
        observed = Some(observer.finish());
    });
    (peak, observed.expect("ran"))
}

fn report(name: &str, body: &[u8], peak: usize) {
    println!(
        "{name}: body {} bytes, peak {peak} bytes ({:.2} MiB)",
        body.len(),
        peak as f64 / MIB as f64
    );
}

#[test]
fn hostile_non_streamed_bodies_stay_within_twice_the_cap() {
    let json = headers("application/json");
    let usage = "\"usage\":{\"input_tokens\":1,\"output_tokens\":2}";
    let cases = [
        // The review's shape: many empty arrays where `usage` belongs.
        (
            "usage of empty arrays",
            padded("{\"usage\":[", "[]", ",", "]}", JSON_CAP),
        ),
        // A Message whose `content` is most of the body; `usage` comes last.
        (
            "message with large content",
            padded(
                "{\"type\":\"message\",\"model\":\"m\",\"content\":[",
                "{\"type\":\"text\",\"text\":\"x\"}",
                ",",
                &format!("],\"stop_reason\":\"end_turn\",{usage}}}"),
                JSON_CAP,
            ),
        ),
        // Deep nesting where an object is expected.
        ("deeply nested usage", {
            let depth = (JSON_CAP - 16) / 2;
            let mut body = b"{\"usage\":".to_vec();
            body.extend(std::iter::repeat_n(b'[', depth));
            body.extend(std::iter::repeat_n(b']', depth));
            body.extend_from_slice(b"}");
            body
        }),
        // One long label with escapes, which the parser must unescape to read.
        (
            "long escaped model",
            padded("{\"model\":\"", "\\n", "", "\"}", JSON_CAP),
        ),
    ];
    for (name, body) in &cases {
        assert!(body.len() <= JSON_CAP, "{name}");
        let (peak, observed) = measure(&json, body);
        report(name, body, peak);
        assert!(peak <= 2 * JSON_CAP, "{name}: peak {peak}");
        if *name == "message with large content" {
            assert_eq!(observed.model.as_deref(), Some("m"));
            assert!(observed.usage.is_some(), "usage after the content is read");
        }
    }
}

#[test]
fn hostile_sse_events_stay_within_twice_the_buffers() {
    let sse = headers("text/event-stream");
    let limit = SSE_LINE_CAP;
    let usage = "\"usage\":{\"input_tokens\":1,\"output_tokens\":2}";
    let cases = [
        (
            "message_start with a large content",
            padded(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"content\":[",
                "[]",
                ",",
                &format!("],{usage}}}}}\n\n"),
                limit,
            ),
        ),
        (
            "message_start with usage of empty arrays",
            padded(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":[",
                "[]",
                ",",
                "]}}\n\n",
                limit,
            ),
        ),
        (
            "unnamed message_delta with a large delta",
            padded(
                "data: {\"type\":\"message_delta\",\"delta\":{\"pad\":[",
                "{}",
                ",",
                "]}}\n\n",
                limit,
            ),
        ),
    ];
    for (name, body) in &cases {
        let (peak, observed) = measure(&sse, body);
        report(name, body, peak);
        assert!(
            peak <= 2 * (SSE_LINE_CAP + SSE_DATA_CAP),
            "{name}: peak {peak}"
        );
        if name.starts_with("message_start with a large") {
            assert_eq!(observed.model.as_deref(), Some("m"));
            assert!(observed.usage.is_some());
        }
    }
}
