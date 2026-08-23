//! Terrain layout, source-texture sampling, package generation, and mesh generation.
//!
//! This module exposes four terrain-related pipelines:
//! - **Layout** – groups landscape cells into contiguous atlas regions and assigns each
//!   region a packed cell offset within the atlas span.
//! - **Texture** – loads merged terrain cells and their per-cell source textures, and
//!   exposes the sampling primitives used by the package builder.
//! - **Package** – builds the `terrain.bin` plus its companion DDS textures.
//! - **Mesh** – generates terrain geometry from dense LAND grids simplified with
//!   meshopt, emitting `TerrainMesh` records with packed normals and vertex
//!   colors.
//!
//! All public symbols from the submodules are re-exported at this level so callers only
//! need to import `crate::*`.

pub use distantland_formats as mge_xe;
pub use distantland_foundation::{IndexMap, IndexSet};
pub use distantland_texture::{dds, texture_dedupe, texture_io};
pub use distantland_usage as usage;
pub use distantland_usage::UsageInfo;
pub use distantland_vfs as vfs;

/// Default terrain mesh simplifier weight for smoothed vertex normals.
///
/// Zero by default: position error and vertex color already guard the features that read as
/// terrain detail at distance, so the normal term mostly protects curvature on gentle slopes that
/// are already within the height budget. Disabling it decimates those slopes a little further for
/// a visual difference measured as negligible in-game, and skips building the smoothed-normal map
/// entirely (see `mesh.rs`), cutting terrain mesh CPU by roughly 9%. Raise it to restore
/// curvature-aware collapse at that cost.
pub const DEFAULT_TERRAIN_MESH_SMOOTHED_NORMAL_WEIGHT: f32 = 0.0;
/// Default terrain mesh simplifier weight for vertex colors.
pub const DEFAULT_TERRAIN_MESH_COLOR_WEIGHT: f32 = 1.6875;
/// Default terrain generation toggle.
pub const GENERATE_TERRAIN: bool = true;
/// Default maximum logical tile size for the terrain texture atlas.
pub const DEFAULT_TERRAIN_TEXTURE_SIZE: u32 = 256;
/// Supported terrain texture-size presets exposed through jobs and the CLI.
pub const SUPPORTED_TERRAIN_TEXTURE_SIZES: &[u32] = &[64, 128, 256, 512];
/// Default cap for the terrain texture atlas's dimension, in texels.
pub const DEFAULT_TERRAIN_ATLAS_MAX_SIZE: u32 = 16384;
/// Supported power-of-two terrain-atlas size caps exposed through jobs and the CLI.
pub const SUPPORTED_TERRAIN_ATLAS_SIZES: &[u32] = &[2048, 4096, 8192, 16384];
/// Default cap for either side of the rectangular terrain control maps, in texels.
pub const MAX_TERRAIN_CONTROL_TEXTURE_SIZE: u32 = 16384;
/// Default byte-budget cap for the rectangular terrain control maps and patch albedo.
pub const MAX_TERRAIN_CONTROL_TEXTURE_BYTES: u64 = 256 * 1024 * 1024;

/// Terrain atlas packing helpers.
pub mod layout;
pub use layout::*;

/// Terrain source-texture sampling and cache helpers.
pub mod texture;
pub use texture::*;

/// Terrain mesh generation helpers.
pub mod mesh;
pub use mesh::*;

/// Landscape patch texture planning, vendored from the tes3 `landscape` branch.
pub mod landscape_plan;

/// Terrain package generation helpers.
pub mod package;
