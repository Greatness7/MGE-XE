//! Snapshot comparison: digest equality plus a bounded, path-addressed structural diff.
//!
//! Equality is decided by the compact-canonical BLAKE3 digests. When digests differ, a merge
//! walk over the two canonical trees reports up to a bounded number of concrete mismatches,
//! each named by a `/`-separated logical path over the canonical layout (for example
//! `logical/statics/records/3/subsets/0/material/texels`), plus a count of omitted
//! differences. Arrays of equal length diff positionally (fixed tuples and paired multiset
//! elements); arrays of unequal length diff as sorted multisets, naming unpaired elements by
//! a truncated content digest.

use serde::Serialize;
use serde_json::Value;

use crate::verification::logical::compact_json;
use crate::verification::snapshot::StaticOutputSnapshot;

/// Rendered-value length cap in mismatch reports.
const RENDER_LIMIT: usize = 96;

/// One concrete logical difference between two snapshots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotMismatch {
    /// `/`-separated path over the canonical logical layout.
    pub path: String,
    /// Truncated canonical rendering of the left value; `None` when absent on the left.
    pub left: Option<String>,
    /// Truncated canonical rendering of the right value; `None` when absent on the right.
    pub right: Option<String>,
}

/// Result of comparing two logical snapshots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotComparison {
    /// Whether the logical digests are equal.
    pub equal: bool,
    /// Left snapshot digest.
    pub left_digest: String,
    /// Right snapshot digest.
    pub right_digest: String,
    /// Up to the requested bound of concrete mismatches, in canonical walk order.
    pub mismatches: Vec<SnapshotMismatch>,
    /// Number of additional mismatches found but not reported.
    pub omitted_mismatches: usize,
}

/// Compares two snapshots and reports a bounded structural diff of their logical content.
///
/// Diagnostics never participate: only the `logical` values are compared. `limit` bounds the
/// reported mismatches; further differences are counted in `omitted_mismatches`.
pub(super) fn compare_snapshots(
    left: &StaticOutputSnapshot,
    right: &StaticOutputSnapshot,
    limit: usize,
) -> SnapshotComparison {
    let left_digest = left.digest();
    let right_digest = right.digest();
    if left_digest == right_digest {
        return SnapshotComparison {
            equal: true,
            left_digest,
            right_digest,
            mismatches: Vec::new(),
            omitted_mismatches: 0,
        };
    }

    let mut collector = Collector {
        limit,
        mismatches: Vec::new(),
        omitted: 0,
    };
    diff_value("logical", &left.logical, &right.logical, &mut collector);
    if collector.mismatches.is_empty() && collector.omitted == 0 {
        // Unreachable for well-formed snapshots (equal trees produce equal digests), but a
        // digest/tree disagreement must never be reported as silently equal.
        collector.mismatches.push(SnapshotMismatch {
            path: "logical".to_owned(),
            left: Some("digest mismatch without a structural difference".to_owned()),
            right: Some("digest mismatch without a structural difference".to_owned()),
        });
    }
    SnapshotComparison {
        equal: false,
        left_digest,
        right_digest,
        mismatches: collector.mismatches,
        omitted_mismatches: collector.omitted,
    }
}

struct Collector {
    limit: usize,
    mismatches: Vec<SnapshotMismatch>,
    omitted: usize,
}

impl Collector {
    fn record(&mut self, path: String, left: Option<&Value>, right: Option<&Value>) {
        if self.mismatches.len() < self.limit {
            self.mismatches.push(SnapshotMismatch {
                path,
                left: left.map(render),
                right: right.map(render),
            });
        } else {
            self.omitted += 1;
        }
    }
}

/// Renders a value compactly, truncating long payloads (texel hex, digests) for readability.
fn render(value: &Value) -> String {
    let encoded = compact_json(value);
    if encoded.len() <= RENDER_LIMIT {
        return encoded;
    }
    let cut = encoded
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|&index| index <= RENDER_LIMIT)
        .last()
        .unwrap_or(0);
    format!("{}… ({} chars)", &encoded[..cut], encoded.len())
}

/// Short content digest used to name unpaired multiset elements in mismatch paths.
fn element_id(encoding: &str) -> String {
    blake3::hash(encoding.as_bytes()).to_hex()[..16].to_owned()
}

fn diff_value(path: &str, left: &Value, right: &Value, out: &mut Collector) {
    match (left, right) {
        (Value::Object(left_map), Value::Object(right_map)) => {
            for (key, left_value) in left_map {
                match right_map.get(key) {
                    Some(right_value) => diff_value(&format!("{path}/{key}"), left_value, right_value, out),
                    None => out.record(format!("{path}/{key}"), Some(left_value), None),
                }
            }
            for (key, right_value) in right_map {
                if !left_map.contains_key(key) {
                    out.record(format!("{path}/{key}"), None, Some(right_value));
                }
            }
        }
        (Value::Array(left_items), Value::Array(right_items)) if left_items.len() == right_items.len() => {
            for (index, (left_item, right_item)) in left_items.iter().zip(right_items).enumerate() {
                diff_value(&format!("{path}/{index}"), left_item, right_item, out);
            }
        }
        (Value::Array(left_items), Value::Array(right_items)) => {
            diff_multisets(path, left_items, right_items, out);
        }
        _ => {
            if left != right {
                out.record(path.to_owned(), Some(left), Some(right));
            }
        }
    }
}

/// Merge-diffs two canonically sorted arrays of unequal length, reporting unpaired elements.
fn diff_multisets(path: &str, left_items: &[Value], right_items: &[Value], out: &mut Collector) {
    let left_encoded: Vec<String> = left_items.iter().map(compact_json).collect();
    let right_encoded: Vec<String> = right_items.iter().map(compact_json).collect();
    let mut left_index = 0;
    let mut right_index = 0;
    while left_index < left_items.len() && right_index < right_items.len() {
        match left_encoded[left_index].cmp(&right_encoded[right_index]) {
            std::cmp::Ordering::Equal => {
                left_index += 1;
                right_index += 1;
            }
            std::cmp::Ordering::Less => {
                out.record(
                    format!("{path}/{}", element_id(&left_encoded[left_index])),
                    Some(&left_items[left_index]),
                    None,
                );
                left_index += 1;
            }
            std::cmp::Ordering::Greater => {
                out.record(
                    format!("{path}/{}", element_id(&right_encoded[right_index])),
                    None,
                    Some(&right_items[right_index]),
                );
                right_index += 1;
            }
        }
    }
    for index in left_index..left_items.len() {
        out.record(
            format!("{path}/{}", element_id(&left_encoded[index])),
            Some(&left_items[index]),
            None,
        );
    }
    for index in right_index..right_items.len() {
        out.record(
            format!("{path}/{}", element_id(&right_encoded[index])),
            None,
            Some(&right_items[index]),
        );
    }
}
