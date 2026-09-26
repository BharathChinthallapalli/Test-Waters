//! RFC 8785 conformance of the serialiser behind event hashes (R4.7).
//!
//! Feeds every published test vector in `tests/rfc8785/input` through
//! `cs_core::event::canonical_json` and compares the result byte for byte with
//! the file of the same name in `tests/rfc8785/output`. See
//! `tests/rfc8785/README.md` for where the vectors come from.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use cs_core::event::canonical_json;
use serde_json::Value;

const EXPECTED_VECTORS: [&str; 6] = [
    "arrays.json",
    "french.json",
    "structures.json",
    "unicode.json",
    "values.json",
    "weird.json",
];

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rfc8785")
}

fn vector_names() -> BTreeSet<String> {
    fs::read_dir(vectors_dir().join("input"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

#[test]
fn every_rfc8785_vector_matches_byte_for_byte() {
    let names = vector_names();
    // A missing or extra file must fail the test, not shrink it.
    assert_eq!(
        names,
        EXPECTED_VECTORS
            .iter()
            .map(|name| name.to_string())
            .collect()
    );

    for name in &names {
        let input = fs::read(vectors_dir().join("input").join(name)).unwrap();
        let expected = fs::read(vectors_dir().join("output").join(name)).unwrap();
        let value: Value = serde_json::from_slice(&input).unwrap();

        let actual = canonical_json(&value).unwrap();

        assert_eq!(
            actual,
            expected,
            "vector {name}: got {}",
            String::from_utf8_lossy(&actual)
        );
    }
    println!("RFC 8785 vectors matched byte for byte: {}", names.len());
}

/// Upstream `values.json` also holds `333333333.33333329`, which canon-json's
/// copy drops (see the README). Serialise that double directly: Rust's `f64`
/// parser rounds correctly, so this checks the formatter on its own.
#[test]
fn upstream_number_dropped_from_values_json_formats_as_published() {
    let number: f64 = "333333333.33333329".parse().unwrap();

    assert_eq!(canonical_json(&number).unwrap(), b"333333333.3333333");
}
