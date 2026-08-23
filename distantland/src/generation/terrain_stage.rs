//! Terrain-package decision, preparation, and publication.

use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::Result;
use hashbrown::{HashMap, HashSet};
use tracing::{field, info_span};

use crate::generation::units::TerrainMeshWorkKey;
use crate::mge_xe::distant_terrain::{TerrainFile, TerrainMesh, deserialize_terrain_file};
use crate::terrain::mesh::{
    TerrainMeshBuilder, TerrainMeshWorkItem, TerrainMeshWorkResult, assemble_terrain_mesh_set, enumerate_terrain_mesh_work,
};
use crate::terrain::package::{
    TerrainPackageInputs, TerrainPackagePlan, plan_terrain_package, prepare_terrain_package_inputs,
};
use crate::{
    TerrainAtlasLayout, UsageInfo, write_bc1_dds_from_chain_unflipped, write_bc1_dds_unflipped, write_rgba8_dds_unflipped,
};

use super::identity::ContentIdentityCollector;
use super::metrics::{CacheMetadata, GenerationMetrics, TerrainReuseMode, summarize_terrain_file};
use super::projection::Projections;
use super::state_db::{GenerationDiff, GenerationState};
use super::storage::durable::SyncClass;
use super::storage::fault;
use super::storage::state::{ArtifactKind, RequiredArtifact, artifact_from_written};
use super::unit_fingerprint;
use super::units::TerrainChunkUnitKey;
use super::{
    GenerationStage, GenerationWarning, OutputWriteDecision, ProgressReporter, PublicationWrites, StageContext, run_stage,
};

/// Eventual result of the asynchronous terrain package writer.
pub(super) type TerrainPackageWrite = JoinHandle<Result<TerrainPackageArtifacts>>;

/// Immutable terrain preparation, computed before the unit diff is available.
pub(super) enum TerrainPreparation<'a> {
    /// Terrain generation is switched off for this run.
    Disabled,
    /// Terrain inputs are loaded and terrain unit state is populated.
    Enabled(Box<EnabledTerrainPreparation<'a>>),
}

/// Prepared inputs for an enabled terrain run.
pub(super) struct EnabledTerrainPreparation<'a> {
    inputs: TerrainPackageInputs<'a>,
    occlusion: OcclusionPlan,
}

/// How the terrain occlusion artifact is produced, decided once during preparation.
enum OcclusionPlan {
    /// The committed grid still describes the current height domain; re-publish it untouched.
    Carry(RequiredArtifact),
    /// The grid changed; write these bytes. The digest is kept beside them so the content gate
    /// never re-hashes a multi-megabyte payload.
    Write { bytes: Vec<u8>, content_blake3: [u8; 32] },
}

impl OcclusionPlan {
    /// Returns the bytes publication must write, or `None` when the committed artifact stands.
    fn pending_write(&self) -> Option<&[u8]> {
        match self {
            OcclusionPlan::Carry(_) => None,
            OcclusionPlan::Write { bytes, .. } => Some(bytes),
        }
    }

    /// Content digest of the occlusion artifact this run publishes, however it was produced.
    fn content_blake3(&self) -> [u8; 32] {
        match self {
            OcclusionPlan::Carry(entry) => entry.content_blake3,
            OcclusionPlan::Write { content_blake3, .. } => *content_blake3,
        }
    }
}

/// How the terrain mesh payload is produced.
enum TerrainMeshPlan<'a> {
    /// Every terrain artifact is unchanged; the committed payload and inventory are re-published.
    CarryPackage {
        payload: RequiredArtifact,
        inventory: Vec<RequiredArtifact>,
        emitted_keys: Vec<TerrainMeshWorkKey>,
    },
    /// Assemble a new `terrain.bin` from rebuilt work plus any reusable committed records.
    Assemble {
        plan: Box<TerrainPackagePlan<'a>>,
        work: Vec<TerrainMeshWorkItem>,
        /// Records carried from the committed file, keyed by their emitting work key. A clean key
        /// absent from this map carried a prior absence. `None` rebuilds every item.
        reused: Option<HashMap<TerrainMeshWorkKey, TerrainMesh>>,
        /// The complete dirty absolute chunk set a work item's dependencies are tested against.
        dirty_chunks: HashSet<TerrainChunkUnitKey>,
        dds: TerrainDdsPlan,
    },
}

/// How the five height-independent DDS products are produced.
enum TerrainDdsPlan {
    /// Their exact input digest is unchanged; carry the committed inventory entries.
    Carry(Vec<RequiredArtifact>),
    /// Rebuild the complete five-DDS bundle.
    Rebuild,
}

enum TerrainPublishMode<'a> {
    Disabled,
    Enabled {
        mesh: TerrainMeshPlan<'a>,
        occlusion: OcclusionPlan,
    },
}

/// Complete terrain gate decision and immutable source preparation.
pub(super) struct TerrainStagePlan<'a> {
    mode: TerrainPublishMode<'a>,
}

/// Synchronous terrain results plus an optional background package writer.
pub(super) struct TerrainPublishResult {
    pub(super) artifacts: Vec<RequiredArtifact>,
    pub(super) writer: Option<TerrainPackageWrite>,
    /// Emitted record keys already known without waiting for the writer (whole-package carry).
    pub(super) emitted_keys: Option<Vec<TerrainMeshWorkKey>>,
}

/// Artifacts returned by the background terrain writer.
pub(super) struct TerrainPackageArtifacts {
    pub(super) artifacts: Vec<RequiredArtifact>,
    /// Work key of every record actually written to `terrain.bin`, in file order.
    pub(super) emitted_keys: Vec<TerrainMeshWorkKey>,
}

impl TerrainStagePlan<'_> {
    /// Returns whether every terrain artifact is unchanged.
    pub(super) fn is_hit(&self) -> bool {
        match &self.mode {
            TerrainPublishMode::Disabled => true,
            TerrainPublishMode::Enabled {
                mesh: TerrainMeshPlan::CarryPackage { .. },
                occlusion,
            } => occlusion.pending_write().is_none(),
            TerrainPublishMode::Enabled { .. } => false,
        }
    }

    /// Emitted record keys for a whole-package carry, which publication re-commits unchanged.
    pub(super) fn carried_emitted_keys(&self) -> Option<&[TerrainMeshWorkKey]> {
        match &self.mode {
            TerrainPublishMode::Enabled {
                mesh: TerrainMeshPlan::CarryPackage { emitted_keys, .. },
                ..
            } => Some(emitted_keys),
            _ => None,
        }
    }

    /// Returns a conservative cumulative-preflight contribution.
    pub(super) fn publication_bytes_estimate(&self, previous_payload_size: u64, previous_inventory_size: u64) -> u64 {
        match &self.mode {
            TerrainPublishMode::Disabled => 0,
            TerrainPublishMode::Enabled { mesh, occlusion } => {
                let occlusion = occlusion.pending_write().map_or(0, |bytes| bytes.len() as u64);
                let package = match mesh {
                    TerrainMeshPlan::Assemble { .. } => previous_payload_size
                        .saturating_add(previous_inventory_size)
                        .saturating_mul(5)
                        .div_ceil(4)
                        .max(1024 * 1024 * 1024),
                    TerrainMeshPlan::CarryPackage { .. } => 0,
                };
                package.saturating_add(occlusion)
            }
        }
    }
}

/// Prepares terrain identities, unit state, both domain digests, and the occlusion plan without
/// writing.
///
/// Deliberately makes no *package* gate decision: the complete unit diff the planner needs is not
/// available until this state has been populated. Occlusion is the exception. It depends only on
/// the terrain domain digest computed here, so `previous_terrain_domain_digest` lets an unchanged
/// domain skip the whole-domain height scan outright.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_terrain_stage<'a>(
    ctx: &StageContext<'a>,
    terrain_atlas_layout: &TerrainAtlasLayout,
    usage_info: &UsageInfo<'a>,
    state: &mut GenerationState,
    previous_terrain_domain_digest: Option<[u8; 32]>,
    base_inventory: &[RequiredArtifact],
    projections: &Projections,
    metrics: &mut GenerationMetrics,
    warnings: &mut Vec<GenerationWarning>,
    identities: &mut ContentIdentityCollector,
) -> Result<TerrainPreparation<'a>> {
    let StageContext { job, vfs, .. } = *ctx;
    if !job.settings.generate_terrain {
        unit_fingerprint::populate_terrain_state(state, &job.settings, projections, identities, terrain_atlas_layout, None);
        warnings.push(GenerationWarning {
            code: "terrain_package_disabled".to_string(),
            message:
                "Terrain package generation was disabled, so the generated output is not a complete terrain runtime set."
                    .to_string(),
        });
        return Ok(TerrainPreparation::Disabled);
    }

    let prepare_span = info_span!(
        "terrain.prepare_package_inputs",
        report = true,
        occlusion_unchanged = field::Empty,
        occlusion_hash = field::Empty,
    );
    let _prepare_guard = prepare_span.enter();
    let inputs = prepare_terrain_package_inputs(
        usage_info,
        vfs,
        job.settings.terrain_mesh_target_error(),
        crate::terrain::mesh::MeshSimplifierWeights {
            smoothed_normal: job.settings.terrain_mesh_smoothed_normal_weight,
            color: job.settings.terrain_mesh_color_weight,
        },
        job.settings.max_terrain_texture_size,
        job.settings.max_terrain_atlas_size,
        job.settings.texture_dedupe_mode,
        crate::terrain::package::TerrainControlTextureLimits {
            max_size: job.settings.max_terrain_control_texture_size,
            max_bytes: job.settings.max_terrain_control_texture_bytes,
        },
    )?;
    for (key, hash) in inputs.texture_identities() {
        identities.record_terrain_texture(key.to_owned(), hash);
    }
    let gate_inputs = inputs.gate_inputs();
    unit_fingerprint::populate_terrain_state(
        state,
        &job.settings,
        projections,
        identities,
        terrain_atlas_layout,
        Some(&gate_inputs),
    );
    state.terrain_domain_digest = unit_fingerprint::terrain_domain_digest(state);
    state.terrain_dds_domain_digest = unit_fingerprint::terrain_dds_domain_digest(
        &job.settings,
        projections,
        identities,
        terrain_atlas_layout,
        &gate_inputs,
    );

    metrics.terrain.terrain_texture_count = inputs.texture_count();
    metrics.terrain.atlas_size = inputs.atlas_size();
    metrics.terrain.logical_tile_size = inputs.logical_tile_size();
    metrics.terrain.physical_tile_size = inputs.physical_tile_size();
    metrics.terrain.tiles_per_row = inputs.tiles_per_row();
    metrics.terrain.atlas_max_lod = inputs.atlas_max_lod();
    metrics.terrain.material_size_xy = inputs.material_size_xy();
    metrics.terrain.estimated_control_texture_bytes = inputs.estimated_control_texture_bytes();
    metrics.terrain.estimated_source_atlas_bytes = inputs.estimated_source_atlas_bytes();

    let force_occlusion = ctx.force_rebuild();
    let previous_occlusion = base_inventory
        .iter()
        .find(|entry| entry.kind == ArtifactKind::TerrainOcclusion);
    // The grid is a pure max-Z reduction over every LAND cell's heights plus the control
    // region's origin and extent, and `terrain_domain_digest` covers all three: per-cell
    // `height_hash` through the terrain-cell fingerprint table, and `origin_cell` / `cell_size_xy`
    // through `terrain_global`. An unchanged digest therefore means an unchanged grid, so the
    // committed artifact stands without scanning the whole height domain again.
    // A migration may have decoded the previous state under a different recipe, so it withholds
    // trust in the recorded digest exactly as the whole-package carry does.
    let domain_unchanged = !ctx.migration && previous_terrain_domain_digest == Some(state.terrain_domain_digest);
    let occlusion = match previous_occlusion.filter(|_| !force_occlusion && domain_unchanged) {
        Some(committed) => OcclusionPlan::Carry(committed.clone()),
        None => {
            // The builder is deliberately coarse: it scans the whole height domain and
            // produces a global max-Z base grid, then falls back on a byte-level content gate.
            let (origin_cell, cell_size_xy) = inputs.occlusion_region();
            let occlusion = crate::mge_xe::terrain_occlusion::build_terrain_occlusion(
                usage_info
                    .terrain_cells
                    .iter()
                    .map(|(&grid, cell)| (grid, cell.heights.as_ref())),
                origin_cell,
                cell_size_xy,
                crate::mge_xe::terrain_occlusion::TERRAIN_OCCLUSION_GRID_SPACING,
            )?;
            let bytes = crate::mge_xe::terrain_occlusion::serialize_terrain_occlusion_file(&occlusion)?;
            metrics.terrain.occlusion_built_coarse = true;
            let content_blake3 = *blake3::hash(&bytes).as_bytes();
            match previous_occlusion.filter(|entry| {
                !force_occlusion && entry.byte_length == bytes.len() as u64 && entry.content_blake3 == content_blake3
            }) {
                Some(committed) => OcclusionPlan::Carry(committed.clone()),
                None => OcclusionPlan::Write { bytes, content_blake3 },
            }
        }
    };
    prepare_span.record("occlusion_unchanged", occlusion.pending_write().is_none());
    prepare_span.record(
        "occlusion_hash",
        tracing::field::display(blake3::Hash::from(occlusion.content_blake3()).to_hex()),
    );

    Ok(TerrainPreparation::Enabled(Box::new(EnabledTerrainPreparation {
        inputs,
        occlusion,
    })))
}

/// Chooses whole-package carry, partial mesh rebuild, or full rebuild from the complete unit diff.
#[allow(clippy::too_many_arguments)]
pub(super) fn plan_terrain_stage<'a>(
    preparation: TerrainPreparation<'a>,
    ctx: &StageContext<'a>,
    terrain_atlas_layout: &TerrainAtlasLayout,
    usage_info: &UsageInfo<'a>,
    state: &GenerationState,
    previous_state: Option<&GenerationState>,
    base_payload: Option<&RequiredArtifact>,
    base_inventory: &[RequiredArtifact],
    diff: &GenerationDiff,
    metrics: &mut GenerationMetrics,
) -> Result<TerrainStagePlan<'a>> {
    let TerrainPreparation::Enabled(preparation) = preparation else {
        return Ok(TerrainStagePlan {
            mode: TerrainPublishMode::Disabled,
        });
    };
    let EnabledTerrainPreparation { inputs, occlusion } = *preparation;
    let force_rebuild = ctx.force_rebuild();
    let reusable = !ctx.migration && !force_rebuild && diff.comparable;
    let previous = reusable.then_some(previous_state).flatten();

    let dds_inventory: Vec<RequiredArtifact> = base_inventory
        .iter()
        .filter(|entry| entry.kind == ArtifactKind::TerrainDds)
        .cloned()
        .collect();

    // The existing coarse gate still owns the whole-package carry: an unchanged terrain domain
    // means neither the records nor the header nor the DDS products can have changed.
    let carried_package = previous.is_some_and(|previous| previous.terrain_domain_digest == state.terrain_domain_digest)
        && base_payload.is_some()
        && dds_inventory.len() == 5;
    if carried_package {
        let previous = previous.expect("carry requires a previous state");
        metrics.terrain.reuse_mode = TerrainReuseMode::CarriedPackage;
        metrics.terrain.dds_carried = true;
        return Ok(TerrainStagePlan {
            mode: TerrainPublishMode::Enabled {
                mesh: TerrainMeshPlan::CarryPackage {
                    payload: base_payload.expect("carry requires a payload").clone(),
                    inventory: dds_inventory,
                    emitted_keys: previous.terrain_records.clone(),
                },
                occlusion,
            },
        });
    }

    let plan = plan_terrain_package(inputs, usage_info)?;
    let work = enumerate_terrain_mesh_work(terrain_atlas_layout)?;
    metrics.terrain.mesh_work_items = work.len();

    let reused = match previous {
        Some(previous) => match try_reuse_terrain_records(ctx, previous, state, &plan, &work, base_payload, diff) {
            Ok(records) => {
                metrics.terrain.reuse_mode = TerrainReuseMode::PartialRebuild;
                Some(records)
            }
            Err(reason) => {
                crate::logging::info!("Terrain record reuse unavailable ({reason}); rebuilding every mesh work item.");
                metrics.terrain.reuse_mode = TerrainReuseMode::FullRebuild;
                metrics.terrain.reuse_fallback_reason = Some(reason);
                None
            }
        },
        None => {
            metrics.terrain.reuse_mode = TerrainReuseMode::FullRebuild;
            metrics.terrain.reuse_fallback_reason = Some(if force_rebuild {
                TERRAIN_REUSE_FORCED.to_owned()
            } else {
                TERRAIN_REUSE_NO_PREVIOUS_STATE.to_owned()
            });
            None
        }
    };

    let dds = if previous.is_some_and(|previous| previous.terrain_dds_domain_digest == state.terrain_dds_domain_digest)
        && dds_inventory.len() == 5
    {
        metrics.terrain.dds_carried = true;
        TerrainDdsPlan::Carry(dds_inventory)
    } else {
        metrics.terrain.dds_carried = false;
        TerrainDdsPlan::Rebuild
    };

    Ok(TerrainStagePlan {
        mode: TerrainPublishMode::Enabled {
            mesh: TerrainMeshPlan::Assemble {
                plan: Box::new(plan),
                work,
                reused,
                dirty_chunks: diff.dirty_terrain_chunks.iter().copied().collect(),
                dds,
            },
            occlusion,
        },
    })
}

/// Stable reason codes reported when record reuse is rejected.
const TERRAIN_REUSE_FORCED: &str = "force_rebuild";
const TERRAIN_REUSE_NO_PREVIOUS_STATE: &str = "no_comparable_previous_state";

/// Validates and decodes the committed `terrain.bin` into reusable records.
///
/// Fails closed: every rejection returns a stable reason and leaves the caller performing a full
/// mesh rebuild rather than failing an otherwise regenerable run.
fn try_reuse_terrain_records(
    ctx: &StageContext<'_>,
    previous: &GenerationState,
    state: &GenerationState,
    plan: &TerrainPackagePlan<'_>,
    work: &[TerrainMeshWorkItem],
    base_payload: Option<&RequiredArtifact>,
    diff: &GenerationDiff,
) -> std::result::Result<HashMap<TerrainMeshWorkKey, TerrainMesh>, String> {
    let _span = info_span!("terrain.validate_reusable_records", report = true).entered();
    if !previous.terrain_enabled || !state.terrain_enabled {
        return Err("terrain_disabled_in_state".to_owned());
    }
    if diff.terrain_global_changed || previous.terrain_global != state.terrain_global {
        return Err("terrain_global_changed".to_owned());
    }
    if diff.terrain_cells_added_or_removed {
        return Err("terrain_cells_added_or_removed".to_owned());
    }
    let payload = base_payload.ok_or_else(|| "terrain_payload_absent".to_owned())?;

    let current_keys: HashSet<TerrainMeshWorkKey> = work.iter().map(|item| item.key).collect();
    if current_keys.len() != work.len() {
        return Err("duplicate_current_work_key".to_owned());
    }
    if !previous.terrain_records.iter().all(|key| current_keys.contains(key)) {
        return Err("record_key_not_in_current_work_set".to_owned());
    }

    let path = ctx.output_paths.terrain_path.clone();
    let file = std::fs::File::open(&path).map_err(|error| format!("terrain_payload_unreadable: {error}"))?;
    // SAFETY: The map is read-only and lives only for this decode. The exclusive writer session
    // prevents replacement or mutation of `terrain.bin` while the map is live.
    let bytes =
        unsafe { memmap2::MmapOptions::new().map(&file) }.map_err(|error| format!("terrain_payload_unreadable: {error}"))?;
    if bytes.len() as u64 != payload.byte_length {
        return Err("terrain_payload_length_mismatch".to_owned());
    }
    if *blake3::hash(&bytes).as_bytes() != payload.content_blake3 {
        return Err("terrain_payload_hash_mismatch".to_owned());
    }
    let file = deserialize_terrain_file(&bytes).map_err(|error| format!("terrain_payload_undecodable: {error}"))?;
    if !file.header_matches(&plan.assemble_terrain_file(Vec::new())) {
        return Err("terrain_header_facts_changed".to_owned());
    }
    if file.meshes.len() != previous.terrain_records.len() {
        return Err("record_count_mismatch".to_owned());
    }

    Ok(previous.terrain_records.iter().copied().zip(file.meshes).collect())
}

/// Publishes occlusion and either carries or rebuilds the terrain package.
pub(super) fn publish_terrain_stage<'a>(
    plan: TerrainStagePlan<'a>,
    ctx: &StageContext<'a>,
    usage_info: &UsageInfo<'a>,
    writes: &PublicationWrites,
    metrics: &mut GenerationMetrics,
    cache: &mut CacheMetadata,
    reporter: &mut dyn ProgressReporter,
) -> Result<TerrainPublishResult> {
    let TerrainPublishMode::Enabled { mesh, occlusion } = plan.mode else {
        return Ok(TerrainPublishResult {
            artifacts: Vec::new(),
            writer: None,
            emitted_keys: None,
        });
    };
    let output = ctx.output_paths;
    let mut artifacts = Vec::new();
    match occlusion {
        OcclusionPlan::Carry(committed) => {
            cache.writes.terrain_occlusion = Some(OutputWriteDecision::SkippedUnchanged);
            artifacts.push(committed);
        }
        OcclusionPlan::Write { bytes, .. } => {
            let written = writes.write_durable(&output.terrain_occlusion_path, &bytes, SyncClass::SmallArtifact)?;
            cache.writes.terrain_occlusion = Some(OutputWriteDecision::Written);
            artifacts.push(artifact_from_written(
                ArtifactKind::TerrainOcclusion,
                "terrain_occlusion.bin",
                written.byte_length,
                written.content_blake3,
            ));
        }
    }

    match mesh {
        TerrainMeshPlan::CarryPackage {
            payload,
            inventory,
            emitted_keys,
        } => {
            artifacts.push(payload);
            artifacts.extend(inventory);
            set_terrain_write_decisions(cache, OutputWriteDecision::SkippedUnchanged);
            Ok(TerrainPublishResult {
                artifacts,
                writer: None,
                emitted_keys: Some(emitted_keys),
            })
        }
        TerrainMeshPlan::Assemble {
            mut plan,
            work,
            reused,
            dirty_chunks,
            dds,
        } => run_stage(GenerationStage::WriteTerrainPackage, reporter, || {
            // `run_stage` only drives the progress reporter; the report's stage timings come
            // from spans named `stage.*`. Without this one, the terrain mesh build is absent from
            // the reported timeline. It is the single largest CPU consumer in a full rebuild.
            // The span deliberately ends at the spawn below: the writer thread's own work is
            // tracked by `io.write_terrain_package_async` and awaited under `stage.await_terrain_writer`.
            let _stage_guard = info_span!("stage.write_terrain_package", report = true).entered();
            let (terrain_file, emitted_keys) =
                assemble_reused_terrain_file(&plan, usage_info, &work, reused, &dirty_chunks, metrics)?;
            let (mesh_count, vertex_count, triangle_count) = summarize_terrain_file(&terrain_file);
            metrics.terrain.terrain_mesh_count = mesh_count;
            metrics.terrain.terrain_mesh_vertex_count = vertex_count;
            metrics.terrain.terrain_mesh_triangle_count = triangle_count;

            let images = match &dds {
                TerrainDdsPlan::Carry(_) => None,
                TerrainDdsPlan::Rebuild => Some(plan.build_dds_images()),
            };
            match &dds {
                TerrainDdsPlan::Carry(inventory) => {
                    artifacts.extend(inventory.iter().cloned());
                    set_dds_write_decisions(cache, OutputWriteDecision::SkippedUnchanged);
                }
                TerrainDdsPlan::Rebuild => set_dds_write_decisions(cache, OutputWriteDecision::Written),
            }
            cache.writes.terrain_bin = Some(OutputWriteDecision::Written);

            let output = output.clone();
            let writes = writes.clone();
            let handle = std::thread::spawn(move || {
                let span = info_span!(
                    "io.write_terrain_package_async",
                    report = true,
                    path = field::display(output.terrain_path.display()),
                    serialization_us = field::Empty,
                );
                let _guard = span.enter();
                let serialize_started = Instant::now();
                let terrain_bytes = crate::mge_xe::distant_terrain::serialize_terrain_file(&terrain_file)?;
                span.record(
                    "serialization_us",
                    u64::try_from(serialize_started.elapsed().as_micros()).unwrap_or(u64::MAX),
                );
                let terrain = writes.write_durable(&output.terrain_path, terrain_bytes, SyncClass::Payload)?;
                let mut package_artifacts = vec![artifact_from_written(
                    ArtifactKind::TerrainPayload,
                    "terrain.bin",
                    terrain.byte_length,
                    terrain.content_blake3,
                )];

                if let Some(images) = images {
                    // Representative interruption point: new-generation payloads are already on
                    // disk under an invalidated state, and publication has not begun.
                    fault::exit_if_requested("mid_terrain_dds");
                    let mut atlas = writes.create_durable(&output.terrain_atlas_path, SyncClass::Payload)?;
                    write_bc1_dds_from_chain_unflipped(&images.terrain_atlas, &mut atlas)?;
                    package_artifacts.push(written_dds("terrain_atlas.dds", atlas.finish()?));

                    let mut material = writes.create_durable(&output.terrain_material_path, SyncClass::Payload)?;
                    write_rgba8_dds_unflipped(images.terrain_material, &mut material, false)?;
                    package_artifacts.push(written_dds("terrain_material.dds", material.finish()?));

                    let mut flags = writes.create_durable(&output.terrain_material_flags_path, SyncClass::Payload)?;
                    write_rgba8_dds_unflipped(images.terrain_material_flags, &mut flags, false)?;
                    package_artifacts.push(written_dds("terrain_material_flags.dds", flags.finish()?));

                    let mut albedo = writes.create_durable(&output.terrain_patch_albedo_path, SyncClass::Payload)?;
                    write_bc1_dds_unflipped(images.terrain_patch_albedo, &mut albedo, true)?;
                    package_artifacts.push(written_dds("terrain_patch_albedo.dds", albedo.finish()?));

                    let mut patterns = writes.create_durable(&output.terrain_blend_patterns_path, SyncClass::Payload)?;
                    write_rgba8_dds_unflipped(images.terrain_blend_patterns, &mut patterns, false)?;
                    package_artifacts.push(written_dds("terrain_blend_patterns.dds", patterns.finish()?));
                }

                Ok(TerrainPackageArtifacts {
                    artifacts: package_artifacts,
                    emitted_keys,
                })
            });
            Ok(TerrainPublishResult {
                artifacts,
                writer: Some(handle),
                emitted_keys: None,
            })
        }),
    }
}

/// Builds only dirty work, merges it with carried records and absences, and assembles the file.
///
/// Returns the file plus the work key of every record it emitted, in file order.
fn assemble_reused_terrain_file<'a>(
    plan: &TerrainPackagePlan<'a>,
    usage_info: &UsageInfo<'a>,
    work: &[TerrainMeshWorkItem],
    reused: Option<HashMap<TerrainMeshWorkKey, TerrainMesh>>,
    dirty_chunks: &HashSet<TerrainChunkUnitKey>,
    metrics: &mut GenerationMetrics,
) -> Result<(TerrainFile, Vec<TerrainMeshWorkKey>)> {
    let mut results: Vec<TerrainMeshWorkResult> = Vec::with_capacity(work.len());
    let rebuild: Vec<TerrainMeshWorkItem> = match reused {
        None => {
            metrics.terrain.rebuilt_work_items = work.len();
            work.to_vec()
        }
        Some(mut records) => {
            let mut rebuild = Vec::new();
            for item in work {
                if item.depends_on_any(dirty_chunks) {
                    rebuild.push(item.clone());
                    continue;
                }
                match records.remove(&item.key) {
                    Some(mesh) => {
                        metrics.terrain.reused_records += 1;
                        results.push(TerrainMeshWorkResult {
                            key: item.key,
                            mesh: Some(mesh),
                        });
                    }
                    None => {
                        metrics.terrain.reused_absent_work_items += 1;
                        results.push(TerrainMeshWorkResult {
                            key: item.key,
                            mesh: None,
                        });
                    }
                }
            }
            metrics.terrain.rebuilt_work_items = rebuild.len();
            rebuild
        }
    };

    // The builder now smooths normals only for the cells the rebuilt work items can sample, but
    // there is still no reason to construct it (or derive target keys) when nothing is dirty. A
    // terrain-texture edit dirties cells but no chunk, leaving zero work items to rebuild.
    if !rebuild.is_empty() {
        let builder = TerrainMeshBuilder::new(usage_info, plan.mesh_simplifier_config(), &rebuild);
        results.extend(builder.build_all(&rebuild));
    }
    let set = assemble_terrain_mesh_set(results)?;
    metrics.terrain.rebuilt_keys = bounded_keys(
        rebuild.iter().map(|item| item.key),
        &mut metrics.terrain.rebuilt_keys_truncated,
    );
    Ok((plan.assemble_terrain_file(set.meshes), set.emitted_keys))
}

/// Bounded logical key list for observability, mirroring the unit report's truncation rule.
fn bounded_keys(keys: impl Iterator<Item = TerrainMeshWorkKey>, truncated: &mut bool) -> Vec<TerrainMeshWorkKey> {
    let mut keys: Vec<_> = keys.collect();
    keys.sort_unstable();
    *truncated = keys.len() > TERRAIN_KEY_REPORT_LIMIT;
    keys.truncate(TERRAIN_KEY_REPORT_LIMIT);
    keys
}

/// Bound on the logical key lists reported in metrics.
const TERRAIN_KEY_REPORT_LIMIT: usize = 64;

pub(super) fn set_terrain_write_decisions(cache: &mut CacheMetadata, decision: OutputWriteDecision) {
    cache.writes.terrain_bin = Some(decision);
    set_dds_write_decisions(cache, decision);
}

pub(super) fn set_dds_write_decisions(cache: &mut CacheMetadata, decision: OutputWriteDecision) {
    cache.writes.terrain_atlas = Some(decision);
    cache.writes.terrain_material = Some(decision);
    cache.writes.terrain_material_flags = Some(decision);
    cache.writes.terrain_patch_albedo = Some(decision);
    cache.writes.terrain_blend_patterns = Some(decision);
}

fn written_dds(relative_path: &str, written: super::WrittenFile) -> RequiredArtifact {
    artifact_from_written(
        ArtifactKind::TerrainDds,
        relative_path,
        written.byte_length,
        written.content_blake3,
    )
}
