//! Geometry-informed static texture resolution.
//!
//! Measures the effective texel density of every atlas-eligible texture use directly from the
//! transformed geometry and raw UV mapping, aggregates per `(texture, domain)`, and selects a
//! per-texture atlas dimension. The [`TextureAxisCaps`] baseline is an unconditional upper bound;
//! this only ever reduces further. Default mode is `Downscale`; the `Off` mode is
//! behaviour-identical to having no sizing pass.

use std::collections::BTreeMap;

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

use super::usable_texture_dim;
use crate::texture_io::limited_dimensions;

/// Operating mode for geometry-informed static texture resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaticTextureSizingMode {
    /// Disabled: every texture keeps its baseline dimensions (native, bounded by the page cap).
    #[default]
    Off,
    /// Measure and report without changing selected texture dimensions.
    Report,
    /// Measure, report, and downscale eligible opaque textures only.
    ///
    /// Alpha-tested art (foliage, lattices) is left at baseline. Reducing it thins the alpha-test
    /// mask, which also feeds shadow rendering; keep this mode if that proves visible.
    DownscaleOpaque,
    /// Measure, report, and downscale eligible textures in both domains.
    Downscale,
}

impl StaticTextureSizingMode {
    /// Whether a reduction selected for `domain` is actually applied to the plan.
    pub fn reduces(self, domain: AtlasDomain) -> bool {
        match self {
            Self::Off | Self::Report => false,
            Self::DownscaleOpaque => domain == AtlasDomain::Opaque,
            Self::Downscale => true,
        }
    }
}

/// Settings controlling geometry-informed static texture resolution.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct StaticTextureSizingSettings {
    /// Operating mode.
    pub mode: StaticTextureSizingMode,
    /// Required texel density in texels per game unit.
    pub protected_density: f32,
    /// Floor for the longest selected dimension, in texels.
    pub min_texture_size: u32,
    /// Maximum additional whole-mip reductions past the baseline dimension.
    pub max_mip_reduction: u8,
}

impl Default for StaticTextureSizingSettings {
    fn default() -> Self {
        Self {
            mode: StaticTextureSizingMode::Downscale,
            protected_density: 0.2,
            min_texture_size: 64,
            max_mip_reduction: 4,
        }
    }
}

mod analyze;
mod math;
mod plan;

pub use analyze::{
    SourceTextureInfo, TextureUsageAggregate, analyze_static_texture_usage, collect_static_texture_source_info,
    fingerprints_from_source_info,
};
pub use plan::{StaticTextureSizingMetrics, merge_dedupe_alias_requirements, plan_static_texture_resolutions};

/// Atlas domain a sizing decision applies to. Opaque and alpha requirements stay independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtlasDomain {
    /// Opaque (DXT1) atlas domain, the only domain eligible for downscaling.
    Opaque,
    /// Alpha-tested (DXT5) atlas domain, analyzed and reported but never downscaled.
    Alpha,
}

impl AtlasDomain {
    /// Stable lowercase tag used in the per-texture report.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Alpha => "alpha",
        }
    }
}

/// Independent caps on a source texture's longer and shorter axis, in texels.
///
/// Scaling always preserves aspect ratio, so the two caps resolve per texture into the single
/// longest-side limit the rest of the pipeline carries. Equal caps reproduce a plain "longest
/// side" cap exactly. Raising `long` above `short` is what lets a pre-made atlas keep its stacking
/// axis: Project Atlas stacks its sub-textures along one axis, giving shapes like 512x8192,
/// while ordinary square art is still bounded by `short`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureAxisCaps {
    /// Cap on whichever axis is longer.
    pub long: u32,
    /// Cap on whichever axis is shorter.
    pub short: u32,
}

impl TextureAxisCaps {
    /// The configured caps, floored by what can actually be placed on one `atlas_max_size` page.
    ///
    /// The packable dimension is strictly below the page cap (border padding and edge extrusion
    /// consume the difference), so a texture sized at the raw page cap could never be packed.
    pub fn for_atlas(long: u32, short: u32, atlas_max_size: u32) -> Self {
        let usable = usable_texture_dim(atlas_max_size);
        Self {
            long: long.min(usable),
            short: short.min(usable),
        }
    }

    /// One cap applied to both axes.
    pub fn uniform(cap: u32) -> Self {
        Self { long: cap, short: cap }
    }

    /// The effective longest-side limit for a source of `width` x `height`.
    ///
    /// `short * long_axis / short_axis` is the longest side at which the shorter axis lands
    /// exactly on its cap, so the smaller of that and `long` satisfies both. The result is then
    /// snapped down to a whole mip step of the source.
    pub fn longest_for(self, width: u32, height: u32) -> u32 {
        let long_axis = width.max(height);
        let short_axis = width.min(height);
        if short_axis == 0 {
            return self.long;
        }
        let from_short = (u64::from(self.short) * u64::from(long_axis) / u64::from(short_axis)).min(u64::from(u32::MAX));
        snap_to_mip_step(long_axis, self.long.min(from_short as u32))
    }
}

/// Rounds `cap` down to a whole mip step of `source_long`, so every downscale the generator
/// performs selects a mip level rather than resampling between two.
///
/// A no-op whenever `cap` is already a power-of-two division of the source, which is every
/// selectable cap against the power-of-two art the game and its replacers ship. It bites on the
/// page-fit floor, which sits 48 texels below a power of two: an 8192 source under an 8192 page
/// would otherwise be resampled to 8144, blurring across every internal boundary of a pre-made
/// atlas to save 0.6% of a page.
pub(crate) fn snap_to_mip_step(source_long: u32, cap: u32) -> u32 {
    let mut dim = source_long;
    while dim > cap && dim > 1 {
        dim >>= 1;
    }
    dim
}

/// Baseline dimensions for one probed source under `caps`, or `None` when unprobeable.
pub(crate) fn baseline_dims_for(info: &SourceTextureInfo, caps: TextureAxisCaps) -> Option<(u32, u32)> {
    if info.width == 0 || info.height == 0 {
        return None;
    }
    Some(limited_dimensions(
        info.width,
        info.height,
        caps.longest_for(info.width, info.height),
    ))
}

/// Per-texture atlas-dimension plan: an axis-derived baseline per texture plus a compact, sorted,
/// domain-tagged deviation overlay listing only the textures reduced below that baseline.
///
/// Empty overlays mean every texture resolves to its baseline. The atlas resolves
/// `dim = override.get(key).or(baseline.get(key)).unwrap_or(caps.long)`.
///
/// The two override overlays must stay ordered maps. `AtlasFamilyConfig::current` serializes
/// them into the atlas cache as a flat `Vec`, and the cache validator rejects a family whose
/// `sizing_overrides` is not strictly ascending by key, so an unordered map here would not
/// corrupt anything, it would silently invalidate every atlas family on every run. `baselines`
/// carries no such constraint because it never reaches the cache.
#[derive(Clone, Debug)]
pub struct SizingPlan {
    /// Axis caps the baselines were derived from, already floored to what fits one atlas page.
    pub(crate) caps: TextureAxisCaps,
    /// Unconditional per-texture upper bound: texture key → longest dimension permitted by `caps`.
    ///
    /// This has to be its own map rather than an entry in the overlays below, because those are
    /// populated only when the mode reduces the domain and are discarded wholesale for a dedupe
    /// alias group that needs full resolution. A mandatory cap must survive both.
    ///
    /// Unordered: only ever point-queried by `dim_for`, never iterated into output.
    pub(crate) baselines: HashMap<String, u32>,
    /// Opaque-domain reductions: texture key → selected longest dimension (below its baseline).
    ///
    /// Ordered on purpose. See the overlay ordering note on [`SizingPlan`].
    pub(crate) opaque_overrides: BTreeMap<String, u32>,
    /// Alpha-domain reductions; populated only in `Downscale` mode.
    ///
    /// Ordered on purpose. See the overlay ordering note on [`SizingPlan`].
    pub(crate) alpha_overrides: BTreeMap<String, u32>,
}

impl SizingPlan {
    /// A plan with no reductions: every probed texture resolves to its axis-derived baseline.
    pub fn baseline(caps: TextureAxisCaps, source_info: &HashMap<String, SourceTextureInfo>) -> Self {
        let baselines = source_info
            .iter()
            .filter_map(|(key, info)| {
                let (width, height) = baseline_dims_for(info, caps)?;
                Some((key.clone(), width.max(height)))
            })
            .collect();
        Self {
            caps,
            baselines,
            opaque_overrides: BTreeMap::new(),
            alpha_overrides: BTreeMap::new(),
        }
    }

    /// A plan with no reductions and no probed sources: every texture resolves to `cap`.
    pub fn uniform(cap: u32) -> Self {
        Self {
            caps: TextureAxisCaps::uniform(cap),
            baselines: HashMap::new(),
            opaque_overrides: BTreeMap::new(),
            alpha_overrides: BTreeMap::new(),
        }
    }

    /// Returns the selected longest-dimension cap for `key` in `domain`.
    ///
    /// An unprobeable texture has no baseline and falls back to the long cap, which is what the
    /// whole pipeline did before axis caps existed. A gap degrades to the old behaviour.
    pub fn dim_for(&self, key: &str, domain: AtlasDomain) -> u32 {
        let overrides = match domain {
            AtlasDomain::Opaque => &self.opaque_overrides,
            AtlasDomain::Alpha => &self.alpha_overrides,
        };
        overrides
            .get(key)
            .or_else(|| self.baselines.get(key))
            .copied()
            .unwrap_or(self.caps.long)
    }

    /// Returns the mutable reduction overlay for `domain`.
    pub(crate) fn overrides_mut(&mut self, domain: AtlasDomain) -> &mut BTreeMap<String, u32> {
        match domain {
            AtlasDomain::Opaque => &mut self.opaque_overrides,
            AtlasDomain::Alpha => &mut self.alpha_overrides,
        }
    }
}

#[cfg(test)]
mod tests;
