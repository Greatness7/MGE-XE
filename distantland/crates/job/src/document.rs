//! Tolerant TOML loading, serialization, and comment annotation of generation job documents.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::job::{GenerationJob, GenerationJobWarning, GenerationSettings, default_auto_sync_plugins};
use crate::tolerant::{
    PathSegment, deserialize, deserialize_ignored, display_path, dotted_segments, error_segments, remove_bad_value,
};

/// Current embedded generation job schema version.
pub const GENERATION_JOB_FILE_VERSION: u32 = 3;

/// Root table owned by the distant-land generator in `mgeXE.toml`.
pub const GENERATION_JOB_NAMESPACE: &str = "generation";

const fn current_generation_job_version() -> u32 {
    GENERATION_JOB_FILE_VERSION
}

/// A generation job loaded from parseable TOML, plus recoverable schema warnings.
#[derive(Clone, Debug)]
pub struct GenerationJobLoad {
    /// Effective job after invalid values have fallen back to defaults.
    pub job: GenerationJob,
    /// Problems encountered while loading the job.
    pub warnings: Vec<GenerationJobWarning>,
    /// Whether the source document contained the owned `[generation]` namespace.
    pub namespace_present: bool,
}

#[derive(Debug, Default, Deserialize)]
struct MgeConfigFile {
    #[serde(default)]
    generation: Option<GenerationJobFile>,
}

#[derive(Serialize)]
struct MgeConfigJob<'a> {
    generation: GenerationJobFileRef<'a>,
}

/// Borrowing serialization mirror of [`GenerationJobFile`].
///
/// Job fields are mirrored so additions to [`crate::job::GenerationJob`] remain compile-time visible here.
#[derive(Serialize)]
struct GenerationJobFileRef<'a> {
    version: u32,
    morrowind_ini: &'a Option<PathBuf>,
    data_dirs: &'a Option<Vec<PathBuf>>,
    plugins: &'a Option<Vec<PathBuf>>,
    grass_plugins: &'a Option<Vec<PathBuf>>,
    output_root: &'a Option<PathBuf>,
    auto_sync_plugins: bool,
    settings: &'a GenerationSettings,
}

impl<'a> GenerationJobFileRef<'a> {
    /// Borrows `job`'s fields at the job table's top level.
    ///
    /// The exhaustive destructuring is intentional: adding a field to [`crate::job::GenerationJob`] must fail
    /// compilation until the field is mirrored here.
    fn new(version: u32, job: &'a GenerationJob) -> Self {
        let GenerationJob {
            morrowind_ini,
            data_dirs,
            plugins,
            grass_plugins,
            output_root,
            auto_sync_plugins,
            settings,
        } = job;
        Self {
            version,
            morrowind_ini,
            data_dirs,
            plugins,
            grass_plugins,
            output_root,
            auto_sync_plugins: *auto_sync_plugins,
            settings,
        }
    }
}

/// Versioned wrapper used when loading or writing job files.
///
/// The job's fields live directly in this table rather than under a nested `job` key. See
/// `GenerationJobFileRef` for why the fields are mirrored instead of flattened.
#[derive(Clone, Debug, Deserialize)]
pub struct GenerationJobFile {
    /// Job file schema version. Mismatches warn and normalize on the next save.
    #[serde(default = "current_generation_job_version")]
    pub version: u32,
    /// See [`crate::job::GenerationJob::morrowind_ini`].
    #[serde(default)]
    pub morrowind_ini: Option<PathBuf>,
    /// See [`crate::job::GenerationJob::data_dirs`].
    #[serde(default)]
    pub data_dirs: Option<Vec<PathBuf>>,
    /// See [`crate::job::GenerationJob::plugins`].
    #[serde(default)]
    pub plugins: Option<Vec<PathBuf>>,
    /// See [`crate::job::GenerationJob::grass_plugins`].
    #[serde(default)]
    pub grass_plugins: Option<Vec<PathBuf>>,
    /// See [`crate::job::GenerationJob::output_root`].
    #[serde(default)]
    pub output_root: Option<PathBuf>,
    /// See [`crate::job::GenerationJob::auto_sync_plugins`].
    ///
    /// Field-level `default` rather than the struct's, because this one is not `false`: a v3 file
    /// written before the key existed must deserialize as opted in, matching a fresh job.
    #[serde(default = "default_auto_sync_plugins")]
    pub auto_sync_plugins: bool,
    /// See [`crate::job::GenerationJob::settings`].
    #[serde(default)]
    pub settings: GenerationSettings,
}

impl GenerationJobFile {
    /// Returns the generation request described by this file, discarding the version.
    pub fn into_job(self) -> GenerationJob {
        let Self {
            version: _,
            morrowind_ini,
            data_dirs,
            plugins,
            grass_plugins,
            output_root,
            auto_sync_plugins,
            settings,
        } = self;
        GenerationJob {
            morrowind_ini,
            data_dirs,
            plugins,
            grass_plugins,
            output_root,
            auto_sync_plugins,
            settings,
        }
    }
}

/// Loads a versioned generation job file without rebasing relative paths.
///
/// Relative paths in the returned job retain Rust/OS process-current-directory semantics.
/// Call `resolve_generation_job_paths` when a host wants job-file-directory semantics.
///
/// # Errors
///
/// Returns an error if the file cannot be read or is not valid TOML. Recoverable
/// schema problems are logged and use local defaults.
pub fn load_generation_job_file(path: &Path) -> Result<GenerationJob> {
    let loaded = load_generation_job_file_with_warnings(path)?;
    for warning in &loaded.warnings {
        tracing::warn!(path = %warning.path, "generation job setting ignored: {}", warning.message);
    }
    Ok(loaded.job)
}

/// Loads a generation job while retaining recoverable schema diagnostics.
///
/// TOML syntax errors remain fatal. Unknown keys and invalid values inside the
/// owned namespace are ignored locally so the rest of the job remains usable.
///
/// # Errors
///
/// Returns an error if the file cannot be read or is not valid TOML.
pub fn load_generation_job_file_with_warnings(path: &Path) -> Result<GenerationJobLoad> {
    let source =
        fs::read_to_string(path).with_context(|| format!("Failed to read generation job file {}", path.display()))?;
    let mut document: toml_edit::DocumentMut = source
        .parse()
        .with_context(|| format!("Failed to parse generation job file {}", path.display()))?;
    let declared_version = document
        .get(GENERATION_JOB_NAMESPACE)
        .and_then(toml_edit::Item::as_table)
        .and_then(|table| table.get("version"))
        .and_then(toml_edit::Item::as_integer)
        .and_then(|version| u32::try_from(version).ok());
    let namespace_present = document.get(GENERATION_JOB_NAMESPACE).is_some();
    let mut warnings = Vec::new();
    let mut reset_namespace = false;

    let job_file = loop {
        match deserialize::<MgeConfigFile>(&document) {
            Ok(config) => {
                let Some(job_file) = config.generation else {
                    return Ok(GenerationJobLoad {
                        job: GenerationJob::default(),
                        warnings,
                        namespace_present,
                    });
                };
                let issues = job_file.settings.validation_issues();
                if issues.is_empty() {
                    break job_file;
                }
                let issue = issues.into_iter().next().expect("non-empty issues checked above");
                let path = format!("{GENERATION_JOB_NAMESPACE}.settings.{}", issue.path);
                let segments = dotted_segments(&path);
                if remove_bad_value(&mut document, &segments) {
                    warnings.push(GenerationJobWarning {
                        path,
                        message: format!("{}; using the default", issue.message),
                    });
                    continue;
                }
            }
            Err(error) => {
                let segments = error_segments(error.path());
                if !segments.is_empty() && remove_bad_value(&mut document, &segments) {
                    warnings.push(GenerationJobWarning {
                        path: display_path(&segments),
                        message: format!("{}; using the default", error.inner()),
                    });
                    continue;
                }
            }
        }

        if reset_namespace {
            return Ok(GenerationJobLoad {
                job: GenerationJob::default(),
                warnings,
                namespace_present,
            });
        }
        reset_namespace = true;
        document.remove(GENERATION_JOB_NAMESPACE);
        warnings.push(GenerationJobWarning {
            path: GENERATION_JOB_NAMESPACE.into(),
            message: "generation settings could not be decoded; using defaults".into(),
        });
    };

    let (_, ignored) =
        deserialize_ignored::<MgeConfigFile>(&document).context("Failed to inspect ignored generation job keys")?;
    for segments in ignored {
        if !matches!(segments.first(), Some(PathSegment::Key(root)) if root == GENERATION_JOB_NAMESPACE)
            || segments.len() == 1
        {
            continue;
        }
        warnings.push(GenerationJobWarning {
            path: display_path(&segments),
            message: "unknown key was ignored and will be removed when saved".into(),
        });
    }
    if declared_version.is_some_and(|version| version != GENERATION_JOB_FILE_VERSION) {
        warnings.push(GenerationJobWarning {
            path: format!("{GENERATION_JOB_NAMESPACE}.version"),
            message: format!(
                "version {} differs from this build's version {GENERATION_JOB_FILE_VERSION}; saving will write {GENERATION_JOB_FILE_VERSION}",
                declared_version.expect("checked above")
            ),
        });
    }

    Ok(GenerationJobLoad {
        job: job_file.into_job(),
        warnings,
        namespace_present,
    })
}

/// Serializes a generation job as the owned namespace of a full `mgeXE.toml` document.
///
/// The document is annotated with a one-line comment above each key so the generator's table
/// matches the house style of the rest of `mgeXE.toml`. The host splices this table into the
/// live document verbatim, so the formatting chosen here is what reaches disk.
///
/// # Errors
///
/// Returns an error if TOML serialization fails.
pub fn serialize_generation_job_document(job: &GenerationJob) -> Result<String> {
    let rendered = toml::to_string_pretty(&MgeConfigJob {
        generation: GenerationJobFileRef::new(GENERATION_JOB_FILE_VERSION, job),
    })?;
    let mut document: toml_edit::DocumentMut = rendered.parse().context("Serialized generation job is not valid TOML")?;
    annotate_generation_job_document(&mut document);
    Ok(document.to_string())
}

/// One-line explanations emitted above each key of the `[generation]` table.
const GENERATION_KEY_COMMENTS: &[(&str, &str)] = &[
    ("version", "Job schema version. A mismatch is rejected, never migrated."),
    (
        "morrowind_ini",
        "Path to Morrowind.ini. Omit to discover the default install.",
    ),
    (
        "data_dirs",
        "Explicit data directory layers; later entries override earlier ones.",
    ),
    ("plugins", "Explicit plugin load order, preserved exactly as listed."),
    ("grass_plugins", "Generator-only grass plugins, outside the game load order."),
    ("output_root", "Data root. Outputs are written to distantland\\ beneath it."),
    (
        "auto_sync_plugins",
        "Re-read plugins from the live Morrowind.ini load order on every run.",
    ),
];

/// One-line explanations emitted above each key of `[generation.settings]`.
const SETTINGS_KEY_COMMENTS: &[(&str, &str)] = &[
    (
        "min_static_size",
        "Smallest bounding-sphere radius a static may have and still be kept.",
    ),
    (
        "max_static_texture_long_axis",
        "Maximum longer edge in texels for one source texture entering the atlas. Its ratio to the short axis is the stacking a pre-made atlas may keep; 8:1 by default. Raising it widens atlas pages without adding detail.",
    ),
    (
        "max_static_texture_short_axis",
        "Maximum shorter edge in texels for one source texture entering the atlas. Governs ordinary art and, equally, the sub-texture resolution of a stacked pre-made atlas.",
    ),
    (
        "max_static_atlas_size",
        "Maximum static atlas page edge in texels; capped to a GPU-safe power of two.",
    ),
    (
        "grass_density",
        "Scalar applied to terrain grass density, clamped to 0.0..=1.0.",
    ),
    (
        "force_rebuild",
        "Regenerate even when caches and fingerprints would allow reuse.",
    ),
    ("use_override_list", "Apply configured global override sources."),
    ("override_files", "Override .ovr, .txt, or TOML files applied in order."),
    (
        "use_plugin_metadata",
        "Discover mod-shipped -metadata.toml directives for active plugins.",
    ),
    ("include_activators", "Include activator records in distant statics."),
    (
        "include_misc",
        "Include containers, lights, and similar. Doors are always included.",
    ),
    (
        "include_behaves_like_exterior",
        "Include interiors MGE-XE treats as exteriors.",
    ),
    ("include_interiors_with_water", "Include interior cells that contain water."),
    (
        "include_large_interiors",
        "Include interior cells large enough to be worth drawing far.",
    ),
    (
        "exclude_script_disable_targets",
        "Drop persistent references that a script disables via \"SomeId->Disable\".",
    ),
    (
        "generate_terrain",
        "Generate the world-space terrain package alongside statics.",
    ),
    (
        "max_terrain_texture_size",
        "Maximum edge of a single terrain land texture, in texels.",
    ),
    ("max_terrain_atlas_size", "Maximum terrain atlas edge in texels."),
    (
        "max_terrain_control_texture_size",
        "Maximum terrain control texture edge in texels.",
    ),
    (
        "max_terrain_control_texture_bytes",
        "Byte ceiling for the terrain control texture.",
    ),
    (
        "terrain_detail",
        "Terrain mesh preset: ultra_high, very_high, high, medium, or low.",
    ),
    (
        "terrain_mesh_smoothed_normal_weight",
        "Weight of smoothed normals when simplifying terrain.",
    ),
    (
        "terrain_mesh_color_weight",
        "Weight of vertex color when simplifying terrain.",
    ),
    ("static_mesh_target_error", "Relative meshopt target error for static meshes."),
    (
        "static_mesh_normal_weight",
        "Weight of vertex normals when simplifying statics.",
    ),
    ("static_mesh_color_weight", "Weight of vertex color when simplifying statics."),
    (
        "static_mesh_merge_error_multiplier",
        "Error budget multiplier applied to merged static groups.",
    ),
    ("door_size_multiplier", "Scales the effective size of doors during filtering."),
    (
        "merge_group_radius",
        "World-unit radius within which nearby statics merge into one group.",
    ),
    ("texture_dedupe_mode", "Texture deduplication: off or exact."),
    (
        "deep_water_static_cull_depth",
        "Distance in game units below water level at which non-grass statics are culled.",
    ),
    ("static_texture_sizing", "Static texture downscaling policy."),
];

/// One-line explanations emitted above each key of the `static_texture_sizing` table.
const STATIC_TEXTURE_SIZING_KEY_COMMENTS: &[(&str, &str)] = &[
    ("mode", "Sizing policy: off, report, downscale_opaque, or downscale."),
    (
        "protected_density",
        "Texel-density floor below which a texture is left alone.",
    ),
    ("min_texture_size", "Smallest edge a texture may be reduced to, in texels."),
    (
        "max_mip_reduction",
        "Maximum number of mip levels a texture may be reduced by.",
    ),
];

/// Writes the per-key comments into an already-serialized job document.
fn annotate_generation_job_document(document: &mut toml_edit::DocumentMut) {
    let Some(generation) = document[GENERATION_JOB_NAMESPACE].as_table_mut() else {
        return;
    };
    annotate_table(generation, GENERATION_KEY_COMMENTS);

    let Some(settings) = generation["settings"].as_table_mut() else {
        return;
    };
    settings
        .decor_mut()
        .set_prefix("\n# Generation settings applied to the resolved load order.\n");
    annotate_table(settings, SETTINGS_KEY_COMMENTS);

    // Trails the other settings as its own sub-table because it is the last field of
    // `GenerationSettings`; an inline table here would carry no line comments.
    if let Some(sizing) = settings["static_texture_sizing"].as_table_mut() {
        sizing.decor_mut().set_prefix("\n# Static texture downscaling policy.\n");
        annotate_table(sizing, STATIC_TEXTURE_SIZING_KEY_COMMENTS);
    }
}

/// Prefixes each key of `table` with its `# ` comment, preserving existing separation.
///
/// Keys absent from `comments` are left undecorated; `every_emitted_key_has_a_comment` fails if
/// the generator emits such a key.
fn annotate_table(table: &mut toml_edit::Table, comments: &[(&str, &str)]) {
    for (mut key, item) in table.iter_mut() {
        // A sub-table renders from its own header decor; decorating the key here would splice
        // the comment into the middle of the `[a.b]` header instead.
        if item.is_table() || item.is_array_of_tables() {
            continue;
        }
        let Some((_, comment)) = comments.iter().find(|(name, _)| *name == key.get()) else {
            continue;
        };
        let decor = key.leaf_decor_mut();
        let existing = decor.prefix().and_then(toml_edit::RawString::as_str).unwrap_or("").to_owned();
        decor.set_prefix(format!("{existing}# {comment}\n"));
    }
}
