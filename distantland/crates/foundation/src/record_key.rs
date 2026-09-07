//! Typed identity of one packed static-mesh record in the sharded XESTAT06 output.
//!
//! [`StaticRecordKey`](crate::record_key::StaticRecordKey) is the single source of truth for where a record lives (which
//! shard, and its position inside that shard) and how it is named in the persisted
//! per-shard key lists. Its [`render`](crate::record_key::StaticRecordKey::render) is byte-identical to the
//! string keys used in the live `DistantStatics` map:
//!
//! - an ordinary owner renders as its mesh/reference id verbatim;
//! - a synthetic merged owner renders as `CELL ({x}, {y}) GROUP ({i})`. This module is the
//!   sole producer of that spelling; `statics`' `MergeGroup::synthetic_id` calls `render`.
//!
//! Shard assignment and intra-shard ordering both derive from these rendered bytes
//! (via [`static_mesh_shard_id`](crate::record_key::static_mesh_shard_id) and byte-order sorting), so the typed key and the raw
//! map-key string agree by construction. This is what lets the owner-partial path
//! carry, splice, and re-order records without re-deriving the global ordinal space.

use std::borrow::Cow;
use std::cmp::Ordering;

use crate::output::STATIC_MESH_SHARD_COUNT;

/// Stable assignment domain for the fixed static shard set.
pub const STATIC_SHARD_ASSIGNMENT_MAGIC: &[u8] = b"tes3-distantland-static-shard-assignment-v2\0";

/// Assigns a packed-static key to its fixed shard.
pub fn static_mesh_shard_id(key: &str) -> usize {
    static_mesh_shard_id_bytes(key.as_bytes())
}

/// Byte-slice core of [`static_mesh_shard_id`].
///
/// Hashes assignment magic, shard count, length, and raw key bytes.
pub fn static_mesh_shard_id_bytes(key: &[u8]) -> usize {
    let mut hasher = blake3::Hasher::new();
    hasher.update(STATIC_SHARD_ASSIGNMENT_MAGIC);
    hasher.update(&(STATIC_MESH_SHARD_COUNT as u32).to_le_bytes());
    hasher.update(&(key.len() as u64).to_le_bytes());
    hasher.update(key);
    let prefix = <[u8; 4]>::try_from(&hasher.finalize().as_bytes()[..4]).expect("four-byte hash prefix");
    (u32::from_le_bytes(prefix) as usize) & (STATIC_MESH_SHARD_COUNT - 1)
}

/// Formats the synthetic merged spelling `CELL ({cell_x}, {cell_y}) GROUP ({group_idx})`.
///
/// The sole producer of that spelling: `render`, `shard_id`, and `Ord` all route through
/// [`StaticRecordKey::rendered`] to here, so the three can never disagree.
fn render_merged(cell_x: i32, cell_y: i32, group_idx: u32) -> String {
    format!("CELL ({cell_x}, {cell_y}) GROUP ({group_idx})")
}

/// The identity of one packed static-mesh record.
///
/// # Ordering
///
/// `Ord`/`PartialOrd` compare by [`render`](StaticRecordKey::render) bytes so that sorting
/// a key list reproduces the shard's intra-order (`finalize_distant_statics` sorts records
/// by `key.as_bytes()` within a shard). `Eq`/`PartialEq` are *structural*. These agree in
/// practice because every key originates from a string-keyed map (one string ⇒ one variant
/// via [`parse`](StaticRecordKey::parse)), so no two structurally-distinct keys ever share a
/// rendering within the same collection; any residual divergence is caught fail-closed by the
/// assembly key-vector equality check during splicing.
#[derive(Clone, Debug, Eq, Hash, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum StaticRecordKey {
    /// An ordinary owner keyed by its mesh/reference id string.
    Mesh {
        /// Mesh or reference id that owns the record.
        id: String,
    },
    /// A synthetic merged owner keyed by its exterior cell and group index.
    Merged {
        /// Exterior cell X coordinate of the merge group.
        cell_x: i32,
        /// Exterior cell Y coordinate of the merge group.
        cell_y: i32,
        /// Zero-based group index within the cell.
        group_idx: u32,
    },
}

impl StaticRecordKey {
    /// Renders the key to its `DistantStatics` map-key string.
    ///
    /// The output is byte-identical to the string a full build would use for this record.
    pub fn render(&self) -> String {
        self.rendered().into_owned()
    }

    /// Borrows the rendered map-key string, allocating only for the synthetic merged form.
    ///
    /// The comparison and hashing paths use this instead of [`render`](Self::render) so an
    /// ordinary mesh key costs no allocation.
    fn rendered(&self) -> Cow<'_, str> {
        match self {
            StaticRecordKey::Mesh { id } => Cow::Borrowed(id),
            StaticRecordKey::Merged {
                cell_x,
                cell_y,
                group_idx,
            } => Cow::Owned(render_merged(*cell_x, *cell_y, *group_idx)),
        }
    }

    /// Classifies a rendered map-key string back into a typed key.
    ///
    /// A string matching the exact synthetic form `CELL ({i32}, {i32}) GROUP ({u32})`
    /// becomes [`StaticRecordKey::Merged`]; every other string is an ordinary
    /// [`StaticRecordKey::Mesh`]. This is the inverse of [`render`](Self::render) for
    /// rendered keys and is total: classification of a persisted key always yields a key.
    pub fn parse(value: &str) -> StaticRecordKey {
        match parse_merged(value) {
            Some((cell_x, cell_y, group_idx)) => StaticRecordKey::Merged {
                cell_x,
                cell_y,
                group_idx,
            },
            None => StaticRecordKey::Mesh { id: value.to_owned() },
        }
    }

    /// Returns the fixed shard this record is assigned to.
    ///
    /// Hashes the same rendered key bytes as [`static_mesh_shard_id`] over [`render`](Self::render),
    /// matching the packing performed by `prepare_statics_bundle`.
    pub fn shard_id(&self) -> usize {
        static_mesh_shard_id(&self.rendered())
    }
}

/// Parses the exact synthetic merged-owner form `CELL ({i32}, {i32}) GROUP ({u32})`.
///
/// Returns `None` for any string that does not match the form exactly, including strings
/// with extra surrounding text or non-integer fields. Those are ordinary mesh keys.
///
/// This is the borrowing half of [`StaticRecordKey::parse`]: callers that only need to
/// classify a map key, or that need the merged coordinates, should use this instead of
/// parsing, whose `Mesh` arm copies the whole key string.
pub fn parse_merged(value: &str) -> Option<(i32, i32, u32)> {
    let rest = value.strip_prefix("CELL (")?;
    let (coords, rest) = rest.split_once(") GROUP (")?;
    let group = rest.strip_suffix(')')?;
    let (x, y) = coords.split_once(", ")?;
    Some((x.parse().ok()?, y.parse().ok()?, group.parse().ok()?))
}

impl Ord for StaticRecordKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // Textual map-key order: exactly the byte order a full build's string keys sort into.
        self.rendered().as_bytes().cmp(other.rendered().as_bytes())
    }
}

impl PartialOrd for StaticRecordKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_key::static_mesh_shard_id;
    use itertools::Itertools;

    #[test]
    fn render_matches_literal_map_keys() {
        assert_eq!(
            StaticRecordKey::Mesh {
                id: "meshes\\x\\my_static.nif".to_owned(),
            }
            .render(),
            "meshes\\x\\my_static.nif"
        );
        assert_eq!(
            StaticRecordKey::Merged {
                cell_x: 1,
                cell_y: 2,
                group_idx: 3
            }
            .render(),
            "CELL (1, 2) GROUP (3)"
        );
        // Negative coordinates and a zero group index round-trip the synthetic form.
        assert_eq!(
            StaticRecordKey::Merged {
                cell_x: -5,
                cell_y: -7,
                group_idx: 0
            }
            .render(),
            "CELL (-5, -7) GROUP (0)"
        );
    }

    #[test]
    fn parse_is_inverse_of_render() {
        let keys = [
            StaticRecordKey::Mesh {
                id: "furn_de_bench_01".to_owned(),
            },
            StaticRecordKey::Mesh {
                id: "meshes\\f\\flora_tree.nif".to_owned(),
            },
            StaticRecordKey::Merged {
                cell_x: 3,
                cell_y: -4,
                group_idx: 9,
            },
            StaticRecordKey::Merged {
                cell_x: 0,
                cell_y: 0,
                group_idx: 0,
            },
        ];
        for key in keys {
            assert_eq!(StaticRecordKey::parse(&key.render()), key);
        }
    }

    #[test]
    fn parse_rejects_malformed_synthetic_forms_as_mesh() {
        // Synthetic-looking but malformed strings classify as ordinary mesh keys.
        for value in [
            "CELL (1, 2) GROUP (abc)",      // non-integer group
            "CELL (1, 2) GROUP (3) x",      // trailing text
            "prefix CELL (1, 2) GROUP (3)", // leading text
            "CELL (1,2) GROUP (3)",         // missing space after comma
            "CELL (1, 2) GROUP (3.0)",      // non-integer group
        ] {
            assert_eq!(StaticRecordKey::parse(value), StaticRecordKey::Mesh { id: value.to_owned() });
        }
    }

    #[test]
    fn shard_id_matches_raw_string_assignment() {
        let mesh = StaticRecordKey::Mesh {
            id: "meshes\\x\\a.nif".to_owned(),
        };
        assert_eq!(mesh.shard_id(), static_mesh_shard_id("meshes\\x\\a.nif"));
        let merged = StaticRecordKey::Merged {
            cell_x: 7,
            cell_y: -3,
            group_idx: 2,
        };
        assert_eq!(merged.shard_id(), static_mesh_shard_id("CELL (7, -3) GROUP (2)"));
    }

    #[test]
    fn ordering_matches_rendered_byte_sort() {
        let mut keys = [
            StaticRecordKey::Merged {
                cell_x: 1,
                cell_y: 2,
                group_idx: 0,
            },
            StaticRecordKey::Mesh { id: "zzz".to_owned() },
            StaticRecordKey::Mesh { id: "aaa".to_owned() },
            StaticRecordKey::Merged {
                cell_x: 1,
                cell_y: 2,
                group_idx: 10,
            },
            StaticRecordKey::Mesh {
                id: "CELL (1, 2) GROUP (5)".to_owned(),
            },
        ];
        let mut rendered = keys.iter().map(StaticRecordKey::render).collect_vec();
        keys.sort();
        rendered.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        let sorted_rendered = keys.iter().map(StaticRecordKey::render).collect_vec();
        assert_eq!(sorted_rendered, rendered);
    }

    #[test]
    fn rkyv_round_trip_vec_of_keys() {
        // Guards the archived enum layout persisted in generation state.
        let keys = vec![
            StaticRecordKey::Mesh {
                id: "meshes\\a.nif".to_owned(),
            },
            StaticRecordKey::Merged {
                cell_x: -1,
                cell_y: 2,
                group_idx: 3,
            },
        ];
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&keys).expect("serialize");
        let restored = rkyv::from_bytes::<Vec<StaticRecordKey>, rkyv::rancor::Error>(&bytes).expect("deserialize");
        assert_eq!(restored, keys);
    }
}
