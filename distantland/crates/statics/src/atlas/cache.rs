use super::*;

/// Computes BLAKE3 content fingerprints for each texture in `textures`.
///
/// Loose files are fingerprinted via memory-mapped I/O; BSA assets are read into memory first.
/// The returned `Vec` is parallel to `textures` (same length and order).
pub(crate) fn collect_fingerprints(textures: &IndexSet<String>, vfs: &Vfs) -> Vec<(u64, Hash)> {
    let fingerprints: Vec<_> = textures
        .par_iter()
        .filter_map(|key| {
            let source = vfs.maps.textures.get(crate::vfs::NormalizedStr::from_normalized(key))?;
            match source {
                AssetSource::Loose { path } => {
                    let size = fs::metadata(path).ok()?.len();
                    let hash = Hasher::new().update_mmap(path).ok()?.finalize();
                    Some((size, hash))
                }
                AssetSource::Bsa { .. } => {
                    let asset = vfs.resolve_texture(key)?;
                    let bytes = vfs.read_asset_bytes(&asset).ok()?;
                    let hash = Hasher::new().update(bytes.as_ref()).finalize();
                    Some((bytes.len() as u64, hash))
                }
                AssetSource::Embedded { bytes } => {
                    let hash = Hasher::new().update(bytes).finalize();
                    Some((bytes.len() as u64, hash))
                }
            }
        })
        .collect();

    assert!(textures.len() == fingerprints.len());
    fingerprints
}

pub(crate) fn build_fingerprint_entries(
    textures: &IndexSet<String>,
    fingerprints: &[(u64, Hash)],
) -> Vec<CachedTextureFingerprint> {
    textures
        .iter()
        .zip(fingerprints)
        .map(|(path, &(size, hash))| CachedTextureFingerprint {
            path: path.clone(),
            fingerprint: TextureFingerprint {
                size,
                hash: *hash.as_bytes(),
            },
        })
        .collect()
}

pub(crate) fn fingerprints_match(
    textures: &IndexSet<String>,
    fingerprints: &[(u64, Hash)],
    cached: &[CachedTextureFingerprint],
) -> bool {
    textures.len() == cached.len()
        && textures
            .iter()
            .zip(fingerprints)
            .zip(cached)
            .all(|((path, &(size, hash)), entry)| {
                entry.path == *path && entry.fingerprint.size == size && entry.fingerprint.hash == *hash.as_bytes()
            })
}

/// Prior atlas-family evidence after independent validation.
///
/// Each entry is `Some` when that family's internal structure, bindings, and committed inventory
/// remain valid. Global decode/version/shared-config failures
/// (and unclassifiable inventory) yield both entries as `None`.
pub(crate) type ValidatedAtlasPrior = AtlasTextureSet<Option<CachedAtlasFamily>>;

/// Validates atlas evidence loaded from generator state.
///
/// Global version and shared-configuration mismatches reject both families. After those checks
/// pass, structural, binding, and inventory inconsistencies reject only the affected family
/// so a valid sibling can still feed the family-local planner.
///
/// Source hashes may differ: they select page dirt after structural validation.
pub(crate) fn validate_cache(
    _textures: &AtlasTextureSet<IndexSet<String>>,
    plan: &SizingPlan,
    dedupe_mode: TextureDedupeMode,
    gpu_max: u32,
    committed_atlas_paths: &IndexSet<String>,
    cache: AtlasCache,
) -> ValidatedAtlasPrior {
    let reject_both = || AtlasTextureSet::new(None, None);

    if cache.version != ATLAS_CACHE_VERSION {
        trace!("Atlas cache version mismatch, invalidating both families");
        return reject_both();
    }
    let shared_config = AtlasSharedConfig::current(plan, dedupe_mode, gpu_max);
    if cache.shared_config != shared_config {
        trace!("Atlas cache shared config mismatch, invalidating both families");
        return reject_both();
    }

    // Committed `ArtifactKind::AtlasDds` paths are already restricted to the atlas
    // grammar by state validation. Partition still fail-closes on any unclassifiable path so an
    // unknown name cannot be silently assigned to the opaque family (opaque is a prefix of alpha).
    let Some(committed) = partition_committed_atlas_paths(committed_atlas_paths) else {
        trace!("Atlas cache has unclassifiable committed path, invalidating both families");
        return reject_both();
    };

    let AtlasCache { opaque, alpha, .. } = cache;

    AtlasTextureSet::new(
        accept_family("opaque", opaque, &shared_config, OPAQUE_ATLAS_PREFIX, &committed.opaque),
        accept_family("alpha", alpha, &shared_config, ALPHA_ATLAS_PREFIX, &committed.alpha),
    )
}

/// Builds the exact relative paths expected for one family's committed page inventory.
fn expected_family_paths(prefix: &str, page_count: usize) -> IndexSet<String> {
    (0..page_count)
        .map(|page| format!(r"statics\textures\{}", atlas_page_string(prefix, page)))
        .collect()
}

/// Partitions committed atlas DDS paths into opaque and alpha sets using the page grammar.
///
/// The longer alpha stem is checked before the opaque stem because `_mge_xe_atlas` is a prefix of
/// `_mge_xe_atlas_alpha`. Returns `None` when any path cannot be classified (fail closed).
pub(super) fn partition_committed_atlas_paths(
    committed_atlas_paths: &IndexSet<String>,
) -> Option<AtlasTextureSet<IndexSet<String>>> {
    let mut opaque = IndexSet::default();
    let mut alpha = IndexSet::default();
    for path in committed_atlas_paths {
        match classify_atlas_page_path(path) {
            Some(AtlasDomain::Opaque) => {
                opaque.insert(path.clone());
            }
            Some(AtlasDomain::Alpha) => {
                alpha.insert(path.clone());
            }
            None => return None,
        }
    }
    Some(AtlasTextureSet::new(opaque, alpha))
}

/// Classifies one committed relative path as an opaque or alpha atlas page.
///
/// Accepts only the names produced by [`atlas_page_string`]: bare `prefix.dds` or
/// numbered `prefix_N.dds`. Alpha is matched before opaque because of the shared stem prefix.
pub(super) fn classify_atlas_page_path(path: &str) -> Option<AtlasDomain> {
    let name = path.strip_prefix(r"statics\textures\")?;
    if atlas_page_name_matches(name, ALPHA_ATLAS_PREFIX) {
        return Some(AtlasDomain::Alpha);
    }
    if atlas_page_name_matches(name, OPAQUE_ATLAS_PREFIX) {
        return Some(AtlasDomain::Opaque);
    }
    None
}

/// Matches bare `prefix.dds` or numbered `prefix_N.dds` with the same rules as
/// the output-storage atlas-page parser: no leading zeros, page > 0, and a
/// successful `u32` parse. Other digit strings remain unclassifiable.
fn atlas_page_name_matches(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    if rest == ".dds" {
        return true;
    }
    let Some(digits) = rest.strip_prefix('_').and_then(|rest| rest.strip_suffix(".dds")) else {
        return false;
    };
    // Mirror `parse_numbered_atlas_page`: reject empty, leading-zero, non-digit, zero, and overflow.
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    digits.parse::<u32>().is_ok_and(|page| page > 0)
}

fn accept_family(
    domain: &str,
    family: CachedAtlasFamily,
    shared: &AtlasSharedConfig,
    prefix: &str,
    committed: &IndexSet<String>,
) -> Option<CachedAtlasFamily> {
    validate_family(domain, &family, shared)?;
    let expected = expected_family_paths(prefix, family.pages.len());
    if expected != *committed {
        trace!(domain, "Atlas cache page inventory mismatch, invalidating family");
        return None;
    }
    Some(family)
}

fn validate_family(domain: &str, family: &CachedAtlasFamily, shared: &AtlasSharedConfig) -> Option<()> {
    if !family
        .family_config
        .sizing_overrides
        .windows(2)
        .all(|pair| pair[0].0 < pair[1].0)
        || family
            .family_config
            .sizing_overrides
            .iter()
            .any(|(_, dimension)| *dimension == 0 || *dimension > shared.max_texture_long_dim)
    {
        trace!(domain, "Atlas cache family config is malformed, invalidating");
        return None;
    }
    let fingerprint_paths = sorted_unique_paths(family.texture_fingerprints.iter().map(|entry| entry.path.as_str()))?;
    let key_slot_paths = sorted_unique_paths(family.key_slots.iter().map(|entry| entry.path.as_str()))?;
    let binding_paths = sorted_unique_paths(family.bindings.iter().map(|entry| entry.path.as_str()))?;
    if fingerprint_paths != key_slot_paths || fingerprint_paths != binding_paths {
        trace!(domain, "Atlas cache family logical relation coverage mismatch, invalidating");
        return None;
    }

    if family.slots.is_empty() && (!family.key_slots.is_empty() || !family.bindings.is_empty()) {
        trace!(
            domain,
            "Atlas cache family has logical keys without active slots, invalidating"
        );
        return None;
    }
    if family.slots.is_empty() && !family.pages.is_empty() {
        trace!(domain, "Atlas cache family has only trailing empty pages, invalidating");
        return None;
    }
    if !family.pages.is_empty()
        && !family
            .slots
            .iter()
            .any(|slot| slot.page_id as usize == family.pages.len() - 1)
    {
        trace!(domain, "Atlas cache family has an empty trailing page, invalidating");
        return None;
    }
    for page in &family.pages {
        if page.width == 0
            || page.height == 0
            || page.width > shared.gpu_max
            || page.height > shared.gpu_max
            || !page.width.is_power_of_two()
            || !page.height.is_power_of_two()
            || page
                .width
                .checked_sub(shared.border_padding.checked_mul(2)?)
                .is_none_or(|value| value == 0)
            || page
                .height
                .checked_sub(shared.border_padding.checked_mul(2)?)
                .is_none_or(|value| value == 0)
        {
            trace!(domain, "Atlas cache page dimensions are invalid, invalidating");
            return None;
        }
    }

    let mut previous_slot_id = None;
    for slot in &family.slots {
        if previous_slot_id.is_some_and(|previous| previous >= slot.slot_id) {
            trace!(domain, "Atlas cache slots are not sorted and unique, invalidating");
            return None;
        }
        previous_slot_id = Some(slot.slot_id);
        let page = family.pages.get(slot.page_id as usize)?;
        let usable = CachedAtlasRect {
            x: shared.border_padding,
            y: shared.border_padding,
            width: page.width - shared.border_padding * 2,
            height: page.height - shared.border_padding * 2,
        };
        let offset = shared.texture_extrusion.checked_add(shared.texture_padding / 2)?;
        let twice_extrusion = shared.texture_extrusion.checked_mul(2)?;
        let required_width = slot
            .destination
            .width
            .checked_add(shared.texture_padding)?
            .checked_add(twice_extrusion)?;
        let required_height = slot
            .destination
            .height
            .checked_add(shared.texture_padding)?
            .checked_add(twice_extrusion)?;
        if slot.rotated
            || !rect_within_rect(&slot.reserved_rect, &usable)
            || !rect_within_rect(&slot.destination, &slot.reserved_rect)
            || !rect_within(&slot.source, slot.source_size[0], slot.source_size[1])
            || slot.destination.width != slot.source.width
            || slot.destination.height != slot.source.height
            || slot.destination.x != slot.reserved_rect.x.checked_add(offset)?
            || slot.destination.y != slot.reserved_rect.y.checked_add(offset)?
            || required_width > slot.reserved_rect.width
            || required_height > slot.reserved_rect.height
        {
            trace!(domain, "Atlas cache slot geometry is invalid, invalidating");
            return None;
        }
    }
    if family.slots.last().is_some_and(|slot| family.next_slot_id <= slot.slot_id) {
        trace!(
            domain,
            "Atlas cache next slot id is not greater than every active id, invalidating"
        );
        return None;
    }

    let slot_by_id: HashMap<u64, &CachedAtlasSlot> = family.slots.iter().map(|slot| (slot.slot_id, slot)).collect();
    let mut key_count_by_slot = HashMap::<u64, usize>::new();
    for relation in &family.key_slots {
        slot_by_id.get(&relation.slot_id)?;
        *key_count_by_slot.entry(relation.slot_id).or_default() += 1;
    }
    for slot in &family.slots {
        if key_count_by_slot.get(&slot.slot_id).copied().unwrap_or(0) == 0
            || family
                .key_slots
                .binary_search_by(|relation| relation.path.as_str().cmp(slot.provider_key.as_str()))
                .ok()
                .is_none_or(|index| family.key_slots[index].slot_id != slot.slot_id)
        {
            trace!(domain, "Atlas cache slot provider/group relation is invalid, invalidating");
            return None;
        }
    }

    for (index, left) in family.slots.iter().enumerate() {
        for right in &family.slots[index + 1..] {
            if left.page_id == right.page_id && rects_overlap(&left.reserved_rect, &right.reserved_rect) {
                trace!(domain, "Atlas cache reservations overlap, invalidating");
                return None;
            }
        }
    }

    let expected_bindings =
        super::reconcile::bindings_from_relations(&family.pages, &family.slots, &family.key_slots).ok()?;
    for binding in &family.bindings {
        let &(page, bound) = expected_bindings.get(&binding.path)?;
        if binding.page as usize != page
            || binding.min_x.to_bits() != bound.min_x.to_bits()
            || binding.max_x.to_bits() != bound.max_x.to_bits()
            || binding.min_y.to_bits() != bound.min_y.to_bits()
            || binding.max_y.to_bits() != bound.max_y.to_bits()
        {
            trace!(domain, "Atlas cache binding does not match its slot geometry, invalidating");
            return None;
        }
    }
    Some(())
}

fn sorted_unique_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Option<Vec<&'a str>> {
    let paths: Vec<_> = paths.collect();
    paths.windows(2).all(|pair| pair[0] < pair[1]).then_some(paths)
}

fn rect_within(rect: &CachedAtlasRect, width: u32, height: u32) -> bool {
    rect.width > 0
        && rect.height > 0
        && rect.x.checked_add(rect.width).is_some_and(|right| right <= width)
        && rect.y.checked_add(rect.height).is_some_and(|bottom| bottom <= height)
}

fn rect_within_rect(inner: &CachedAtlasRect, outer: &CachedAtlasRect) -> bool {
    inner.width > 0
        && inner.height > 0
        && inner.x >= outer.x
        && inner.y >= outer.y
        && inner
            .x
            .checked_add(inner.width)
            .zip(outer.x.checked_add(outer.width))
            .is_some_and(|(inner_right, outer_right)| inner_right <= outer_right)
        && inner
            .y
            .checked_add(inner.height)
            .zip(outer.y.checked_add(outer.height))
            .is_some_and(|(inner_bottom, outer_bottom)| inner_bottom <= outer_bottom)
}

fn rects_overlap(left: &CachedAtlasRect, right: &CachedAtlasRect) -> bool {
    let Some(left_right) = left.x.checked_add(left.width) else {
        return true;
    };
    let Some(left_bottom) = left.y.checked_add(left.height) else {
        return true;
    };
    let Some(right_edge) = right.x.checked_add(right.width) else {
        return true;
    };
    let Some(right_bottom) = right.y.checked_add(right.height) else {
        return true;
    };
    !(left.x >= right_edge || right.x >= left_right || left.y >= right_bottom || right.y >= left_bottom)
}
