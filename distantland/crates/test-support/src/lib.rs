//! Hermetic fixtures, typed fixture mutations, and static-output equivalence assertions.

use hashbrown::DefaultHashBuilder;

pub(crate) use distantland_formats as mge_xe;
pub(crate) use distantland_foundation::output_index;
pub(crate) use distantland_job::{GenerationJob, GenerationSettings};
pub(crate) use distantland_statics as statics;
pub(crate) use distantland_texture::dds;

pub(crate) type IndexMap<K, V> = indexmap::IndexMap<K, V, DefaultHashBuilder>;

mod fixture;
mod mutation;
mod verification;

pub use fixture::*;
pub use mutation::{edit_fixture_land_height, edit_fixture_nif_geometry, rewrite_fixture_texture};
pub use verification::assert_static_outputs_equivalent;

/// Writes a generation job as an embedded `mgeXE.toml` document for process-level tests.
pub fn write_generation_job_file(path: &std::path::Path, job: &GenerationJob) -> anyhow::Result<()> {
    let document = distantland_job::serialize_generation_job_document(job)?;
    std::fs::write(path, document)?;
    Ok(())
}
