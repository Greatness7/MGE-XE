//! Turns per-texture density aggregates into a [`SizingPlan`] and bounded metrics.

use std::collections::BTreeMap;

use hashbrown::HashMap;
use itertools::Itertools;

use super::analyze::{SourceTextureInfo, TextureUsageAggregate};
use super::{AtlasDomain, SizingPlan, TextureAxisCaps, baseline_dims_for};
use crate::texture_dedupe::source_fingerprint;
use crate::{AtlasTextureSet, IndexSet, StaticTextureSizingSettings, TextureDedupeMode};

/// Share of a texture's triangles that may be unmeasurable before it falls back to baseline.
///
/// Per-triangle failures are common in third-party meshes; a single one is not evidence that the
/// measured density is wrong. Beyond this share we have genuinely not measured the texture.
const UNCERTAIN_FALLBACK_RATIO: f64 = 0.25;

/// Counters used by the sizing diagnostic span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StaticTextureSizingMetrics {
    /// Pairs whose proposed selected dimension is below baseline, in every mode.
    ///
    /// This includes reductions that the selected mode leaves unapplied, distinguishing measured
    /// work from no proposal in `Report` and alpha `DownscaleOpaque` cases.
    pub reduced_texture_count: u64,
}

/// Builds the per-texture atlas-dimension plan and its decision counters.
///
/// `Downscale` reduces both domains; `DownscaleOpaque` reduces opaque only, leaving alpha analyzed
/// and reported but unreduced. `Report` and `Off` populate no overlays.
pub fn plan_static_texture_resolutions(
    source_info: &HashMap<String, SourceTextureInfo>,
    usage: &AtlasTextureSet<HashMap<String, TextureUsageAggregate>>,
    settings: &StaticTextureSizingSettings,
    caps: TextureAxisCaps,
) -> (SizingPlan, StaticTextureSizingMetrics) {
    let mut plan = SizingPlan::baseline(caps, source_info);
    let mut metrics = StaticTextureSizingMetrics::default();

    for (key, aggregate) in sorted_entries(&usage.opaque) {
        evaluate(
            key,
            AtlasDomain::Opaque,
            source_info.get(key),
            aggregate,
            settings,
            caps,
            &mut plan,
            &mut metrics,
        );
    }
    for (key, aggregate) in sorted_entries(&usage.alpha) {
        evaluate(
            key,
            AtlasDomain::Alpha,
            source_info.get(key),
            aggregate,
            settings,
            caps,
            &mut plan,
            &mut metrics,
        );
    }

    (plan, metrics)
}

fn sorted_entries(map: &HashMap<String, TextureUsageAggregate>) -> Vec<(&String, &TextureUsageAggregate)> {
    let mut pairs: Vec<_> = map.iter().collect();
    pairs.sort_unstable_by(|a, b| a.0.cmp(b.0));
    pairs
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    key: &str,
    domain: AtlasDomain,
    info: Option<&SourceTextureInfo>,
    aggregate: &TextureUsageAggregate,
    settings: &StaticTextureSizingSettings,
    caps: TextureAxisCaps,
    plan: &mut SizingPlan,
    metrics: &mut StaticTextureSizingMetrics,
) {
    let baseline_dims = info.and_then(|i| baseline_dims_for(i, caps));
    let (baseline_width, baseline_height) = baseline_dims.unwrap_or((0, 0));
    let baseline_longest = baseline_width.max(baseline_height);

    let measured = aggregate.valid_count + aggregate.uncertain_count;
    let too_uncertain = measured > 0 && (aggregate.uncertain_count as f64) > (measured as f64) * UNCERTAIN_FALLBACK_RATIO;
    let fallback = baseline_dims.is_none()
        || aggregate.needs_baseline
        || aggregate.valid_count == 0
        || too_uncertain
        || !aggregate.area_density_min.is_finite();

    // Uncertain measurements leave the texture at its baseline.
    if fallback {
        return;
    }
    let selected_longest = select_longest(baseline_longest, aggregate.area_density_min, settings);
    if selected_longest < baseline_longest {
        metrics.reduced_texture_count += 1;
        if settings.mode.reduces(domain) {
            plan.overrides_mut(domain).insert(key.to_owned(), selected_longest);
        }
    }
}

/// Applies the reduction policy to select a longest dimension.
///
/// `required_scale = clamp(protected / area_density, 0, 1)`; `reduction = min(floor(log2(1/scale)),
/// max_mip_reduction)`; `selected = clamp(baseline >> reduction, floor, baseline)` where the floor
/// is `min(min_texture_size, baseline)` so a sub-floor source never upscales.
///
/// A zero density (a rank-1 use, which spans no texel area) clamps to a full-scale requirement and
/// therefore keeps the baseline.
fn select_longest(baseline_longest: u32, area_density: f32, settings: &StaticTextureSizingSettings) -> u32 {
    let required_scale = (settings.protected_density / area_density).clamp(0.0, 1.0);
    if required_scale <= 0.0 {
        return baseline_longest; // Fail closed; unreachable given validated protected_density > 0.
    }
    let reduction = if required_scale >= 1.0 {
        0
    } else {
        ((1.0 / required_scale).log2().floor().max(0.0) as u32).min(settings.max_mip_reduction as u32)
    };
    let floor = settings.min_texture_size.min(baseline_longest);
    (baseline_longest >> reduction).max(floor).min(baseline_longest)
}

/// Lifts every source-byte-identical alias group to the group's largest selected dimension so a
/// shared decoded texture is never under-sized for any of its uses. Runs after planning and before
/// atlas setup; a no-op when `dedupe_mode == Off` (no aliasing).
pub fn merge_dedupe_alias_requirements(
    textures: &AtlasTextureSet<IndexSet<String>>,
    source_info: &HashMap<String, SourceTextureInfo>,
    dedupe_mode: TextureDedupeMode,
    plan: &mut SizingPlan,
) {
    if dedupe_mode == TextureDedupeMode::Off {
        return;
    }
    let caps = plan.caps;
    reconcile_domain(&textures.opaque, source_info, caps, &mut plan.opaque_overrides);
    reconcile_domain(&textures.alpha, source_info, caps, &mut plan.alpha_overrides);
}

fn reconcile_domain(
    keys: &IndexSet<String>,
    source_info: &HashMap<String, SourceTextureInfo>,
    caps: TextureAxisCaps,
    overrides: &mut BTreeMap<String, u32>,
) {
    let groups = keys
        .iter()
        .filter_map(|key| {
            let info = source_info.get(key)?;
            Some((source_fingerprint(info.size, &info.hash), key))
        })
        .into_group_map();

    for members in groups.into_values() {
        if members.len() < 2 {
            continue;
        }
        let mut baseline = 0u32;
        let mut group_selected = 0u32;
        for &key in &members {
            let info = source_info.get(key).expect("group member has source info");
            let member_baseline = baseline_longest_of(info, caps);
            baseline = baseline.max(member_baseline);
            group_selected = group_selected.max(overrides.get(key).copied().unwrap_or(member_baseline));
        }
        if group_selected >= baseline {
            // Some member needs full resolution; the shared texture cannot be reduced.
            for &key in &members {
                overrides.remove(key);
            }
        } else {
            for &key in &members {
                overrides.insert(key.clone(), group_selected);
            }
        }
    }
}

/// Longest baseline dimension for a source, falling back to the long cap when unprobeable so an
/// unprobeable group never reduces.
fn baseline_longest_of(info: &SourceTextureInfo, caps: TextureAxisCaps) -> u32 {
    baseline_dims_for(info, caps).map_or(caps.long, |(width, height)| width.max(height))
}

#[cfg(test)]
mod tests;
