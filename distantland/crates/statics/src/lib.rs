#![feature(trim_prefix_suffix)]

//! Static mesh extraction, merging, atlas generation, and serialization.

pub use distantland_formats as mge_xe;
pub use distantland_formats::PackedDistantStatics;
pub use distantland_foundation::{IndexMap, IndexSet, output::OPAQUE_ATLAS_PREFIX};
pub use distantland_texture::texture_dedupe::{TextureDedupeDomainStats, TextureDedupeMode};
pub use distantland_texture::{dds, texture_dedupe, texture_io};
pub use distantland_usage as usage;
pub use distantland_usage::{StaticOverrides, UsageInfo};
pub use distantland_vfs as vfs;
pub use distantland_vfs::{TextureSym, Vfs};

/// Intermediate distant statics keyed by normalized source mesh path.
pub type DistantStatics = IndexMap<String, model::DistantStatic>;

/// Default relative simplification error budget for static mesh simplification.
/// A value of `0.01` allows up to 1% geometric error relative to the mesh's maximum AABB-axis extent.
pub const DEFAULT_STATIC_MESH_TARGET_ERROR: f32 = 0.01;
/// Default attribute weight applied to vertex normals during static mesh simplification.
pub const DEFAULT_STATIC_MESH_NORMAL_WEIGHT: f32 = 0.5;
/// Default attribute weight applied to vertex colors during static mesh simplification.
pub const DEFAULT_STATIC_MESH_COLOR_WEIGHT: f32 = 0.5;
/// Default cap multiplier for additional simplification while merging exterior references.
///
/// `4.0` permits the merge stage to simplify merged exterior-reference groups beyond the
/// initial per-static simplification pass.
pub const DEFAULT_STATIC_MESH_MERGE_ERROR_MULTIPLIER: f32 = 4.0;

/// Default maximum half-diagonal, in game units on the horizontal plane, for one static merge group.
pub const DEFAULT_MERGE_GROUP_RADIUS: f32 = 12288.0;

/// Default cap on a static-atlas source texture's longer axis, in texels.
///
/// Eight times the short-axis default, because 8:1 is the stacking ratio pre-made atlases actually
/// use (surveyed content is overwhelmingly 2048x16384 and 1024x8192, with 4:1 next and 16:1
/// vanishingly rare). At that ratio a stacked atlas keeps its sub-textures at exactly the
/// short-axis cap, so atlased art is neither penalized nor favored against equivalent plain art.
///
/// This is a footprint guard, not a quality knob. Raising it does not buy detail, it only widens
/// pages. A texture sitting at the cap occupies `cap + 32` and page sizing adds 24 more, so
/// `next_pow2` always lands on twice the cap: 2048 floors atlas pages at 4096, whereas 8192 would
/// force the 16384 maximum.
pub const DEFAULT_STATIC_TEXTURE_LONG_SIZE: u32 = 2048;
/// Default cap on a static-atlas source texture's shorter axis, in texels.
///
/// The quality knob, and the one number that means the same thing for both texture kinds: in a
/// stacked atlas the shorter axis *is* the sub-texture resolution. 256 is the vanilla Morrowind
/// texture size, so the default reads as "distant statics get at most vanilla resolution" and
/// passes the largest population of real art through untouched.
///
/// Mostly quality-neutral in practice: [`crate::atlas::sizing`] measures texel density through the
/// capped dimensions, so halving this cap halves the measured density and removes exactly one mip
/// of geometry-informed reduction, leaving the selected size unchanged. The cap only binds art the
/// density pass declines to reduce: sources already below `protected_density`, or clipped by
/// `max_mip_reduction` or `min_texture_size`.
pub const DEFAULT_STATIC_TEXTURE_SHORT_SIZE: u32 = 256;
/// Selectable caps for a static-atlas source texture's longer axis, in texels.
///
/// Stops at 8192: edge extrusion inflates an 8192 texture past 8192, so its page rounds up to
/// 16384, and a 16384 entry could not be placed on any supported page.
pub const SUPPORTED_STATIC_TEXTURE_LONG_SIZES: &[u32] = &[64, 128, 256, 512, 1024, 2048, 4096, 8192];
/// Selectable caps for a static-atlas source texture's shorter axis, in texels. Also the value set
/// for the `static_texture_sizing.min_texture_size` floor.
pub const SUPPORTED_STATIC_TEXTURE_SHORT_SIZES: &[u32] = &[64, 128, 256, 512, 1024, 2048, 4096];
/// Default GPU-safe cap for a static atlas page's longest dimension, in texels.
pub const DEFAULT_STATIC_ATLAS_MAX_SIZE: u32 = 16384;
/// Supported static-atlas page size caps exposed through jobs and the CLI.
pub const SUPPORTED_STATIC_ATLAS_SIZES: &[u32] = &[2048, 4096, 8192, 16384];

pub mod atlas;
pub use atlas::*;

mod extract;
pub use extract::{MeshConsumption, create_distant_statics_with_identities};

pub mod merge;
pub use merge::*;

pub mod metadata;
pub mod model;
pub use model::*;

pub mod overrides;
pub use overrides::*;

pub mod write;
pub use write::*;

mod burial;
pub use burial::is_buried;

pub mod nif;
pub use nif::*;
