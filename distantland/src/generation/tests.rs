use std::borrow::Cow;
use std::path::{Path, PathBuf};

use glam::{Vec2, Vec3};
use itertools::Itertools;

use super::cache::fingerprint_static_meshes_inputs;
use super::statics_stage::finalize_distant_statics;
use super::*;

#[test]
fn terrain_detail_presets_keep_current_meshopt_target_errors() {
    let expectations = [
        (TerrainDetail::UltraHigh, 15.0),
        (TerrainDetail::VeryHigh, 64.0),
        (TerrainDetail::High, 128.0),
        (TerrainDetail::Medium, 192.0),
        (TerrainDetail::Low, 256.0),
    ];

    let mut previous = None;
    for (detail, target_error) in expectations {
        assert_eq!(detail.target_error(), target_error);
        assert_eq!(
            crate::terrain::mesh::selected_mesh_simplifier_config(
                detail.target_error(),
                crate::terrain::mesh::MeshSimplifierWeights {
                    smoothed_normal: crate::DEFAULT_TERRAIN_MESH_SMOOTHED_NORMAL_WEIGHT,
                    color: crate::DEFAULT_TERRAIN_MESH_COLOR_WEIGHT,
                },
            )
            .target_error,
            target_error
        );
        if let Some(previous) = previous {
            assert!(target_error > previous);
        }
        previous = Some(target_error);
    }
}

#[test]
fn generation_settings_derive_target_error_from_detail() {
    let settings = GenerationSettings::default();

    assert_eq!(settings.terrain_mesh_target_error(), TerrainDetail::High.target_error());
}

#[test]
fn generation_job_json_uses_defaults() {
    let job: GenerationJob = serde_json::from_str(
        r#"{
                "plugins": ["Morrowind.esm"],
                "settings": {
                    "min_static_size": 512.0,
                    "terrain_detail": "low"
                }
            }"#,
    )
    .unwrap();

    assert_eq!(job.morrowind_ini, None);
    assert_eq!(job.data_dirs, None);
    assert_eq!(job.plugins, Some(vec![PathBuf::from("Morrowind.esm")]));
    assert_eq!(job.output_root, None);
    assert_eq!(job.settings.min_static_size, 512.0);
    assert_eq!(job.settings.terrain_detail, TerrainDetail::Low);
    assert_eq!(job.settings.grass_density, 1.0);
    assert!(!job.settings.force_rebuild);
    assert_eq!(
        job.settings.max_terrain_control_texture_size,
        crate::MAX_TERRAIN_CONTROL_TEXTURE_SIZE
    );
    assert_eq!(
        job.settings.max_terrain_control_texture_bytes,
        crate::MAX_TERRAIN_CONTROL_TEXTURE_BYTES
    );
}

#[test]
fn generation_job_deserializes_grass_density() {
    let job: GenerationJob = serde_json::from_str(
        r#"{
                "settings": {
                    "grass_density": 0.25
                }
            }"#,
    )
    .unwrap();

    assert_eq!(job.settings.grass_density, 0.25);
}

#[test]
fn generation_job_rejects_invalid_grass_density() {
    let mut job = GenerationJob::default();
    job.settings.grass_density = 1.5;

    let error = job.validate_for_generation().unwrap_err().to_string();
    assert!(error.contains("grass_density"));
}

#[test]
fn generation_job_rejects_zero_terrain_control_texture_caps() {
    let mut job = GenerationJob::default();
    job.settings.max_terrain_control_texture_size = 0;
    assert!(
        job.validate_for_generation()
            .unwrap_err()
            .to_string()
            .contains("max_terrain_control_texture_size")
    );

    job.settings.max_terrain_control_texture_size = crate::MAX_TERRAIN_CONTROL_TEXTURE_SIZE;
    job.settings.max_terrain_control_texture_bytes = 0;
    assert!(
        job.validate_for_generation()
            .unwrap_err()
            .to_string()
            .contains("max_terrain_control_texture_bytes")
    );
}

#[test]
fn generation_job_rejects_duplicate_plugin_filenames() {
    let job = GenerationJob {
        plugins: Some(vec![
            PathBuf::from(r"C:\Mods\First\Morrowind.esm"),
            PathBuf::from(r"C:\Mods\Second\morrowind.esm"),
        ]),
        ..GenerationJob::default()
    };

    let error = job.validate_for_generation().unwrap_err().to_string();
    assert!(error.contains("duplicate filenames"));
}

#[test]
fn output_paths_match_mge_xe_contract() {
    let paths = OutputPaths::new(PathBuf::from(r"C:\Test\Data Files"));

    assert_eq!(paths.version_path, PathBuf::from(r"C:\Test\Data Files\distantland\version"));
    assert_eq!(
        paths.terrain_path,
        PathBuf::from(r"C:\Test\Data Files\distantland\terrain.bin")
    );
    assert_eq!(
        paths.terrain_occlusion_path,
        PathBuf::from(r"C:\Test\Data Files\distantland\terrain_occlusion.bin")
    );
    assert_eq!(
        paths.terrain_atlas_path,
        PathBuf::from(r"C:\Test\Data Files\distantland\terrain_atlas.dds")
    );
    assert_eq!(
        paths.terrain_material_path,
        PathBuf::from(r"C:\Test\Data Files\distantland\terrain_material.dds")
    );
    assert_eq!(
        paths.terrain_material_flags_path,
        PathBuf::from(r"C:\Test\Data Files\distantland\terrain_material_flags.dds")
    );
    assert_eq!(
        paths.terrain_patch_albedo_path,
        PathBuf::from(r"C:\Test\Data Files\distantland\terrain_patch_albedo.dds")
    );
    assert_eq!(
        paths.terrain_blend_patterns_path,
        PathBuf::from(r"C:\Test\Data Files\distantland\terrain_blend_patterns.dds")
    );
    assert_eq!(
        paths.usage_data_path,
        PathBuf::from(r"C:\Test\Data Files\distantland\statics\usage.data")
    );
    assert_eq!(
        paths.static_mesh_shard_paths[0],
        PathBuf::from(r"C:\Test\Data Files\distantland\statics\static_meshes_000")
    );
    assert_eq!(
        paths.atlas_texture_dir,
        PathBuf::from(r"C:\Test\Data Files\distantland\statics\textures")
    );
}

#[test]
fn output_contract_validation_reports_missing_required_files() {
    let dir = tempfile::tempdir().unwrap();
    let output_paths = OutputPaths::new(dir.path());
    output_paths.ensure_parent_dirs().unwrap();

    fs::write(&output_paths.version_path, [MGE_DL_VERSION]).unwrap();

    let validation = output_paths.validate_mge_xe_contract();

    assert!(!validation.complete);
    assert!(
        validation
            .required_files
            .iter()
            .any(|file| file.relative_path == r"distantland\version" && file.exists)
    );
    assert!(
        validation
            .issues
            .iter()
            .any(|issue| issue.relative_path == r"distantland\generation_state.bin")
    );
    assert!(
        validation
            .issues
            .iter()
            .any(|issue| issue.relative_path == r"distantland\statics\usage.data")
    );
    assert!(
        validation
            .issues
            .iter()
            .any(|issue| issue.relative_path == r"distantland\statics\static_meshes_000")
    );
}

#[test]
fn output_contract_is_complete_with_required_files() {
    let dir = tempfile::tempdir().unwrap();
    let output_paths = OutputPaths::new(dir.path());
    output_paths.ensure_parent_dirs().unwrap();

    fs::write(&output_paths.version_path, [MGE_DL_VERSION]).unwrap();
    fs::write(&output_paths.generation_state_path, b"state").unwrap();
    fs::write(&output_paths.usage_data_path, b"usage").unwrap();
    for path in &output_paths.static_mesh_shard_paths {
        fs::write(path, b"statics").unwrap();
    }

    let validation = output_paths.validate_mge_xe_contract();

    assert!(validation.complete);
    assert!(validation.issues.is_empty());
}

#[test]
fn mge_xe_version_byte_is_current() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("version");
    fs::write(&path, [MGE_DL_VERSION]).unwrap();
    assert_eq!(fs::read(path).unwrap(), [MGE_DL_VERSION]);
}

#[test]
fn generation_report_describes_outputs_and_serializes_without_nulls() {
    let dir = tempfile::tempdir().unwrap();
    let output_paths = OutputPaths::new(dir.path());
    output_paths.ensure_parent_dirs().unwrap();

    fs::write(&output_paths.version_path, [MGE_DL_VERSION]).unwrap();
    fs::write(&output_paths.generation_state_path, b"state").unwrap();
    write_test_terrain_file(&output_paths.terrain_path);
    fs::write(&output_paths.terrain_occlusion_path, b"occlusion").unwrap();
    fs::write(&output_paths.terrain_atlas_path, b"atlas").unwrap();
    fs::write(&output_paths.terrain_material_path, b"material").unwrap();
    fs::write(&output_paths.terrain_material_flags_path, b"flags").unwrap();
    fs::write(&output_paths.terrain_patch_albedo_path, b"patch").unwrap();
    fs::write(&output_paths.terrain_blend_patterns_path, b"patterns").unwrap();
    fs::write(&output_paths.usage_data_path, b"usage").unwrap();
    for path in &output_paths.static_mesh_shard_paths {
        fs::write(path, b"statics").unwrap();
    }
    fs::write(output_paths.atlas_texture_dir.join("_mge_xe_atlas.dds"), b"atlas").unwrap();

    let job = GenerationJob {
        output_root: Some(output_paths.output_root.clone()),
        ..GenerationJob::default()
    };
    let metrics = sample_generation_metrics();
    let warnings = vec![];

    let report = build_generation_report_data(
        &job,
        &output_paths,
        TraceSummary {
            total_elapsed_ms: 7,
            stage_timings: vec![crate::StageTiming {
                stage: "stage.write_terrain_package".to_string(),
                elapsed_ms: 7,
                memory: None,
            }],
        },
        &CacheMetadata::default(),
        &metrics,
        &warnings,
        "load-order-fingerprint",
        &UnitsReport::default(),
        Some("settings changed"),
    )
    .unwrap();

    assert!(report.mge_xe_contract_complete);
    assert!(report.mge_xe_contract.complete);
    assert_eq!(report.report_version, GENERATION_REPORT_VERSION);
    assert_eq!(report.rebuild_cause.as_deref(), Some("settings changed"));
    assert_eq!(report.metrics.statics.merge_simplification.group_count, 42);
    assert_eq!(report.trace_summary.total_elapsed_ms, 7);
    assert_eq!(report.trace_summary.stage_timings.len(), 1);
    assert_eq!(report.warnings, warnings);

    assert_eq!(report.metrics.gpu_memory, metrics.gpu_memory);

    let encoded = toml::to_string_pretty(&report).unwrap();
    assert!(encoded.contains("[metrics.gpu_memory]"));
    let decoded: GenerationReportData = toml::from_str(&encoded).unwrap();
    assert_eq!(decoded.metrics.gpu_memory, metrics.gpu_memory);
    assert_eq!(decoded.metrics.gpu_memory.total_bytes(), 4_647_458_176);
    assert!(!encoded.contains("null"));
    assert!(!encoded.contains("morrowind_ini"));
    assert!(!encoded.contains("cached_opaque_page_count"));
    assert!(!encoded.contains("cached_alpha_page_count"));
}

#[test]
fn generation_report_loader_round_trips_a_current_report() {
    let dir = tempfile::tempdir().unwrap();
    let output_paths = OutputPaths::new(dir.path());
    output_paths.ensure_parent_dirs().unwrap();

    let report = build_generation_report_data(
        &GenerationJob {
            output_root: Some(output_paths.output_root.clone()),
            ..GenerationJob::default()
        },
        &output_paths,
        TraceSummary::default(),
        &CacheMetadata::default(),
        &sample_generation_metrics(),
        &[],
        "load-order-fingerprint",
        &UnitsReport::default(),
        None,
    )
    .unwrap();
    std::fs::write(&output_paths.generation_report_path, toml::to_string_pretty(&report).unwrap()).unwrap();

    let loaded = load_generation_report_data(&output_paths.generation_report_path).unwrap();

    assert_eq!(loaded.report_version, GENERATION_REPORT_VERSION);
    assert_eq!(loaded.metrics.gpu_memory, report.metrics.gpu_memory);
}

#[test]
fn generation_report_loader_rejects_a_report_without_gpu_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("generation_report.toml");

    let mut encoded = toml::to_string_pretty(&sample_generation_metrics()).unwrap();
    let gpu_memory_start = encoded.find("[gpu_memory]").unwrap();
    encoded.truncate(gpu_memory_start);
    std::fs::write(&path, &encoded).unwrap();

    assert!(toml::from_str::<GenerationMetrics>(&encoded).is_err());
    assert!(load_generation_report_data(&path).is_err());
}

#[test]
fn generation_report_loader_reports_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();

    assert!(load_generation_report_data(&dir.path().join("absent.toml")).is_err());
}

#[test]
fn plain_observability_write_skips_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("generation_report.toml");

    let first = cache::write_plain_bytes_if_changed(&output_path, b"same").unwrap();
    let second = cache::write_plain_bytes_if_changed(&output_path, b"same").unwrap();

    assert_eq!(first, OutputWriteDecision::Written);
    assert_eq!(second, OutputWriteDecision::SkippedUnchanged);
}

#[test]
fn static_mesh_input_fingerprint_tracks_order() {
    use crate::mge_xe::distant_statics::PackedDistantStatic;

    let first: crate::PackedDistantStatics = [
        ("a.nif".to_string(), PackedDistantStatic::default()),
        ("b.nif".to_string(), PackedDistantStatic::default()),
    ]
    .into_iter()
    .collect();
    let second: crate::PackedDistantStatics = [
        ("b.nif".to_string(), PackedDistantStatic::default()),
        ("a.nif".to_string(), PackedDistantStatic::default()),
    ]
    .into_iter()
    .collect();

    assert_ne!(
        fingerprint_static_meshes_inputs(&first),
        fingerprint_static_meshes_inputs(&second)
    );
}

#[test]
fn finalized_static_order_is_shard_major_and_stabilizes_usage_indices() {
    let vfs = crate::Vfs {
        ini_path: PathBuf::from("Morrowind.ini"),
        data_dirs: vec![],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::DirectoryMaps::default(),
    };
    let finalized = finalize_distant_statics(
        [
            ("b.nif".to_string(), crate::DistantStatic::default()),
            ("a.nif".to_string(), crate::DistantStatic::default()),
        ]
        .into_iter()
        .collect(),
        &vfs,
        1.0,
    );

    assert_eq!(finalized.keys().map(String::as_str).collect_vec(), vec!["a.nif", "b.nif"]);

    let mut usage = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (
            crate::StableRefKey::test(1),
            crate::DistantReference {
                id: Cow::Borrowed("a.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::ZERO,
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
        (
            crate::StableRefKey::test(2),
            crate::DistantReference {
                id: Cow::Borrowed("b.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::ZERO,
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
    ]);
    usage.sort_for_deterministic_output();

    let ordinals = crate::usage::StaticOrdinalView::from_packed(&finalized);
    let bytes = crate::usage::serialize_usage_data(
        &usage,
        &ordinals,
        &crate::DynamicVisData::default(),
        crate::GenerationSettings::default().min_static_size,
    )
    .unwrap();

    assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(bytes[48..52].try_into().unwrap()), 1);
}

fn sample_generation_metrics() -> GenerationMetrics {
    let mut metrics = GenerationMetrics::default();
    metrics.usage.terrain_cell_count = 5_081;
    metrics.terrain.estimated_control_texture_bytes = 11_361_280;
    metrics.terrain.estimated_source_atlas_bytes = 44_040_192;
    metrics.statics.merge_simplification.group_count = 42;
    metrics.gpu_memory = crate::DistantLandGpuMemoryEstimate {
        static_geometry_bytes: 3_886_192_000,
        static_texture_bytes: 242_221_056,
        terrain_geometry_bytes: 456_130_560,
        terrain_texture_bytes: 62_914_560,
    };
    metrics
}

#[test]
fn static_metrics_default_merge_diagnostics_when_deserializing() {
    let mut value = serde_json::to_value(StaticMetrics::default()).unwrap();
    value.as_object_mut().unwrap().remove("merge_simplification");

    let decoded: StaticMetrics = serde_json::from_value(value).unwrap();

    assert_eq!(decoded.merge_simplification, crate::MergeSimplificationMetrics::default());
}

fn write_test_terrain_file(path: &Path) {
    let terrain = crate::mge_xe::distant_terrain::TerrainFile {
        cell_size: 8_192.0,
        patch_size: 512.0,
        origin_cell: [-40, -32],
        cell_size_xy: [80, 64],
        world_origin: Vec2::new(-327_680.0, -262_144.0),
        world_size: Vec2::new(655_360.0, 524_288.0),
        atlas_size: 8_192,
        logical_tile_size: 256,
        gutter_size: 16,
        physical_tile_size: 288,
        tiles_per_row: 28,
        atlas_max_lod: 2,
        material_size_xy: [1_280, 1_024],
        pattern_count: 11,
        pattern_tile_size: 32,
        pattern_gutter_size: 2,
        pattern_physical_size: 36,
        patterns_per_row: 4,
        meshes: vec![],
    };
    let bytes = crate::mge_xe::distant_terrain::serialize_terrain_file(&terrain).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn generate_with_output_root_refuses_a_complete_future_tree_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &root.path().join("inputs"))
            .unwrap();
    let output_root = root.path().join("output");
    let job = crate::test_support::hermetic_generation_job(&fixture, &output_root);
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(&output_root);
    std::fs::write(&paths.version_path, [MGE_DL_VERSION + 1]).unwrap();
    let before = snapshot_future_tree(&paths.distantland_dir);
    let error = generate_with_output_root(&job, None, &mut NullProgressReporter).unwrap_err();

    assert!(
        format!("{error:#}").contains(&format!("unsupported distant-land output version {}", MGE_DL_VERSION + 1)),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        snapshot_future_tree(&paths.distantland_dir),
        before,
        "Publish(None) changed the future-version tree"
    );
    assert!(before.contains_key(Path::new(".writer.lock")));
    assert_eq!(before.get(Path::new("version")), Some(&vec![MGE_DL_VERSION + 1]));
}

#[test]
fn second_in_process_cached_generate_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &root.path().join("inputs"))
            .unwrap();
    let output_root = root.path().join("output");
    let mut job = crate::test_support::hermetic_generation_job(&fixture, &output_root);
    // Seed with an explicit force-rebuild so the committed settings identity must not trap a
    // later cached generate into republishing state.
    job.settings.force_rebuild = true;

    let first = generate(&job, &mut NullProgressReporter).unwrap();
    let published = snapshot_tree_identity(&output_root);
    assert!(
        !published.is_empty(),
        "forced first build published nothing; report_written={:?}, writes={:?}",
        first.report_written,
        first.report.cache.writes
    );

    job.settings.force_rebuild = false;
    let second = generate(&job, &mut NullProgressReporter).unwrap();

    // The cached run must decide to skip every output, the advisory report included.
    assert_eq!(
        second.report_written,
        OutputWriteDecision::SkippedUnchanged,
        "cached generate rewrote the generation report"
    );
    let writes = &second.report.cache.writes;
    // Pin the four decisions a terrain-enabled no-op always records, so the sweep below cannot
    // pass merely because every decision was left unset.
    for (name, decision) in [
        ("usage.data", writes.usage_data),
        ("static_meshes", writes.static_meshes),
        ("terrain.bin", writes.terrain_bin),
        ("terrain_occlusion.bin", writes.terrain_occlusion),
    ] {
        assert_eq!(
            decision,
            Some(OutputWriteDecision::SkippedUnchanged),
            "cached generate did not record {name} as skipped"
        );
    }
    for (name, decision) in [
        ("usage.data", writes.usage_data),
        ("static_meshes", writes.static_meshes),
        ("terrain.bin", writes.terrain_bin),
        ("terrain_occlusion.bin", writes.terrain_occlusion),
        ("terrain_atlas.dds", writes.terrain_atlas),
        ("terrain_material.dds", writes.terrain_material),
        ("terrain_material_flags.dds", writes.terrain_material_flags),
        ("terrain_patch_albedo.dds", writes.terrain_patch_albedo),
        ("terrain_blend_patterns.dds", writes.terrain_blend_patterns),
    ] {
        assert_ne!(decision, Some(OutputWriteDecision::Written), "cached generate rewrote {name}");
    }

    // Independently confirm against the disk. Comparing modification times as well as bytes
    // catches a redundant durable write that happens to reproduce the same content, which is
    // exactly the regression the skip-if-unchanged paths exist to prevent.
    assert_eq!(
        snapshot_tree_identity(&output_root),
        published,
        "cached generate mutated the published output tree"
    );
}

#[test]
fn reported_gpu_memory_matches_the_published_output_tree() {
    fn file_len(path: &Path) -> u64 {
        fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
    }

    let root = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &root.path().join("inputs"))
            .unwrap();
    let output_root = root.path().join("output");
    let job = crate::test_support::hermetic_generation_job(&fixture, &output_root);

    let generated = generate(&job, &mut NullProgressReporter).unwrap();
    let gpu_memory = generated.report.metrics.gpu_memory;
    let paths = OutputPaths::new(&output_root);

    let static_geometry: u64 = paths.static_mesh_shard_paths.iter().map(|path| file_len(path)).sum();
    let static_textures: u64 = fs::read_dir(&paths.atlas_texture_dir)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    let terrain_textures: u64 = [
        &paths.terrain_atlas_path,
        &paths.terrain_material_path,
        &paths.terrain_material_flags_path,
        &paths.terrain_patch_albedo_path,
        &paths.terrain_blend_patterns_path,
    ]
    .into_iter()
    .map(|path| file_len(path))
    .sum();

    assert_eq!(gpu_memory.static_geometry_bytes, static_geometry);
    assert_eq!(gpu_memory.static_texture_bytes, static_textures);
    assert_eq!(gpu_memory.terrain_geometry_bytes, file_len(&paths.terrain_path));
    assert_eq!(gpu_memory.terrain_texture_bytes, terrain_textures);
    assert!(gpu_memory.total_bytes() > 0);
    // The four equalities above also pin the exclusions: usage.data, the version byte, and
    // occlusion exist in this tree but contribute to none of the counted categories.
    assert!(file_len(&paths.usage_data_path) > 0);
    assert!(file_len(&paths.terrain_occlusion_path) > 0);
}

/// Snapshots every published file as `(length, modification time, bytes)`.
///
/// Skips `.writer.lock`, whose whole purpose is to be opened by every run.
fn snapshot_tree_identity(root: &Path) -> std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime, Vec<u8>)> {
    fn visit(
        root: &Path,
        dir: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime, Vec<u8>)>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else if path.file_name().is_some_and(|name| name != ".writer.lock") {
                let metadata = std::fs::metadata(&path).unwrap();
                let bytes = std::fs::read(&path).unwrap();
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    (metadata.len(), metadata.modified().unwrap(), bytes),
                );
            }
        }
    }

    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn snapshot_future_tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), std::fs::read(path).unwrap());
            }
        }
    }

    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}
