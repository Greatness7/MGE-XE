use super::*;

/// Hashes normalized text independent of segmentation.
pub(crate) fn hash_normalized_bytes<H: Hasher>(state: &mut H, text: &str) {
    crate::normalize::hash_normalized_text(state, text);
}

/// Appends the key terminator used by full and segmented hashes.
pub(crate) fn finish_asset_key_hash<H: Hasher>(state: &mut H) {
    state.write_u8(0xff);
}

pub(crate) fn key_equals_parts_from_slice(stored: &str, parts: &[&str]) -> bool {
    debug_assert!(is_normalized(stored));
    debug_assert!(parts.iter().all(|part| is_normalized(part)));
    let mut stored = stored.as_bytes();

    for part in parts {
        for byte in part.bytes() {
            let Some((&actual, rest)) = stored.split_first() else {
                return false;
            };
            if actual != byte {
                return false;
            }
            stored = rest;
        }
    }

    stored.is_empty()
}
