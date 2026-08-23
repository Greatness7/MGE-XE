//! MGE-XE-specific binary format structures used by generated distant-land outputs.

use hashbrown::DefaultHashBuilder;

#[doc(hidden)]
pub mod prelude {
    pub use bytes_io::*;
    pub use std::io;
}

/// Binary structures for `distantland\statics\static_meshes`.
pub mod distant_statics;
/// Binary structures for `distantland\terrain.bin`.
pub mod distant_terrain;
/// Binary structures for `distantland\terrain_occlusion.bin`.
pub mod terrain_occlusion;
/// Binary structures for `distantland\statics\usage.data`.
pub mod usage_data;
/// Fixed Morrowind world-space constants.
pub mod world;

mod static_mesh_writer;
pub use static_mesh_writer::serialize_static_meshes;

/// Final distant-static meshes keyed by normalized source mesh path.
pub type PackedDistantStatics = indexmap::IndexMap<String, distant_statics::PackedDistantStatic, DefaultHashBuilder>;
