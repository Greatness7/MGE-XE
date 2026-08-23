//! Library entry point for generating MGE-XE-compatible distant land outputs.

#[cfg(test)]
mod test_support;

use hashbrown::DefaultHashBuilder;

/// MGE-XE-specific binary-format structures used for generated output files.
pub use distantland_formats as mge_xe;

#[doc(hidden)]
pub mod prelude {
    pub use bytes_io::*;
    pub use std::io;
}

/// Logging setup and tracing helpers used by the CLI and generation pipeline.
pub use distantland_diagnostics::logging;

pub use logging::*;

/// Exact texture deduplication: shared alias-map core and fingerprint helpers.
pub use distantland_texture::texture_dedupe;
pub use texture_dedupe::{TextureDedupeDomainStats, TextureDedupeMode};

/// In-process tracing collection used to summarize generation work in the advisory report.
pub use distantland_diagnostics::tracing_report;

pub use tracing_report::*;

/// Generation job types, progress reporting, startup checks, and output metadata.
pub mod generation;
pub use generation::*;

/// Version-keyed, lock-pinned readers for committed distant-land output.
pub use distantland_foundation::output_index;

/// Virtual filesystem resolution for Morrowind data directories, BSAs, and assets.
pub use distantland_vfs as vfs;
pub use vfs::{
    TextureSym, Vfs, VfsLoadOptions, find_morrowind_ini, morrowind_data_dirs, parse_morrowind_game_files,
    parse_morrowind_game_files_with_data_dirs,
};

/// NIF traversal and normalization helpers used during static extraction.
pub use distantland_statics::nif;

/// Terrain layout, source-texture sampling, package generation, and mesh helpers.
pub use distantland_terrain as terrain;

pub use terrain::layout::*;
pub use terrain::mesh::*;
pub use terrain::texture::*;
pub use terrain::{
    DEFAULT_TERRAIN_ATLAS_MAX_SIZE, DEFAULT_TERRAIN_MESH_COLOR_WEIGHT, DEFAULT_TERRAIN_MESH_SMOOTHED_NORMAL_WEIGHT,
    DEFAULT_TERRAIN_TEXTURE_SIZE, GENERATE_TERRAIN, MAX_TERRAIN_CONTROL_TEXTURE_BYTES, MAX_TERRAIN_CONTROL_TEXTURE_SIZE,
    SUPPORTED_TERRAIN_ATLAS_SIZES, SUPPORTED_TERRAIN_TEXTURE_SIZES,
};

/// Static mesh extraction, merging, atlas generation, and cache writing.
pub use distantland_statics as statics;

pub use statics::atlas;
pub use statics::atlas::*;
pub use statics::merge::*;
pub use statics::model::*;
pub use statics::overrides::*;
pub use statics::{
    DEFAULT_MERGE_GROUP_RADIUS, DEFAULT_STATIC_ATLAS_MAX_SIZE, DEFAULT_STATIC_MESH_COLOR_WEIGHT,
    DEFAULT_STATIC_MESH_MERGE_ERROR_MULTIPLIER, DEFAULT_STATIC_MESH_NORMAL_WEIGHT, DEFAULT_STATIC_MESH_TARGET_ERROR,
    DEFAULT_STATIC_TEXTURE_LONG_SIZE, DEFAULT_STATIC_TEXTURE_SHORT_SIZE, DistantStatics, PackedDistantStatics,
    SUPPORTED_STATIC_ATLAS_SIZES, SUPPORTED_STATIC_TEXTURE_LONG_SIZES, SUPPORTED_STATIC_TEXTURE_SHORT_SIZES,
};

/// Plugin usage scanning and `usage.data` serialization helpers.
pub use distantland_usage as usage;
pub use usage::info::*;

/// `IndexMap` configured with the crate's default hash builder.
pub type IndexMap<K, V> = indexmap::IndexMap<K, V, DefaultHashBuilder>;
/// `IndexSet` configured with the crate's default hash builder.
pub type IndexSet<V> = indexmap::IndexSet<V, DefaultHashBuilder>;
