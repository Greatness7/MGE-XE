//! Traced facade over the MGE-XE static-mesh binary serializer.

use tracing::info_span;

use crate::PackedDistantStatics;

/// Serializes distant statics into the current MGE-XE `static_meshes` binary format.
///
/// # Errors
///
/// Returns an error when a count, offset, component record, or serialized size violates the
/// format bounds.
#[tracing::instrument(skip_all)]
pub fn serialize_static_meshes(distant_statics: &PackedDistantStatics) -> anyhow::Result<Vec<u8>> {
    let serialize_span = info_span!(
        "io.serialize_static_meshes",
        report = true,
        generated_static_count = distant_statics.len() as u64,
        bytes = tracing::field::Empty
    );
    let _guard = serialize_span.enter();
    let bytes = distantland_formats::serialize_static_meshes(distant_statics)?;
    serialize_span.record("bytes", bytes.len() as u64);
    Ok(bytes)
}
