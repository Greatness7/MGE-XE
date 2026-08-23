//! Static texture atlas generation and cache reuse.

use std::fs;
use std::path::{Path, PathBuf};

use blake3::{Hash, Hasher};
use hashbrown::HashMap;
use image_dds::ddsfile::D3DFormat;
use rayon::prelude::*;
use rkyv::{Archive, Deserialize, Serialize};

use tex_packer_core::{
    AlgorithmFamily, LayoutItem, MaxRectsHeuristic, PackerConfig, Rect, compute_trim_rect, pack_layout_items,
};
use tracing::info_span;

use crate::mge_xe::distant_statics::StaticType;
use crate::model::UvBound;
use crate::texture_dedupe::{
    TextureDedupeDomainStats, TextureDedupeMode, build_alias_map, decoded_fingerprint, source_fingerprint,
};
use crate::vfs::{ResolvedAsset, directory_map::AssetSource};
use crate::{DistantStatics, IndexSet, Vfs};
use tracing::{error, info, trace};

mod cache;
mod manager;
mod metrics;
mod pack;
mod plan;
mod reconcile;
/// Geometry-informed planning for static texture resolutions.
pub mod sizing;
mod types;
mod uv;
mod write;

use cache::{build_fingerprint_entries, collect_fingerprints, validate_cache};
use sizing::{AtlasDomain, SizingPlan};
use types::{
    ATLAS_BORDER_PADDING, ATLAS_TEXTURE_EXTRUSION, ATLAS_TEXTURE_PADDING, AtlasFamilyConfig, AtlasSharedConfig,
    TextureFingerprint,
};
pub use uv::BindingDelta;
use uv::{cached_uv_map, map_to_cached_bounds, update_uv_bounds_from_maps};
use write::atlas_page_path;
pub(crate) use write::atlas_page_string;

pub use plan::{AtlasPageWrite, AtlasPublishPlan};

pub use metrics::{AtlasBindingDeltaMetrics, AtlasFamilyMetrics, AtlasFamilyPlanMode, AtlasPlanMetrics};
pub use sizing::{StaticTextureSizingMetrics, StaticTextureSizingMode, StaticTextureSizingSettings, TextureAxisCaps};
pub use types::*;

#[cfg(test)]
mod tests;
