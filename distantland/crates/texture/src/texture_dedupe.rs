//! Exact texture deduplication: collapse equivalent textures to a canonical key.
//!
//! Dedupe is input canonicalization. Each domain (static opaque, static alpha, terrain)
//! groups its texture keys by an exact content fingerprint and picks one canonical key per
//! distinct fingerprint. Downstream consumers then resolve every original key back through
//! the alias map, so atlas/material outputs shrink while per-key wiring stays intact.

use std::hash::Hash;

use blake3::Hasher;
use hashbrown::HashMap;

/// Texture deduplication mode. Exact-only for the initial release.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Debug,
    Default,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TextureDedupeMode {
    /// No deduplication; every original texture is treated as a distinct input.
    Off,
    /// Exact deduplication: collapse byte-identical (and, for statics, decoded-identical)
    /// textures to a single canonical key.
    #[default]
    Exact,
}

/// Per-domain deduplication metrics for log and manifest reporting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextureDedupeDomainStats {
    /// Distinct original input keys before dedupe.
    pub input_count: usize,
    /// Canonical keys after dedupe.
    pub canonical_count: usize,
    /// Inputs collapsed by Tier-0 source-byte identity.
    pub source_alias_count: usize,
    /// Inputs collapsed by Tier-1 decoded identity (statics only; always 0 for terrain).
    pub decoded_alias_count: usize,
    /// Known inputs that failed to load and intentionally mapped to the default (terrain only).
    pub missing_to_default_count: usize,
}

/// 32-byte BLAKE3 content fingerprint. Callers fold any disambiguator (dimensions, source
/// size) into the hash before grouping.
pub type Fingerprint = [u8; 32];

/// Groups `keyed` by fingerprint, returning the canonical set (first key seen per fingerprint,
/// in iteration order) and an alias map (every original key -> its canonical key; canonicals
/// map to themselves).
///
/// `keyed` MUST arrive in deterministic order: the first key seen for a fingerprint becomes its
/// canonical, so iteration order fixes the result.
pub fn build_alias_map<K: Eq + Hash + Clone>(keyed: impl IntoIterator<Item = (K, Fingerprint)>) -> (Vec<K>, HashMap<K, K>) {
    let keyed = keyed.into_iter();
    // Every input key becomes one `alias` entry, so the lower size hint is exact for sized sources
    // and `alias` is the map that would otherwise rehash its way up from zero. `canonicals` and
    // `canonical_for` hold only the deduplicated keys, which is what this function exists to
    // shrink, so sizing them to the input would over-allocate exactly when dedupe pays off.
    let mut canonical_for: HashMap<Fingerprint, usize> = HashMap::new();
    let mut canonicals = Vec::new();
    let mut alias = HashMap::with_capacity(keyed.size_hint().0);
    for (key, fp) in keyed {
        let index = *canonical_for.entry(fp).or_insert_with(|| {
            let index = canonicals.len();
            canonicals.push(key.clone());
            index
        });
        let canonical = canonicals[index].clone();
        alias.insert(key, canonical);
    }
    (canonicals, alias)
}

/// Tier-0 fingerprint for a source texture: file size folded with the BLAKE3 hash of its
/// on-disk bytes.
pub fn source_fingerprint(size: u64, bytes_hash: &[u8; 32]) -> Fingerprint {
    let mut hasher = Hasher::new();
    hasher.update(&size.to_le_bytes());
    hasher.update(bytes_hash);
    *hasher.finalize().as_bytes()
}

/// Tier-1 fingerprint for a decoded image: dimensions folded with the raw RGBA bytes.
pub fn decoded_fingerprint(width: u32, height: u32, rgba: &[u8]) -> Fingerprint {
    let mut hasher = Hasher::new();
    hasher.update(&width.to_le_bytes());
    hasher.update(&height.to_le_bytes());
    hasher.update(rgba);
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(byte: u8) -> Fingerprint {
        [byte; 32]
    }

    #[test]
    fn identical_fingerprints_collapse_to_first_key() {
        let (canon, alias) = build_alias_map(vec![("b", fp(1)), ("a", fp(1)), ("c", fp(2))]);
        assert_eq!(canon, vec!["b", "c"]); // first-seen wins, even though "a" < "b"
        assert_eq!(alias[&"a"], "b");
        assert_eq!(alias[&"b"], "b");
        assert_eq!(alias[&"c"], "c");
    }

    #[test]
    fn distinct_fingerprints_stay_separate() {
        let (canon, alias) = build_alias_map(vec![("a", fp(1)), ("b", fp(2))]);
        assert_eq!(canon, vec!["a", "b"]);
        assert_eq!(alias[&"a"], "a");
        assert_eq!(alias[&"b"], "b");
    }

    #[test]
    fn iteration_order_fixes_canonical() {
        let (c1, _) = build_alias_map(vec![("a", fp(1)), ("b", fp(1))]);
        let (c2, _) = build_alias_map(vec![("b", fp(1)), ("a", fp(1))]);
        assert_eq!(c1, vec!["a"]);
        assert_eq!(c2, vec!["b"]);
    }

    #[test]
    fn source_fingerprint_distinguishes_size_and_bytes() {
        let h = [7u8; 32];
        assert_eq!(source_fingerprint(10, &h), source_fingerprint(10, &h));
        assert_ne!(source_fingerprint(10, &h), source_fingerprint(11, &h));
        assert_ne!(source_fingerprint(10, &h), source_fingerprint(10, &[8u8; 32]));
    }

    #[test]
    fn decoded_fingerprint_distinguishes_dims_and_pixels() {
        assert_eq!(decoded_fingerprint(2, 2, &[0; 16]), decoded_fingerprint(2, 2, &[0; 16]));
        assert_ne!(decoded_fingerprint(2, 2, &[0; 16]), decoded_fingerprint(4, 1, &[0; 16]));
        assert_ne!(decoded_fingerprint(2, 2, &[0; 16]), decoded_fingerprint(2, 2, &[1; 16]));
    }

    #[test]
    fn default_mode_is_exact() {
        assert_eq!(TextureDedupeMode::default(), TextureDedupeMode::Exact);
    }

    #[test]
    fn mixed_unique_and_duplicate_fingerprints_preserve_order_and_dense_aliases() {
        // Multiple unique fps plus multi-member groups; first-seen order and dense identity rows.
        let (canon, alias) = build_alias_map(vec![
            ("a", fp(1)),
            ("b", fp(2)),
            ("c", fp(1)),
            ("d", fp(3)),
            ("e", fp(2)),
            ("f", fp(2)),
        ]);
        assert_eq!(canon, vec!["a", "b", "d"]);
        assert_eq!(alias.len(), 6);
        assert_eq!(alias[&"a"], "a");
        assert_eq!(alias[&"b"], "b");
        assert_eq!(alias[&"c"], "a");
        assert_eq!(alias[&"d"], "d");
        assert_eq!(alias[&"e"], "b");
        assert_eq!(alias[&"f"], "b");
    }

    #[test]
    fn decoded_fingerprint_matches_sequential_reference_for_large_buffer() {
        // A buffer comfortably above blake3's ~128 KiB Rayon crossover, with an irregular
        // length so no hashing path can assume block alignment. Content is deterministic and
        // nonuniform; blake3 is data-independent, so only the byte sequence and length matter.
        let len: usize = 300 * 1024 + 123;
        let rgba: Vec<u8> = (0..len).map(|i| i.wrapping_mul(31).wrapping_add(7) as u8).collect();
        let (width, height) = (517_u32, 149_u32);

        // The exact sequential update sequence decoded_fingerprint is defined by.
        let reference = {
            let mut hasher = Hasher::new();
            hasher.update(&width.to_le_bytes());
            hasher.update(&height.to_le_bytes());
            hasher.update(&rgba);
            *hasher.finalize().as_bytes()
        };
        assert_eq!(decoded_fingerprint(width, height, &rgba), reference);

        // update_rayon must yield a byte-identical digest to the serial update, which is what
        // lets large-buffer hashing switch to the Rayon path without bumping DEDUPE_ALGO_VERSION.
        let rayon_digest = {
            let mut hasher = Hasher::new();
            hasher.update(&width.to_le_bytes());
            hasher.update(&height.to_le_bytes());
            hasher.update_rayon(&rgba);
            *hasher.finalize().as_bytes()
        };
        assert_eq!(rayon_digest, reference);
    }

    #[test]
    fn parallel_fingerprint_collection_fixes_canonical_by_input_order() {
        use rayon::prelude::*;

        // Mirrors the atlas call sites: decoded-identical duplicates (a==c, b==e) fingerprinted
        // under an outer par_iter and collected in order. par_iter over a slice is order-preserving,
        // so the first key of each decoded-equal group stays canonical regardless of thread timing.
        let a = [1u8, 2, 3, 4];
        let b = [9u8, 8, 7, 6];
        let d = [5u8, 5, 5, 5];
        let inputs: Vec<(&str, u32, u32, &[u8])> = vec![
            ("a", 1, 1, &a),
            ("b", 1, 1, &b),
            ("c", 1, 1, &a),
            ("d", 1, 1, &d),
            ("e", 1, 1, &b),
        ];

        let keyed: Vec<_> = inputs
            .par_iter()
            .map(|&(key, width, height, rgba)| (key, decoded_fingerprint(width, height, rgba)))
            .collect();
        let (canon, alias) = build_alias_map(keyed);

        assert_eq!(canon, vec!["a", "b", "d"]);
        assert_eq!(alias[&"a"], "a");
        assert_eq!(alias[&"c"], "a"); // decoded-identical to "a"; first-seen wins
        assert_eq!(alias[&"b"], "b");
        assert_eq!(alias[&"e"], "b"); // decoded-identical to "b"; first-seen wins
        assert_eq!(alias[&"d"], "d");
    }
}
