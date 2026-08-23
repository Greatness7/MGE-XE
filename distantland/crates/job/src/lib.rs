//! Generation job schemas, validation, and path-resolution helpers.
//!
//! This is a leaf crate so that `distantland_test_support` can build jobs without
//! depending on `distantland` itself, which dev-depends on test-support in turn. The
//! two together formed a dependency cycle that Cargo tolerates but rust-analyzer cannot
//! represent, and that forced test-support's `GenerationJob` and the root crate's unit-test
//! `GenerationJob` to be distinct types bridged by a serde round-trip.
//!
//! `distantland` re-exports this crate as its `generation::job` module, so
//! `distantland::GenerationJob` and friends continue to resolve here.
//!
//! Implementation lives in private modules; every public item is re-exported here, making the
//! crate root the only supported API surface.

mod canonical;
mod document;
mod job;
mod tolerant;

#[cfg(test)]
mod tests;

pub use canonical::{
    static_texture_sizing_mode_tag, terrain_detail_tag, texture_dedupe_mode_tag, write_generation_settings_canonical,
};
pub use document::{
    GENERATION_JOB_FILE_VERSION, GENERATION_JOB_NAMESPACE, GenerationJobFile, GenerationJobLoad, load_generation_job_file,
    load_generation_job_file_with_warnings, serialize_generation_job_document,
};
pub use job::{
    GenerationJob, GenerationJobWarning, GenerationSettings, TerrainDetail, resolve_generation_job_paths,
    sync_plugins_from_load_order,
};
