//! Orchestration for the full distant-land generation pipeline.
//!
//! `generate_with_output_root` is a thin sequencer: the early stages (VFS, overrides,
//! plugins plus projection capture, terrain layout, statics, and atlas) run inline, while the two heavyweight
//! cache-gated stages live in `statics_stage` and `terrain_stage`.

mod cache;
pub(crate) mod identity;
mod metrics;
mod output;
mod progress;
mod projection;
mod startup;
mod statics_stage;
pub(crate) mod storage;
mod terrain_stage;
mod unit_fingerprint;
mod unit_settings;
pub(crate) mod units;

use distantland_foundation::commit;
pub(crate) use distantland_foundation::record_key;
pub(crate) use distantland_foundation::state_db;

use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tracing::{Span, field, info, info_span};

use crate::statics::atlas::sizing::{
    SizingPlan, TextureAxisCaps, analyze_static_texture_usage, collect_static_texture_source_info,
    fingerprints_from_source_info, merge_dedupe_alias_requirements, plan_static_texture_resolutions,
};
use crate::statics::atlas::{AtlasPageWrite, AtlasPublishPlan};
use crate::statics::metadata::{
    apply_override_source_with_identity, apply_plugin_metadata_with_identity, discover_plugin_metadata,
};
use crate::{
    AtlasManager, AtlasTextureSet, IndexSet, OverridesBuilder, StaticTextureSizingMode, TraceSummary, UsageFilterOptions,
    UsageInfo, Vfs, build_terrain_atlas_layout, vfs::VfsLoadOptions,
};

#[doc(hidden)]
pub use commit::PublicationWrites;
pub(crate) use commit::WrittenFile;

pub use distantland_job as job;
pub use job::*;
pub use metrics::*;
pub use output::*;
pub use progress::*;
pub use startup::*;
pub use state_db::{
    GenerationStateStatus, UnitDomainReport, UnitDomainReports, UnitKindReport, UnitKindReports, UnitKindStatus,
    UnitStateReport, UnitsReport,
};
pub use units::TerrainMeshWorkKey;

pub(crate) use output::{build_generation_report_data, write_generation_report_data};

/// Runs the full distant-land generation pipeline for `job`.
///
/// Output is committed in place under `job.output_root`.
///
/// # Errors
///
/// Returns an error if validation fails or if any pipeline stage cannot complete.
pub fn generate(job: &GenerationJob, reporter: &mut dyn ProgressReporter) -> Result<GenerationReport> {
    generate_with_output_root(job, None, reporter)
}

/// Routes meshopt's internal C++ scratch allocations through mimalloc.
///
/// meshopt defaults to `operator new`/`operator delete` on the CRT heap; the Rust-side
/// mimalloc `#[global_allocator]` does not cover those. Idempotent and thread-safe.
pub fn init_meshopt_allocator() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| unsafe {
        meshopt::ffi::meshopt_setAllocator(Some(libmimalloc_sys::mi_malloc), Some(libmimalloc_sys::mi_free));
    });
}

/// Read-only per-run inputs shared by the extracted pipeline stages.
#[derive(Clone, Copy)]
struct StageContext<'a> {
    job: &'a GenerationJob,
    vfs: &'a Vfs,
    /// Root that runtime output files are written to.
    output_paths: &'a OutputPaths,
    /// Whether this commit has no Routine-valid base to carry from.
    migration: bool,
}

impl StageContext<'_> {
    /// Whether every domain and unit is dirty by fiat.
    ///
    /// The `force_rebuild` setting is global: there is one switch, and it dirties statics, the
    /// atlas, terrain, occlusion, and every unit alike. Invalid/absent state is migration instead:
    /// nothing is carried forward.
    fn force_rebuild(&self) -> bool {
        self.job.settings.force_rebuild
    }
}

/// Private control after the front half / unit-diff boundary.
enum RunMode {
    Publish,
    Inspect,
    Ensure { return_if_current: bool },
}

enum GenerationAuthority {
    /// Ordinary publish path used by `generate`.
    Publish(Option<storage::authority::WriterSession>),
    /// Read-only currency check used by `check_output_status`.
    Inspect {
        output_paths: OutputPaths,
        base_state: storage::state::CommittedState,
    },
    /// Locked ensure path: one front half that may early-return when current or continue to publish.
    Ensure {
        writer: storage::authority::WriterSession,
        /// When true and the unit diff is clean, finish with `AlreadyCurrent` (no plan/publish).
        return_if_current: bool,
    },
}

/// Fails when a root the caller already holds authority over disagrees with the resolved one.
///
/// `held_kind` names how the caller obtained its root: `"locked"` for a writer session, `"pinned"`
/// for an inspection. A mismatch means that authority does not cover the tree this run resolved,
/// so the run must not proceed against it.
fn ensure_output_root_matches(held_kind: &str, held: &Path, resolved: &Path) -> Result<()> {
    if held != resolved {
        bail!(
            "{held_kind} output root {} does not match resolved job output root {}",
            held.display(),
            resolved.display()
        );
    }
    Ok(())
}

/// Crate-private successful publish result after under-session Routine validation.
#[derive(Debug)]
pub(crate) struct ValidatedGeneration {
    /// Public generation report (includes the advisory contract probe).
    pub(crate) report: GenerationReport,
}

#[derive(Debug)]
pub(crate) enum GenerationRunResult {
    /// Ensure mode found a clean diff and released without planning or writing.
    AlreadyCurrent,
    /// Independent inspect mode finished at the diff boundary.
    Inspected(GenerationInputStatus),
    /// Publish or dirty ensure completed and Routine-validated under the exclusive session.
    Published(ValidatedGeneration),
}

#[cfg(test)]
thread_local! {
    static FRONT_HALF_INVOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Returns and clears the number of front-half entries observed on this thread.
#[cfg(test)]
pub(crate) fn take_front_half_invocations() -> usize {
    FRONT_HALF_INVOCATIONS.with(|count| count.replace(0))
}

/// Currency result retained from an inspection run.
#[derive(Clone, Debug)]
pub(crate) struct GenerationInputStatus {
    /// Whether the structured unit inputs are current.
    pub(crate) current: bool,
    /// Human-readable cause when the inputs are stale.
    pub(crate) rebuild_cause: Option<String>,
}

/// Single pre-write decision object consumed by the publish phase.
struct CommitPlan<'a> {
    atlas: AtlasPublishPlan,
    statics: statics_stage::StaticsStagePlan,
    terrain: terrain_stage::TerrainStagePlan<'a>,
    units: UnitsReport,
    version_unchanged: bool,
    no_op: bool,
    preflight_bytes: u64,
    writes: PublicationWrites,
}

impl<'a> CommitPlan<'a> {
    fn new(
        atlas: AtlasPublishPlan,
        statics: statics_stage::StaticsStagePlan,
        terrain: terrain_stage::TerrainStagePlan<'a>,
        units: UnitsReport,
        current_state: &state_db::GenerationState,
        base_state: Option<&storage::state::CommittedState>,
        ctx: &StageContext<'_>,
    ) -> Result<Self> {
        let output = ctx.output_paths;
        let version_carryable = base_state.is_some_and(|state| {
            state
                .artifacts
                .iter()
                .any(|entry| entry.kind == storage::state::ArtifactKind::Version)
        });
        let version_unchanged =
            version_carryable && fs::read(&output.version_path).is_ok_and(|existing| existing == [MGE_DL_VERSION]);
        let previous_static_size = base_state.map_or_else(
            || {
                output
                    .static_mesh_shard_paths
                    .iter()
                    .map(|path| file_size_or_zero(path))
                    .sum()
            },
            |state| {
                state
                    .artifacts
                    .iter()
                    .filter(|entry| entry.kind == storage::state::ArtifactKind::StaticShard)
                    .map(|entry| entry.byte_length)
                    .sum()
            },
        );
        let previous_terrain_size = base_state
            .and_then(|state| state.artifact_by_kind(storage::state::ArtifactKind::TerrainPayload))
            .map_or_else(|| file_size_or_zero(&output.terrain_path), |entry| entry.byte_length);
        let previous_terrain_inventory_size = base_state.map_or(0, |state| {
            state
                .artifacts
                .iter()
                .filter(|entry| entry.kind == storage::state::ArtifactKind::TerrainDds)
                .map(|entry| entry.byte_length)
                .sum()
        });
        let preflight_bytes = atlas
            .metrics()
            .publication_bytes_estimate
            .saturating_add(statics.publication_bytes_estimate(previous_static_size))
            .saturating_add(terrain.publication_bytes_estimate(previous_terrain_size, previous_terrain_inventory_size))
            .saturating_add((!version_unchanged) as u64);
        let no_op = !ctx.migration
            && version_unchanged
            && atlas.cache_hit()
            && statics.is_hit()
            && terrain.is_hit()
            && units_are_clean(&units)
            && base_state.is_some_and(|state| state.state().is_some_and(|previous| previous == current_state));
        Ok(Self {
            atlas,
            statics,
            terrain,
            units,
            version_unchanged,
            no_op,
            preflight_bytes,
            writes: PublicationWrites::new(),
        })
    }
}

/// Internal generation entry point that can consume a writer session already acquired by the
/// startup status path, avoiding an unlock/relock race between inspection and generation.
///
/// Successful return means the exclusive session performed final on-disk Routine validation and
/// the in-memory MGE-XE contract probe is complete. The writer is released before this returns.
///
/// # Errors
///
/// Returns an error if validation fails or any pipeline stage cannot complete.
pub(crate) fn generate_with_output_root(
    job: &GenerationJob,
    writer_session: Option<storage::authority::WriterSession>,
    reporter: &mut dyn ProgressReporter,
) -> Result<GenerationReport> {
    match run_generation(job, GenerationAuthority::Publish(writer_session), reporter)? {
        GenerationRunResult::Published(validated) => Ok(validated.report),
        GenerationRunResult::Inspected(_) | GenerationRunResult::AlreadyCurrent => {
            unreachable!("publish mode returned a non-publish result")
        }
    }
}

/// Locked ensure entry: one front half that may return [`GenerationRunResult::AlreadyCurrent`] or
/// continue into publication without reconstructing the front half.
///
/// # Errors
///
/// Returns an error if validation fails or any pipeline stage cannot complete.
pub(crate) fn ensure_with_output_root(
    job: &GenerationJob,
    writer: storage::authority::WriterSession,
    return_if_current: bool,
    reporter: &mut dyn ProgressReporter,
) -> Result<GenerationRunResult> {
    run_generation(
        job,
        GenerationAuthority::Ensure {
            writer,
            return_if_current,
        },
        reporter,
    )
}

/// Runs the same complete input and referenced-asset front half used by generation without
/// authorizing publication.
///
/// `force_rebuild` is execution-only and is cleared for this inspection. A forced clean rebuild
/// must not make the resulting tree look stale to status/`ensure_generated` checks.
///
/// This remains the independent path used by [`check_output_status`]. `ensure_generated` uses the
/// locked ensure authority instead of calling this function.
pub(crate) fn generation_inputs_are_current(
    job: &GenerationJob,
    output_paths: OutputPaths,
    base_state: storage::state::CommittedState,
) -> Result<GenerationInputStatus> {
    let mut job = job.clone();
    job.settings.force_rebuild = false;
    let mut reporter = NullProgressReporter;
    match run_generation(
        &job,
        GenerationAuthority::Inspect {
            output_paths,
            base_state,
        },
        &mut reporter,
    )? {
        GenerationRunResult::Inspected(status) => Ok(status),
        GenerationRunResult::Published(_) | GenerationRunResult::AlreadyCurrent => {
            unreachable!("inspection mode published or early-returned as ensure")
        }
    }
}

fn run_generation(
    job: &GenerationJob,
    authority: GenerationAuthority,
    reporter: &mut dyn ProgressReporter,
) -> Result<GenerationRunResult> {
    init_meshopt_allocator();
    job.validate_for_generation()?;

    let trace_report = crate::logging::trace_report_handle();
    trace_report.clear();
    let mut warnings = Vec::new();
    let mut cache = CacheMetadata::default();
    let mut metrics = GenerationMetrics::default();
    let mut identities = identity::ContentIdentityCollector::default();
    let generation_span = info_span!(
        "generation",
        report = true,
        output_root = field::Empty,
        plugin_count = field::Empty,
        terrain_enabled = job.settings.generate_terrain,
        terrain_detail = %job.settings.terrain_detail,
    );
    let _generation_guard = generation_span.enter();

    let vfs = run_stage(GenerationStage::InitializeVfs, reporter, || {
        let stage_span = info_span!(
            "stage.initialize_vfs",
            report = true,
            data_dir_count = field::Empty,
            selected_plugin_count = field::Empty,
            archive_count = field::Empty,
            mesh_asset_count = field::Empty,
            texture_asset_count = field::Empty,
        );
        let _stage_guard = stage_span.enter();
        let options = VfsLoadOptions {
            morrowind_ini: job.morrowind_ini.clone(),
            data_dirs: job.data_dirs.clone(),
            plugins: job.plugins.clone(),
        };
        let vfs = Vfs::load(&options).context("Failed to initialize VFS")?;
        record_usize(&stage_span, "data_dir_count", vfs.data_dirs().len());
        record_usize(&stage_span, "selected_plugin_count", vfs.active_plugins().len());
        record_usize(&stage_span, "archive_count", vfs.archives.len());
        record_usize(&stage_span, "mesh_asset_count", vfs.maps.meshes.len());
        record_usize(&stage_span, "texture_asset_count", vfs.maps.textures.len());
        Ok(vfs)
    })?;
    let resolved_output_paths = OutputPaths::new(job.resolved_output_root(&vfs));
    #[cfg(test)]
    FRONT_HALF_INVOCATIONS.with(|count| count.set(count.get().saturating_add(1)));

    let (writer, output_paths, migration, base_committed, run_mode) = match authority {
        GenerationAuthority::Publish(writer_session) => {
            let writer = match writer_session {
                Some(writer) => {
                    ensure_output_root_matches("locked", &writer.paths().output_root, &resolved_output_paths.output_root)?;
                    writer
                }
                None => storage::authority::WriterSession::acquire(resolved_output_paths)?,
            };
            let output_paths = writer.paths().clone();
            let migration = writer.is_migration();
            let base_committed = writer.base_state().cloned();
            (Some(writer), output_paths, migration, base_committed, RunMode::Publish)
        }
        GenerationAuthority::Inspect {
            output_paths,
            base_state,
        } => {
            ensure_output_root_matches("pinned", &output_paths.output_root, &resolved_output_paths.output_root)?;
            (None, output_paths, false, Some(base_state), RunMode::Inspect)
        }
        GenerationAuthority::Ensure {
            writer,
            return_if_current,
        } => {
            ensure_output_root_matches("locked", &writer.paths().output_root, &resolved_output_paths.output_root)?;
            let output_paths = writer.paths().clone();
            let migration = writer.is_migration();
            let base_committed = writer.base_state().cloned();
            (
                Some(writer),
                output_paths,
                migration,
                base_committed,
                RunMode::Ensure { return_if_current },
            )
        }
    };
    // `loaded_state` now borrows the committed state's memoized decode, so the absent case needs
    // an owned value to borrow from for the lifetime of `previous_state`.
    let fallback_absent = state_db::LoadedGenerationState::Absent;
    let previous_state = match base_committed.as_ref() {
        Some(committed) => committed.loaded_state(),
        None => &fallback_absent,
    };
    generation_span.record("output_root", tracing::field::display(output_paths.output_root.display()));
    record_usize(&generation_span, "plugin_count", vfs.active_plugins().len());

    crate::logging::debug!("Load order: {:#?}", vfs.active_plugins());
    let grass_plugins =
        crate::vfs::resolve_selected_plugins(job.grass_plugins.as_deref().unwrap_or_default(), vfs.data_dirs())
            .context("Failed to resolve grass plugins")?;
    crate::logging::debug!("Grass plugins: {grass_plugins:#?}");

    let overrides = run_stage(GenerationStage::ParseOverrides, reporter, || {
        let stage_span = info_span!(
            "stage.parse_overrides",
            report = true,
            override_list_enabled = job.settings.use_override_list,
            override_file_count = job.settings.override_files.len() as u64,
            plugin_metadata_enabled = job.settings.use_plugin_metadata,
            plugin_metadata_file_count = field::Empty,
        );
        let _stage_guard = stage_span.enter();
        let mut builder = OverridesBuilder::new();
        if job.settings.use_override_list {
            crate::logging::info!("Parsing override files...");
            for path in &job.settings.override_files {
                identities.record_override(apply_override_source_with_identity(path, &mut builder)?);
            }
        }
        // Plugin metadata merges after all configured override sources, in load order, so mods
        // override the global override-file layer.
        if job.settings.use_plugin_metadata {
            let metadata_files = discover_plugin_metadata(vfs.active_plugins());
            record_usize(&stage_span, "plugin_metadata_file_count", metadata_files.len());
            for path in &metadata_files {
                if let Some(identity) = apply_plugin_metadata_with_identity(path, &mut builder) {
                    identities.record_metadata(identity);
                }
            }
        }
        Ok(builder.finish())
    })?;

    let usage_result = run_stage(GenerationStage::ParsePlugins, reporter, || {
        let stage_span = info_span!(
            "stage.parse_plugins",
            report = true,
            plugin_count = field::Empty,
            grass_plugin_count = field::Empty,
            projection_bucket_count = field::Empty,
            projection_build_us = field::Empty,
        );
        let _stage_guard = stage_span.enter();
        crate::logging::info!("Parsing plugins...");
        let usage_options = UsageFilterOptions::from(&job.settings);
        let (usage_info, plugin_identities, grass_plugin_identities, grass_warnings, capture) =
            UsageInfo::setup_with_grass_plugins_and_capture(
                &vfs,
                &grass_plugins,
                &usage_options,
                &overrides,
                |usage_info| {
                    let started = Instant::now();
                    let projections = projection::Projections::capture(usage_info, &job.settings, &overrides);
                    (projections, started.elapsed())
                },
            )?;
        let (mut projections, mut projection_build_time) = capture;
        let grass_capture_started = Instant::now();
        projections.capture_dedicated_grass(&usage_info, &grass_plugins, &job.settings, &overrides);
        projection_build_time += grass_capture_started.elapsed();
        record_usize(&stage_span, "plugin_count", vfs.active_plugins().len());
        record_usize(&stage_span, "grass_plugin_count", grass_plugins.len());
        record_usize(&stage_span, "projection_bucket_count", projections.reference_bucket_count());
        stage_span.record(
            "projection_build_us",
            u64::try_from(projection_build_time.as_micros()).unwrap_or(u64::MAX),
        );
        Ok((
            usage_info,
            plugin_identities,
            grass_plugin_identities,
            grass_warnings,
            projections,
        ))
    })?;
    let (mut usage_info, plugin_identities, grass_plugin_identities, grass_warnings, _projections) = usage_result;
    identities.set_plugins(plugin_identities);
    identities.set_grass_plugins(grass_plugin_identities);
    warnings.extend(grass_warnings.into_iter().map(GenerationWarning::from));
    metrics.usage.object_count = usage_info.objects.len();
    metrics.usage.mesh_scale_count = usage_info.mesh_scale_maximums.len();
    metrics.usage.landscape_cell_count = usage_info.terrain_cells.len();
    metrics.usage.terrain_cell_count = usage_info.terrain_cells.len();
    metrics.usage.interior_cell_count = usage_info.interior_metadata.len();
    metrics.usage.total_reference_count_after_parse = usage_info.total_references_count();
    metrics.usage.exterior_reference_count_after_parse = usage_info.exterior_references_count();
    metrics.usage.script_disable = usage_info.script_disable.clone();

    // Terrain layout depends only on the set of participating terrain cells, so it can
    // be computed once up front and reused by both texture and world-mesh stages.
    let terrain_atlas_layout = {
        let _span = info_span!(
            "terrain.build_layout",
            report = true,
            terrain_cell_count = usage_info.terrain_cells.len() as u64,
        )
        .entered();
        build_terrain_atlas_layout(usage_info.terrain_cells.keys().copied())
    };
    metrics.terrain.atlas_region_count = terrain_atlas_layout.regions.len();
    metrics.terrain.atlas_span_x = terrain_atlas_layout.span_x;
    metrics.terrain.atlas_span_y = terrain_atlas_layout.span_y;
    metrics.terrain.atlas_covered_cell_count = atlas_covered_cell_count(&terrain_atlas_layout);
    metrics.statics.requested_mesh_count = usage_info.mesh_scale_maximums.len();

    let (mut distant_statics, mesh_consumptions) = run_stage(GenerationStage::GenerateStatics, reporter, || {
        let _stage_guard = info_span!(
            "stage.generate_statics",
            report = true,
            requested_mesh_count = usage_info.mesh_scale_maximums.len() as u64,
        )
        .entered();
        crate::logging::info!("Processing {} NIFs...", usage_info.mesh_scale_maximums.len());
        let (distant_statics, identities) = crate::statics::create_distant_statics_with_identities(
            &usage_info,
            &vfs,
            job.settings.min_static_size,
            job.settings.door_size_multiplier,
            &overrides,
        );
        Ok((distant_statics, identities))
    })?;
    for consumption in mesh_consumptions {
        if let Some(resolution) = consumption.resolution {
            identities.record_mesh_resolution(consumption.requested_key.clone(), resolution);
        }
        if let Some(identity) = consumption.identity {
            identities.record_mesh(consumption.requested_key, identity);
        }
    }
    metrics.statics.generated_static_count = distant_statics.len();

    {
        let _prune_guard = info_span!(
            "stage.prune_usage",
            report = true,
            total_reference_count_before_prune = usage_info.total_references_count() as u64,
            exterior_reference_count_before_prune = usage_info.exterior_references_count() as u64,
        )
        .entered();
        // Usage pruning must happen after static generation so it can discard references
        // that point at meshes which were filtered out or never produced.
        usage_info.discard_unused_references(|key| distant_statics.contains_key(key));
        usage_info.discard_deep_water_references(job.settings.deep_water_static_cull_depth, |key| {
            distant_statics
                .get(key)
                .map(|static_| (static_.static_type, &static_.bounding_box))
        });
        metrics.usage.burial = usage_info.discard_low_visibility_references(
            |key| distant_statics.get(key).map(|static_| static_.static_type),
            |terrain_cells, reference, stats| {
                distant_statics
                    .get(reference.id.as_ref())
                    .is_some_and(|static_| crate::statics::is_buried(terrain_cells, reference, static_, stats))
            },
        );
        metrics.usage.total_reference_count_after_prune = usage_info.total_references_count();
        metrics.usage.exterior_reference_count_after_prune = usage_info.exterior_references_count();
    }

    let textures = AtlasTextureSet::from_distant_statics(&vfs, &distant_statics);
    let texture_caps = TextureAxisCaps::for_atlas(
        job.settings.max_static_texture_long_axis,
        job.settings.max_static_texture_short_axis,
        job.settings.max_static_atlas_size,
    );
    metrics.atlas.opaque_texture_count = textures.opaque.len();
    metrics.atlas.alpha_texture_count = textures.alpha.len();

    // Hoist source analysis and bounded sizing metrics into the no-write decide phase.
    let sizing_settings = job.settings.static_texture_sizing;
    let (fingerprints, sizing_plan, source_info) = run_stage(GenerationStage::AnalyzeTextureDensity, reporter, || {
        let stage_span = info_span!(
            "stage.analyze_texture_density",
            report = true,
            opaque_texture_count = textures.opaque.len() as u64,
            alpha_texture_count = textures.alpha.len() as u64,
            sizing_mode = ?sizing_settings.mode,
            reduced_texture_count = field::Empty,
        );
        let _stage_guard = stage_span.enter();
        let source_info = collect_static_texture_source_info(&vfs, &textures)?;
        let fingerprints = fingerprints_from_source_info(&source_info, &textures);

        let plan = if sizing_settings.mode == StaticTextureSizingMode::Off {
            SizingPlan::baseline(texture_caps, &source_info)
        } else {
            let usage = analyze_static_texture_usage(&vfs, &distant_statics, &source_info, texture_caps);
            let (mut plan, sizing_metrics) =
                plan_static_texture_resolutions(&source_info, &usage, &sizing_settings, texture_caps);
            merge_dedupe_alias_requirements(&textures, &source_info, job.settings.texture_dedupe_mode, &mut plan);
            record_usize(
                &stage_span,
                "reduced_texture_count",
                sizing_metrics.reduced_texture_count as usize,
            );
            plan
        };
        Ok((fingerprints, plan, source_info))
    })?;

    let mut current_state = unit_fingerprint::build_static_state(
        &job.settings,
        &_projections,
        &identities,
        &usage_info,
        &distant_statics,
        &vfs,
        &textures,
        &source_info,
        &sizing_plan,
    );

    let ctx = StageContext {
        job,
        vfs: &vfs,
        output_paths: &output_paths,
        migration,
    };

    let base_static_shards = std::array::from_fn(|shard_id| {
        base_committed
            .as_ref()
            .and_then(|state| state.artifact(&output::static_mesh_shard_relative_path(shard_id)))
    });
    let base_terrain_payload = base_committed
        .as_ref()
        .and_then(|state| state.artifact_by_kind(storage::state::ArtifactKind::TerrainPayload));
    let base_usage = base_committed
        .as_ref()
        .and_then(|state| state.artifact_by_kind(storage::state::ArtifactKind::Usage));
    let base_inventory = base_committed.as_ref().map_or(&[][..], |state| state.artifacts.as_slice());
    let base_atlas_paths: IndexSet<String> = base_committed
        .as_ref()
        .into_iter()
        .flat_map(|state| state.atlas_relative_paths())
        .map(str::to_owned)
        .collect();

    // Terrain preparation and its domain digest must precede atlas planning so all unit state and
    // both coarse gates are available before the first output write.
    let terrain_preparation = terrain_stage::prepare_terrain_stage(
        &ctx,
        &terrain_atlas_layout,
        &usage_info,
        &mut current_state,
        previous_state.state().map(|state| state.terrain_domain_digest),
        base_inventory,
        &_projections,
        &mut metrics,
        &mut warnings,
        &mut identities,
    )?;

    let diff = run_stage(GenerationStage::ComputeUnitDiff, reporter, || {
        let _stage_guard = info_span!(
            "stage.compute_unit_diff",
            report = true,
            mesh_units = current_state.units.mesh.entries.len() as u64,
            merge_units = current_state.units.merge.entries.len() as u64,
            texture_units = current_state.units.texture.entries.len() as u64,
            terrain_cell_units = current_state.units.terrain_cell.entries.len() as u64,
            terrain_chunk_units = current_state.units.terrain_chunk.entries.len() as u64,
        )
        .entered();
        state_db::diff(&current_state, previous_state, ctx.force_rebuild())
    })?;
    let rebuild_cause = state_db::format_rebuild_cause(&diff.report, ctx.force_rebuild(), ctx.migration);
    let input_status = GenerationInputStatus {
        current: units_are_clean(&diff.report),
        rebuild_cause: rebuild_cause.clone(),
    };
    match run_mode {
        RunMode::Inspect => {
            return Ok(GenerationRunResult::Inspected(input_status));
        }
        RunMode::Ensure { return_if_current: true } if input_status.current => {
            let writer = writer.context("ensure mode lost its writer session")?;
            writer.finish_noop();
            return Ok(GenerationRunResult::AlreadyCurrent);
        }
        RunMode::Publish | RunMode::Ensure { .. } => {}
    }
    if let Some(cause) = rebuild_cause.as_deref() {
        info!(cause, "Rebuilding distant land");
    }
    let mut writer = writer.context("publish/ensure mode lost its writer session")?;

    // Terrain planning consumes the complete dirty-chunk set, so it must follow the diff.
    let terrain_plan = terrain_stage::plan_terrain_stage(
        terrain_preparation,
        &ctx,
        &terrain_atlas_layout,
        &usage_info,
        &current_state,
        previous_state.state(),
        base_terrain_payload,
        base_inventory,
        &diff,
        &mut metrics,
    )?;
    // A whole-package carry re-commits the previous records unchanged, so the current state must
    // already carry that metadata for the no-op equality check below to hold.
    if let Some(keys) = terrain_plan.carried_emitted_keys() {
        current_state.terrain_records = keys.to_vec();
    }
    let units = diff.report.clone();

    let atlas_plan = run_stage(GenerationStage::CreateTextureAtlas, reporter, || {
        let stage_span = info_span!(
            "stage.create_texture_atlas",
            report = true,
            opaque_texture_count = textures.opaque.len() as u64,
            alpha_texture_count = textures.alpha.len() as u64,
            opaque_page_count = field::Empty,
            alpha_page_count = field::Empty,
        );
        let _stage_guard = stage_span.enter();
        crate::logging::info!("Creating texture atlas...");
        let atlas_paths = output_paths.atlas_texture_dir.clone();
        let mut atlas_manager = AtlasManager::setup_with_cache_state(
            &textures,
            sizing_plan,
            job.settings.texture_dedupe_mode,
            job.settings.max_static_atlas_size,
            fingerprints,
            atlas_paths,
            previous_state.state().map(|state| state.atlas_cache.as_slice()),
            &base_atlas_paths,
        );
        if ctx.force_rebuild() {
            atlas_manager.clear_prior();
        }
        let atlas_plan = atlas_manager.plan(&vfs, &mut distant_statics, textures)?;
        record_u32(&stage_span, "opaque_page_count", atlas_plan.metrics().page_counts.opaque);
        record_u32(&stage_span, "alpha_page_count", atlas_plan.metrics().page_counts.alpha);
        Ok(atlas_plan)
    })?;
    cache.static_atlas_hit = atlas_plan.cache_hit();
    if atlas_plan.cache_hit() {
        metrics.atlas.cached_opaque_page_count = Some(atlas_plan.metrics().page_counts.opaque);
        metrics.atlas.cached_alpha_page_count = Some(atlas_plan.metrics().page_counts.alpha);
    }
    let atlas_plan_metrics = atlas_plan.metrics();
    metrics.atlas.publication_bytes_estimate = atlas_plan_metrics.publication_bytes_estimate;
    metrics.atlas.dirty_page_count = atlas_plan_metrics.dirty_page_count;
    metrics.atlas.opaque_layout_hit = atlas_plan_metrics.layout_hits.opaque;
    metrics.atlas.alpha_layout_hit = atlas_plan_metrics.layout_hits.alpha;
    metrics.atlas.decoded_texture_count = atlas_plan_metrics.decoded_texture_count;
    metrics.atlas.carried_opaque_page_count = atlas_plan_metrics.carried_page_counts.opaque;
    metrics.atlas.carried_alpha_page_count = atlas_plan_metrics.carried_page_counts.alpha;
    metrics.atlas.built_opaque_page_count = atlas_plan_metrics.built_page_counts.opaque;
    metrics.atlas.built_alpha_page_count = atlas_plan_metrics.built_page_counts.alpha;
    metrics.atlas.opaque_family = atlas_plan_metrics.family_metrics.opaque.clone();
    metrics.atlas.alpha_family = atlas_plan_metrics.family_metrics.alpha.clone();
    metrics.atlas.zero_page_write_reconciliation_count = atlas_plan_metrics.zero_page_write_reconciliation_count;

    // The atlas binding is now final, so the statics coarse gate and all miss-path CPU work can be
    // decided without authorizing a write.
    //
    // Only an exact two-family evidence carry retains the previous serialized bytes. Reconciliation
    // always commits its new v3 slot relation, even when it produces zero atlas page writes.
    current_state.atlas_cache = if atlas_plan.cache_hit() {
        previous_state
            .state()
            .map(|state| state.atlas_cache.clone())
            .filter(|bytes| !bytes.is_empty())
            .unwrap_or_else(|| atlas_plan.cache_bytes().to_vec())
    } else {
        atlas_plan.cache_bytes().to_vec()
    };
    // Set the record-production recipe digest before the domain digest, which folds it in.
    current_state.statics_record_global_digest = unit_fingerprint::statics_record_global_digest();
    let static_bundle_fingerprint = unit_fingerprint::statics_domain_digest(&current_state, &atlas_plan.binding_digest());
    current_state.statics_domain_digest = static_bundle_fingerprint;
    let statics_plan = statics_stage::prepare_statics_bundle(
        &ctx,
        &generation_span,
        static_bundle_fingerprint,
        &current_state,
        previous_state.state(),
        &diff,
        atlas_plan.binding_deltas(),
        &base_static_shards,
        base_usage,
        &overrides,
        &mut usage_info,
        distant_statics,
        &mut metrics,
        &mut cache,
        reporter,
    )?;
    current_state.static_mesh_shards = statics_plan.static_mesh_shards();
    current_state.optimized_mesh_bounds = statics_plan.optimized_mesh_bounds();
    let commit_plan = CommitPlan::new(
        atlas_plan,
        statics_plan,
        terrain_plan,
        units,
        &current_state,
        base_committed.as_ref(),
        &ctx,
    )?;
    let CommitPlan {
        atlas,
        statics,
        terrain,
        units,
        version_unchanged,
        no_op,
        preflight_bytes,
        writes,
    } = commit_plan;
    let atlas_carried_paths = atlas.carried_relative_paths();

    let load_order_identity =
        identity::collected_load_order_identity(&identities).context("Failed to compute load-order identity")?;

    if no_op {
        mark_noop_write_decisions(&mut cache, job.settings.generate_terrain);
        // Build an in-memory report only; do not rewrite observability products.
        let mut report_data = build_generation_report_data(
            job,
            &output_paths,
            TraceSummary::default(),
            &cache,
            &metrics,
            &warnings,
            &load_order_identity,
            &units,
            rebuild_cause.as_deref(),
        )?;
        report_data.trace_summary = trace_report.snapshot();
        if !report_data.mge_xe_contract.complete {
            bail!(
                "no-op generation produced an incomplete MGE-XE contract: {:#?}",
                report_data.mge_xe_contract.issues
            );
        }
        // Base was Routine-validated at acquire; re-confirm under the still-held exclusive session.
        //
        // Not redundant: the lock excludes cooperating writers, not the rest of the machine. An AV
        // scanner quarantining a shard, a sync client rewriting one, or the user deleting a file
        // between acquire and here all land inside this window, and this is the last point where
        // serving can still be refused instead of handing the host a hole. Same threat model that
        // justifies holding file handles in `durable.rs`.
        //
        // It costs one open per listed artifact on the every-launch path; measure before trading
        // that away. Only the pass/fail matters here; the reloaded state itself has no caller.
        writer
            .validate_routine()
            .context("no-op base failed under-session Routine validation")?;
        // True no-ops perform no writes, so no deferred payload sync may be outstanding.
        debug_assert!(writes.pending_durables_is_empty());
        writer.finish_noop();
        return Ok(GenerationRunResult::Published(ValidatedGeneration {
            report: GenerationReport {
                output_root: output_paths.output_root,
                report_path: output_paths.generation_report_path,
                report_written: OutputWriteDecision::SkippedUnchanged,
                report: report_data,
                warnings,
            },
        }));
    }

    writer.begin_dirty(preflight_bytes)?;

    let version = if version_unchanged && !migration {
        base_committed
            .as_ref()
            .and_then(|state| state.artifact_by_kind(storage::state::ArtifactKind::Version))
            .cloned()
            .context("committed state is missing the version inventory entry")?
    } else {
        let written = run_stage(GenerationStage::WriteVersionFile, reporter, || {
            let _stage_guard = info_span!("stage.write_version_file", report = true).entered();
            Ok(writes.write_durable(
                &output_paths.version_path,
                [MGE_DL_VERSION],
                storage::durable::SyncClass::SmallArtifact,
            )?)
        })?;
        storage::state::artifact_from_written(
            storage::state::ArtifactKind::Version,
            "version",
            written.byte_length,
            written.content_blake3,
        )
    };

    fs::create_dir_all(&output_paths.statics_dir)?;
    fs::create_dir_all(&output_paths.atlas_texture_dir)?;
    let mut statics_result = statics_stage::publish_statics_bundle(statics, &ctx, &writes, &mut cache, reporter)?;

    let atlas_publish_span = info_span!(
        "atlas.publish",
        report = true,
        cache_hit = atlas.cache_hit(),
        dirty_page_count = atlas.metrics().dirty_page_count as u64,
        estimated_output_bytes = atlas.metrics().publication_bytes_estimate,
        estimated_peak_bytes = atlas.metrics().publication_peak_bytes,
    );
    let _atlas_publish_guard = atlas_publish_span.enter();
    // Streamed rather than collected: each page is written and dropped before the next is encoded,
    // so publication never holds every encoded atlas page at once.
    let mut atlas_written = Vec::new();
    atlas.render_streaming(|page| {
        let AtlasPageWrite { path, bytes } = page;
        atlas_written.push(writes.write_durable(&path, bytes, storage::durable::SyncClass::Payload)?);
        Ok(())
    })?;
    drop(_atlas_publish_guard);
    let mut atlas_inventory: Vec<_> = atlas_carried_paths
        .iter()
        .map(|path| {
            base_committed
                .as_ref()
                .and_then(|state| state.artifact(path))
                .cloned()
                .with_context(|| format!("committed atlas artifact missing for carried page {path}"))
        })
        .collect::<Result<_>>()?;
    atlas_inventory.extend(
        atlas_written
            .into_iter()
            .map(artifact_from_atlas_written)
            .collect::<Result<Vec<_>>>()?,
    );

    let mut terrain_result =
        terrain_stage::publish_terrain_stage(terrain, &ctx, &usage_info, &writes, &mut metrics, &mut cache, reporter)?;

    if let Some(handle) = statics_result.writer.take() {
        // The shard writer was spawned under `run_stage(WriteStaticMeshes)` and traces its own
        // work as `io.write_static_meshes_async`. Whatever of that work has not already
        // overlapped earlier stages is paid here, so this join is the publish-phase stall.
        let _stage_guard = info_span!("stage.await_static_shard_writer", report = true).entered();
        let written = handle
            .join()
            .map_err(|_| anyhow::anyhow!("static shard writer thread panicked"))??;
        statics_result.shards.extend(written);
        cache.writes.static_meshes = Some(OutputWriteDecision::Written);
    }
    if let Some(handle) = terrain_result.writer.take() {
        // Same shape as the shard writer join above: the residual, non-overlapped wall time of
        // `io.write_terrain_package_async` is paid here.
        let _stage_guard = info_span!("stage.await_terrain_writer", report = true).entered();
        let package = handle
            .join()
            .map_err(|_| anyhow::anyhow!("terrain package writer thread panicked"))??;
        terrain_result.artifacts.extend(package.artifacts);
        terrain_result.emitted_keys = Some(package.emitted_keys);
    }
    // State records what the writer actually committed, so this must follow its join and precede
    // `state_db::encode`.
    if let Some(keys) = terrain_result.emitted_keys.take() {
        current_state.terrain_records = keys;
    }
    anyhow::ensure!(
        statics_result.shards.len() == output::STATIC_MESH_SHARD_COUNT,
        "statics publication produced {} shards, expected {}",
        statics_result.shards.len(),
        output::STATIC_MESH_SHARD_COUNT
    );
    metrics.statics.static_meshes_serialized_size_bytes = statics_result.shards.iter().map(|entry| entry.byte_length).sum();
    metrics.terrain.terrain_bin_size_bytes = terrain_result
        .artifacts
        .iter()
        .find(|entry| entry.kind == storage::state::ArtifactKind::TerrainPayload)
        .map_or(0, |entry| entry.byte_length);

    let mut artifacts = vec![version, statics_result.usage];
    artifacts.extend(statics_result.shards);
    artifacts.extend(terrain_result.artifacts);
    artifacts.extend(atlas_inventory);

    // Summarize the final committed inventory, so carried atlas pages and reused shards are
    // counted the same as freshly written ones. Must precede the move into `CommittedState`.
    metrics.gpu_memory = metrics::summarize_gpu_memory(&artifacts);

    let generator_state = state_db::encode(&current_state)?;
    let committed = storage::state::CommittedState::new(generator_state, artifacts)?;
    writer.finish_publish(committed, &writes)?;

    let (report_data, report_written) = run_stage(GenerationStage::WriteGenerationReport, reporter, || {
        let _stage_guard = info_span!("stage.write_generation_report", report = true).entered();
        let mut report_data = build_generation_report_data(
            job,
            &output_paths,
            TraceSummary::default(),
            &cache,
            &metrics,
            &warnings,
            &load_order_identity,
            &units,
            rebuild_cause.as_deref(),
        )?;
        report_data.trace_summary = trace_report.snapshot();
        let report_written = write_generation_report_data(&output_paths, &report_data)?;
        Ok((report_data, report_written))
    })?;

    if !report_data.mge_xe_contract.complete {
        bail!(
            "published output has an incomplete MGE-XE contract: {:#?}",
            report_data.mge_xe_contract.issues
        );
    }

    // Last serveability step under the exclusive session: reload and Routine-validate on disk.
    // Only the pass/fail matters here; the reloaded state itself has no caller.
    writer
        .validate_published()
        .context("post-publish under-session Routine validation failed")?;

    Ok(GenerationRunResult::Published(ValidatedGeneration {
        report: GenerationReport {
            output_root: output_paths.output_root,
            report_path: output_paths.generation_report_path,
            report_written,
            report: report_data,
            warnings,
        },
    }))
}

fn artifact_from_atlas_written(written: WrittenFile) -> Result<storage::state::RequiredArtifact> {
    let name = written
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("invalid static atlas page path {}", written.path.display()))?
        .to_string();
    Ok(storage::state::artifact_from_written(
        storage::state::ArtifactKind::AtlasDds,
        format!(r"statics\textures\{name}"),
        written.byte_length,
        written.content_blake3,
    ))
}

fn mark_noop_write_decisions(cache: &mut CacheMetadata, terrain_enabled: bool) {
    let skipped = OutputWriteDecision::SkippedUnchanged;
    cache.writes.usage_data = Some(skipped);
    cache.writes.static_meshes = Some(skipped);
    if terrain_enabled {
        cache.writes.terrain_bin = Some(skipped);
        cache.writes.terrain_occlusion = Some(skipped);
        terrain_stage::set_dds_write_decisions(cache, skipped);
    }
}

fn units_are_clean(units: &UnitsReport) -> bool {
    !units.settings_changed
        && units.kinds.mesh.dirty == 0
        && units.kinds.merge.dirty == 0
        && units.kinds.texture.dirty == 0
        && units.kinds.terrain_cell.dirty == 0
        && units.kinds.terrain_chunk.dirty == 0
        && !units.domains.statics.dirty
        && (!units.domains.terrain.enabled || !units.domains.terrain.dirty)
}

/// Notifies the reporter, runs `f`, then reports elapsed time regardless of outcome.
fn run_stage<T>(stage: GenerationStage, reporter: &mut dyn ProgressReporter, f: impl FnOnce() -> Result<T>) -> Result<T> {
    reporter.begin_stage(stage);
    let started_at = Instant::now();
    let result = f();
    reporter.finish_stage(stage, started_at.elapsed().as_secs_f64() * 1000.0);
    result
}

/// Records a `usize` counter into a tracing span field as `u64`.
fn record_usize(span: &Span, field: &'static str, value: usize) {
    span.record(field, value as u64);
}

/// Records a `u32` counter into a tracing span field as `u64`.
fn record_u32(span: &Span, field: &'static str, value: u32) {
    span.record(field, value as u64);
}

/// Returns the total cell count covered by all regions in a terrain atlas layout.
fn atlas_covered_cell_count(layout: &crate::TerrainAtlasLayout) -> usize {
    layout
        .regions
        .iter()
        .map(|region| (region.width() * region.height()) as usize)
        .sum()
}

/// Returns the size of a file in bytes, or `0` if the file is missing or inaccessible.
fn file_size_or_zero(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

#[cfg(test)]
mod tests;
