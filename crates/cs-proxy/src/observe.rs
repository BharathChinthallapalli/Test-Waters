//! Metadata from a copy of the upstream response (design, requirement 4).
//! Owned by task 2 (`observe`).
//!
//! [`ResponseObserver`] is fed every response chunk, in order, after the chunk
//! was sent to the client. It must never fail and must bound its memory: an SSE
//! line or a JSON body over its cap is skipped and the fields it would have
//! given stay `None`. JSON is read into small typed structs; everything else,
//! such as `content`, is skipped by the parser without being built
//! (`tests/observe_memory.rs` measures the peak).
//!
//! Recorded headers are bounded so that one `llm.call` record stays small:
//! `request-id`, `retry-after` and `x-should-retry` are recorded first, then at
//! most 48 `anthropic-ratelimit-*` headers in name order. A header is skipped
//! when its first value isn't UTF-8, its name is over 128 bytes, its value is
//! over 256 bytes, or it would take the total (names plus values) over 8 KiB.
//!
//! Known limits:
//! - A non-streamed body over 4 MiB is not parsed, so its usage is lost:
//!   `usage` comes after `content` in the Message object and can't be reached
//!   without reading the whole body.
//! - An over-cap line voids its event even when it is a comment line.
//! - A body with a `content-encoding` other than `identity` is only counted.
//!   The proxy strips `accept-encoding` from requests (`headers.rs`), so the
//!   upstream shouldn't send one.
//! - `model` is the one the response body names. After a server-side fallback
//!   that can be the model requested rather than the one that served the call;
//!   recording the serving model is a follow-up.
//! - A duplicate key in an object that is read fails the whole parse (the
//!   derived structs reject duplicates); it is not a documented shape.
//!
//! Wire facts (`.kiro/specs/03-passthrough-proxy/sources.md`, sections 2-6):
//! - Streams: <https://platform.claude.com/docs/en/build-with-claude/streaming>
//!   and <https://platform.claude.com/docs/en/api/messages>. `message_start`
//!   carries `message.model` and `message.usage`; `message_delta` carries
//!   `delta.stop_reason` and `usage`, whose counts are cumulative, so they
//!   replace earlier ones and are never added. `ping` and unknown event types
//!   are ignored; an `error` event carries `error.type`.
//! - Non-streamed: the Message object (`model`, `stop_reason`, `usage`) or the
//!   error shape `{"type":"error","error":{"type":…}}`
//!   (<https://platform.claude.com/docs/en/api/errors>).
//! - SSE framing follows the WHATWG "event stream" rules
//!   (<https://html.spec.whatwg.org/multipage/server-sent-events.html>; that
//!   host is blocked here, so the rules were read from MDN's "Using server-sent
//!   events" and undici 7.30.0's `lib/web/eventsource/eventsource-stream.js`,
//!   which quotes the spec): lines end in CRLF, LF or CR; a blank line
//!   dispatches; a line starting with `:` is a comment; the field name runs to
//!   the first `:` and one leading space of the value is dropped; `data` lines
//!   join with LF; a leading BOM is dropped; an event without `data` is not
//!   dispatched, nor is one cut off by the end of the body.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use cs_core::llm::Usage;
use http::{HeaderMap, StatusCode};
use serde::de::value::MapAccessDeserializer;
use serde::de::{self, DeserializeOwned, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// Longest SSE line kept; a longer one voids its event.
const MAX_SSE_LINE_BYTES: usize = 1 << 20;
/// Most `data` kept for one SSE event; more voids the event.
const MAX_SSE_DATA_BYTES: usize = 1 << 20;
/// Largest non-streamed body parsed.
const MAX_JSON_BODY_BYTES: usize = 4 << 20;
/// Longest `model`, `stop_reason` or `error.type` value recorded.
const MAX_LABEL_BYTES: usize = 256;

/// Headers recorded whenever present and within the caps below.
const ALWAYS_RECORDED_HEADERS: [&str; 3] = ["request-id", "retry-after", "x-should-retry"];
const RATE_LIMIT_HEADER_PREFIX: &str = "anthropic-ratelimit-";
/// Most `anthropic-ratelimit-*` headers recorded, taken in name order.
const MAX_RATE_LIMIT_HEADERS: usize = 48;
/// Longest header name recorded.
const MAX_HEADER_NAME_BYTES: usize = 128;
/// Longest header value recorded.
const MAX_HEADER_VALUE_BYTES: usize = 256;
/// Most bytes, names plus values, recorded across all headers.
const MAX_RECORDED_HEADER_BYTES: usize = 8 << 10;

/// What the observer learned about one response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// The response was `text/event-stream`.
    pub streamed: bool,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub stop_reason: Option<String>,
    /// `error.type` from an error body or an SSE `error` event.
    pub error_type: Option<String>,
    /// Provider-reported; `message_delta` usage overwrites `message_start`'s.
    /// `None` unless both input and output tokens were reported.
    pub usage: Option<Usage>,
    /// `request-id`, `retry-after`, `x-should-retry`, `anthropic-ratelimit-*`,
    /// within the caps in the module doc (at most 8 KiB in all).
    pub rate_limit_headers: BTreeMap<String, String>,
    pub response_bytes: u64,
    /// A stream carried `message_stop`.
    pub saw_message_stop: bool,
}

/// Incrementally reads one response.
#[derive(Debug)]
pub struct ResponseObserver {
    observed: Observed,
    usage: UsageParts,
    body: BodyReader,
}

#[derive(Debug)]
enum BodyReader {
    Sse(SseDecoder),
    /// `None` once the body went over [`MAX_JSON_BODY_BYTES`].
    Json(Option<Vec<u8>>),
    /// A content-coded body: its bytes are counted, never read.
    Encoded,
}

impl ResponseObserver {
    /// Starts observing a response with this status and these headers.
    pub fn new(_status: StatusCode, headers: &HeaderMap) -> Self {
        let streamed = headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(is_event_stream);
        let rate_limit_headers = recorded_headers(headers);
        let request_id = rate_limit_headers.get("request-id").cloned();
        let body = if is_content_coded(headers) {
            BodyReader::Encoded
        } else if streamed {
            BodyReader::Sse(SseDecoder::default())
        } else {
            BodyReader::Json(Some(Vec::new()))
        };
        Self {
            observed: Observed {
                streamed,
                request_id,
                rate_limit_headers,
                ..Observed::default()
            },
            usage: UsageParts::default(),
            body,
        }
    }

    /// Feeds the next chunk of the body.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.observed.response_bytes = self
            .observed
            .response_bytes
            .saturating_add(chunk.len() as u64);
        match &mut self.body {
            BodyReader::Sse(decoder) => {
                let (observed, usage) = (&mut self.observed, &mut self.usage);
                decoder.feed(chunk, &mut |name, data| {
                    apply_event(observed, usage, name, data);
                });
            }
            BodyReader::Json(buffer) => {
                if let Some(bytes) = buffer
                    && !bounded_extend(bytes, chunk, MAX_JSON_BODY_BYTES)
                {
                    *buffer = None;
                }
            }
            BodyReader::Encoded => {}
        }
    }

    /// Ends the body and returns what was learned. An SSE event not yet ended
    /// by a blank line is discarded.
    pub fn finish(mut self) -> Observed {
        if let BodyReader::Json(Some(bytes)) = &self.body
            && let Some(body) = parse_fields(bytes)
        {
            if body.kind.0.as_deref() == Some("error") {
                set_error_type(&mut self.observed, body.error);
            } else {
                if let Some(model) = body.model.0 {
                    self.observed.model = Some(model);
                }
                if let Some(reason) = body.stop_reason.0 {
                    self.observed.stop_reason = Some(reason);
                }
                self.usage.overwrite(body.usage);
            }
        }
        self.observed.usage = self.usage.into_usage();
        self.observed
    }

    /// Bytes currently held by the observer's buffers (capacity, not length).
    #[cfg(test)]
    fn buffered_capacity(&self) -> usize {
        match &self.body {
            BodyReader::Sse(decoder) => {
                decoder.line.capacity() + decoder.data.capacity() + decoder.event.capacity()
            }
            BodyReader::Json(buffer) => buffer.as_ref().map_or(0, Vec::capacity),
            BodyReader::Encoded => 0,
        }
    }
}

/// A `content-encoding` listing a coding other than `identity`
/// (<https://www.rfc-editor.org/rfc/rfc9110#section-8.4>: a comma-separated
/// list; `identity` is reserved and shouldn't be sent). An unreadable value
/// counts as coded, so it is never parsed.
fn is_content_coded(headers: &HeaderMap) -> bool {
    headers
        .get_all(http::header::CONTENT_ENCODING)
        .iter()
        .any(|value| {
            value.to_str().map_or(true, |codings| {
                codings
                    .split(',')
                    .map(str::trim)
                    .any(|coding| !coding.is_empty() && !coding.eq_ignore_ascii_case("identity"))
            })
        })
}

/// `text/event-stream`, ignoring parameters and ASCII case.
fn is_event_stream(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// The allowlisted response headers, within the caps: the three always
/// recorded first, then `anthropic-ratelimit-*` in name order. `http` stores
/// names in lower case.
fn recorded_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut recorded = HeaderRecord::default();
    for name in ALWAYS_RECORDED_HEADERS {
        recorded.insert(headers, name);
    }
    let mut rate_limit_names: Vec<&str> = headers
        .keys()
        .map(http::HeaderName::as_str)
        .filter(|name| name.starts_with(RATE_LIMIT_HEADER_PREFIX))
        .collect();
    rate_limit_names.sort_unstable();
    let mut kept = 0;
    for name in rate_limit_names {
        if kept == MAX_RATE_LIMIT_HEADERS {
            break;
        }
        if recorded.insert(headers, name) {
            kept += 1;
        }
    }
    recorded.headers
}

/// Recorded headers and the bytes (names plus values) they hold.
#[derive(Default)]
struct HeaderRecord {
    headers: BTreeMap<String, String>,
    bytes: usize,
}

impl HeaderRecord {
    /// Records `name`'s first value when it is UTF-8, both are within their
    /// caps and the total stays within [`MAX_RECORDED_HEADER_BYTES`].
    fn insert(&mut self, headers: &HeaderMap, name: &str) -> bool {
        let Some(value) = headers
            .get(name)
            .and_then(|value| std::str::from_utf8(value.as_bytes()).ok())
        else {
            return false;
        };
        let size = name.len() + value.len();
        let fits = name.len() <= MAX_HEADER_NAME_BYTES
            && value.len() <= MAX_HEADER_VALUE_BYTES
            && self.bytes + size <= MAX_RECORDED_HEADER_BYTES;
        if fits {
            self.headers.insert(name.to_owned(), value.to_owned());
            self.bytes += size;
        }
        fits
    }
}

/// Appends `bytes` unless `buffer` would exceed `cap`, growing the allocation
/// no further than `cap` (`reserve_exact`). Returns false, leaving `buffer` as
/// it was, when over.
fn bounded_extend(buffer: &mut Vec<u8>, bytes: &[u8], cap: usize) -> bool {
    let Some(needed) = buffer.len().checked_add(bytes.len()) else {
        return false;
    };
    if needed > cap {
        return false;
    }
    if needed > buffer.capacity() {
        // Double as `Vec` would, but stop at the cap.
        let target = buffer.capacity().saturating_mul(2).max(needed).min(cap);
        buffer.reserve_exact(target - buffer.len());
    }
    buffer.extend_from_slice(bytes);
    true
}

/// Incremental SSE decoder. Every byte is looked at once; buffers are capped.
#[derive(Debug)]
struct SseDecoder {
    /// The current line so far, without its terminator.
    line: Vec<u8>,
    /// The current line went over [`MAX_SSE_LINE_BYTES`]; its bytes are dropped.
    line_too_long: bool,
    /// The last byte seen was CR, so a following LF belongs to the same break.
    after_cr: bool,
    /// No line has ended yet (a leading BOM is dropped from it).
    first_line: bool,
    /// The event type buffer, at most [`MAX_LABEL_BYTES`].
    event: Vec<u8>,
    /// The data buffer, lines joined with LF.
    data: Vec<u8>,
    has_data: bool,
    /// The event went over a cap: ignore its lines until the blank line.
    skip_event: bool,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self {
            line: Vec::new(),
            line_too_long: false,
            after_cr: false,
            first_line: true,
            event: Vec::new(),
            data: Vec::new(),
            has_data: false,
            skip_event: false,
        }
    }
}

impl SseDecoder {
    /// Decodes `chunk`, calling `dispatch(event type, data)` per complete event.
    fn feed(&mut self, mut chunk: &[u8], dispatch: &mut dyn FnMut(&[u8], &[u8])) {
        if self.after_cr && !chunk.is_empty() {
            self.after_cr = false;
            if chunk[0] == b'\n' {
                chunk = &chunk[1..];
            }
        }
        while let Some(end) = chunk.iter().position(|&b| b == b'\n' || b == b'\r') {
            self.push_line_bytes(&chunk[..end]);
            let rest = &chunk[end + 1..];
            chunk = if chunk[end] == b'\r' {
                match rest.first() {
                    Some(b'\n') => &rest[1..],
                    Some(_) => rest,
                    None => {
                        self.after_cr = true;
                        rest
                    }
                }
            } else {
                rest
            };
            self.end_line(dispatch);
        }
        self.push_line_bytes(chunk);
    }

    fn push_line_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() || self.line_too_long {
            return;
        }
        if !bounded_extend(&mut self.line, bytes, MAX_SSE_LINE_BYTES) {
            self.line_too_long = true;
            self.line = Vec::new();
        }
    }

    fn end_line(&mut self, dispatch: &mut dyn FnMut(&[u8], &[u8])) {
        let first_line = std::mem::replace(&mut self.first_line, false);
        if std::mem::take(&mut self.line_too_long) {
            // An over-cap line is never blank; it voids its event.
            self.skip_event = true;
            return;
        }
        let line = std::mem::take(&mut self.line);
        let text = match line.strip_prefix(b"\xEF\xBB\xBF") {
            Some(rest) if first_line => rest,
            _ => &line[..],
        };
        self.process_line(text, dispatch);
        self.line = line;
        self.line.clear();
    }

    fn process_line(&mut self, line: &[u8], dispatch: &mut dyn FnMut(&[u8], &[u8])) {
        if line.is_empty() {
            if self.has_data && !self.skip_event {
                dispatch(&self.event, &self.data);
            }
            self.event.clear();
            self.data.clear();
            self.has_data = false;
            self.skip_event = false;
            return;
        }
        if self.skip_event || line[0] == b':' {
            return;
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &[][..]),
        };
        match field {
            b"event" => {
                self.event.clear();
                // A name this long is no known type; keep a stand-in that
                // matches none (0xFF never occurs in UTF-8).
                let name = if value.len() <= MAX_LABEL_BYTES {
                    value
                } else {
                    b"\xFF"
                };
                self.event.extend_from_slice(name);
            }
            b"data" => {
                let fits = (!self.has_data
                    || bounded_extend(&mut self.data, b"\n", MAX_SSE_DATA_BYTES))
                    && bounded_extend(&mut self.data, value, MAX_SSE_DATA_BYTES);
                self.has_data = true;
                if !fits {
                    self.skip_event = true;
                    self.data = Vec::new();
                }
            }
            // `id`, `retry` and unknown fields don't matter here.
            _ => {}
        }
    }
}

/// Parses an event's data or a non-streamed body. Only the fields below are
/// kept; the rest of the JSON (such as `content`) is skipped by the parser
/// without being built, so parsing adds little memory to the bytes buffered.
fn parse_fields(json: &[u8]) -> Option<Fields> {
    serde_json::from_slice::<Object<Fields>>(json).ok()?.0
}

/// The top-level fields read. Each is lenient: a value of the wrong type
/// reads as absent and doesn't fail the parse.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Fields {
    #[serde(rename = "type")]
    kind: Label,
    model: Label,
    stop_reason: Label,
    usage: Object<UsageFields>,
    error: Object<ErrorFields>,
    message: Object<MessageFields>,
    delta: Object<DeltaFields>,
}

/// `message` of `message_start`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MessageFields {
    model: Label,
    usage: Object<UsageFields>,
}

/// `delta` of `message_delta`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DeltaFields {
    stop_reason: Label,
}

/// `error` of an error body or event.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ErrorFields {
    #[serde(rename = "type")]
    kind: Label,
}

/// A `usage` object.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsageFields {
    input_tokens: Count,
    output_tokens: Count,
    cache_creation_input_tokens: Count,
    cache_read_input_tokens: Count,
}

/// A string of at most [`MAX_LABEL_BYTES`]; any other value is `None`.
#[derive(Debug, Default)]
struct Label(Option<String>);

/// A non-negative integer; any other value is `None`.
#[derive(Debug, Default)]
struct Count(Option<u64>);

/// A JSON object read as `T`; any other value is `None`. Only objects are
/// taken, so an array is never read as a struct's fields by position.
#[derive(Debug)]
struct Object<T>(Option<T>);

impl<T> Default for Object<T> {
    fn default() -> Self {
        Self(None)
    }
}

/// A value read from whatever JSON type arrives. A type it doesn't take gives
/// `Default`; an array or object it doesn't take is skipped with
/// [`IgnoredAny`], which keeps nothing of it.
trait Lenient: Default {
    fn from_str(_text: &str) -> Self {
        Self::default()
    }

    fn from_u64(_number: u64) -> Self {
        Self::default()
    }

    fn from_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Self::default())
    }
}

impl Lenient for Label {
    fn from_str(text: &str) -> Self {
        Self((text.len() <= MAX_LABEL_BYTES).then(|| text.to_owned()))
    }
}

impl Lenient for Count {
    fn from_u64(number: u64) -> Self {
        Self(Some(number))
    }
}

impl<T: DeserializeOwned> Lenient for Object<T> {
    fn from_map<'de, A: MapAccess<'de>>(map: A) -> Result<Self, A::Error> {
        T::deserialize(MapAccessDeserializer::new(map)).map(|value| Self(Some(value)))
    }
}

struct LenientVisitor<T>(PhantomData<T>);

impl<'de, T: Lenient> Visitor<'de> for LenientVisitor<T> {
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, _value: bool) -> Result<T, E> {
        Ok(T::default())
    }

    fn visit_i64<E: de::Error>(self, _value: i64) -> Result<T, E> {
        Ok(T::default())
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<T, E> {
        Ok(T::from_u64(value))
    }

    fn visit_f64<E: de::Error>(self, _value: f64) -> Result<T, E> {
        Ok(T::default())
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<T, E> {
        Ok(T::from_str(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<T, E> {
        Ok(T::default())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<T, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(T::default())
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
        T::from_map(map)
    }
}

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LenientVisitor(PhantomData))
    }
}

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LenientVisitor(PhantomData))
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LenientVisitor(PhantomData))
    }
}

/// The SSE event types the observer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    MessageStart,
    MessageDelta,
    MessageStop,
    Error,
    /// `ping`, content blocks and unknown types: nothing recorded.
    Other,
}

impl EventKind {
    fn from_name(name: &[u8]) -> Self {
        match name {
            b"message_start" => Self::MessageStart,
            b"message_delta" => Self::MessageDelta,
            b"message_stop" => Self::MessageStop,
            b"error" => Self::Error,
            _ => Self::Other,
        }
    }
}

/// Applies one dispatched SSE event. Invalid JSON is ignored.
fn apply_event(observed: &mut Observed, usage: &mut UsageParts, name: &[u8], data: &[u8]) {
    let parse = || parse_fields(data);
    // Events are named, with a matching `type` in the data; fall back to
    // `type` when the name is missing. Only the events used are parsed.
    let (kind, fields) = if name.is_empty() {
        let fields = parse();
        let kind = fields
            .as_ref()
            .and_then(|fields| fields.kind.0.as_deref())
            .map_or(EventKind::Other, |kind| {
                EventKind::from_name(kind.as_bytes())
            });
        (kind, fields)
    } else {
        (EventKind::from_name(name), None)
    };
    match kind {
        EventKind::MessageStart => {
            let Some(message) = fields.or_else(parse).and_then(|fields| fields.message.0) else {
                return;
            };
            if let Some(model) = message.model.0 {
                observed.model = Some(model);
            }
            usage.overwrite(message.usage);
        }
        EventKind::MessageDelta => {
            let Some(fields) = fields.or_else(parse) else {
                return;
            };
            // A usage-only delta with a null stop reason keeps the earlier one.
            if let Some(reason) = fields.delta.0.and_then(|delta| delta.stop_reason.0) {
                observed.stop_reason = Some(reason);
            }
            usage.overwrite(fields.usage);
        }
        EventKind::MessageStop => observed.saw_message_stop = true,
        EventKind::Error => {
            if let Some(fields) = fields.or_else(parse) {
                set_error_type(observed, fields.error);
            }
        }
        EventKind::Other => {}
    }
}

/// Records the first `error.type` seen.
fn set_error_type(observed: &mut Observed, error: Object<ErrorFields>) {
    if observed.error_type.is_none() {
        observed.error_type = error.0.and_then(|error| error.kind.0);
    }
}

/// Token counts reported so far; each report replaces the fields it carries.
#[derive(Debug, Default)]
struct UsageParts {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

impl UsageParts {
    /// Overwrites (never adds) each count present as a non-negative integer;
    /// absent or null counts keep their earlier value.
    fn overwrite(&mut self, usage: Object<UsageFields>) {
        let Some(usage) = usage.0 else {
            return;
        };
        let reports = [
            (&mut self.input_tokens, usage.input_tokens),
            (&mut self.output_tokens, usage.output_tokens),
            (
                &mut self.cache_creation_input_tokens,
                usage.cache_creation_input_tokens,
            ),
            (
                &mut self.cache_read_input_tokens,
                usage.cache_read_input_tokens,
            ),
        ];
        for (slot, count) in reports {
            if count.0.is_some() {
                *slot = count.0;
            }
        }
    }

    /// The usage, or `None` when input or output tokens were never reported.
    fn into_usage(self) -> Option<Usage> {
        Some(Usage {
            input_tokens: self.input_tokens?,
            output_tokens: self.output_tokens?,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use http::HeaderValue;
    use http::header::CONTENT_TYPE;

    use super::*;

    // Hand-written fixtures; each cites the documented example it follows.
    const TEXT: &[u8] = include_bytes!("../tests/fixtures/observe/text.sse");
    const TOOL_USE: &[u8] = include_bytes!("../tests/fixtures/observe/tool_use.sse");
    const CACHE: &[u8] = include_bytes!("../tests/fixtures/observe/cache.sse");
    const UNKNOWN_EVENT: &[u8] = include_bytes!("../tests/fixtures/observe/unknown_event.sse");
    const ERROR_MID_STREAM: &[u8] =
        include_bytes!("../tests/fixtures/observe/error_mid_stream.sse");
    const NO_MESSAGE_STOP: &[u8] = include_bytes!("../tests/fixtures/observe/no_message_stop.sse");
    /// Shape of the `Message` object, <https://platform.claude.com/docs/en/api/messages>.
    const MESSAGE_JSON: &[u8] = include_bytes!("../tests/fixtures/observe/message.json");
    /// Error shape, <https://platform.claude.com/docs/en/api/errors>.
    const ERROR_JSON: &[u8] = include_bytes!("../tests/fixtures/observe/error.json");

    const SSE_FIXTURES: [(&str, &[u8]); 6] = [
        ("text", TEXT),
        ("tool_use", TOOL_USE),
        ("cache", CACHE),
        ("unknown_event", UNKNOWN_EVENT),
        ("error_mid_stream", ERROR_MID_STREAM),
        ("no_message_stop", NO_MESSAGE_STOP),
    ];
    const JSON_FIXTURES: [(&str, &[u8]); 2] = [("message", MESSAGE_JSON), ("error", ERROR_JSON)];

    /// Most an SSE observer may hold: a line, an event's data and its name.
    const SSE_BOUND: usize = MAX_SSE_LINE_BYTES + MAX_SSE_DATA_BYTES + MAX_LABEL_BYTES;

    fn headers(content_type: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_str(content_type).unwrap());
        headers
    }

    fn sse() -> HeaderMap {
        headers("text/event-stream; charset=utf-8")
    }

    fn json() -> HeaderMap {
        headers("application/json")
    }

    fn observe<'a>(headers: &HeaderMap, chunks: impl IntoIterator<Item = &'a [u8]>) -> Observed {
        let mut observer = ResponseObserver::new(StatusCode::OK, headers);
        for chunk in chunks {
            observer.feed(chunk);
        }
        observer.finish()
    }

    fn whole(headers: &HeaderMap, body: &[u8]) -> Observed {
        observe(headers, [body])
    }

    fn usage(input: u64, output: u64, creation: Option<u64>, read: Option<u64>) -> Option<Usage> {
        Some(Usage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: creation,
            cache_read_input_tokens: read,
        })
    }

    /// The SSE events the decoder dispatches, as (type, data) strings.
    fn decode<'a>(chunks: impl IntoIterator<Item = &'a [u8]>) -> Vec<(String, String)> {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for chunk in chunks {
            decoder.feed(chunk, &mut |name, data| {
                events.push((
                    String::from_utf8_lossy(name).into_owned(),
                    String::from_utf8_lossy(data).into_owned(),
                ));
            });
        }
        events
    }

    fn with_line_endings(body: &[u8], ending: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(body.len() * 2);
        for &byte in body {
            if byte == b'\n' {
                out.extend_from_slice(ending);
            } else {
                out.push(byte);
            }
        }
        out
    }

    #[test]
    fn text_stream() {
        let observed = whole(&sse(), TEXT);
        assert_eq!(
            observed,
            Observed {
                streamed: true,
                model: Some("claude-opus-5-5".into()),
                stop_reason: Some("end_turn".into()),
                usage: usage(25, 15, None, None),
                response_bytes: TEXT.len() as u64,
                saw_message_stop: true,
                ..Observed::default()
            }
        );
    }

    #[test]
    fn tool_use_stream_with_input_json_deltas() {
        let observed = whole(&sse(), TOOL_USE);
        assert_eq!(observed.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(observed.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(observed.usage, usage(472, 89, None, None));
        assert!(observed.saw_message_stop);
        assert_eq!(observed.error_type, None);
    }

    #[test]
    fn cache_tokens_come_from_the_last_report() {
        let observed = whole(&sse(), CACHE);
        assert_eq!(observed.usage, usage(10682, 510, Some(2051), Some(1024)));
        assert_eq!(observed.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn ping_and_unknown_events_are_ignored() {
        // The unknown event carries usage, a stop reason and an error; none count.
        let observed = whole(&sse(), UNKNOWN_EVENT);
        assert_eq!(observed.usage, usage(40, 7, None, None));
        assert_eq!(observed.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(observed.error_type, None);
        assert!(observed.saw_message_stop);

        let only_pings =
            b"event: ping\ndata: {\"type\": \"ping\"}\n\nevent: ping\ndata: {\"type\": \"ping\"}\n\n";
        let observed = whole(&sse(), only_pings);
        assert_eq!(
            observed,
            Observed {
                streamed: true,
                response_bytes: only_pings.len() as u64,
                ..Observed::default()
            }
        );
    }

    #[test]
    fn mid_stream_error_sets_error_type() {
        let observed = whole(&sse(), ERROR_MID_STREAM);
        assert_eq!(observed.error_type.as_deref(), Some("overloaded_error"));
        assert_eq!(observed.usage, usage(30, 1, None, None));
        assert_eq!(observed.stop_reason, None);
        assert!(!observed.saw_message_stop);
    }

    #[test]
    fn first_error_type_is_kept() {
        let body = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n\
                     event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\"}}\n\n";
        assert_eq!(
            whole(&sse(), body).error_type.as_deref(),
            Some("overloaded_error")
        );
    }

    #[test]
    fn stream_ending_without_message_stop() {
        let observed = whole(&sse(), NO_MESSAGE_STOP);
        assert!(!observed.saw_message_stop);
        assert_eq!(observed.stop_reason.as_deref(), Some("max_tokens"));
        assert_eq!(observed.usage, usage(12, 256, None, None));
    }

    #[test]
    fn a_trailing_event_without_its_blank_line_is_not_dispatched() {
        let mut body = NO_MESSAGE_STOP.to_vec();
        body.extend_from_slice(b"event: message_stop\ndata: {\"type\": \"message_stop\"}\n");
        assert!(!whole(&sse(), &body).saw_message_stop);
        body.extend_from_slice(b"\n");
        assert!(whole(&sse(), &body).saw_message_stop);
    }

    #[test]
    fn message_delta_usage_overwrites_and_is_never_summed() {
        let body = b"event: message_start\n\
            data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n\
            event: message_delta\n\
            data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\n\
            event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(whole(&sse(), body).usage, usage(25, 15, None, None));

        // Several deltas: the last one wins; nulls and absent fields keep the
        // earlier value, and a null stop reason doesn't clear the earlier one.
        let body = b"event: message_start\n\
            data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25,\"output_tokens\":1,\"cache_read_input_tokens\":7}}}\n\n\
            event: message_delta\n\
            data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":10}}\n\n\
            event: message_delta\n\
            data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":null},\"usage\":{\"input_tokens\":null,\"cache_read_input_tokens\":null,\"output_tokens\":15}}\n\n";
        let observed = whole(&sse(), body);
        assert_eq!(observed.usage, usage(25, 15, None, Some(7)));
        assert_eq!(observed.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn usage_is_never_invented() {
        // No usage anywhere (the documented thinking example has none).
        let body = b"event: message_start\n\
            data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"content\":[]}}\n\n\
            event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n\
            event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let observed = whole(&sse(), body);
        assert_eq!(observed.usage, None);
        assert_eq!(observed.model.as_deref(), Some("m"));

        // Output tokens alone are not a usage: input tokens would be a guess.
        let body = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":15}}\n\n";
        assert_eq!(whole(&sse(), body).usage, None);

        // Wrong types are not counts.
        let body = b"event: message_start\n\
            data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":-1,\"output_tokens\":1.5}}}\n\n\
            event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":\"3\",\"output_tokens\":[4]}}\n\n";
        assert_eq!(whole(&sse(), body).usage, None);

        assert_eq!(whole(&sse(), b"").usage, None);
        assert_eq!(whole(&json(), b"").usage, None);
    }

    #[test]
    fn invalid_json_never_panics_and_is_ignored() {
        let mut nested = b"event: message_start\ndata: ".to_vec();
        nested.extend(std::iter::repeat_n(b'[', 100_000));
        nested.extend_from_slice(b"\n\n");
        let bad: [&[u8]; 10] = [
            b"event: message_start\ndata: {\n\n",
            b"event: message_start\ndata: null\n\n",
            b"event: message_start\ndata: []\n\n",
            b"event: message_start\ndata: {\"message\":\"text\"}\n\n",
            b"event: message_start\ndata: {\"message\":{\"model\":7,\"usage\":[1,2]}}\n\n",
            b"event: message_delta\ndata: {\"delta\":\"x\",\"usage\":\"y\"}\n\n",
            b"event: error\ndata: {\"error\":\"x\"}\n\n",
            b"event: message_start\ndata: \xff\xfe{\"type\"}\n\n",
            b"event: \xff\ndata: {\"type\": \"message_stop\"}\n\n",
            &nested,
        ];
        for body in bad {
            let observed = whole(&sse(), body);
            assert_eq!(observed.model, None);
            assert_eq!(observed.usage, None);
            assert_eq!(observed.error_type, None);
            assert!(!observed.saw_message_stop);
            let observed = whole(&json(), body);
            assert_eq!(observed.model, None);
        }
        let mut nested_json = vec![b'['; 100_000];
        nested_json.extend_from_slice(b"{\"model\":\"m\"}");
        assert_eq!(whole(&json(), &nested_json).model, None);
        assert_eq!(whole(&json(), b"{\"model\":\"m\"").model, None);
        // A duplicate key is rejected by the parser, not trusted.
        let dup = b"{\"model\":\"a\",\"model\":\"b\"}";
        assert_eq!(whole(&json(), dup).model, None);
    }

    #[test]
    fn non_streamed_success() {
        let observed = whole(&json(), MESSAGE_JSON);
        assert_eq!(
            observed,
            Observed {
                streamed: false,
                model: Some("claude-opus-5".into()),
                stop_reason: Some("end_turn".into()),
                usage: usage(2095, 503, Some(2051), Some(2051)),
                response_bytes: MESSAGE_JSON.len() as u64,
                ..Observed::default()
            }
        );
    }

    #[test]
    fn non_streamed_error() {
        let observed = whole(&json(), ERROR_JSON);
        assert_eq!(observed.error_type.as_deref(), Some("rate_limit_error"));
        assert_eq!(observed.usage, None);
        assert_eq!(observed.model, None);
        assert!(!observed.streamed);
    }

    #[test]
    fn non_streamed_usage_needs_both_counts() {
        // Input tokens alone would need an invented output count.
        let body = b"{\"type\":\"message\",\"model\":\"m\",\"usage\":{\"input_tokens\":42}}";
        let observed = whole(&json(), body);
        assert_eq!(observed.model.as_deref(), Some("m"));
        assert_eq!(observed.usage, None);
        let body = b"{\"type\":\"message\",\"usage\":{\"input_tokens\":42,\"output_tokens\":0}}";
        assert_eq!(whole(&json(), body).usage, usage(42, 0, None, None));
    }

    #[test]
    fn wrong_typed_fields_read_as_absent_without_failing_the_parse() {
        let body = b"{\"type\":\"message\",\"model\":\"m\",\"stop_reason\":[1],\
            \"usage\":{\"input_tokens\":3,\"output_tokens\":4,\"cache_read_input_tokens\":{\"x\":1}},\
            \"delta\":7,\"message\":[{\"model\":\"not read\"}],\"error\":\"x\",\"content\":[{\"a\":[[]]}]}";
        let observed = whole(&json(), body);
        assert_eq!(observed.model.as_deref(), Some("m"));
        assert_eq!(observed.stop_reason, None);
        assert_eq!(observed.usage, usage(3, 4, None, None));

        // An array where an object belongs is not read by position.
        let body = b"{\"usage\":[5,6,7,8]}";
        assert_eq!(whole(&json(), body).usage, None);
        let body = b"event: message_start\ndata: {\"message\":[\"m\",[1,2]]}\n\n";
        let observed = whole(&sse(), body);
        assert_eq!((observed.model, observed.usage), (None, None));
        let body = b"event: message_start\n\
            data: {\"message\":{\"model\":\"m\",\"usage\":[1,2]}}\n\n";
        let observed = whole(&sse(), body);
        assert_eq!(observed.model.as_deref(), Some("m"));
        assert_eq!(observed.usage, None);
    }

    #[test]
    fn content_coded_bodies_are_only_counted() {
        for coding in ["gzip", "br", "identity, gzip", "x-unknown", "Identity,br"] {
            let mut map = sse();
            map.insert(
                http::header::CONTENT_ENCODING,
                HeaderValue::from_static(coding),
            );
            map.insert("request-id", HeaderValue::from_static("req_fixture02"));
            let observed = whole(&map, TEXT);
            assert_eq!(
                observed,
                Observed {
                    streamed: true,
                    request_id: Some("req_fixture02".into()),
                    rate_limit_headers: [("request-id".to_owned(), "req_fixture02".to_owned())]
                        .into(),
                    response_bytes: TEXT.len() as u64,
                    ..Observed::default()
                },
                "{coding}"
            );
            let mut map = json();
            map.insert(
                http::header::CONTENT_ENCODING,
                HeaderValue::from_static(coding),
            );
            let mut observer = ResponseObserver::new(StatusCode::OK, &map);
            observer.feed(MESSAGE_JSON);
            assert_eq!(observer.buffered_capacity(), 0, "{coding}");
            assert_eq!(observer.finish().model, None, "{coding}");
        }
        // `identity` (or an empty value) is no coding: the body is read.
        for coding in [
            &b"identity"[..],
            b"IDENTITY",
            b" identity , identity",
            b"",
            b" ",
        ] {
            let mut map = sse();
            map.insert(
                http::header::CONTENT_ENCODING,
                HeaderValue::from_bytes(coding).unwrap(),
            );
            assert_eq!(whole(&map, TEXT), whole(&sse(), TEXT), "{coding:?}");
        }
        // A value that isn't text counts as coded.
        let mut map = sse();
        map.insert(
            http::header::CONTENT_ENCODING,
            HeaderValue::from_bytes(b"gz\xffip").unwrap(),
        );
        assert_eq!(whole(&map, TEXT).usage, None);
    }

    #[test]
    fn non_streamed_body_over_cap_is_not_parsed() {
        let padding = " ".repeat(MAX_JSON_BODY_BYTES - MESSAGE_JSON.len());
        let mut at_cap = MESSAGE_JSON.to_vec();
        at_cap.extend_from_slice(padding.as_bytes());
        assert_eq!(at_cap.len(), MAX_JSON_BODY_BYTES);
        assert_eq!(
            observe(&json(), at_cap.chunks(64 * 1024)).model.as_deref(),
            Some("claude-opus-5")
        );

        let mut over = at_cap.clone();
        over.push(b' ');
        let mut observer = ResponseObserver::new(StatusCode::OK, &json());
        for chunk in over.chunks(64 * 1024) {
            observer.feed(chunk);
            assert!(observer.buffered_capacity() <= MAX_JSON_BODY_BYTES);
        }
        assert_eq!(observer.buffered_capacity(), 0, "buffer released");
        observer.feed(b"more");
        let observed = observer.finish();
        assert_eq!(observed.model, None);
        assert_eq!(observed.usage, None);
        assert_eq!(observed.response_bytes, over.len() as u64 + 4);
    }

    #[test]
    fn crlf_and_cr_line_endings_give_the_same_result() {
        for (name, body) in SSE_FIXTURES {
            let expected = whole(&sse(), body);
            for ending in [&b"\r\n"[..], b"\r"] {
                let converted = with_line_endings(body, ending);
                let observed = whole(&sse(), &converted);
                assert_eq!(
                    Observed {
                        response_bytes: expected.response_bytes,
                        ..observed
                    },
                    expected,
                    "{name} with {ending:?}"
                );
            }
        }
    }

    #[test]
    fn every_split_point_and_chunk_size_gives_the_same_result() {
        let mut cases: Vec<(String, HeaderMap, Vec<u8>)> = Vec::new();
        for (name, body) in SSE_FIXTURES {
            cases.push((name.into(), sse(), body.to_vec()));
            cases.push((
                format!("{name} crlf"),
                sse(),
                with_line_endings(body, b"\r\n"),
            ));
            cases.push((format!("{name} cr"), sse(), with_line_endings(body, b"\r")));
        }
        for (name, body) in JSON_FIXTURES {
            cases.push((name.into(), json(), body.to_vec()));
        }
        for (name, headers, body) in &cases {
            let expected = whole(headers, body);
            for at in 0..=body.len() {
                let (head, tail) = body.split_at(at);
                assert_eq!(
                    observe(headers, [head, tail]),
                    expected,
                    "{name} split at {at}"
                );
            }
            for size in [1, 2, 3, 7] {
                assert_eq!(
                    observe(headers, body.chunks(size)),
                    expected,
                    "{name} in {size}-byte chunks"
                );
            }
        }
    }

    #[test]
    fn over_cap_line_is_skipped_until_the_blank_line() {
        // `data: ` plus the padding is over the cap. The whole event is voided,
        // including the valid data line and the event name that follow it.
        let mut body = b"event: message_delta\ndata: ".to_vec();
        body.extend_from_slice("x".repeat(MAX_SSE_LINE_BYTES).as_bytes());
        body.extend_from_slice(
            b"\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"}}\n\
              event: message_stop\n\n",
        );
        // The next events are read normally.
        body.extend_from_slice(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
        );
        body.extend_from_slice(
            b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\"}}\n\n",
        );

        for chunk_size in [body.len(), 4096, 1] {
            let mut observer = ResponseObserver::new(StatusCode::OK, &sse());
            for chunk in body.chunks(chunk_size) {
                observer.feed(chunk);
                assert!(observer.buffered_capacity() <= SSE_BOUND);
            }
            let observed = observer.finish();
            assert_eq!(observed.stop_reason, None, "{chunk_size}");
            assert!(!observed.saw_message_stop, "{chunk_size}");
            assert_eq!(observed.usage, usage(3, 1, None, None));
            assert_eq!(observed.error_type.as_deref(), Some("api_error"));
            assert_eq!(observed.response_bytes, body.len() as u64);
        }
    }

    #[test]
    fn line_exactly_at_cap_is_kept() {
        let prefix =
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},\"pad\":\"";
        let suffix = b"\"}";
        let pad = "x".repeat(MAX_SSE_LINE_BYTES - prefix.len() - suffix.len());
        let mut body = b"event: message_delta\n".to_vec();
        body.extend_from_slice(prefix);
        body.extend_from_slice(pad.as_bytes());
        body.extend_from_slice(suffix);
        body.extend_from_slice(b"\n\n");
        assert_eq!(whole(&sse(), &body).stop_reason.as_deref(), Some("refusal"));
    }

    #[test]
    fn data_over_cap_across_lines_is_skipped() {
        let line = format!("data: {}\n", "y".repeat(MAX_SSE_DATA_BYTES / 2));
        let mut body = b"event: message_stop\n".to_vec();
        for _ in 0..3 {
            body.extend_from_slice(line.as_bytes());
        }
        body.extend_from_slice(b"\n");
        let mut observer = ResponseObserver::new(StatusCode::OK, &sse());
        for chunk in body.chunks(1000) {
            observer.feed(chunk);
            assert!(observer.buffered_capacity() <= SSE_BOUND);
        }
        assert!(!observer.finish().saw_message_stop);
        // Under the cap the same event counts.
        let mut body = b"event: message_stop\n".to_vec();
        body.extend_from_slice(format!("data: {}\n", "y".repeat(1000)).as_bytes());
        body.extend_from_slice(b"\n");
        assert!(whole(&sse(), &body).saw_message_stop);
    }

    #[test]
    fn decoder_follows_the_event_stream_rules() {
        let body = b"\xEF\xBB\xBFdata: first\n\
            : comment\n\
            data:second\n\
            data:  two spaces\n\
            data\n\
            id: 7\n\
            retry: 10\n\
            ignored: field\n\
            \n\
            event: named\n\
            \n\
            event: named\r\n\
            data: {}\r\
            \r\
            data: after\n\
            \n\
            \xEF\xBB\xBFdata: a BOM after the start is kept, so this field is unknown\n\n";
        let expected = vec![
            (String::new(), "first\nsecond\n two spaces\n".to_owned()),
            ("named".to_owned(), "{}".to_owned()),
            (String::new(), "after".to_owned()),
        ];
        assert_eq!(decode([&body[..]]), expected);
        for size in [1, 2, 3, 7] {
            assert_eq!(decode(body.chunks(size)), expected, "{size}-byte chunks");
        }
    }

    #[test]
    fn decoder_handles_split_crlf() {
        // CR at a chunk end, LF at the next start: one line break, not two.
        let events = decode([&b"event: a\r"[..], b"\ndata: 1\r", b"\n\r", b"\n"]);
        assert_eq!(events, vec![("a".to_owned(), "1".to_owned())]);
        // An empty chunk between CR and LF changes nothing.
        let events = decode([&b"data: 1\r"[..], b"", b"\n\r\n"]);
        assert_eq!(events, vec![(String::new(), "1".to_owned())]);
    }

    #[test]
    fn unnamed_events_fall_back_to_the_data_type() {
        let body = b"data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n\
            data: {\"type\":\"message_stop\"}\n\n";
        let observed = whole(&sse(), body);
        assert_eq!(observed.model.as_deref(), Some("m"));
        assert_eq!(observed.usage, usage(2, 1, None, None));
        assert!(observed.saw_message_stop);
    }

    #[test]
    fn long_labels_are_not_recorded() {
        let long = "m".repeat(MAX_LABEL_BYTES + 1);
        let body = format!("{{\"model\":\"{long}\",\"stop_reason\":\"end_turn\"}}");
        let observed = whole(&json(), body.as_bytes());
        assert_eq!(observed.model, None);
        assert_eq!(observed.stop_reason.as_deref(), Some("end_turn"));

        // An over-long event name matches no type, not even through `type`.
        let body = format!(
            "event: {}\ndata: {{\"type\":\"message_stop\"}}\n\n",
            "n".repeat(MAX_LABEL_BYTES + 1)
        );
        assert!(!whole(&sse(), body.as_bytes()).saw_message_stop);
        let events = decode([body.as_bytes()]);
        assert_eq!(events[0].0.len(), 3, "one U+FFFD stand-in");
    }

    #[test]
    fn content_type_decides_streaming() {
        for (content_type, streamed) in [
            ("text/event-stream", true),
            ("text/event-stream; charset=utf-8", true),
            ("Text/Event-Stream", true),
            (" text/event-stream ;x=y", true),
            ("application/json", false),
            ("text/event-streamx", false),
        ] {
            let observer = ResponseObserver::new(StatusCode::OK, &headers(content_type));
            assert_eq!(observer.finish().streamed, streamed, "{content_type}");
        }
        let observer = ResponseObserver::new(StatusCode::OK, &HeaderMap::new());
        assert!(!observer.finish().streamed);
        // A JSON body under a stream content type is read as SSE (and finds nothing).
        assert_eq!(whole(&sse(), MESSAGE_JSON).model, None);
    }

    #[test]
    fn recorded_headers_are_allowlisted() {
        let mut map = json();
        for (name, value) in [
            ("request-id", &b"req_fixture01"[..]),
            ("retry-after", b"7"),
            ("x-should-retry", b"true"),
            ("anthropic-ratelimit-requests-limit", b"50"),
            ("anthropic-ratelimit-tokens-reset", b"2026-09-27T12:00:00Z"),
            // Unified names are undocumented (sources.md 4); any suffix counts.
            ("anthropic-ratelimit-unified-fixture", "é ok".as_bytes()),
            ("anthropic-ratelimit-bad-utf8", b"\xff\xfe"),
            ("x-api-key", b"fixture-not-a-key"),
            ("authorization", b"Bearer fixture"),
            ("anthropic-organization-id", b"org"),
            ("set-cookie", b"a=b"),
        ] {
            map.append(name, HeaderValue::from_bytes(value).unwrap());
        }
        // A repeated header keeps its first value.
        map.append("retry-after", HeaderValue::from_static("99"));

        let observed = ResponseObserver::new(StatusCode::TOO_MANY_REQUESTS, &map).finish();
        let expected: BTreeMap<String, String> = [
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-tokens-reset", "2026-09-27T12:00:00Z"),
            ("anthropic-ratelimit-unified-fixture", "é ok"),
            ("request-id", "req_fixture01"),
            ("retry-after", "7"),
            ("x-should-retry", "true"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        assert_eq!(observed.rate_limit_headers, expected);
        assert_eq!(observed.request_id.as_deref(), Some("req_fixture01"));
    }

    #[test]
    fn two_hundred_hostile_headers_stay_within_the_caps() {
        let mut map = sse();
        let value = "v".repeat(250);
        let eligible_name = |i: usize| format!("anthropic-ratelimit-hostile-{i:03}");
        // Inserted in reverse so that name order, not arrival order, decides.
        for i in (0..120).rev() {
            map.append(
                http::HeaderName::from_bytes(eligible_name(i).as_bytes()).unwrap(),
                HeaderValue::from_str(&value).unwrap(),
            );
        }
        for i in 0..30 {
            // Name over 128 bytes; sorts before the eligible ones.
            let name = format!("anthropic-ratelimit-{}{i:02}", "a".repeat(120));
            map.append(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static("1"),
            );
            // Value over 256 bytes.
            let name = format!("anthropic-ratelimit-big-{i:02}");
            map.append(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(&"b".repeat(257)).unwrap(),
            );
        }
        for i in 0..17 {
            let name = format!("anthropic-ratelimit-bin-{i:02}");
            map.append(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_bytes(b"\xff\xfe").unwrap(),
            );
        }
        // The always-recorded headers arrive last, after the budget could be spent.
        map.append("request-id", HeaderValue::from_static("req_fixture03"));
        map.append("retry-after", HeaderValue::from_static("30"));
        map.append("x-should-retry", HeaderValue::from_static("true"));
        let allowlisted = map
            .keys()
            .filter(|name| name.as_str() != "content-type")
            .count();
        assert_eq!(allowlisted, 200);

        let recorded = ResponseObserver::new(StatusCode::OK, &map)
            .finish()
            .rate_limit_headers;
        assert_eq!(recorded["request-id"], "req_fixture03");
        assert_eq!(recorded["retry-after"], "30");
        assert_eq!(recorded["x-should-retry"], "true");

        let total: usize = recorded
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum();
        assert!(total <= MAX_RECORDED_HEADER_BYTES, "{total}");
        for (name, value) in &recorded {
            assert!(name.len() <= MAX_HEADER_NAME_BYTES, "{name}");
            assert!(value.len() <= MAX_HEADER_VALUE_BYTES, "{name}");
        }

        // The budget left after the three buys this many eligible headers,
        // taken in name order.
        let always = "request-id".len() + 13 + "retry-after".len() + 2 + "x-should-retry".len() + 4;
        let each = eligible_name(0).len() + value.len();
        let fitting = (MAX_RECORDED_HEADER_BYTES - always) / each;
        assert!(fitting < MAX_RATE_LIMIT_HEADERS, "the byte cap binds here");
        let expected: Vec<String> = (0..fitting).map(eligible_name).collect();
        let kept: Vec<String> = recorded
            .keys()
            .filter(|name| name.starts_with(RATE_LIMIT_HEADER_PREFIX))
            .cloned()
            .collect();
        assert_eq!(kept, expected);
    }

    #[test]
    fn at_most_48_rate_limit_headers_in_name_order() {
        let mut map = json();
        for i in (0..60).rev() {
            let name = format!("anthropic-ratelimit-n-{i:02}");
            map.append(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static("1"),
            );
        }
        // The value cap applies to the always-recorded headers too.
        map.append(
            "request-id",
            HeaderValue::from_str(&"r".repeat(257)).unwrap(),
        );
        map.append(
            "retry-after",
            HeaderValue::from_str(&"7".repeat(256)).unwrap(),
        );

        let recorded = ResponseObserver::new(StatusCode::OK, &map).finish();
        assert_eq!(recorded.request_id, None);
        assert_eq!(recorded.rate_limit_headers["retry-after"].len(), 256);
        let kept: Vec<&String> = recorded
            .rate_limit_headers
            .keys()
            .filter(|name| name.starts_with(RATE_LIMIT_HEADER_PREFIX))
            .collect();
        let expected: Vec<String> = (0..MAX_RATE_LIMIT_HEADERS)
            .map(|i| format!("anthropic-ratelimit-n-{i:02}"))
            .collect();
        assert_eq!(kept, expected.iter().collect::<Vec<_>>());
    }

    #[test]
    fn response_bytes_counts_every_fed_byte() {
        let mut observer = ResponseObserver::new(StatusCode::OK, &sse());
        observer.feed(b"");
        observer.feed(b"garbage\r");
        observer.feed(&[0xff; 10]);
        assert_eq!(observer.finish().response_bytes, 18);
    }

    #[test]
    fn five_megabytes_in_one_byte_chunks_is_linear() {
        // Many small events plus one ~900 KB line: a decoder that re-scans its
        // buffer per chunk would take hours on this.
        let mut body = Vec::new();
        body.extend_from_slice(b"event: content_block_delta\ndata: {\"pad\":\"");
        body.extend_from_slice("z".repeat(900 * 1024).as_bytes());
        body.extend_from_slice(b"\"}\n\n");
        while body.len() < 5_000_000 {
            body.extend_from_slice(TEXT);
        }
        let started = Instant::now();
        let observed = observe(&sse(), body.chunks(1));
        let elapsed = started.elapsed();
        println!("{} bytes in 1-byte chunks: {elapsed:?}", body.len());
        assert_eq!(observed.usage, usage(25, 15, None, None));
        assert!(observed.saw_message_stop);
        // Generous so an unoptimised build on a slow runner passes; the printed
        // time is the real figure.
        assert!(elapsed < Duration::from_secs(20), "took {elapsed:?}");
    }
}
