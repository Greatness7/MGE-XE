use super::*;

use std::collections::BTreeMap;

use anyhow::{Context, bail};
use itertools::Itertools;
use tex_packer_core::{Frame, Page};

use crate::atlas::pack::{AtlasAliasRelation, pack_layout_only};

mod freelist;
mod graph;

use freelist::SeededFreeList;
use graph::min_cost_positive_matching;

type FrameMap = HashMap<String, (usize, UvBound)>;

pub(crate) struct PreparedAtlasGroup {
    pub(crate) group_key: String,
    pub(crate) logical_keys: Vec<String>,
    pub(crate) item: LayoutItem,
    pub(crate) content_fingerprint: [u8; 32],
}

pub(crate) struct ReconciledPage {
    pub(crate) page: Page,
    pub(crate) dirty: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AtlasAllocationStats {
    pub(crate) retained_slots: usize,
    pub(crate) allocated_slots: usize,
    pub(crate) freed_slots: usize,
    pub(crate) relocated_slots: usize,
    pub(crate) provider_promoted_slots: usize,
    pub(crate) active_area: u64,
    pub(crate) reserved_area: u64,
    pub(crate) page_area: u64,
    pub(crate) usable_page_area: u64,
    pub(crate) fragmentation_ppm: u32,
    pub(crate) appended_pages: usize,
    pub(crate) retained_empty_pages: usize,
    pub(crate) truncated_pages: usize,
}

pub(crate) struct AllocatedAtlasFamily {
    pub(crate) cache: CachedAtlasFamily,
    pub(crate) bindings: FrameMap,
    pub(crate) pages: Vec<ReconciledPage>,
    /// Prepared-image keys to rename before composing pages, as `(prepared key, provider key)`.
    ///
    /// Frames are keyed by `slot.provider_key`, so a retained slot that keeps a provider other than
    /// its group's prepared key leaves the prepared image filed under the wrong name. Only
    /// reconciliation can produce these; a fresh pack always provides under the prepared key.
    pub(crate) provider_renames: Vec<(String, String)>,
    pub(crate) stats: AtlasAllocationStats,
}

/// Builds sorted groups by resolving each logical key through both alias tiers.
///
/// Alias maps are dense and fail closed on a missing entry.
pub(crate) fn build_prepared_groups(
    keys: &IndexSet<String>,
    tier0: &AtlasAliasRelation,
    tier1: &AtlasAliasRelation,
    items: &[LayoutItem],
    content_fingerprints: &HashMap<String, [u8; 32]>,
) -> anyhow::Result<Vec<PreparedAtlasGroup>> {
    let mut logical_by_provider = BTreeMap::<String, Vec<String>>::new();
    for key in keys {
        let tier0_provider = tier0
            .resolve(key)
            .with_context(|| format!("prepared atlas tier-0 alias missing for {key}"))?;
        let provider = tier1.resolve(tier0_provider).with_context(|| {
            format!("prepared atlas tier-1 alias missing for {key} (via tier-0 provider {tier0_provider})")
        })?;
        logical_by_provider.entry(provider.to_owned()).or_default().push(key.clone());
    }
    let item_by_key: HashMap<&str, &LayoutItem> = items.iter().map(|item| (item.key.as_str(), item)).collect();
    logical_by_provider
        .into_iter()
        .map(|(provider, logical_keys)| {
            let item = item_by_key
                .get(provider.as_str())
                .with_context(|| format!("prepared atlas layout item missing for {provider}"))?;
            let content_fingerprint = *content_fingerprints
                .get(&provider)
                .with_context(|| format!("prepared atlas content fingerprint missing for {provider}"))?;
            Ok(PreparedAtlasGroup {
                group_key: logical_keys[0].clone(),
                logical_keys,
                item: (*item).clone(),
                content_fingerprint,
            })
        })
        .collect()
}

/// Captures the packer reservation hidden behind a visible frame.
pub(crate) fn capture_reservation(
    frame: Rect,
    page_width: u32,
    page_height: u32,
    shared: &AtlasSharedConfig,
) -> anyhow::Result<CachedAtlasRect> {
    let offset = shared
        .texture_extrusion
        .checked_add(shared.texture_padding / 2)
        .context("atlas reservation origin overflow")?;
    let x = frame.x.checked_sub(offset).context("atlas reservation x underflow")?;
    let y = frame.y.checked_sub(offset).context("atlas reservation y underflow")?;
    let twice_extrusion = shared
        .texture_extrusion
        .checked_mul(2)
        .context("atlas reservation extrusion overflow")?;
    let width = frame
        .w
        .checked_add(shared.texture_padding)
        .and_then(|value| value.checked_add(twice_extrusion))
        .context("atlas reservation width overflow")?;
    let height = frame
        .h
        .checked_add(shared.texture_padding)
        .and_then(|value| value.checked_add(twice_extrusion))
        .context("atlas reservation height overflow")?;
    let reserved = CachedAtlasRect { x, y, width, height };
    let usable = usable_rect(page_width, page_height, shared.border_padding)?;
    if !contains(&usable, &reserved) {
        bail!("atlas reservation is outside the page usable interior");
    }
    Ok(reserved)
}

/// Fresh-packs a family with deterministic initial slot ids.
pub(crate) fn fresh_allocate_family(
    family_config: AtlasFamilyConfig,
    layout_input_digest: [u8; 32],
    groups: &[PreparedAtlasGroup],
    texture_fingerprints: Vec<CachedTextureFingerprint>,
    shared: &AtlasSharedConfig,
) -> anyhow::Result<AllocatedAtlasFamily> {
    if groups.is_empty() {
        let cache = CachedAtlasFamily {
            family_config,
            layout_input_digest,
            next_slot_id: 0,
            pages: Vec::new(),
            slots: Vec::new(),
            key_slots: Vec::new(),
            bindings: Vec::new(),
            texture_fingerprints,
        };
        return Ok(AllocatedAtlasFamily {
            cache,
            bindings: HashMap::new(),
            pages: Vec::new(),
            provider_renames: Vec::new(),
            stats: AtlasAllocationStats::default(),
        });
    }

    let atlas = pack_layout_only(groups.iter().map(|group| group.item.clone()).collect(), shared.gpu_max)
        .context("atlas texture packing failed")?;
    let group_by_provider: HashMap<&str, &PreparedAtlasGroup> =
        groups.iter().map(|group| (group.item.key.as_str(), group)).collect();
    let mut captured = Vec::with_capacity(groups.len());
    for page in &atlas.pages {
        for frame in &page.frames {
            let reserved = capture_reservation(frame.frame, page.width, page.height, shared)?;
            captured.push((page.id, reserved, frame));
        }
    }
    captured.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.y.cmp(&right.1.y))
            .then_with(|| left.1.x.cmp(&right.1.x))
            .then_with(|| left.2.key.cmp(&right.2.key))
    });

    let mut slots = Vec::with_capacity(captured.len());
    let mut provider_slots = HashMap::with_capacity(captured.len());
    for (slot_id, (page_id, reserved_rect, frame)) in captured.into_iter().enumerate() {
        let group = group_by_provider[frame.key.as_str()];
        let slot_id = slot_id as u64;
        provider_slots.insert(frame.key.clone(), slot_id);
        slots.push(slot_from_frame(
            slot_id,
            page_id,
            reserved_rect,
            frame,
            frame.key.clone(),
            group.content_fingerprint,
        )?);
    }
    slots.sort_unstable_by_key(|slot| slot.slot_id);
    let key_slots = key_slots_for_groups(groups, |group| provider_slots[&group.item.key]);
    let pages: Vec<CachedAtlasPage> = atlas
        .pages
        .iter()
        .map(|page| CachedAtlasPage {
            width: page.width,
            height: page.height,
        })
        .collect();
    let bindings = bindings_from_relations(&pages, &slots, &key_slots)?;
    let cache = CachedAtlasFamily {
        family_config,
        layout_input_digest,
        next_slot_id: slots.len() as u64,
        pages,
        slots,
        key_slots,
        bindings: map_to_cached_bounds(&bindings),
        texture_fingerprints,
    };
    let pages = pages_from_cache(&cache, &BTreeMap::from_iter((0..cache.pages.len()).map(|page| (page, true))))?;
    let mut stats = allocation_stats(&cache, shared)?;
    stats.allocated_slots = cache.slots.len();
    Ok(AllocatedAtlasFamily {
        cache,
        bindings,
        pages,
        provider_renames: Vec::new(),
        stats,
    })
}

/// Reconciles current groups against valid prior family evidence.
pub(crate) fn reconcile_family(
    prior: &CachedAtlasFamily,
    family_config: AtlasFamilyConfig,
    layout_input_digest: [u8; 32],
    groups: &[PreparedAtlasGroup],
    texture_fingerprints: Vec<CachedTextureFingerprint>,
    shared: &AtlasSharedConfig,
) -> anyhow::Result<AllocatedAtlasFamily> {
    let prior_groups = prior_groups(prior)?;
    let matching = maximum_weight_matching(groups, &prior_groups, shared)?;
    let mut group_slot = vec![None; groups.len()];
    for (group_index, slot_index) in matching {
        group_slot[group_index] = Some(slot_index);
    }

    let mut dirty_pages = BTreeMap::<usize, bool>::new();
    let mut slots = Vec::with_capacity(groups.len());
    let mut stats = AtlasAllocationStats::default();
    let mut next_slot_id = prior.next_slot_id;
    let mut final_provider_by_group = vec![String::new(); groups.len()];
    // Delay renames until allocation succeeds so a failed reconciliation leaves prepared images reusable.
    let mut provider_renames = Vec::new();
    for (group_index, group) in groups.iter().enumerate() {
        let Some(slot_index) = group_slot[group_index] else {
            continue;
        };
        let old = prior_groups[slot_index].slot;
        let provider_key = if group.logical_keys.binary_search(&old.provider_key).is_ok() {
            old.provider_key.clone()
        } else {
            stats.provider_promoted_slots += 1;
            group.logical_keys[0].clone()
        };
        if provider_key != group.item.key {
            provider_renames.push((group.item.key.clone(), provider_key.clone()));
        }
        final_provider_by_group[group_index] = provider_key.clone();
        let destination = destination_in_reservation(old.reserved_rect, group.item.w, group.item.h, shared)?;
        let slot = CachedAtlasSlot {
            slot_id: old.slot_id,
            page_id: old.page_id,
            reserved_rect: old.reserved_rect,
            destination,
            source: cached_source(&group.item)?,
            source_size: cached_source_size(&group.item)?,
            rotated: false,
            trimmed: group.item.trimmed,
            provider_key,
            content_fingerprint: group.content_fingerprint,
        };
        if old.destination != slot.destination || old.content_fingerprint != slot.content_fingerprint {
            dirty_pages.insert(slot.page_id as usize, true);
        }
        slots.push(slot);
        stats.retained_slots += 1;
    }
    stats.freed_slots = prior_groups.len() - stats.retained_slots;

    let mut free_pages = Vec::with_capacity(prior.pages.len());
    for (page_id, page) in prior.pages.iter().enumerate() {
        let occupied = slots
            .iter()
            .filter(|slot| slot.page_id as usize == page_id)
            .map(|slot| (slot.slot_id, slot.reserved_rect));
        free_pages.push(SeededFreeList::new(page.width, page.height, shared.border_padding, occupied)?);
    }

    let mut unmatched: Vec<usize> = group_slot
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| slot.is_none().then_some(index))
        .collect();
    unmatched.sort_unstable_by(|&left, &right| {
        reservation_area(&groups[right].item, shared)
            .cmp(&reservation_area(&groups[left].item, shared))
            .then_with(|| groups[left].group_key.cmp(&groups[right].group_key))
    });
    let mut append = Vec::new();
    for group_index in unmatched {
        let group = &groups[group_index];
        let (width, height) = reservation_dimensions(group.item.w, group.item.h, shared)?;
        let best = free_pages
            .iter()
            .enumerate()
            .filter_map(|(page_id, free)| free.best_fit(width, height).map(|score| (score.with_page(page_id), page_id)))
            .min_by_key(|(score, _)| *score);
        let Some((_, page_id)) = best else {
            append.push(group_index);
            continue;
        };
        let reserved_rect = free_pages[page_id]
            .insert(width, height)
            .context("selected atlas free-list placement disappeared")?;
        let provider_key = group.item.key.clone();
        final_provider_by_group[group_index] = provider_key.clone();
        slots.push(slot_from_group(
            next_slot_id,
            page_id,
            reserved_rect,
            group,
            provider_key,
            shared,
        )?);
        next_slot_id = next_slot_id.checked_add(1).context("atlas slot id overflow")?;
        dirty_pages.insert(page_id, true);
        stats.allocated_slots += 1;
        if prior_groups
            .iter()
            .any(|prior_group| overlap_count(&group.logical_keys, &prior_group.logical_keys) > 0)
        {
            stats.relocated_slots += 1;
        }
    }

    let mut pages = prior.pages.clone();
    if !append.is_empty() {
        let atlas = pack_layout_only(
            append.iter().map(|&index| groups[index].item.clone()).collect(),
            shared.gpu_max,
        )
        .context("atlas texture packing failed while appending reconciled pages")?;
        let page_offset = pages.len();
        let group_index_by_provider: HashMap<&str, usize> =
            append.iter().map(|&index| (groups[index].item.key.as_str(), index)).collect();
        let mut captured = Vec::with_capacity(append.len());
        for page in &atlas.pages {
            pages.push(CachedAtlasPage {
                width: page.width,
                height: page.height,
            });
            for frame in &page.frames {
                let reserved = capture_reservation(frame.frame, page.width, page.height, shared)?;
                captured.push((page_offset + page.id, reserved, frame));
            }
        }
        captured.sort_unstable_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.y.cmp(&right.1.y))
                .then_with(|| left.1.x.cmp(&right.1.x))
                .then_with(|| left.2.key.cmp(&right.2.key))
        });
        for (page_id, reserved_rect, frame) in captured {
            let group_index = group_index_by_provider[frame.key.as_str()];
            let group = &groups[group_index];
            final_provider_by_group[group_index] = frame.key.clone();
            slots.push(slot_from_frame(
                next_slot_id,
                page_id,
                reserved_rect,
                frame,
                frame.key.clone(),
                group.content_fingerprint,
            )?);
            next_slot_id = next_slot_id.checked_add(1).context("atlas slot id overflow")?;
            dirty_pages.insert(page_id, true);
            stats.allocated_slots += 1;
            if prior_groups
                .iter()
                .any(|prior_group| overlap_count(&group.logical_keys, &prior_group.logical_keys) > 0)
            {
                stats.relocated_slots += 1;
            }
        }
        stats.appended_pages = atlas.pages.len();
    }

    slots.sort_unstable_by_key(|slot| slot.slot_id);
    let mut slot_by_provider = HashMap::with_capacity(slots.len());
    for slot in &slots {
        slot_by_provider.insert(slot.provider_key.as_str(), slot.slot_id);
    }
    let group_index_by_key: HashMap<&str, usize> = groups
        .iter()
        .enumerate()
        .map(|(index, group)| (group.group_key.as_str(), index))
        .collect();
    let key_slots = key_slots_for_groups(groups, |group| {
        let group_index = group_index_by_key[group.group_key.as_str()];
        slot_by_provider[final_provider_by_group[group_index].as_str()]
    });

    let original_page_count = pages.len();
    while pages.last().is_some_and(|_| {
        let last = pages.len() - 1;
        !slots.iter().any(|slot| slot.page_id as usize == last)
    }) {
        pages.pop();
    }
    stats.truncated_pages = original_page_count - pages.len();
    dirty_pages.retain(|page_id, _| *page_id < pages.len());

    let bindings = bindings_from_relations(&pages, &slots, &key_slots)?;
    let cache = CachedAtlasFamily {
        family_config,
        layout_input_digest,
        next_slot_id,
        pages,
        slots,
        key_slots,
        bindings: map_to_cached_bounds(&bindings),
        texture_fingerprints,
    };
    let result_pages = pages_from_cache(&cache, &dirty_pages)?;
    let derived = allocation_stats(&cache, shared)?;
    stats.active_area = derived.active_area;
    stats.reserved_area = derived.reserved_area;
    stats.page_area = derived.page_area;
    stats.usable_page_area = derived.usable_page_area;
    stats.fragmentation_ppm = derived.fragmentation_ppm;
    stats.retained_empty_pages = cache
        .pages
        .iter()
        .enumerate()
        .filter(|(page_id, _)| !cache.slots.iter().any(|slot| slot.page_id as usize == *page_id))
        .count();
    Ok(AllocatedAtlasFamily {
        cache,
        bindings,
        pages: result_pages,
        provider_renames,
        stats,
    })
}

struct PriorGroup<'a> {
    slot: &'a CachedAtlasSlot,
    logical_keys: Vec<String>,
}

fn prior_groups(prior: &CachedAtlasFamily) -> anyhow::Result<Vec<PriorGroup<'_>>> {
    let mut keys = prior
        .key_slots
        .iter()
        .map(|relation| (relation.slot_id, relation.path.clone()))
        .into_group_map();
    prior
        .slots
        .iter()
        .map(|slot| {
            Ok(PriorGroup {
                slot,
                logical_keys: keys
                    .remove(&slot.slot_id)
                    .with_context(|| format!("atlas slot {} has no logical keys", slot.slot_id))?,
            })
        })
        .collect()
}

fn maximum_weight_matching(
    groups: &[PreparedAtlasGroup],
    prior: &[PriorGroup<'_>],
    shared: &AtlasSharedConfig,
) -> anyhow::Result<Vec<(usize, usize)>> {
    let mut edges = Vec::<(usize, usize, usize)>::new();
    for (group_index, group) in groups.iter().enumerate() {
        let (required_width, required_height) = reservation_dimensions(group.item.w, group.item.h, shared)?;
        for (slot_index, old) in prior.iter().enumerate() {
            let weight = overlap_count(&group.logical_keys, &old.logical_keys);
            if weight > 0
                && required_width <= old.slot.reserved_rect.width
                && required_height <= old.slot.reserved_rect.height
            {
                edges.push((group_index, slot_index, weight));
            }
        }
    }
    edges.sort_unstable_by(|left, right| {
        prior[left.1]
            .slot
            .slot_id
            .cmp(&prior[right.1].slot.slot_id)
            .then_with(|| groups[left.0].group_key.cmp(&groups[right.0].group_key))
    });
    min_cost_positive_matching(groups.len(), prior.len(), &edges)
}

/// Grows a content size to the page area it reserves, adding padding and extrusion on both edges.
fn reservation_dimensions(width: u32, height: u32, shared: &AtlasSharedConfig) -> anyhow::Result<(u32, u32)> {
    let twice_extrusion = shared
        .texture_extrusion
        .checked_mul(2)
        .context("atlas reservation extrusion overflow")?;
    let reserved_width = width
        .checked_add(shared.texture_padding)
        .and_then(|value| value.checked_add(twice_extrusion))
        .context("atlas reservation width overflow")?;
    let reserved_height = height
        .checked_add(shared.texture_padding)
        .and_then(|value| value.checked_add(twice_extrusion))
        .context("atlas reservation height overflow")?;
    Ok((reserved_width, reserved_height))
}

fn reservation_area(item: &LayoutItem, shared: &AtlasSharedConfig) -> u64 {
    reservation_dimensions(item.w, item.h, shared)
        .map(|(width, height)| u64::from(width) * u64::from(height))
        .unwrap_or(u64::MAX)
}

fn destination_in_reservation(
    reserved: CachedAtlasRect,
    width: u32,
    height: u32,
    shared: &AtlasSharedConfig,
) -> anyhow::Result<CachedAtlasRect> {
    let (required_width, required_height) = reservation_dimensions(width, height, shared)?;
    if required_width > reserved.width || required_height > reserved.height {
        bail!("current atlas content does not fit its prior reservation");
    }
    let offset = shared.texture_extrusion + shared.texture_padding / 2;
    Ok(CachedAtlasRect {
        x: reserved.x.checked_add(offset).context("atlas destination x overflow")?,
        y: reserved.y.checked_add(offset).context("atlas destination y overflow")?,
        width,
        height,
    })
}

fn slot_from_group(
    slot_id: u64,
    page_id: usize,
    reserved_rect: CachedAtlasRect,
    group: &PreparedAtlasGroup,
    provider_key: String,
    shared: &AtlasSharedConfig,
) -> anyhow::Result<CachedAtlasSlot> {
    Ok(CachedAtlasSlot {
        slot_id,
        page_id: page_id.try_into().context("atlas page id exceeds u32")?,
        reserved_rect,
        destination: destination_in_reservation(reserved_rect, group.item.w, group.item.h, shared)?,
        source: cached_source(&group.item)?,
        source_size: cached_source_size(&group.item)?,
        rotated: false,
        trimmed: group.item.trimmed,
        provider_key,
        content_fingerprint: group.content_fingerprint,
    })
}

fn slot_from_frame(
    slot_id: u64,
    page_id: usize,
    reserved_rect: CachedAtlasRect,
    frame: &Frame,
    provider_key: String,
    content_fingerprint: [u8; 32],
) -> anyhow::Result<CachedAtlasSlot> {
    Ok(CachedAtlasSlot {
        slot_id,
        page_id: page_id.try_into().context("atlas page id exceeds u32")?,
        reserved_rect,
        destination: cached_rect(frame.frame),
        source: cached_rect(frame.source),
        source_size: [frame.source_size.0, frame.source_size.1],
        rotated: frame.rotated,
        trimmed: frame.trimmed,
        provider_key,
        content_fingerprint,
    })
}

fn cached_rect(rect: Rect) -> CachedAtlasRect {
    CachedAtlasRect {
        x: rect.x,
        y: rect.y,
        width: rect.w,
        height: rect.h,
    }
}

fn cached_source(item: &LayoutItem) -> anyhow::Result<CachedAtlasRect> {
    item.source
        .map(cached_rect)
        .context("prepared atlas item is missing its source rectangle")
}

fn cached_source_size(item: &LayoutItem) -> anyhow::Result<[u32; 2]> {
    item.source_size
        .map(|(width, height)| [width, height])
        .context("prepared atlas item is missing its source size")
}

fn key_slots_for_groups(
    groups: &[PreparedAtlasGroup],
    slot_for: impl Fn(&PreparedAtlasGroup) -> u64,
) -> Vec<CachedAtlasKeySlot> {
    groups
        .iter()
        .flat_map(|group| {
            let slot_id = slot_for(group);
            group.logical_keys.iter().map(move |path| CachedAtlasKeySlot {
                path: path.clone(),
                slot_id,
            })
        })
        .sorted_unstable_by(|left, right| left.path.cmp(&right.path))
        .collect_vec()
}

pub(crate) fn bindings_from_relations(
    pages: &[CachedAtlasPage],
    slots: &[CachedAtlasSlot],
    key_slots: &[CachedAtlasKeySlot],
) -> anyhow::Result<FrameMap> {
    let slot_by_id: HashMap<u64, &CachedAtlasSlot> = slots.iter().map(|slot| (slot.slot_id, slot)).collect();
    key_slots
        .iter()
        .map(|relation| {
            let slot = slot_by_id
                .get(&relation.slot_id)
                .with_context(|| format!("atlas key {} references missing slot {}", relation.path, relation.slot_id))?;
            let page = pages
                .get(slot.page_id as usize)
                .with_context(|| format!("atlas slot {} references missing page {}", slot.slot_id, slot.page_id))?;
            let width = page.width as f32;
            let height = page.height as f32;
            Ok((
                relation.path.clone(),
                (
                    slot.page_id as usize,
                    UvBound {
                        min_x: slot.destination.x as f32 / width,
                        max_x: (slot.destination.x + slot.destination.width) as f32 / width,
                        min_y: slot.destination.y as f32 / height,
                        max_y: (slot.destination.y + slot.destination.height) as f32 / height,
                    },
                ),
            ))
        })
        .collect()
}

fn pages_from_cache(cache: &CachedAtlasFamily, dirty_pages: &BTreeMap<usize, bool>) -> anyhow::Result<Vec<ReconciledPage>> {
    cache
        .pages
        .iter()
        .enumerate()
        .map(|(page_id, page)| {
            let frames = cache
                .slots
                .iter()
                .filter(|slot| slot.page_id as usize == page_id)
                .map(|slot| Frame {
                    key: slot.provider_key.clone(),
                    frame: restored_rect(slot.destination),
                    rotated: slot.rotated,
                    trimmed: slot.trimmed,
                    source: restored_rect(slot.source),
                    source_size: (slot.source_size[0], slot.source_size[1]),
                })
                .collect();
            Ok(ReconciledPage {
                page: Page {
                    id: page_id,
                    width: page.width,
                    height: page.height,
                    frames,
                },
                dirty: dirty_pages.contains_key(&page_id),
            })
        })
        .collect()
}

fn restored_rect(rect: CachedAtlasRect) -> Rect {
    Rect::new(rect.x, rect.y, rect.width, rect.height)
}

/// Refiles prepared images under the provider keys chosen by allocation.
///
/// Images are lifted before reinsertion so rename chains are order-independent.
///
/// # Errors
///
/// Fails closed when a prepared image is missing or two groups claim the same provider key. Both
/// are internal inconsistencies, since groups partition the logical keys.
pub(crate) fn apply_provider_renames(
    images: &mut HashMap<String, image::RgbaImage>,
    renames: &[(String, String)],
) -> anyhow::Result<()> {
    let mut lifted = Vec::with_capacity(renames.len());
    for (prepared_key, provider_key) in renames {
        let image = images
            .remove(prepared_key)
            .with_context(|| format!("prepared atlas image missing for {prepared_key}"))?;
        lifted.push((provider_key, image));
    }
    for (provider_key, image) in lifted {
        if images.insert(provider_key.clone(), image).is_some() {
            bail!("atlas provider key {provider_key} is claimed by more than one exact group");
        }
    }
    Ok(())
}

fn overlap_count(left: &[String], right: &[String]) -> usize {
    let mut left_index = 0;
    let mut right_index = 0;
    let mut count = 0;
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                count += 1;
                left_index += 1;
                right_index += 1;
            }
        }
    }
    count
}

pub(crate) fn allocation_stats(
    family: &CachedAtlasFamily,
    shared: &AtlasSharedConfig,
) -> anyhow::Result<AtlasAllocationStats> {
    let active_area = family.slots.iter().try_fold(0u64, |sum, slot| {
        sum.checked_add(area(slot.destination)).context("atlas active area overflow")
    })?;
    let reserved_area = family.slots.iter().try_fold(0u64, |sum, slot| {
        sum.checked_add(area(slot.reserved_rect))
            .context("atlas reserved area overflow")
    })?;
    let page_area = family.pages.iter().try_fold(0u64, |sum, page| {
        sum.checked_add(u64::from(page.width) * u64::from(page.height))
            .context("atlas page area overflow")
    })?;
    let usable_page_area = family.pages.iter().try_fold(0u64, |sum, page| {
        let usable = usable_rect(page.width, page.height, shared.border_padding)?;
        sum.checked_add(area(usable)).context("atlas usable page area overflow")
    })?;
    if reserved_area > usable_page_area {
        bail!("atlas reserved area exceeds usable page area");
    }
    let fragmentation_ppm: u32 = (usable_page_area - reserved_area)
        .checked_mul(1_000_000)
        .context("atlas fragmentation numerator overflow")?
        .checked_div(usable_page_area)
        .unwrap_or(0)
        .try_into()
        .context("atlas fragmentation exceeds u32")?;
    Ok(AtlasAllocationStats {
        active_area,
        reserved_area,
        page_area,
        usable_page_area,
        fragmentation_ppm,
        ..AtlasAllocationStats::default()
    })
}

fn usable_rect(width: u32, height: u32, border: u32) -> anyhow::Result<CachedAtlasRect> {
    let twice = border.checked_mul(2).context("atlas border overflow")?;
    let usable_width = width.checked_sub(twice).context("atlas page is narrower than its borders")?;
    let usable_height = height.checked_sub(twice).context("atlas page is shorter than its borders")?;
    if usable_width == 0 || usable_height == 0 {
        bail!("atlas page has no usable interior");
    }
    Ok(CachedAtlasRect {
        x: border,
        y: border,
        width: usable_width,
        height: usable_height,
    })
}

fn area(rect: CachedAtlasRect) -> u64 {
    u64::from(rect.width) * u64::from(rect.height)
}

fn right(rect: CachedAtlasRect) -> anyhow::Result<u32> {
    rect.x.checked_add(rect.width).context("atlas rectangle right edge overflow")
}

fn bottom(rect: CachedAtlasRect) -> anyhow::Result<u32> {
    rect.y
        .checked_add(rect.height)
        .context("atlas rectangle bottom edge overflow")
}

fn contains(outer: &CachedAtlasRect, inner: &CachedAtlasRect) -> bool {
    let Some(outer_right) = outer.x.checked_add(outer.width) else {
        return false;
    };
    let Some(outer_bottom) = outer.y.checked_add(outer.height) else {
        return false;
    };
    let Some(inner_right) = inner.x.checked_add(inner.width) else {
        return false;
    };
    let Some(inner_bottom) = inner.y.checked_add(inner.height) else {
        return false;
    };
    inner.x >= outer.x && inner.y >= outer.y && inner_right <= outer_right && inner_bottom <= outer_bottom
}

fn intersects(left: &CachedAtlasRect, right_rect: &CachedAtlasRect) -> bool {
    let Some(left_right) = left.x.checked_add(left.width) else {
        return true;
    };
    let Some(left_bottom) = left.y.checked_add(left.height) else {
        return true;
    };
    let Some(right_edge) = right_rect.x.checked_add(right_rect.width) else {
        return true;
    };
    let Some(right_bottom) = right_rect.y.checked_add(right_rect.height) else {
        return true;
    };
    !(left.x >= right_edge || right_rect.x >= left_right || left.y >= right_bottom || right_rect.y >= left_bottom)
}

#[cfg(test)]
mod tests;
