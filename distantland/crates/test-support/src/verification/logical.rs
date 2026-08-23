//! Bit-preserving primitives for the V1 canonical logical output model.
//!
//! These helpers define the canonical JSON building blocks shared by snapshot construction
//! and comparison:
//!
//! - floats are never serialized as JSON numbers: [`f32_bits`] and [`f16_bits`] emit exact
//!   stored bits as fixed-width lowercase hex strings, preserving `-0.0` and NaN payloads;
//! - multisets are JSON arrays sorted by each element's compact canonical encoding, with
//!   duplicate elements retained ([`canonical_multiset`]);
//! - closed polygons and triangles use the lexicographically smallest cyclic rotation while
//!   preserving winding (`canonical_triangle_corners`, in `verification::snapshot::statics`);
//! - byte strings that are not valid UTF-8 are encoded as `{"hex": …}` objects instead of
//!   lossy conversion ([`byte_string_value`]), and display-name object keys use an injective
//!   `hex!`-prefixed escape ([`escaped_name_key`]).
//!
//! Canonical compact JSON is deterministic because `serde_json`'s default map is ordered by
//! key bytes and every array is either a fixed tuple or a pre-sorted multiset.

use half::f16;
use serde_json::Value;

/// Encodes an `f32` as its exact stored bit pattern: eight lowercase hex digits.
pub fn f32_bits(value: f32) -> Value {
    Value::String(format!("{:08x}", value.to_bits()))
}

/// Encodes an `f16` as its exact stored bit pattern: four lowercase hex digits.
pub fn f16_bits(value: f16) -> Value {
    Value::String(format!("{:04x}", value.to_bits()))
}

/// Encodes arbitrary bytes as lowercase hex.
pub fn bytes_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 15)] as char);
    }
    out
}

/// Encodes a byte string: valid UTF-8 becomes a JSON string, anything else `{"hex": …}`.
///
/// The two encodings can never collide because one is a string and the other an object,
/// so the mapping is injective over byte strings.
pub fn byte_string_value(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => Value::String(text.to_owned()),
        Err(_) => {
            let mut map = serde_json::Map::new();
            map.insert("hex".to_owned(), Value::String(bytes_hex(bytes)));
            Value::Object(map)
        }
    }
}

/// Encodes a display-name byte string as a deterministic, injective JSON object key.
///
/// Names that are valid UTF-8, contain no control characters, and do not begin with the
/// reserved `hex!` prefix are used directly; every other name becomes `hex!<lowercase-hex>`.
pub fn escaped_name_key(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.starts_with("hex!") && !text.chars().any(char::is_control) => text.to_owned(),
        _ => format!("hex!{}", bytes_hex(bytes)),
    }
}

/// Serializes a canonical value to compact canonical JSON.
///
/// Object keys are emitted in lexicographic byte order (the default `serde_json` map is
/// ordered), arrays keep their canonical order, and no whitespace is emitted.
pub fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).expect("canonical JSON values always serialize")
}

/// Returns the BLAKE3 hex digest of a value's compact canonical encoding.
pub fn value_digest(value: &Value) -> String {
    blake3::hash(compact_json(value).as_bytes()).to_hex().to_string()
}

/// Sorts multiset elements by their compact canonical encoding, retaining duplicates.
pub fn canonical_multiset(items: Vec<Value>) -> Value {
    let mut keyed: Vec<(String, Value)> = items.into_iter().map(|item| (compact_json(&item), item)).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Array(keyed.into_iter().map(|(_, item)| item).collect())
}
