//! Output path calculation and MGE-XE contract validation helpers.

use std::fs;
use std::path::{Path, PathBuf};

use itertools::Itertools;

use crate::mge_xe::distant_terrain::{
    TERRAIN_ATLAS_FILE_NAME, TERRAIN_BLEND_PATTERNS_FILE_NAME, TERRAIN_FILE_NAME, TERRAIN_MATERIAL_FILE_NAME,
    TERRAIN_MATERIAL_FLAGS_FILE_NAME, TERRAIN_PATCH_ALBEDO_FILE_NAME,
};
use crate::mge_xe::terrain_occlusion::TERRAIN_OCCLUSION_FILE_NAME;
use serde::{Deserialize, Serialize};

/// MGE XE distant land format version (must match `d3d8/cpp/mge/mgeversion.h`;
/// `mgeHost64/src/abi/constants.rs` asserts the two are equal).
pub const MGE_DL_VERSION: u8 = 16;
/// Filename prefix shared by every opaque static-atlas page.
pub const OPAQUE_ATLAS_PREFIX: &str = "_mge_xe_atlas";

/// Fixed number of XESTAT05 static-mesh shards owned by the current output format.
pub const STATIC_MESH_SHARD_COUNT: usize = 128;

/// Fixed numeric width used in sharded static-mesh file names.
pub const STATIC_MESH_SHARD_ID_WIDTH: usize = if STATIC_MESH_SHARD_COUNT <= 100 { 2 } else { 3 };

const _: () = assert!(STATIC_MESH_SHARD_COUNT.is_power_of_two());
const _: () = assert!(STATIC_MESH_SHARD_COUNT <= u8::MAX as usize + 1);
/// Returns the filename for one static-mesh shard.
///
/// # Panics
///
/// Panics when `shard_id` is outside the fixed static-mesh shard range.
pub fn static_mesh_shard_file_name(shard_id: usize) -> String {
    assert!(shard_id < STATIC_MESH_SHARD_COUNT, "static shard id is out of range");
    format!("static_meshes_{shard_id:0width$}", width = STATIC_MESH_SHARD_ID_WIDTH)
}

/// Returns the distantland-relative inventory path for one static-mesh shard.
///
/// # Panics
///
/// Panics when `shard_id` is outside the fixed static-mesh shard range.
pub fn static_mesh_shard_relative_path(shard_id: usize) -> String {
    format!("statics\\{}", static_mesh_shard_file_name(shard_id))
}
/// Filename used for the advisory generation report under `distantland\`.
pub const GENERATION_REPORT_NAME: &str = "generation_report.toml";

/// Recorded metadata for one generated file.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GeneratedFileInfo {
    pub relative_path: String,
    pub size_bytes: u64,
}

/// Presence and size information for one required runtime output.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OutputFileStatus {
    pub relative_path: String,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// One validation issue found while checking the MGE-XE output contract.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OutputValidationIssue {
    /// Stable issue code.
    pub code: String,
    /// Output path relative to the selected output root.
    pub relative_path: String,
    /// Human-readable validation message.
    pub message: String,
}

/// Result of validating the generated output tree against MGE-XE expectations.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OutputContractValidation {
    /// Whether all required runtime files were present and valid.
    pub complete: bool,
    /// Status of each required runtime file.
    pub required_files: Vec<OutputFileStatus>,
    /// Atlas pages that were discovered on disk.
    pub atlas_files: Vec<GeneratedFileInfo>,
    /// Validation issues found during the check.
    pub issues: Vec<OutputValidationIssue>,
}

/// Filesystem layout for generated distant-land outputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPaths {
    pub output_root: PathBuf,
    /// `distantland\` directory.
    pub distantland_dir: PathBuf,
    /// `distantland\statics\` directory.
    pub statics_dir: PathBuf,
    /// `distantland\statics\textures\` directory that holds generated atlas pages.
    pub atlas_texture_dir: PathBuf,
    /// `distantland\version`.
    pub version_path: PathBuf,
    /// `distantland\terrain.bin` payload when terrain is enabled.
    pub terrain_path: PathBuf,
    /// `distantland\terrain_occlusion.bin`.
    pub terrain_occlusion_path: PathBuf,
    /// `distantland\terrain_atlas.dds`.
    pub terrain_atlas_path: PathBuf,
    /// `distantland\terrain_material.dds`.
    pub terrain_material_path: PathBuf,
    /// `distantland\terrain_material_flags.dds`.
    pub terrain_material_flags_path: PathBuf,
    /// `distantland\terrain_patch_albedo.dds`.
    pub terrain_patch_albedo_path: PathBuf,
    /// `distantland\terrain_blend_patterns.dds`.
    pub terrain_blend_patterns_path: PathBuf,
    /// `distantland\statics\usage.data`.
    pub usage_data_path: PathBuf,
    /// Fixed static-mesh shard payload set for this build's shard count.
    pub static_mesh_shard_paths: [PathBuf; STATIC_MESH_SHARD_COUNT],
    /// `distantland\generation_report.toml` (optional observability; not authority).
    pub generation_report_path: PathBuf,
    /// Authoritative complete-or-absent state at `distantland\generation_state.bin`.
    pub generation_state_path: PathBuf,
    /// Exclusive writer lock at `distantland\.writer.lock`.
    pub writer_lock_path: PathBuf,
}

/// One file that should exist in the generated output tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedOutputFile {
    /// Stable path relative to the output root, used in reports.
    pub relative_path: String,
    /// Absolute path on disk.
    pub path: PathBuf,
}

impl OutputPaths {
    /// Builds the output layout rooted at `output_root`.
    pub fn new(output_root: impl Into<PathBuf>) -> Self {
        let output_root = output_root.into();
        let distantland_dir = output_root.join("distantland");
        let statics_dir = distantland_dir.join("statics");
        let atlas_texture_dir = statics_dir.join("textures");
        Self {
            output_root,
            version_path: distantland_dir.join("version"),
            terrain_path: distantland_dir.join(TERRAIN_FILE_NAME),
            terrain_occlusion_path: distantland_dir.join(TERRAIN_OCCLUSION_FILE_NAME),
            terrain_atlas_path: distantland_dir.join(TERRAIN_ATLAS_FILE_NAME),
            terrain_material_path: distantland_dir.join(TERRAIN_MATERIAL_FILE_NAME),
            terrain_material_flags_path: distantland_dir.join(TERRAIN_MATERIAL_FLAGS_FILE_NAME),
            terrain_patch_albedo_path: distantland_dir.join(TERRAIN_PATCH_ALBEDO_FILE_NAME),
            terrain_blend_patterns_path: distantland_dir.join(TERRAIN_BLEND_PATTERNS_FILE_NAME),
            usage_data_path: statics_dir.join("usage.data"),
            static_mesh_shard_paths: std::array::from_fn(|shard_id| statics_dir.join(static_mesh_shard_file_name(shard_id))),
            generation_report_path: distantland_dir.join(GENERATION_REPORT_NAME),
            generation_state_path: distantland_dir.join("generation_state.bin"),
            writer_lock_path: distantland_dir.join(".writer.lock"),
            distantland_dir,
            statics_dir,
            atlas_texture_dir,
        }
    }

    /// Creates the parent directories required for writing generated outputs.
    ///
    /// # Errors
    ///
    /// Returns an error if the required directories cannot be created.
    pub fn ensure_parent_dirs(&self) -> anyhow::Result<()> {
        fs::create_dir_all(&self.statics_dir)?;
        fs::create_dir_all(&self.atlas_texture_dir)?;
        Ok(())
    }

    /// Enumerates the always-required non-terrain runtime files for advisory contract checks.
    ///
    /// Authoritative validation is inventory-driven via `generation_state.bin` and is
    /// terrain-conditional. This list remains the advisory surface used by observability products.
    pub(super) fn generated_runtime_outputs(&self) -> Vec<ExpectedOutputFile> {
        let mut outputs = vec![
            ExpectedOutputFile {
                relative_path: "distantland\\version".to_string(),
                path: self.version_path.clone(),
            },
            ExpectedOutputFile {
                relative_path: "distantland\\generation_state.bin".to_string(),
                path: self.generation_state_path.clone(),
            },
            ExpectedOutputFile {
                relative_path: "distantland\\statics\\usage.data".to_string(),
                path: self.usage_data_path.clone(),
            },
        ];
        outputs.extend(self.static_mesh_shard_paths.iter().map(|path| ExpectedOutputFile {
            relative_path: self.runtime_relative_path(path),
            path: path.clone(),
        }));
        outputs
    }

    fn runtime_relative_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.output_root)
            .unwrap_or(path)
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .join("\\")
    }

    /// Validates that the generated output tree contains every runtime file MGE-XE expects.
    ///
    /// Also verifies the `distantland\version` byte matches `MGE_DL_VERSION` when it is
    /// readable. Version mismatches are treated as issues rather than hard errors so the
    /// caller still receives a complete `OutputContractValidation` describing all gaps.
    pub fn validate_mge_xe_contract(&self) -> OutputContractValidation {
        let mut issues = Vec::new();
        let required_files = self
            .generated_runtime_outputs()
            .into_iter()
            .map(|file| {
                let metadata = fs::metadata(&file.path);
                match metadata {
                    Ok(metadata) => OutputFileStatus {
                        relative_path: file.relative_path,
                        exists: true,
                        size_bytes: Some(metadata.len()),
                    },
                    Err(_) => {
                        issues.push(OutputValidationIssue {
                            code: "missing_required_file".to_string(),
                            relative_path: file.relative_path.clone(),
                            message: format!("Required MGE-XE output file is missing: {}", file.relative_path),
                        });
                        OutputFileStatus {
                            relative_path: file.relative_path,
                            exists: false,
                            size_bytes: None,
                        }
                    }
                }
            })
            .collect_vec();

        match fs::read(&self.version_path) {
            Ok(bytes) if bytes.as_slice() == &[MGE_DL_VERSION][..] => {}
            Ok(_) => issues.push(OutputValidationIssue {
                code: "invalid_version_file".to_string(),
                relative_path: r"distantland\version".to_string(),
                message: format!("version must contain the single byte {MGE_DL_VERSION}"),
            }),
            Err(_) => {}
        }

        let atlas_files = self
            .generated_atlas_outputs()
            .into_iter()
            .filter_map(|file| {
                fs::metadata(&file.path).ok().map(|metadata| GeneratedFileInfo {
                    relative_path: file.relative_path,
                    size_bytes: metadata.len(),
                })
            })
            .collect_vec();

        OutputContractValidation {
            complete: issues.is_empty(),
            required_files,
            atlas_files,
            issues,
        }
    }

    /// Enumerates the atlas page files implied by the current atlas directory contents.
    ///
    /// Returns files whose names start with the atlas filename prefix and end with `".dds"`, sorted by
    /// relative path so manifests and status checks are deterministic across platforms.
    pub(super) fn generated_atlas_outputs(&self) -> Vec<ExpectedOutputFile> {
        let Ok(entries) = fs::read_dir(&self.atlas_texture_dir) else {
            return vec![];
        };

        entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with(OPAQUE_ATLAS_PREFIX) || !name.ends_with(".dds") {
                    return None;
                }

                Some(ExpectedOutputFile {
                    relative_path: format!("distantland\\statics\\textures\\{name}"),
                    path: entry.path(),
                })
            })
            .sorted_by(|left, right| left.relative_path.cmp(&right.relative_path))
            .collect_vec()
    }
}
