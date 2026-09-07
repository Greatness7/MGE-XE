//! Traced facade over the MGE-XE static-mesh binary serializer.

use tracing::info_span;

use crate::PackedDistantStatics;
use distantland_formats::distant_statics::ComponentRecord;

fn validate_subset_provenance(key: &str, components: &[ComponentRecord]) -> anyhow::Result<()> {
    use distantland_foundation::record_key::StaticRecordKey;

    match StaticRecordKey::parse(key) {
        StaticRecordKey::Mesh { .. } if !components.is_empty() => {
            anyhow::bail!("ordinary static {key:?} contains merged component provenance")
        }
        StaticRecordKey::Merged { .. } if components.is_empty() => {
            anyhow::bail!("merged static {key:?} contains a componentless subset")
        }
        _ => Ok(()),
    }
}

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
    for (key, distant_static) in distant_statics {
        for subset in &distant_static.subsets {
            validate_subset_provenance(key, &subset.components)?;
        }
    }
    let bytes = distantland_formats::serialize_static_meshes(distant_statics)?;
    serialize_span.record("bytes", bytes.len() as u64);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_static_rejects_component_provenance() {
        assert!(validate_subset_provenance("meshes\\x.nif", &[ComponentRecord::default()]).is_err());
    }

    #[test]
    fn merged_static_rejects_componentless_subset() {
        assert!(validate_subset_provenance("CELL (1, -2) GROUP (3)", &[]).is_err());
    }

    #[test]
    fn provenance_classes_accept_their_expected_subset_shape() {
        assert!(validate_subset_provenance("meshes\\x.nif", &[]).is_ok());
        assert!(validate_subset_provenance("CELL (1, -2) GROUP (3)", &[ComponentRecord::default()],).is_ok());
    }
}
