use super::*;

pub use crate::OPAQUE_ATLAS_PREFIX;
/// Filename prefix for alpha-tested (DXT5) atlas pages.
pub const ALPHA_ATLAS_PREFIX: &str = "_mge_xe_atlas_alpha";
/// Padding in texels added around the entire atlas border to avoid edge-bleeding artifacts.
pub(crate) const ATLAS_BORDER_PADDING: u32 = 8;
/// Per-frame padding in texels between packed textures (additional separation beyond extrusion).
pub(crate) const ATLAS_TEXTURE_PADDING: u32 = 0;
/// Pixels extruded at each texture edge to prevent bilinear bleed across frame boundaries.
pub(crate) const ATLAS_TEXTURE_EXTRUSION: u32 = 16;
/// Whether transparent borders are trimmed before packing.
pub(crate) const ATLAS_TRIM: bool = true;

/// Largest texture, in texels on the longest side, that can actually be placed on a page whose
/// cap is `atlas_max_size`.
///
/// The packer shrinks the usable canvas by `ATLAS_BORDER_PADDING` on each side and inflates
/// every texture by `ATLAS_TEXTURE_PADDING` plus `ATLAS_TEXTURE_EXTRUSION` on each side, so a
/// texture sized at the raw cap can never be placed on any page, however empty. Downscaling to
/// this value instead lands the resulting page at exactly `atlas_max_size` with no waste.
pub fn usable_texture_dim(atlas_max_size: u32) -> u32 {
    atlas_max_size
        .saturating_sub(2 * ATLAS_BORDER_PADDING)
        .saturating_sub(ATLAS_TEXTURE_PADDING)
        .saturating_sub(2 * ATLAS_TEXTURE_EXTRUSION)
}
/// Version of the atlas evidence recipe embedded in `generation_state.bin`.
pub(crate) const ATLAS_CACHE_VERSION: u32 = 5;

#[derive(Clone, Debug, Default)]
pub struct AtlasTextureSet<T> {
    pub opaque: T,
    pub alpha: T,
}

impl<T> AtlasTextureSet<T> {
    pub(crate) fn new(opaque: T, alpha: T) -> Self {
        Self { opaque, alpha }
    }
}

impl AtlasTextureSet<IndexSet<String>> {
    /// Collects unique normalized texture keys referenced by the current statics.
    pub fn from_distant_statics(vfs: &Vfs, distant_statics: &DistantStatics) -> Self {
        let span = info_span!(
            "atlas.collect_textures",
            report = true,
            opaque_texture_count = tracing::field::Empty,
            alpha_texture_count = tracing::field::Empty
        );
        let _guard = span.enter();
        let mut opaque = IndexSet::default();
        for key in distant_statics
            .values()
            .filter(|ds| ds.static_type != StaticType::StaticGrass)
            .flat_map(|ds| &ds.subsets)
            .filter(|subset| !subset.has_uv_controller)
            .filter(|subset| subset.is_opaque())
            .filter_map(|subset| vfs.texture_key_for_sym(subset.texture.source_sym()?))
        {
            if !opaque.contains(key) {
                opaque.insert(key.to_owned());
            }
        }
        opaque.sort_unstable();

        let mut alpha = IndexSet::default();
        for key in distant_statics
            .values()
            .filter(|ds| ds.static_type != StaticType::StaticGrass)
            .flat_map(|ds| &ds.subsets)
            .filter(|subset| !subset.has_uv_controller)
            .filter(|subset| subset.has_alpha())
            .filter_map(|subset| vfs.texture_key_for_sym(subset.texture.source_sym()?))
        {
            if !alpha.contains(key) {
                alpha.insert(key.to_owned());
            }
        }
        alpha.sort_unstable();

        span.record("opaque_texture_count", opaque.len() as u64);
        span.record("alpha_texture_count", alpha.len() as u64);
        Self { opaque, alpha }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub(crate) struct TextureFingerprint {
    /// File size in bytes.
    pub(crate) size: u64,
    /// BLAKE3 content hash.
    pub(crate) hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub(crate) struct AtlasSharedConfig {
    pub(crate) gpu_max: u32,
    pub(crate) border_padding: u32,
    pub(crate) texture_padding: u32,
    pub(crate) texture_extrusion: u32,
    pub(crate) trim: bool,
    /// Unconditional cap on any source texture's longer axis before atlas packing.
    pub(crate) max_texture_long_dim: u32,
    /// Unconditional cap on any source texture's shorter axis. Recorded separately because the
    /// per-texture baselines derive from both, and lowering only this one must repack.
    pub(crate) max_texture_short_dim: u32,
    /// Texture deduplication mode. Changing it repacks atlases and reassigns frames.
    pub(crate) dedupe_mode: TextureDedupeMode,
    /// Exact-dedupe algorithm version. A bump invalidates caches built by an older algorithm.
    pub(crate) recipe_version: u32,
}

impl AtlasSharedConfig {
    /// Returns the shared configuration for the current atlas settings.
    pub(crate) fn current(plan: &SizingPlan, dedupe_mode: TextureDedupeMode, gpu_max: u32) -> Self {
        Self {
            gpu_max,
            border_padding: ATLAS_BORDER_PADDING,
            texture_padding: ATLAS_TEXTURE_PADDING,
            texture_extrusion: ATLAS_TEXTURE_EXTRUSION,
            trim: ATLAS_TRIM,
            max_texture_long_dim: plan.caps.long,
            max_texture_short_dim: plan.caps.short,
            dedupe_mode,
            recipe_version: distantland_foundation::units::STATICS_RECIPE_VERSION,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub(crate) struct AtlasFamilyConfig {
    /// Sorted per-texture sizing deviations below the shared global cap.
    pub(crate) sizing_overrides: Vec<(String, u32)>,
}

impl AtlasFamilyConfig {
    /// Returns the family-local sizing configuration for `domain`.
    ///
    /// The ordered overlay is serialized directly; cache validation requires its keys to remain
    /// strictly ascending.
    pub(crate) fn current(plan: &SizingPlan, domain: AtlasDomain) -> Self {
        let overrides = match domain {
            AtlasDomain::Opaque => &plan.opaque_overrides,
            AtlasDomain::Alpha => &plan.alpha_overrides,
        };
        Self {
            sizing_overrides: overrides.iter().map(|(key, &dim)| (key.clone(), dim)).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Archive, Serialize, Deserialize)]
pub struct CachedUvBound {
    /// Normalized source texture key.
    pub path: String,
    /// Atlas page index.
    pub page: u32,
    /// Minimum U coordinate.
    pub min_x: f32,
    /// Maximum U coordinate.
    pub max_x: f32,
    /// Minimum V coordinate.
    pub min_y: f32,
    /// Maximum V coordinate.
    pub max_y: f32,
}

#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub(crate) struct CachedTextureFingerprint {
    pub(crate) path: String,
    pub(crate) fingerprint: TextureFingerprint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub(crate) struct CachedAtlasRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub(crate) struct CachedAtlasPage {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub(crate) struct CachedAtlasSlot {
    /// Monotonic family-local identity. Freed ids are never reused.
    pub(crate) slot_id: u64,
    /// Stable page index.
    pub(crate) page_id: u32,
    /// Complete rectangle withheld from the allocator, including extrusion and padding.
    pub(crate) reserved_rect: CachedAtlasRect,
    /// Visible composited destination within `reserved_rect`.
    pub(crate) destination: CachedAtlasRect,
    /// Visible source rectangle within the prepared provider image.
    pub(crate) source: CachedAtlasRect,
    pub(crate) source_size: [u32; 2],
    pub(crate) rotated: bool,
    pub(crate) trimmed: bool,
    /// Logical key used to name the retained prepared image.
    pub(crate) provider_key: String,
    /// BLAKE3 of visible width, height, and row-major RGBA bytes.
    pub(crate) content_fingerprint: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub(crate) struct CachedAtlasKeySlot {
    pub(crate) path: String,
    pub(crate) slot_id: u64,
}

#[derive(Clone, Debug, PartialEq, Archive, Serialize, Deserialize)]
pub(crate) struct CachedAtlasFamily {
    pub(crate) family_config: AtlasFamilyConfig,
    pub(crate) layout_input_digest: [u8; 32],
    /// Next never-issued slot id.
    pub(crate) next_slot_id: u64,
    pub(crate) pages: Vec<CachedAtlasPage>,
    /// Active slots sorted by `slot_id`.
    pub(crate) slots: Vec<CachedAtlasSlot>,
    /// Complete logical-key relation sorted by path.
    pub(crate) key_slots: Vec<CachedAtlasKeySlot>,
    pub(crate) bindings: Vec<CachedUvBound>,
    pub(crate) texture_fingerprints: Vec<CachedTextureFingerprint>,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
pub struct AtlasCache {
    pub(crate) version: u32,
    pub(crate) shared_config: AtlasSharedConfig,
    pub(crate) opaque: CachedAtlasFamily,
    pub(crate) alpha: CachedAtlasFamily,
}

pub struct AtlasManager {
    /// Independently reusable prior opaque/alpha family evidence from committed state.
    ///
    /// Each entry may be present without the other after family-local validation rejects only one
    /// sibling. Both entries are `Some` only for a complete two-family prior cache. This is not the
    /// persisted root `AtlasCache` schema. Planning always emits a complete cache for publication.
    pub(crate) prior: AtlasTextureSet<Option<CachedAtlasFamily>>,
    /// Content fingerprints for opaque and alpha source textures.
    pub fingerprints: AtlasTextureSet<Vec<(u64, Hash)>>,
    /// Per-texture atlas-dimension plan (carries the global cap plus any reductions).
    pub(crate) plan: SizingPlan,
    /// Texture deduplication mode applied during packing and recorded in the cache config.
    pub dedupe_mode: TextureDedupeMode,
    /// Maximum atlas page dimension in texels (GPU-safe cap recorded in the cache config).
    pub(crate) atlas_max_size: u32,
    /// Directory that receives atlas page DDS files.
    pub texture_dir: PathBuf,
}
