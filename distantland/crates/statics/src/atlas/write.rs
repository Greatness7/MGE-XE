use super::*;

/// Returns the filesystem path for atlas page `page_index` under `texture_dir`.
///
/// Page 0 uses the bare `<prefix>.dds` name; subsequent pages append `_<index>`.
pub(crate) fn atlas_page_path(texture_dir: &Path, prefix: &str, page_index: usize) -> PathBuf {
    if page_index == 0 {
        texture_dir.join(format!("{prefix}.dds"))
    } else {
        texture_dir.join(format!("{prefix}_{page_index}.dds"))
    }
}

/// Returns the texture filename written into the subset's `texture` field after atlas packing.
///
/// MGE-XE resolves static subset textures relative to `distantland\statics\textures\`, so the
/// binary stores just the atlas page filename rather than a parent-directory escape path.
pub(crate) fn atlas_page_string(prefix: &str, page_index: usize) -> String {
    if page_index == 0 {
        format!("{prefix}.dds")
    } else {
        format!("{prefix}_{page_index}.dds")
    }
}
