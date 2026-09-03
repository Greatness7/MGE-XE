use super::*;

use anyhow::anyhow;

/// Exhaustive sorted binding changes for one comparable atlas family.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BindingDelta {
    /// Logical keys added to the binding relation.
    pub added: Vec<String>,
    /// Logical keys removed from the binding relation.
    pub removed: Vec<String>,
    /// Logical keys whose page or UV bounds changed.
    pub changed: Vec<String>,
    /// Number of logical keys whose binding remained unchanged.
    pub unchanged: usize,
}

pub(crate) fn cached_uv_map(uv_bounds: &[CachedUvBound]) -> HashMap<String, (usize, UvBound)> {
    uv_bounds
        .iter()
        .map(|uv| {
            (
                uv.path.clone(),
                (
                    uv.page as usize,
                    UvBound {
                        min_x: uv.min_x,
                        max_x: uv.max_x,
                        min_y: uv.min_y,
                        max_y: uv.max_y,
                    },
                ),
            )
        })
        .collect()
}

/// Canonically fingerprints the source-texture-to-atlas bindings consumed by UV lowering.
pub(crate) fn atlas_binding_digest(
    opaque_map: &HashMap<String, (usize, UvBound)>,
    alpha_map: &HashMap<String, (usize, UvBound)>,
) -> [u8; 32] {
    let mut entries = Vec::with_capacity(opaque_map.len() + alpha_map.len());
    entries.extend(opaque_map.iter().map(|(key, value)| (0u8, key, value)));
    entries.extend(alpha_map.iter().map(|(key, value)| (1u8, key, value)));
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"static_atlas_bindings_v1\n");
    hasher.update(&(entries.len() as u64).to_le_bytes());
    for (family, key, (page, bound)) in entries {
        hasher.update(&[family]);
        hasher.update(&(key.len() as u64).to_le_bytes());
        hasher.update(key.as_bytes());
        hasher.update(&(*page as u64).to_le_bytes());
        hasher.update(&bound.min_x.to_bits().to_le_bytes());
        hasher.update(&bound.max_x.to_bits().to_le_bytes());
        hasher.update(&bound.min_y.to_bits().to_le_bytes());
        hasher.update(&bound.max_y.to_bits().to_le_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// Writes per-vertex UV bounds and updates each subset's `texture` to the atlas page id.
///
/// For every subset, the normalized texture key is looked up in `opaque_map` or `alpha_map`
/// depending on `has_alpha`. Every vertex in the subset receives the same [`UvBound`], which
/// the shader uses as a clamp region to avoid sampling the neighboring frame.
///
/// # Errors
///
/// Returns an error if a subset's texture key is not found in the appropriate map,
/// which indicates stale or malformed atlas cache data.
pub(crate) fn update_uv_bounds_from_maps(
    distant_statics: &mut DistantStatics,
    opaque_map: &HashMap<String, (usize, UvBound)>,
    alpha_map: &HashMap<String, (usize, UvBound)>,
    vfs: &Vfs,
) -> anyhow::Result<()> {
    for ds in distant_statics.values_mut() {
        // Grass is not atlased (see collect_textures); leave its identity uv_bound and source
        // texture untouched so it keeps native textures and samples passthrough at runtime.
        if ds.static_type == StaticType::StaticGrass {
            continue;
        }
        for subset in ds.subsets.iter_mut() {
            if subset.has_uv_controller {
                continue;
            }
            let (atlas_kind, map) = if subset.has_alpha() {
                ("alpha", alpha_map)
            } else {
                ("opaque", opaque_map)
            };
            let texture_sym = subset
                .texture
                .source_sym()
                .ok_or_else(|| anyhow!("subset has no source texture before {atlas_kind} atlas UV update"))?;
            let texture_key = vfs.texture_key_for_sym(texture_sym).ok_or_else(|| {
                anyhow!("subset source texture symbol could not be resolved for {atlas_kind} atlas UV update")
            })?;
            let &(page_id, bound) = map
                .get(texture_key)
                .ok_or_else(|| anyhow!("missing {atlas_kind} atlas UV bounds for texture key '{texture_key}'"))?;
            for vertex in subset.vertices.iter_mut() {
                vertex.uv_bound = bound;
            }
            // The only point in the pipeline where a subset is known to hold exactly one bound.
            subset.uv_bounds = vec![bound];
            let page_id: u32 = page_id.try_into().map_err(|_| anyhow!("atlas page id exceeds u32"))?;
            subset.texture = crate::SubsetTexture::AtlasPage(page_id);
        }
    }
    Ok(())
}

pub(crate) fn map_to_cached_bounds(map: &HashMap<String, (usize, UvBound)>) -> Vec<CachedUvBound> {
    let mut entries: Vec<_> = map
        .iter()
        .map(|(key, &(page, bound))| CachedUvBound {
            path: key.clone(),
            page: page as u32,
            min_x: bound.min_x,
            max_x: bound.max_x,
            min_y: bound.min_y,
            max_y: bound.max_y,
        })
        .collect();
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    entries
}

/// Compares binding relations by page and raw `f32` bits.
pub(crate) fn binding_delta(previous: &[CachedUvBound], current: &[CachedUvBound]) -> BindingDelta {
    let mut result = BindingDelta::default();
    let mut previous_index = 0;
    let mut current_index = 0;
    while previous_index < previous.len() || current_index < current.len() {
        match (previous.get(previous_index), current.get(current_index)) {
            (Some(old), Some(new)) => match old.path.cmp(&new.path) {
                std::cmp::Ordering::Less => {
                    result.removed.push(old.path.clone());
                    previous_index += 1;
                }
                std::cmp::Ordering::Greater => {
                    result.added.push(new.path.clone());
                    current_index += 1;
                }
                std::cmp::Ordering::Equal => {
                    if binding_bits_equal(old, new) {
                        result.unchanged += 1;
                    } else {
                        result.changed.push(new.path.clone());
                    }
                    previous_index += 1;
                    current_index += 1;
                }
            },
            (Some(old), None) => {
                result.removed.push(old.path.clone());
                previous_index += 1;
            }
            (None, Some(new)) => {
                result.added.push(new.path.clone());
                current_index += 1;
            }
            (None, None) => break,
        }
    }
    result
}

fn binding_bits_equal(left: &CachedUvBound, right: &CachedUvBound) -> bool {
    left.page == right.page
        && left.min_x.to_bits() == right.min_x.to_bits()
        && left.max_x.to_bits() == right.max_x.to_bits()
        && left.min_y.to_bits() == right.min_y.to_bits()
        && left.max_y.to_bits() == right.max_y.to_bits()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_digest_is_order_independent_and_sensitive() {
        let bound = UvBound {
            min_x: 0.1,
            max_x: 0.9,
            min_y: 0.2,
            max_y: 0.8,
        };
        let first = HashMap::from([("b".to_owned(), (2, bound)), ("a".to_owned(), (1, bound))]);
        let second = HashMap::from([("a".to_owned(), (1, bound)), ("b".to_owned(), (2, bound))]);
        let empty = HashMap::new();
        let digest = atlas_binding_digest(&first, &empty);
        assert_eq!(digest, atlas_binding_digest(&second, &empty));
        let different_page = HashMap::from([("a".to_owned(), (9, bound)), ("b".to_owned(), (2, bound))]);
        assert_ne!(digest, atlas_binding_digest(&different_page, &empty));
        let different_bound = HashMap::from([
            ("a".to_owned(), (1, UvBound { min_x: 0.3, ..bound })),
            ("b".to_owned(), (2, bound)),
        ]);
        assert_ne!(digest, atlas_binding_digest(&different_bound, &empty));
        assert_ne!(digest, atlas_binding_digest(&empty, &first));
    }
}
