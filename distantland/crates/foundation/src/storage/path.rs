//! Inventory path grammar and related path services.
//!
//! Paths are relative to `distantland`, use backslashes, are lower-case ASCII, and never contain
//! absolute or parent-directory components. Epoch path formatting is gone: payloads use fixed paths.
//! This module also owns distantland-relative resolution and recognition of superseded v13 names.

use std::path::{Path, PathBuf};

const MAX_PATH_BYTES: usize = 64;
const ATLAS_PREFIX: &str = "statics\\textures\\_mge_xe_atlas";
const ATLAS_ALPHA_PREFIX: &str = "statics\\textures\\_mge_xe_atlas_alpha";

/// Relative paths of the five terrain companion DDS outputs.
pub const TERRAIN_DDS_PATHS: [&str; 5] = [
    "terrain_atlas.dds",
    "terrain_blend_patterns.dds",
    "terrain_material.dds",
    "terrain_material_flags.dds",
    "terrain_patch_albedo.dds",
];

/// Returns the id encoded by a static-shard path for this build.
pub fn parse_static_shard(path: &str) -> Option<usize> {
    if !common_path_rules(path) {
        return None;
    }
    let digits = path.strip_prefix("statics\\static_meshes_")?;
    if digits.len() != crate::output::STATIC_MESH_SHARD_ID_WIDTH || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let shard_id = digits.parse::<usize>().ok()?;
    (shard_id < crate::output::STATIC_MESH_SHARD_COUNT).then_some(shard_id)
}

/// Returns the atlas page encoded by a page path.
pub fn parse_atlas_page(path: &str) -> Option<u32> {
    if !common_path_rules(path) {
        return None;
    }
    // `strip_prefix(P) == Some(".dds")` is exactly `path == format!("{P}.dds")`, without building
    // the two constant spellings on every candidate path.
    if path.strip_prefix(ATLAS_PREFIX) == Some(".dds") || path.strip_prefix(ATLAS_ALPHA_PREFIX) == Some(".dds") {
        return Some(0);
    }
    parse_numbered_atlas_page(path, ATLAS_ALPHA_PREFIX).or_else(|| parse_numbered_atlas_page(path, ATLAS_PREFIX))
}

/// Returns whether `path` satisfies the common bounded, lower-case ASCII representation rules.
pub fn common_path_rules(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && path.is_ascii()
        && path.bytes().all(|byte| !byte.is_ascii_uppercase())
}

/// Resolves a distantland-relative path with the backslash path grammar.
pub fn resolve_relative(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('\\')
        .fold(root.to_path_buf(), |resolved, component| resolved.join(component))
}

/// Renders an absolute output path in the distantland-relative inventory grammar.
///
/// Returns `None` when `path` lies outside `root`, which for an authoritative write means the
/// writer escaped the output tree entirely.
pub fn inventory_relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let components: Vec<_> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect();
    (!components.is_empty()).then(|| components.join("\\"))
}

/// Returns whether `file_name` is a known superseded generator-owned artifact.
pub fn is_superseded_output_path(file_name: &str) -> bool {
    matches!(
        file_name,
        "commit_journal.bin"
            | "generation_index_a.bin"
            | "generation_index_b.bin"
            | "atlas_cache.bin"
            | "manifest.json"
            | "static_texture_sizing_report.json"
    ) || is_static_epoch_name(file_name)
        || is_terrain_epoch_name(file_name)
        || is_version_15_static_shard_name(file_name)
}

fn is_version_15_static_shard_name(file_name: &str) -> bool {
    let Some(digits) = file_name.strip_prefix("static_meshes_") else {
        return false;
    };
    digits.len() == 2
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && digits.parse::<usize>().is_ok_and(|shard_id| shard_id < 32)
}

fn is_static_epoch_name(name: &str) -> bool {
    let Some(digits) = name.strip_prefix("static_meshes.e") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_terrain_epoch_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("terrain.e") else {
        return false;
    };
    let Some(digits) = rest.strip_suffix(".bin") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_numbered_atlas_page(path: &str, prefix: &str) -> Option<u32> {
    let digits = path.strip_prefix(prefix)?.strip_prefix('_')?.strip_suffix(".dds")?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let page = digits.parse::<u32>().ok()?;
    (page > 0).then_some(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_paths_are_not_superseded() {
        for path in [
            "version",
            "statics\\static_meshes_00",
            "terrain.bin",
            "statics\\textures\\_mge_xe_atlas.dds",
        ] {
            assert!(!is_superseded_output_path(path), "{path}");
        }
    }

    #[test]
    fn static_shard_paths_use_the_build_count_and_width() {
        for shard_id in 0..crate::output::STATIC_MESH_SHARD_COUNT {
            let file_name = crate::output::static_mesh_shard_file_name(shard_id);
            let relative_path = format!("statics\\{file_name}");
            assert_eq!(parse_static_shard(&relative_path), Some(shard_id));
        }

        let out_of_range = format!(
            "statics\\static_meshes_{:0width$}",
            crate::output::STATIC_MESH_SHARD_COUNT,
            width = crate::output::STATIC_MESH_SHARD_ID_WIDTH,
        );
        assert_eq!(parse_static_shard(&out_of_range), None);
    }

    #[test]
    fn atlas_page_zero_and_numbered() {
        assert_eq!(parse_atlas_page("statics\\textures\\_mge_xe_atlas.dds"), Some(0));
        assert_eq!(parse_atlas_page("statics\\textures\\_mge_xe_atlas_1.dds"), Some(1));
        assert_eq!(parse_atlas_page("statics\\textures\\_mge_xe_atlas_alpha.dds"), Some(0));
        assert_eq!(parse_atlas_page("statics\\textures\\_mge_xe_atlas_01.dds"), None);
    }

    #[test]
    fn resolve_relative_splits_on_backslash() {
        let root = Path::new("distantland");
        assert_eq!(
            resolve_relative(root, "statics\\usage.data"),
            PathBuf::from("distantland").join("statics").join("usage.data")
        );
        assert_eq!(
            resolve_relative(root, "version"),
            PathBuf::from("distantland").join("version")
        );
    }

    #[test]
    fn inventory_relative_uses_the_backslash_grammar() {
        let root = Path::new("out").join("distantland");
        assert_eq!(
            inventory_relative(&root, &root.join("statics").join("usage.data")).as_deref(),
            Some("statics\\usage.data")
        );
        assert_eq!(inventory_relative(&root, &root.join("version")).as_deref(), Some("version"));
        assert_eq!(inventory_relative(&root, &root), None);
        assert_eq!(
            inventory_relative(&root, Path::new("elsewhere").join("version").as_path()),
            None
        );
    }

    #[test]
    fn recognizes_superseded_output_names() {
        assert!(is_superseded_output_path("commit_journal.bin"));
        assert!(is_superseded_output_path("generation_index_a.bin"));
        assert!(is_superseded_output_path("generation_index_b.bin"));
        assert!(is_superseded_output_path("atlas_cache.bin"));
        assert!(is_superseded_output_path("static_meshes.e000001"));
        assert!(is_superseded_output_path("static_meshes.e1"));
        assert!(is_superseded_output_path("terrain.e000001.bin"));
        assert!(is_superseded_output_path("terrain.e9.bin"));
        assert!(is_superseded_output_path("static_meshes_00"));
        assert!(is_superseded_output_path("static_meshes_31"));

        assert!(!is_superseded_output_path("generation_state.bin"));
        assert!(!is_superseded_output_path("static_meshes"));
        assert!(!is_superseded_output_path("static_meshes.e"));
        assert!(!is_superseded_output_path("static_meshes.ex"));
        assert!(!is_superseded_output_path("static_meshes_32"));
        assert!(!is_superseded_output_path("static_meshes_000"));
        assert!(!is_superseded_output_path("static_meshes_127"));
        assert!(!is_superseded_output_path("terrain.bin"));
        assert!(!is_superseded_output_path("terrain.e.bin"));
        assert!(!is_superseded_output_path("terrain.e1"));
        assert!(!is_superseded_output_path("terrain.ex.bin"));
    }
}
