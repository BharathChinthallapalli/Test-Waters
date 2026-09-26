//! Event hashing (ADR 0007, feature 02 design "Event hashing").
//!
//! Owned by feature 02 unit `event-hash`: the hashed-object type, `ZERO_HASH`,
//! `event_hash()` over RFC 8785 canonical JSON, and the number check that keeps
//! floats and integers outside ±(2^53 − 1) off the hash path.
//!
//! An event's hash is `hex(SHA-256(canonical JSON))` of exactly this object,
//! which is every stored field except `event_hash` itself:
//!
//! ```json
//! {"body":{…},"globalPos":7,"kind":"…","prevHash":"<64 hex>","runId":"…","seq":3,"tsMs":1790000000000}
//! ```
//!
//! `prevHash` is [`ZERO_HASH`] for a run's first event (`seq = 1`) and the
//! previous event's hash otherwise. Hashes are 64 lowercase hex characters.
//!
//! **No floats on the hash path.** RFC 8785 formats numbers as IEEE-754
//! doubles, the way JavaScript does. To keep that formatting out of every
//! hash, and every integer exact in JavaScript too, [`event_hash`] refuses an
//! event whose body holds any number that is not an integer within
//! ±(2^53 − 1) ([`check_body_numbers`]), or whose `seq`, `globalPos` or `tsMs`
//! is above 2^53 − 1.

use std::fmt;

use canon_json::CanonJsonSerialize;
use serde::Serialize;
use serde_json::{Number, Value};
use sha2::{Digest, Sha256};

/// `prevHash` of a run's first event: 64 `0` characters (32 zero bytes).
pub const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The largest integer JavaScript represents exactly: 2^53 − 1.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// How deeply arrays and objects may nest in an event body.
///
/// Well below serde_json's default parser limit (127 levels), so a stored body
/// always parses again, even inside the hashed object or another envelope. It
/// also keeps hashing a hand-built body from exhausting the stack.
pub const MAX_BODY_DEPTH: usize = 64;

/// Longest path, in bytes, that a [`BodyError`] keeps; longer ones end in `…`.
pub const MAX_ERROR_PATH_LEN: usize = 256;

/// The object an event hash covers: the event without `event_hash`.
///
/// Serialises with camelCase keys, as in the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashedEvent<'a> {
    pub run_id: &'a str,
    /// Position in the run's chain, from 1.
    pub seq: u64,
    /// Position in the daemon's global commit order, from 1.
    pub global_pos: u64,
    pub kind: &'a str,
    /// Unix milliseconds.
    pub ts_ms: u64,
    /// Must refer to content only by its address, never embed it (R4.4). This
    /// module can't tell content from metadata; the writer (task 7) enforces it.
    pub body: &'a Value,
    /// [`ZERO_HASH`] for `seq = 1`, otherwise the previous event's hash.
    pub prev_hash: &'a str,
}

/// Why an event body can't be hashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyErrorKind {
    /// A number serde_json holds as a float: one with a fraction or an
    /// exponent (`1.5`, `1.0`, `1e3`), `-0`, or an integer too large for a
    /// 64-bit integer.
    NotAnInteger,
    /// An integer outside ±(2^53 − 1).
    IntegerOutOfRange,
    /// Arrays and objects nested deeper than [`MAX_BODY_DEPTH`].
    NestedTooDeep,
}

impl fmt::Display for BodyErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotAnInteger => "number is not an integer",
            Self::IntegerOutOfRange => "integer is outside ±(2^53 − 1)",
            Self::NestedTooDeep => "value nests too deeply",
        })
    }
}

/// A value in an event body that isn't allowed on the hash path.
///
/// Names where the value is and what is wrong with it, never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyError {
    pub kind: BodyErrorKind,
    /// JSON Pointer (RFC 6901) to the value; empty for the body itself.
    /// Cut to [`MAX_ERROR_PATH_LEN`] bytes.
    pub path: String,
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "event body: {}", self.kind)
        } else {
            // Escaped, so a key can't break a log line.
            write!(
                f,
                "event body at {}: {}",
                self.path.escape_debug(),
                self.kind
            )
        }
    }
}

impl std::error::Error for BodyError {}

/// The canonical JSON serialiser refused a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalizationError;

impl fmt::Display for CanonicalizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("value cannot be serialised as RFC 8785 canonical JSON")
    }
}

impl std::error::Error for CanonicalizationError {}

/// Why [`event_hash`] refused an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventHashError {
    /// The body failed [`check_body_numbers`].
    Body(BodyError),
    /// `seq`, `globalPos` or `tsMs` (named here) is above [`MAX_SAFE_INTEGER`].
    FieldOutOfRange(&'static str),
    /// Not expected once the checks above pass; kept so hashing never panics.
    Canonicalization(CanonicalizationError),
}

impl fmt::Display for EventHashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body(error) => error.fmt(f),
            Self::FieldOutOfRange(field) => write!(f, "event {field} is above 2^53 − 1"),
            Self::Canonicalization(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for EventHashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Body(error) => Some(error),
            Self::FieldOutOfRange(_) => None,
            Self::Canonicalization(error) => Some(error),
        }
    }
}

impl From<BodyError> for EventHashError {
    fn from(error: BodyError) -> Self {
        Self::Body(error)
    }
}

impl From<CanonicalizationError> for EventHashError {
    fn from(error: CanonicalizationError) -> Self {
        Self::Canonicalization(error)
    }
}

/// Computes an event's hash: 64 lowercase hex characters of
/// SHA-256 over the RFC 8785 canonical JSON of `event`.
///
/// Refuses the event, before hashing anything, if its body fails
/// [`check_body_numbers`] or a numeric field is above [`MAX_SAFE_INTEGER`].
pub fn event_hash(event: &HashedEvent<'_>) -> Result<String, EventHashError> {
    check_numeric_fields(event)?;
    check_body_numbers(event.body)?;
    let canonical = to_canonical_bytes(event)?;
    Ok(hex::encode(Sha256::digest(&canonical)))
}

/// Serialises `value` as RFC 8785 canonical JSON, the bytes an event hash
/// covers. The RFC 8785 test vectors are checked through this function.
///
/// Takes a [`Value`] because canon-json 0.2.1 panics on a map whose keys are
/// not strings (`lib.rs`, "Unhandled write into object key"); a `Value`'s
/// object keys always are.
///
/// Checks no numbers: for event data, run [`check_body_numbers`] (or
/// [`event_hash`], which runs it) first.
pub fn canonical_json(value: &Value) -> Result<Vec<u8>, CanonicalizationError> {
    to_canonical_bytes(value)
}

/// Canonical JSON for types whose maps all have string keys: [`Value`] and
/// [`HashedEvent`]. Private so no other type can reach canon-json's panic.
fn to_canonical_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, CanonicalizationError> {
    value.to_canon_json_vec().map_err(|_| CanonicalizationError)
}

/// Checks that every number anywhere in `body` is an integer within
/// ±(2^53 − 1), and that arrays and objects nest no deeper than
/// [`MAX_BODY_DEPTH`]. Reports the first offending value.
pub fn check_body_numbers(body: &Value) -> Result<(), BodyError> {
    check_value(body, &mut Vec::new())
}

fn check_numeric_fields(event: &HashedEvent<'_>) -> Result<(), EventHashError> {
    let fields = [
        ("seq", event.seq),
        ("globalPos", event.global_pos),
        ("tsMs", event.ts_ms),
    ];
    match fields.iter().find(|(_, value)| *value > MAX_SAFE_INTEGER) {
        Some((name, _)) => Err(EventHashError::FieldOutOfRange(name)),
        None => Ok(()),
    }
}

/// One step of the path from the body to a value.
enum PathSegment<'a> {
    Key(&'a str),
    Index(usize),
}

fn check_value<'a>(value: &'a Value, path: &mut Vec<PathSegment<'a>>) -> Result<(), BodyError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) => check_number(number).map_err(|kind| body_error(kind, path)),
        Value::Array(items) => {
            check_depth(path)?;
            for (index, item) in items.iter().enumerate() {
                check_child(item, PathSegment::Index(index), path)?;
            }
            Ok(())
        }
        Value::Object(members) => {
            check_depth(path)?;
            for (key, member) in members {
                check_child(member, PathSegment::Key(key), path)?;
            }
            Ok(())
        }
    }
}

fn check_child<'a>(
    child: &'a Value,
    segment: PathSegment<'a>,
    path: &mut Vec<PathSegment<'a>>,
) -> Result<(), BodyError> {
    path.push(segment);
    let result = check_value(child, path);
    path.pop();
    result
}

/// `path` holds one segment per enclosing array or object.
fn check_depth(path: &[PathSegment<'_>]) -> Result<(), BodyError> {
    if path.len() < MAX_BODY_DEPTH {
        Ok(())
    } else {
        Err(body_error(BodyErrorKind::NestedTooDeep, path))
    }
}

fn check_number(number: &Number) -> Result<(), BodyErrorKind> {
    let magnitude = match (number.as_i64(), number.as_u64()) {
        (Some(signed), _) => signed.unsigned_abs(),
        (None, Some(unsigned)) => unsigned,
        (None, None) => return Err(BodyErrorKind::NotAnInteger),
    };
    if magnitude <= MAX_SAFE_INTEGER {
        Ok(())
    } else {
        Err(BodyErrorKind::IntegerOutOfRange)
    }
}

fn body_error(kind: BodyErrorKind, path: &[PathSegment<'_>]) -> BodyError {
    BodyError {
        kind,
        path: json_pointer(path),
    }
}

/// Formats `path` as a JSON Pointer (RFC 6901 section 3), stopping once it
/// is longer than [`MAX_ERROR_PATH_LEN`] so a huge key costs nothing.
fn json_pointer(path: &[PathSegment<'_>]) -> String {
    let mut pointer = String::new();
    for segment in path {
        pointer.push('/');
        match segment {
            PathSegment::Key(key) => {
                for c in key.chars() {
                    match c {
                        '~' => pointer.push_str("~0"),
                        '/' => pointer.push_str("~1"),
                        c => pointer.push(c),
                    }
                    if pointer.len() > MAX_ERROR_PATH_LEN {
                        return cut(pointer);
                    }
                }
            }
            PathSegment::Index(index) => pointer.push_str(&index.to_string()),
        }
        if pointer.len() > MAX_ERROR_PATH_LEN {
            return cut(pointer);
        }
    }
    pointer
}

/// Cuts `pointer` to [`MAX_ERROR_PATH_LEN`] bytes on a character boundary and
/// marks the cut with `…`.
fn cut(mut pointer: String) -> String {
    let mut end = MAX_ERROR_PATH_LEN;
    while !pointer.is_char_boundary(end) {
        end -= 1;
    }
    pointer.truncate(end);
    pointer.push('…');
    pointer
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PREV: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn sample_body() -> Value {
        // Keys deliberately out of order: canonical JSON sorts them.
        json!({
            "tool": "read_file",
            "nested": { "b": 1, "a": "é" },
            "flags": [true, null],
            "delta": -5,
            "attempt": 2
        })
    }

    fn sample_event(body: &Value) -> HashedEvent<'_> {
        HashedEvent {
            run_id: "run-01",
            seq: 3,
            global_pos: 7,
            kind: "tool.called",
            ts_ms: 1_790_000_000_000,
            body,
            prev_hash: PREV,
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn body_error_of(body: Value) -> BodyError {
        check_body_numbers(&body).unwrap_err()
    }

    #[test]
    fn zero_hash_is_64_zero_characters() {
        assert_eq!(ZERO_HASH.len(), 64);
        assert!(ZERO_HASH.chars().all(|c| c == '0'));
    }

    #[test]
    fn known_answer_hash_matches_the_hand_written_canonical_object() {
        let body = sample_body();
        let event = sample_event(&body);
        // Written by hand from RFC 8785: keys sorted by UTF-16 code units at
        // every level, no whitespace, "é" as raw UTF-8.
        let expected = concat!(
            r#"{"body":{"attempt":2,"delta":-5,"flags":[true,null],"#,
            r#""nested":{"a":"é","b":1},"tool":"read_file"},"#,
            r#""globalPos":7,"kind":"tool.called","#,
            r#""prevHash":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","#,
            r#""runId":"run-01","seq":3,"tsMs":1790000000000}"#,
        );

        assert_eq!(to_canonical_bytes(&event).unwrap(), expected.as_bytes());

        let hash = event_hash(&event).unwrap();
        assert_eq!(hash, sha256_hex(expected.as_bytes()));
        // Pinned; also `printf '%s' "$expected" | sha256sum`.
        assert_eq!(
            hash,
            "bcd2d007d1b7e5ff89216677dcd55f7d5cb86b2dc995fbbc6a66d3433df75b80"
        );
    }

    #[test]
    fn known_answer_hash_for_a_run_s_first_event() {
        let body = json!({});
        let event = HashedEvent {
            run_id: "run-01",
            seq: 1,
            global_pos: 1,
            kind: "run.started",
            ts_ms: 1_790_000_000_000,
            body: &body,
            prev_hash: ZERO_HASH,
        };
        let expected = concat!(
            r#"{"body":{},"globalPos":1,"kind":"run.started","#,
            r#""prevHash":"0000000000000000000000000000000000000000000000000000000000000000","#,
            r#""runId":"run-01","seq":1,"tsMs":1790000000000}"#,
        );

        assert_eq!(to_canonical_bytes(&event).unwrap(), expected.as_bytes());
        assert_eq!(
            event_hash(&event).unwrap(),
            "322193c3b358aa7be24c5fc29b198cb2c5a1f2c906908ea6b31bbfcd97d13daa"
        );
    }

    #[test]
    fn hash_is_64_lowercase_hex_characters() {
        let body = sample_body();
        let hash = event_hash(&sample_event(&body)).unwrap();

        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    }

    #[test]
    fn changing_any_field_changes_the_hash() {
        let body = sample_body();
        let other_body = json!({ "attempt": 3 });
        let base = sample_event(&body);
        let variants = [
            HashedEvent {
                run_id: "run-02",
                ..base
            },
            HashedEvent { seq: 4, ..base },
            HashedEvent {
                global_pos: 8,
                ..base
            },
            HashedEvent {
                kind: "tool.returned",
                ..base
            },
            HashedEvent {
                ts_ms: 1_790_000_000_001,
                ..base
            },
            HashedEvent {
                body: &other_body,
                ..base
            },
            HashedEvent {
                prev_hash: ZERO_HASH,
                ..base
            },
        ];

        let mut hashes = vec![event_hash(&base).unwrap()];
        hashes.extend(variants.iter().map(|event| event_hash(event).unwrap()));

        let distinct: std::collections::HashSet<&String> = hashes.iter().collect();
        assert_eq!(distinct.len(), hashes.len());
    }

    #[test]
    fn rejects_a_fraction() {
        assert_eq!(
            body_error_of(json!({ "x": 1.5 })),
            BodyError {
                kind: BodyErrorKind::NotAnInteger,
                path: "/x".to_owned()
            }
        );
    }

    #[test]
    fn rejects_integral_numbers_written_as_floats() {
        let parsed: Value = serde_json::from_str(r#"{"x":1.0,"y":1e3}"#).unwrap();

        assert_eq!(
            check_body_numbers(&parsed).unwrap_err().kind,
            BodyErrorKind::NotAnInteger
        );
    }

    #[test]
    fn rejects_integers_beyond_2_pow_53_minus_1() {
        let too_big = MAX_SAFE_INTEGER + 1;
        let too_small = -(MAX_SAFE_INTEGER as i64) - 1;

        for body in [
            json!({ "n": too_big }),
            json!({ "n": too_small }),
            json!({ "n": u64::MAX }),
            json!({ "n": i64::MIN }),
        ] {
            assert_eq!(
                body_error_of(body),
                BodyError {
                    kind: BodyErrorKind::IntegerOutOfRange,
                    path: "/n".to_owned()
                }
            );
        }
    }

    #[test]
    fn rejects_an_integer_too_large_for_serde_json_as_not_an_integer() {
        let parsed: Value = serde_json::from_str(r#"{"n":18446744073709551616}"#).unwrap();

        assert_eq!(
            check_body_numbers(&parsed).unwrap_err().kind,
            BodyErrorKind::NotAnInteger
        );
    }

    #[test]
    fn accepts_the_safe_integer_bounds() {
        let body = json!({
            "max": MAX_SAFE_INTEGER,
            "min": -(MAX_SAFE_INTEGER as i64),
            "zero": 0,
            "list": [MAX_SAFE_INTEGER, -(MAX_SAFE_INTEGER as i64)]
        });

        assert_eq!(check_body_numbers(&body), Ok(()));
        assert!(event_hash(&sample_event(&body)).is_ok());
    }

    #[test]
    fn rejects_a_float_nested_in_arrays_and_objects() {
        assert_eq!(
            body_error_of(json!({ "a": [{ "b": [0, 0.5] }] })),
            BodyError {
                kind: BodyErrorKind::NotAnInteger,
                path: "/a/0/b/1".to_owned()
            }
        );
    }

    #[test]
    fn rejects_a_bare_number_body_with_an_empty_path() {
        assert_eq!(
            body_error_of(json!(2.5)),
            BodyError {
                kind: BodyErrorKind::NotAnInteger,
                path: String::new()
            }
        );
    }

    #[test]
    fn escapes_keys_in_the_path_as_json_pointer() {
        assert_eq!(body_error_of(json!({ "a/b~c": 0.1 })).path, "/a~1b~0c");
    }

    #[test]
    fn error_messages_name_the_path_but_not_the_value() {
        let error = body_error_of(json!({ "usage": { "cost": 1.2345 } }));
        let message = EventHashError::from(error).to_string();

        assert_eq!(
            message,
            "event body at /usage/cost: number is not an integer"
        );
        assert!(!message.contains("1.2345"));
    }

    #[test]
    fn error_messages_escape_control_characters_in_keys() {
        let message = body_error_of(json!({ "a\nb": 0.5 })).to_string();

        assert!(!message.contains('\n'));
    }

    #[test]
    fn event_hash_refuses_a_body_with_a_float() {
        let body = json!({ "cost": 1.5 });

        assert!(matches!(
            event_hash(&sample_event(&body)),
            Err(EventHashError::Body(BodyError {
                kind: BodyErrorKind::NotAnInteger,
                ..
            }))
        ));
    }

    #[test]
    fn event_hash_refuses_numeric_fields_above_2_pow_53_minus_1() {
        let body = sample_body();
        let base = sample_event(&body);
        let cases = [
            (
                HashedEvent {
                    seq: MAX_SAFE_INTEGER + 1,
                    ..base
                },
                "seq",
            ),
            (
                HashedEvent {
                    global_pos: u64::MAX,
                    ..base
                },
                "globalPos",
            ),
            (
                HashedEvent {
                    ts_ms: MAX_SAFE_INTEGER + 1,
                    ..base
                },
                "tsMs",
            ),
        ];

        for (event, field) in cases {
            assert_eq!(
                event_hash(&event),
                Err(EventHashError::FieldOutOfRange(field))
            );
        }
        let at_limit = HashedEvent {
            seq: MAX_SAFE_INTEGER,
            global_pos: MAX_SAFE_INTEGER,
            ts_ms: MAX_SAFE_INTEGER,
            ..base
        };
        assert!(event_hash(&at_limit).is_ok());
    }

    fn nested_arrays(depth: usize) -> Value {
        (0..depth).fold(json!(0), |inner, _| Value::Array(vec![inner]))
    }

    #[test]
    fn accepts_nesting_up_to_the_limit_and_rejects_deeper() {
        assert_eq!(check_body_numbers(&nested_arrays(MAX_BODY_DEPTH)), Ok(()));

        let error = check_body_numbers(&nested_arrays(MAX_BODY_DEPTH + 1)).unwrap_err();
        assert_eq!(error.kind, BodyErrorKind::NestedTooDeep);
        assert_eq!(error.path, "/0".repeat(MAX_BODY_DEPTH));
    }

    #[test]
    fn the_deepest_accepted_body_parses_again_inside_an_envelope() {
        let body = nested_arrays(MAX_BODY_DEPTH);
        let canonical =
            String::from_utf8(to_canonical_bytes(&sample_event(&body)).unwrap()).unwrap();
        // The hashed object inside eight more levels, such as an export or an
        // RPC response would add.
        let wrapped = format!("{}{canonical}{}", "[".repeat(8), "]".repeat(8));

        assert!(event_hash(&sample_event(&body)).is_ok());
        assert!(serde_json::from_str::<Value>(&wrapped).is_ok());
    }

    #[test]
    fn rejects_negative_zero_which_serde_json_holds_as_a_float() {
        let parsed: Value = serde_json::from_str(r#"{"x":-0}"#).unwrap();

        assert_eq!(
            check_body_numbers(&parsed).unwrap_err().kind,
            BodyErrorKind::NotAnInteger
        );
    }

    #[test]
    fn cuts_long_paths_in_errors() {
        let long_key = "k".repeat(10_000);
        let error = body_error_of(json!({ long_key: 0.5 }));

        assert_eq!(
            error.path,
            format!("/{}…", "k".repeat(MAX_ERROR_PATH_LEN - 1))
        );
    }

    #[test]
    fn cuts_long_paths_on_a_character_boundary() {
        let long_key = "é".repeat(MAX_ERROR_PATH_LEN);
        let error = body_error_of(json!({ long_key: 0.5 }));

        assert!(error.path.ends_with('…'));
        assert!(error.path.len() <= MAX_ERROR_PATH_LEN + '…'.len_utf8());
    }
}
