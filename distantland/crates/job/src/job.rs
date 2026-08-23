//! Generation job schema: settings, validation, plugin synchronization, and path rebasing.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use distantland_statics::{
    StaticMeshSimplifierConfig, StaticTextureSizingMode, StaticTextureSizingSettings, TextureDedupeMode,
};
use distantland_usage::UsageFilterOptions;
use distantland_vfs::{Vfs, morrowind_data_dirs, parse_morrowind_game_files_with_data_dirs};

/// One recoverable problem found while loading a generation job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationJobWarning {
    /// TOML path of the ignored or defaulted value.
    pub path: String,
    /// Human-readable explanation of the fallback applied.
    pub message: String,
}

/// MGE-XE-compatible terrain detail presets for terrain mesh generation.
///
/// Each preset selects one production meshopt absolute target error.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerrainDetail {
    /// Matches MGE-XE's highest-detail terrain mesh preset.
    UltraHigh,
    /// Matches MGE-XE's very-high terrain mesh preset.
    VeryHigh,
    /// Matches MGE-XE's default terrain mesh preset.
    #[default]
    High,
    /// Matches MGE-XE's medium-detail terrain mesh preset.
    Medium,
    /// Matches MGE-XE's lowest-detail terrain mesh preset.
    Low,
}

impl TerrainDetail {
    /// Returns the absolute meshopt target error used by this terrain detail preset.
    ///
    /// Larger values allow more simplification and produce lower-detail terrain meshes.
    pub const fn target_error(self) -> f32 {
        match self {
            Self::UltraHigh => 15.0,
            Self::VeryHigh => 64.0,
            Self::High => 128.0,
            Self::Medium => 192.0,
            Self::Low => 256.0,
        }
    }
}

impl fmt::Display for TerrainDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UltraHigh => "ultra_high",
            Self::VeryHigh => "very_high",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        })
    }
}

/// User-configurable generation settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct GenerationSettings {
    /// Minimum effective bounding-sphere radius required for a static to survive filtering.
    pub min_static_size: f32,
    /// Maximum extent allowed for a static-atlas source texture's longer axis, in texels.
    ///
    /// Only an elongation allowance: aspect ratio is preserved, so this binds instead of
    /// `max_static_texture_short_axis` only on art far from square, typically a pre-made atlas.
    /// The ratio between the two is what a stacked atlas is allowed to keep; the default pair
    /// encodes 8:1. Raising this buys no detail, only wider atlas pages.
    pub max_static_texture_long_axis: u32,
    /// Maximum extent allowed for a static-atlas source texture's shorter axis, in texels.
    ///
    /// The cap that governs ordinary near-square art, and equally the sub-texture resolution of a
    /// stacked pre-made atlas. The shorter axis means the same thing in both cases.
    pub max_static_texture_short_axis: u32,
    /// Maximum width or height allowed for static atlas pages, in texels. Capped to a GPU-safe,
    /// power-of-two value.
    pub max_static_atlas_size: u32,
    /// Scalar applied to terrain grass density, clamped to `0.0..=1.0`.
    pub grass_density: f32,
    /// Forces regeneration even when fingerprints or caches would allow reuse.
    pub force_rebuild: bool,
    /// Enables configured global override-source classification behavior.
    pub use_override_list: bool,
    /// Ordered `.ovr`, `.txt`, or `[tools.mge-xe.distantland]` TOML sources.
    ///
    /// Files are opened as provided. Relative paths use the process current directory unless
    /// `resolve_generation_job_paths` is applied after loading.
    pub override_files: Vec<PathBuf>,
    /// Enables automatic discovery of mod-shipped `-metadata.toml` files for active
    /// plugins. Discovered `[tools.mge-xe.distantland]` directives merge after
    /// `override_files`, in load order.
    pub use_plugin_metadata: bool,
    /// Includes activator records in distant-static generation.
    pub include_activators: bool,
    /// Includes miscellaneous object records such as containers and lights.
    /// Doors (DOOR) are always included regardless of this flag, like statics.
    pub include_misc: bool,
    /// Includes interior cells that MGE-XE treats like exteriors.
    pub include_behaves_like_exterior: bool,
    /// Includes interior cells that contain water.
    pub include_interiors_with_water: bool,
    /// Includes unusually large interior cells.
    pub include_large_interiors: bool,
    /// Excludes persistent references to objects that any script in the load order disables via
    /// `SomeId->Disable`.
    ///
    /// Morrowind can only resolve such a call against a reference loaded into its records handler,
    /// which means one serialized into a plugin's persistent block, and it affects exactly one
    /// reference. Temporary placements of the same object are unreachable and stay.
    ///
    /// Turn this off to keep those references; a `dynamic_visibility` / `unique_object` group or an
    /// `include_objects` entry overrides the exclusion for a single object without doing so.
    pub exclude_script_disable_targets: bool,
    /// Generates the terrain runtime package (`terrain.bin` plus companion DDS files).
    ///
    /// Disabling this preserves job-file compatibility, but the resulting output tree
    /// will not satisfy the full terrain contract.
    pub generate_terrain: bool,
    /// Maximum logical tile size used when building `terrain_atlas.dds`.
    pub max_terrain_texture_size: u32,
    /// Maximum dimension (width = height) of the terrain texture atlas, in texels. Power-of-two.
    pub max_terrain_atlas_size: u32,
    /// Maximum allowed width or height of the rectangular terrain control maps.
    pub max_terrain_control_texture_size: u32,
    /// Maximum allowed estimated byte footprint of the rectangular terrain control maps.
    pub max_terrain_control_texture_bytes: u64,
    /// Detail preset used to select the terrain mesh simplifier target error.
    pub terrain_detail: TerrainDetail,
    /// Terrain mesh simplifier weight for smoothed vertex normals.
    /// Must be finite and non-negative.
    pub terrain_mesh_smoothed_normal_weight: f32,
    /// Terrain mesh simplifier weight for vertex colors.
    /// Must be finite and non-negative.
    pub terrain_mesh_color_weight: f32,
    /// Relative simplification error budget for static mesh simplification.
    ///
    /// Controls how much geometric error (as a fraction of the mesh's maximum AABB-axis
    /// extent) is permitted. Larger values produce coarser meshes. Must be finite and
    /// non-negative.
    pub static_mesh_target_error: f32,
    /// Attribute weight applied to vertex normals during static mesh simplification.
    ///
    /// Higher values cause the simplifier to preserve shading normals more aggressively.
    /// Must be finite and non-negative.
    pub static_mesh_normal_weight: f32,
    /// Attribute weight applied to vertex colors during static mesh simplification.
    ///
    /// Higher values cause the simplifier to preserve vertex colors more aggressively.
    /// Must be finite and non-negative.
    pub static_mesh_color_weight: f32,
    /// Maximum merge-stage relative error as a multiple of `static_mesh_target_error`.
    ///
    /// `1.0` disables additional simplification while batching references. Larger values allow
    /// bounded extra simplification without letting full-cell extents impose unbounded relative
    /// targets on small subsets. Must be finite and `>= 1.0`.
    pub static_mesh_merge_error_multiplier: f32,
    /// Multiplier on a door static's effective size. Larger values keep doors in distant
    /// buildings and push them into the same draw-distance bucket as the building, preventing
    /// see-through holes. Rendered geometry is unchanged. Must be finite and `>= 1.0`.
    /// `1.0` = no change.
    pub door_size_multiplier: f32,
    /// Maximum half-diagonal (in game units, horizontal plane) of a BVH node whose nearby
    /// exterior references may be batched into a single merged static. Larger values merge more
    /// references per synthetic static; smaller values keep merges spatially local.
    /// Must be finite and `> 0.0`.
    pub merge_group_radius: f32,
    /// Exact texture deduplication mode applied before atlas/material outputs are built.
    /// Collapsing equivalent textures intentionally changes atlas layout and material IDs.
    pub texture_dedupe_mode: TextureDedupeMode,
    /// Distance (in game units) below applicable water level at which non-grass references are culled.
    ///
    /// Non-grass references whose transformed bounding box maximum Z is below
    /// `water_level - deep_water_static_cull_depth` are discarded during usage pruning.
    /// Larger values retain deeper underwater statics. Must be finite and non-negative.
    pub deep_water_static_cull_depth: f32,
    /// Geometry-informed static texture resolution. Defaults to `Downscale` with a calibrated
    /// `protected_density`; set the mode to `Off` to keep baseline dimensions, `Report` to measure
    /// without downscaling, or `DownscaleOpaque` to spare alpha-tested art.
    pub static_texture_sizing: StaticTextureSizingSettings,
}

impl Default for GenerationSettings {
    fn default() -> Self {
        Self {
            min_static_size: 150.0,
            max_static_texture_long_axis: distantland_statics::DEFAULT_STATIC_TEXTURE_LONG_SIZE,
            max_static_texture_short_axis: distantland_statics::DEFAULT_STATIC_TEXTURE_SHORT_SIZE,
            max_static_atlas_size: distantland_statics::DEFAULT_STATIC_ATLAS_MAX_SIZE,
            grass_density: 1.0,
            force_rebuild: false,
            use_override_list: true,
            override_files: vec![],
            use_plugin_metadata: true,
            include_activators: true,
            include_misc: true,
            include_behaves_like_exterior: true,
            include_interiors_with_water: false,
            include_large_interiors: true,
            // `true` because the blast radius is small and bounded: across an 11-plugin load order
            // covering vanilla, Tribunal, Bloodmoon, Tamriel Rebuilt, Skyrim/Cyrodiil and OAAB, the
            // 378 distant-land eligible targets account for 112 exterior persistent references, at
            // most 2 for any one target.
            exclude_script_disable_targets: true,
            generate_terrain: distantland_terrain::GENERATE_TERRAIN,
            max_terrain_texture_size: distantland_terrain::DEFAULT_TERRAIN_TEXTURE_SIZE,
            max_terrain_atlas_size: distantland_terrain::DEFAULT_TERRAIN_ATLAS_MAX_SIZE,
            max_terrain_control_texture_size: distantland_terrain::MAX_TERRAIN_CONTROL_TEXTURE_SIZE,
            max_terrain_control_texture_bytes: distantland_terrain::MAX_TERRAIN_CONTROL_TEXTURE_BYTES,
            terrain_detail: TerrainDetail::default(),
            terrain_mesh_smoothed_normal_weight: distantland_terrain::DEFAULT_TERRAIN_MESH_SMOOTHED_NORMAL_WEIGHT,
            terrain_mesh_color_weight: distantland_terrain::DEFAULT_TERRAIN_MESH_COLOR_WEIGHT,
            static_mesh_target_error: distantland_statics::DEFAULT_STATIC_MESH_TARGET_ERROR,
            static_mesh_normal_weight: distantland_statics::DEFAULT_STATIC_MESH_NORMAL_WEIGHT,
            static_mesh_color_weight: distantland_statics::DEFAULT_STATIC_MESH_COLOR_WEIGHT,
            static_mesh_merge_error_multiplier: distantland_statics::DEFAULT_STATIC_MESH_MERGE_ERROR_MULTIPLIER,
            // `2.0` keeps distant doors in their building's draw bucket; `1.0` would leave them at
            // true size (legacy behavior).
            door_size_multiplier: 2.0,
            merge_group_radius: distantland_statics::DEFAULT_MERGE_GROUP_RADIUS,
            texture_dedupe_mode: TextureDedupeMode::Exact,
            deep_water_static_cull_depth: 128.0,
            static_texture_sizing: StaticTextureSizingSettings::default(),
        }
    }
}

impl From<&GenerationSettings> for UsageFilterOptions {
    fn from(settings: &GenerationSettings) -> Self {
        Self {
            include_activators: settings.include_activators,
            include_misc: settings.include_misc,
            include_interiors_with_water: settings.include_interiors_with_water,
            include_behaves_like_exterior: settings.include_behaves_like_exterior,
            include_large_interiors: settings.include_large_interiors,
            exclude_script_disable_targets: settings.exclude_script_disable_targets,
            grass_density: settings.grass_density,
        }
    }
}

impl GenerationSettings {
    /// Validates settings that affect generation behavior and output compatibility.
    ///
    /// # Errors
    ///
    /// Returns an error when any numeric setting is out of range or when duplicate
    /// override files would make resolution ambiguous.
    pub fn validate(&self) -> Result<()> {
        if let Some(issue) = self.validation_issues().into_iter().next() {
            bail!("{} {}", issue.path, issue.message);
        }
        Ok(())
    }

    pub(crate) fn validation_issues(&self) -> Vec<GenerationJobWarning> {
        let mut issues = Vec::new();
        let mut check = |valid: bool, path: &str, message: String| {
            if !valid {
                issues.push(GenerationJobWarning {
                    path: path.into(),
                    message,
                });
            }
        };

        check(self.min_static_size >= 0.0, "min_static_size", "must be non-negative".into());
        check(
            distantland_statics::SUPPORTED_STATIC_TEXTURE_LONG_SIZES.contains(&self.max_static_texture_long_axis),
            "max_static_texture_long_axis",
            format!(
                "must be one of {:?}",
                distantland_statics::SUPPORTED_STATIC_TEXTURE_LONG_SIZES
            ),
        );
        check(
            distantland_statics::SUPPORTED_STATIC_TEXTURE_SHORT_SIZES.contains(&self.max_static_texture_short_axis),
            "max_static_texture_short_axis",
            format!(
                "must be one of {:?}",
                distantland_statics::SUPPORTED_STATIC_TEXTURE_SHORT_SIZES
            ),
        );
        check(
            self.max_static_texture_short_axis <= self.max_static_texture_long_axis,
            "max_static_texture_short_axis",
            "must not exceed max_static_texture_long_axis".into(),
        );
        check(
            distantland_statics::SUPPORTED_STATIC_ATLAS_SIZES.contains(&self.max_static_atlas_size),
            "max_static_atlas_size",
            format!("must be one of {:?}", distantland_statics::SUPPORTED_STATIC_ATLAS_SIZES),
        );
        check(self.grass_density.is_finite(), "grass_density", "must be finite".into());
        if self.grass_density.is_finite() {
            check(
                (0.0..=1.0).contains(&self.grass_density),
                "grass_density",
                "must be between 0.0 and 1.0".into(),
            );
        }
        check(
            distantland_terrain::SUPPORTED_TERRAIN_TEXTURE_SIZES.contains(&self.max_terrain_texture_size),
            "max_terrain_texture_size",
            "must be one of 64, 128, 256, or 512".into(),
        );
        check(
            distantland_terrain::SUPPORTED_TERRAIN_ATLAS_SIZES.contains(&self.max_terrain_atlas_size),
            "max_terrain_atlas_size",
            format!("must be one of {:?}", distantland_terrain::SUPPORTED_TERRAIN_ATLAS_SIZES),
        );
        check(
            self.max_terrain_control_texture_size > 0,
            "max_terrain_control_texture_size",
            "must be greater than zero".into(),
        );
        check(
            self.max_terrain_control_texture_bytes > 0,
            "max_terrain_control_texture_bytes",
            "must be greater than zero".into(),
        );
        for (valid, path, message) in [
            (
                self.terrain_mesh_smoothed_normal_weight.is_finite() && self.terrain_mesh_smoothed_normal_weight >= 0.0,
                "terrain_mesh_smoothed_normal_weight",
                "must be finite and non-negative",
            ),
            (
                self.terrain_mesh_color_weight.is_finite() && self.terrain_mesh_color_weight >= 0.0,
                "terrain_mesh_color_weight",
                "must be finite and non-negative",
            ),
            (
                self.static_mesh_target_error.is_finite() && self.static_mesh_target_error >= 0.0,
                "static_mesh_target_error",
                "must be finite and non-negative",
            ),
            (
                self.static_mesh_normal_weight.is_finite() && self.static_mesh_normal_weight >= 0.0,
                "static_mesh_normal_weight",
                "must be finite and non-negative",
            ),
            (
                self.static_mesh_color_weight.is_finite() && self.static_mesh_color_weight >= 0.0,
                "static_mesh_color_weight",
                "must be finite and non-negative",
            ),
            (
                self.static_mesh_merge_error_multiplier.is_finite() && self.static_mesh_merge_error_multiplier >= 1.0,
                "static_mesh_merge_error_multiplier",
                "must be finite and >= 1.0",
            ),
            (
                self.door_size_multiplier.is_finite() && self.door_size_multiplier >= 1.0,
                "door_size_multiplier",
                "must be finite and >= 1.0",
            ),
            (
                self.merge_group_radius.is_finite() && self.merge_group_radius > 0.0,
                "merge_group_radius",
                "must be finite and greater than zero",
            ),
            (
                self.deep_water_static_cull_depth.is_finite() && self.deep_water_static_cull_depth >= 0.0,
                "deep_water_static_cull_depth",
                "must be finite and non-negative",
            ),
        ] {
            check(valid, path, message.into());
        }

        let sizing = &self.static_texture_sizing;
        check(
            sizing.protected_density.is_finite(),
            "static_texture_sizing.protected_density",
            "must be finite".into(),
        );
        if sizing.protected_density.is_finite() {
            check(
                sizing.mode == StaticTextureSizingMode::Off || sizing.protected_density > 0.0,
                "static_texture_sizing.protected_density",
                "must be greater than zero when mode is not off".into(),
            );
        }
        check(
            distantland_statics::SUPPORTED_STATIC_TEXTURE_SHORT_SIZES.contains(&sizing.min_texture_size),
            "static_texture_sizing.min_texture_size",
            format!(
                "must be one of {:?}",
                distantland_statics::SUPPORTED_STATIC_TEXTURE_SHORT_SIZES
            ),
        );
        check(
            sizing.min_texture_size <= self.max_static_texture_short_axis,
            "static_texture_sizing.min_texture_size",
            "must not exceed max_static_texture_short_axis".into(),
        );
        check(
            sizing.max_mip_reduction <= 6,
            "static_texture_sizing.max_mip_reduction",
            "must be at most 6".into(),
        );

        for (index, path) in self.override_files.iter().enumerate() {
            let duplicate = self.override_files[..index]
                .iter()
                .any(|previous| previous.to_string_lossy().eq_ignore_ascii_case(&path.to_string_lossy()));
            check(
                !duplicate,
                &format!("override_files[{index}]"),
                "duplicates an earlier entry".into(),
            );
        }
        issues
    }

    /// Returns the derived mesh simplifier target error used by terrain mesh generation.
    pub const fn terrain_mesh_target_error(&self) -> f32 {
        self.terrain_detail.target_error()
    }

    /// Returns the static mesh simplifier configuration derived from settings.
    pub fn static_mesh_simplifier_config(&self) -> StaticMeshSimplifierConfig {
        StaticMeshSimplifierConfig {
            target_error: self.static_mesh_target_error,
            normal_weight: self.static_mesh_normal_weight,
            color_weight: self.static_mesh_color_weight,
            merge_error_multiplier: self.static_mesh_merge_error_multiplier,
        }
    }
}

/// Full generation request accepted by the library and CLI.
///
/// `Default` is implemented by hand rather than derived: `auto_sync_plugins` defaults to `true`,
/// and the struct's `#[serde(default)]` fills missing keys from that impl, so the derive would
/// silently opt every existing job file *out* of load-order sync.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GenerationJob {
    /// Path to `Morrowind.ini`. Relative paths use the process current directory unless
    /// `resolve_generation_job_paths` is applied after loading. When omitted, the default
    /// install path is discovered.
    pub morrowind_ini: Option<PathBuf>,
    /// Explicit data directory layers. Relative paths use the process current directory unless
    /// `resolve_generation_job_paths` is applied after loading. Later directories override earlier ones.
    pub data_dirs: Option<Vec<PathBuf>>,
    /// Explicit plugin load order. When provided, order is preserved exactly.
    /// Bare plugin filenames are resolved against `data_dirs`; relative plugin paths with a
    /// parent directory use the process current directory unless `resolve_generation_job_paths`
    /// is applied after loading.
    pub plugins: Option<Vec<PathBuf>>,
    /// Generator-only grass/groundcover plugins. These use the same path-resolution rules as
    /// `plugins`, but do not participate in the active game load order or master overrides.
    pub grass_plugins: Option<Vec<PathBuf>>,
    /// Data root. Relative paths use the process current directory unless
    /// `resolve_generation_job_paths` is applied after loading. Outputs are written to
    /// `distantland\` beneath it.
    pub output_root: Option<PathBuf>,
    /// Derive `plugins` from the live `Morrowind.ini` load order instead of trusting the saved
    /// list, so an installed or removed mod is noticed without reopening the configuration GUI.
    ///
    /// Policy, not generation content: it selects what `plugins` becomes, and every consumer
    /// downstream of [`sync_plugins_from_load_order`] sees only the resulting list.
    pub auto_sync_plugins: bool,
    /// Generation settings applied to the resolved load order and output root.
    pub settings: GenerationSettings,
}

pub(crate) const fn default_auto_sync_plugins() -> bool {
    true
}

impl Default for GenerationJob {
    fn default() -> Self {
        Self {
            morrowind_ini: None,
            data_dirs: None,
            plugins: None,
            grass_plugins: None,
            output_root: None,
            auto_sync_plugins: default_auto_sync_plugins(),
            settings: GenerationSettings::default(),
        }
    }
}

/// Replaces `job.plugins` with the live `Morrowind.ini` load order, minus `job.grass_plugins`.
///
/// This is the *only* producer of a synced plugin list. MGEXEgui and `mgeHost64` must agree
/// byte-for-byte on both the entries and their order. `fingerprint_generation_request` hashes the
/// list as written, so two orderings of the same install read as two different requests and the two
/// processes would regenerate over each other on alternating launches.
///
/// `data_dirs` mirrors [`GenerationJob::data_dirs`]: `Some` uses the configured layers verbatim,
/// `None` derives base `Data Files` from `ini_path`. An empty slice is not the same as `None`; it
/// would resolve no plugins at all.
///
/// Order is the parser's, which is Morrowind's own rule (masters first, then by mtime). Grass
/// exclusion is by filename, case-insensitively, and does not reorder what survives it.
///
/// # Errors
///
/// Returns an error if `ini_path` cannot be read or its data directories cannot be derived. There
/// is no fallback: a caller that cannot see the load order must abort rather than generate from a
/// silently empty or stale selection.
pub fn sync_plugins_from_load_order(job: &mut GenerationJob, ini_path: &Path, data_dirs: Option<&[PathBuf]>) -> Result<()> {
    let dirs = match data_dirs {
        Some(dirs) => dirs.to_vec(),
        None => morrowind_data_dirs(ini_path)?,
    };
    let load_order = parse_morrowind_game_files_with_data_dirs(ini_path, &dirs)
        .with_context(|| format!("Failed to read the load order from {}", ini_path.display()))?;

    let grass: Vec<String> = job
        .grass_plugins
        .iter()
        .flatten()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .collect();

    job.plugins = Some(
        load_order
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .is_none_or(|name| !grass.contains(&name.to_string_lossy().to_ascii_lowercase()))
            })
            .collect(),
    );

    Ok(())
}

impl GenerationJob {
    /// Validates a generation request before any filesystem or plugin work begins.
    ///
    /// # Errors
    ///
    /// Returns an error when the request contains invalid settings, empty explicit lists,
    /// or duplicate data-directory/plugin selections.
    pub fn validate_for_generation(&self) -> Result<()> {
        self.settings.validate()?;

        if let Some(data_dirs) = &self.data_dirs {
            ensure!(!data_dirs.is_empty(), "data_dirs must not be empty");
            for (i, dir) in data_dirs.iter().enumerate() {
                let is_dup = data_dirs[..i]
                    .iter()
                    .any(|prev| prev.to_string_lossy().eq_ignore_ascii_case(&dir.to_string_lossy()));
                ensure!(!is_dup, "data_dirs must not contain duplicate entries");
            }
        }

        validate_plugin_list("plugins", self.plugins.as_deref(), false)?;
        validate_plugin_list("grass_plugins", self.grass_plugins.as_deref(), true)?;

        if let (Some(plugins), Some(grass_plugins)) = (&self.plugins, &self.grass_plugins) {
            for grass_plugin in grass_plugins {
                let grass_name = grass_plugin
                    .file_name()
                    .with_context(|| format!("Grass plugin selection entry has no filename: {}", grass_plugin.display()))?;
                ensure!(
                    !plugins
                        .iter()
                        .any(|plugin| plugin.file_name().is_some_and(|name| name.eq_ignore_ascii_case(grass_name))),
                    "plugins and grass_plugins must not contain the same filename: {}",
                    grass_name.to_string_lossy()
                );
            }
        }

        Ok(())
    }

    /// Resolves the output root, defaulting to the active VFS data directory.
    pub fn resolved_output_root(&self, vfs: &Vfs) -> PathBuf {
        self.output_root.clone().unwrap_or_else(|| vfs.data_dir().to_path_buf())
    }
}

fn validate_plugin_list(field: &str, plugins: Option<&[PathBuf]>, allow_empty: bool) -> Result<()> {
    let Some(plugins) = plugins else {
        return Ok(());
    };
    ensure!(allow_empty || !plugins.is_empty(), "{field} must not be empty");

    for (index, plugin) in plugins.iter().enumerate() {
        ensure!(
            plugin
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("esm") || extension.eq_ignore_ascii_case("esp")),
            "{field} entries must have .esm or .esp extension: {}",
            plugin.display()
        );
        let name = plugin
            .file_name()
            .with_context(|| format!("Plugin selection entry has no filename: {}", plugin.display()))?
            .to_string_lossy();
        ensure!(
            !plugins[..index].iter().any(|previous| {
                previous
                    .file_name()
                    .is_some_and(|previous_name| name.eq_ignore_ascii_case(&previous_name.to_string_lossy()))
            }),
            "{field} must not contain duplicate filenames: {name}"
        );
    }

    Ok(())
}

/// Rebase relative paths in a loaded generation job against `base_dir`.
///
/// Bare plugin filenames are left unchanged so VFS plugin-name resolution can still find
/// them in the configured data directories.
pub fn resolve_generation_job_paths(job: &mut GenerationJob, base_dir: &Path) {
    resolve_optional_path(&mut job.morrowind_ini, base_dir);
    resolve_optional_paths(&mut job.data_dirs, base_dir, ResolveBarePath::Yes);
    resolve_optional_paths(&mut job.plugins, base_dir, ResolveBarePath::No);
    resolve_optional_paths(&mut job.grass_plugins, base_dir, ResolveBarePath::No);
    resolve_optional_path(&mut job.output_root, base_dir);
    resolve_paths(&mut job.settings.override_files, base_dir, ResolveBarePath::Yes);
}

/// Controls whether path rebasing should touch bare filenames without a parent component.
#[derive(Clone, Copy)]
enum ResolveBarePath {
    Yes,
    No,
}

fn resolve_optional_path(path: &mut Option<PathBuf>, base_dir: &Path) {
    if let Some(path) = path {
        resolve_path(path, base_dir, ResolveBarePath::Yes);
    }
}

fn resolve_optional_paths(paths: &mut Option<Vec<PathBuf>>, base_dir: &Path, resolve_bare_path: ResolveBarePath) {
    if let Some(paths) = paths {
        resolve_paths(paths, base_dir, resolve_bare_path);
    }
}

fn resolve_paths(paths: &mut [PathBuf], base_dir: &Path, resolve_bare_path: ResolveBarePath) {
    for path in paths {
        resolve_path(path, base_dir, resolve_bare_path);
    }
}

fn resolve_path(path: &mut PathBuf, base_dir: &Path, resolve_bare_path: ResolveBarePath) {
    if path.is_absolute() {
        return;
    }

    let has_parent = path.parent().is_some_and(|parent| !parent.as_os_str().is_empty());
    // Bare plugin names deliberately remain untouched so VFS lookup can still resolve
    // them through the configured data directories.
    if matches!(resolve_bare_path, ResolveBarePath::Yes) || has_parent {
        *path = base_dir.join(&path);
    }
}
