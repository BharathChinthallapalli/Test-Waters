//! W3C Trace Context for recorded calls. Owned by task 1 (`forward`).
//!
//! The trace id comes from a valid `traceparent` request header, or is
//! generated when there is none. A generated id is recorded, never forwarded:
//! the proxy doesn't add or change `traceparent` (the W3C processing model lets
//! a proxy leave it alone), and a client's own header passes through unchanged.
//!
//! Parsing follows the W3C Editor's Draft source (`sources.md`, [W3C-REQ] and
//! [W3C-PM]): `version-trace_id-parent_id-flags` in lower-case hex; version
//! `ff` is invalid; version `00` is exactly 55 characters; a higher version is
//! read by its first four fields when it is at least 55 characters and the
//! flags are followed by the end or a `-`; an all-zero trace id or parent id
//! invalidates the header. An invalid or repeated header starts a new trace.
//!
//! New ids are 16 bytes from ring's system random source, reached through the
//! rustls ring provider already in the build (`CryptoProvider::secure_random`,
//! rustls 0.23.45 `src/crypto/ring/mod.rs`), so no new dependency. If that
//! source ever fails, the id is derived from the clock, a counter and std's
//! randomly keyed hasher instead.

use std::hash::{BuildHasher, Hasher, RandomState};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use http::HeaderMap;
use rustls::crypto::SecureRandom;

/// The request header carrying the client's trace context.
pub const TRACEPARENT: &str = "traceparent";

/// Length of a version `00` `traceparent`.
const V00_LEN: usize = 55;

/// The trace a call belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    /// 32 lower-case hex characters, never all zeros.
    pub trace_id: String,
    /// The id came from the client's `traceparent`.
    pub from_client: bool,
}

/// Reads `traceparent`, or starts a new trace.
pub fn trace_context(request: &HeaderMap) -> TraceContext {
    let mut values = request.get_all(TRACEPARENT).iter();
    if let (Some(value), None) = (values.next(), values.next())
        && let Some(trace_id) = parse_traceparent(value.as_bytes())
    {
        return TraceContext {
            trace_id,
            from_client: true,
        };
    }
    TraceContext {
        trace_id: new_trace_id(),
        from_client: false,
    }
}

/// The trace id of a valid `traceparent` value, or `None`.
pub fn parse_traceparent(value: &[u8]) -> Option<String> {
    if value.len() < V00_LEN {
        return None;
    }
    let version = &value[0..2];
    if !is_lower_hex(version) || version == b"ff" || value[2] != b'-' {
        return None;
    }
    let exact = if version == b"00" {
        value.len() == V00_LEN
    } else {
        value.len() == V00_LEN || value[V00_LEN] == b'-'
    };
    let trace_id = &value[3..35];
    let parent_id = &value[36..52];
    let flags = &value[53..55];
    let valid = exact
        && is_lower_hex(trace_id)
        && !is_all_zero(trace_id)
        && value[35] == b'-'
        && is_lower_hex(parent_id)
        && !is_all_zero(parent_id)
        && value[52] == b'-'
        && is_lower_hex(flags);
    // Lower-case hex is ASCII, so this never fails once `valid` holds.
    valid
        .then(|| std::str::from_utf8(trace_id).ok().map(str::to_owned))
        .flatten()
}

/// A random trace id: 32 lower-case hex characters, never all zeros.
pub fn new_trace_id() -> String {
    let mut bytes = [0u8; 16];
    static RANDOM: OnceLock<&'static dyn SecureRandom> = OnceLock::new();
    let random = RANDOM.get_or_init(|| rustls::crypto::ring::default_provider().secure_random);
    if random.fill(&mut bytes).is_err() {
        bytes = fallback_bytes();
    }
    if bytes == [0; 16] {
        bytes[15] = 1;
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 16 bytes from the clock, a process-wide counter and std's randomly keyed
/// SipHash. Not cryptographic; only used if the system source fails.
fn fallback_bytes() -> [u8; 16] {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0u8; 16];
    for (half, salt) in bytes.chunks_mut(8).zip([0u8, 1]) {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u64(count);
        hasher.write_u8(salt);
        half.copy_from_slice(&hasher.finish().to_be_bytes());
    }
    bytes
}

fn is_lower_hex(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn is_all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == b'0')
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    const VALID: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    fn with(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(TRACEPARENT, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn assert_generated(context: &TraceContext) {
        assert!(!context.from_client);
        assert_eq!(context.trace_id.len(), 32);
        assert!(is_lower_hex(context.trace_id.as_bytes()));
        assert!(!is_all_zero(context.trace_id.as_bytes()));
    }

    #[test]
    fn a_valid_header_gives_its_trace_id() {
        let context = trace_context(&with(&[VALID]));
        assert_eq!(
            context,
            TraceContext {
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
                from_client: true,
            }
        );
        // The W3C spec's unsampled example.
        assert!(
            parse_traceparent(b"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00").is_some()
        );
    }

    #[test]
    fn invalid_headers_start_a_new_trace() {
        let invalid = [
            // all-zero trace id, all-zero parent id
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            // upper-case hex
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00F067AA0BA902B7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0A",
            // version ff, non-hex version, missing dashes
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "0g-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00_4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736_00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7_01",
            // version 00 must be exactly 55 characters
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-1",
            // a higher version with junk right after the flags
            "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01x",
            "",
            "garbage",
        ];
        for value in invalid {
            assert_eq!(parse_traceparent(value.as_bytes()), None, "{value}");
            assert_generated(&trace_context(&with(&[value])));
        }
    }

    #[test]
    fn a_higher_version_is_read_by_its_first_fields() {
        let future =
            "cc-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-what-the-future-holds";
        assert_eq!(
            parse_traceparent(future.as_bytes()).as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        let same_length = "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-09";
        assert!(parse_traceparent(same_length.as_bytes()).is_some());
    }

    #[test]
    fn no_or_repeated_header_starts_a_new_trace() {
        assert_generated(&trace_context(&HeaderMap::new()));
        assert_generated(&trace_context(&with(&[VALID, VALID])));
    }

    #[test]
    fn generated_ids_differ() {
        let first = new_trace_id();
        let second = new_trace_id();
        assert_ne!(first, second);
        assert_ne!(fallback_bytes(), fallback_bytes());
        assert_ne!(fallback_bytes(), [0; 16]);
    }
}
