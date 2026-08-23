//! End-to-end integration tests for the `distantland` crate.

use dedent::dedent;
use distantland::mge_xe::distant_statics::StaticType;
use distantland::{DynamicVisKind, parse_overrides_texts};
use uncased::AsUncased;

/// Verifies that all public generation-API symbols are accessible from external crates.
///
/// Uses function-pointer coercions to confirm that the expected signatures are exported
/// without running the full generation pipeline.
#[test]
fn startup_generation_api_is_public() {
    use distantland::{
        GenerationJob, GenerationOutcome, GenerationReport, NullProgressReporter, OutputStatus, ProgressReporter,
        check_output_status, ensure_generated, format_output_status_issues, generate, load_generation_job_file,
        resolve_generation_job_paths, serialize_generation_job_document,
    };
    use std::path::Path;

    let _check: fn(&GenerationJob) -> anyhow::Result<OutputStatus> = check_output_status;
    let _ensure: fn(&GenerationJob, &mut dyn ProgressReporter) -> anyhow::Result<GenerationOutcome> = ensure_generated;
    let _generate: fn(&GenerationJob, &mut dyn ProgressReporter) -> anyhow::Result<GenerationReport> = generate;
    let _load: fn(&Path) -> anyhow::Result<GenerationJob> = load_generation_job_file;
    let _serialize: fn(&GenerationJob) -> anyhow::Result<String> = serialize_generation_job_document;
    let _resolve: fn(&mut GenerationJob, &Path) = resolve_generation_job_paths;
    let _format: fn(&OutputStatus) -> String = format_output_status_issues;
    let _reporter = NullProgressReporter;

    GenerationJob::default().validate_for_generation().unwrap();
}

/// Tests override-parsing edge cases: colon-prefixed comment lines, inline trailing comments,
/// backslash-colon escapes in key names, and all three interior-section forms
/// (`enable`, `disable`, and bare-name defaulting to `enable`).
#[test]
fn parse_override_sections_comments_and_escapes() {
    let overrides = parse_overrides_texts(&[dedent!(
        r#"
        : comment line
        Mesh\Path.nif = far no_script : trailing comment

        [names]
        Foo\:Bar = enable
        BareName

        [interiors]
        Cell A = enable
        Cell B = disable
        Cell C

        [dynamic_vis]
        Script\:Id = journal C3_DestroyDagoth 0-20 : trailing comment
        "#
    )])
    .expect("failed to parse override file");

    let mesh = overrides.mesh_overrides.get("mesh\\path.nif").expect("missing mesh override");
    assert!(matches!(mesh.static_type, StaticType::StaticFar));
    assert!(mesh.no_script);

    assert_eq!(overrides.names.get("foo:bar"), Some(&true));
    assert_eq!(overrides.names.get("barename"), Some(&false));

    assert_eq!(overrides.interiors.get("cell a".as_uncased()), Some(&true));
    assert_eq!(overrides.interiors.get("cell b".as_uncased()), Some(&false));
    assert_eq!(overrides.interiors.get("cell c".as_uncased()), Some(&true));

    assert_eq!(overrides.dynamic_vis.scripts.get("script:id"), Some(&1));
    assert!(matches!(
        &overrides.dynamic_vis.groups[0].kind,
        DynamicVisKind::Journal { journal_id, ranges }
            if journal_id == "c3_destroydagoth" && ranges.as_slice() == [(0, 21)]
    ));
}

/// Tests multi-file layering: later files win on mesh and name conflicts, and `unique_object`
/// entries from different files merge their linked-ID sets into a single shared group index.
#[test]
fn parse_override_layers_last_wins_and_dedups_dynamic_vis() {
    let first = dedent!(
        r#"
        mesh.nif = near

        [names]
        Thing = disable

        [dynamic_vis]
        Script1 = global State 0-1
        Door = unique_object LinkedOne
        "#
    );
    let second = dedent!(
        r#"
        mesh.nif = far
        [names]
        Thing = enable

        [dynamic_vis]
        Script2 = global State 0-1
        Door = unique_object LinkedTwo
        "#
    );

    let overrides = parse_overrides_texts(&[first, second]).expect("failed to parse layered overrides");

    let mesh = overrides.mesh_overrides.get("mesh.nif").expect("missing mesh override");
    assert!(matches!(mesh.static_type, StaticType::StaticFar));

    assert_eq!(overrides.names.get("thing"), Some(&true));

    assert_eq!(overrides.dynamic_vis.groups.len(), 2);
    assert_eq!(overrides.dynamic_vis.groups[0].index, 1);
    assert_eq!(overrides.dynamic_vis.groups[1].index, 2);
    assert_eq!(overrides.dynamic_vis.scripts.get("script1"), Some(&1));
    assert_eq!(overrides.dynamic_vis.scripts.get("script2"), Some(&1));

    let linked_door = overrides
        .dynamic_vis
        .unique_objects
        .get("door")
        .copied()
        .expect("missing unique object mapping");
    assert_eq!(linked_door, 2);
    assert_eq!(overrides.dynamic_vis.unique_objects.get("linkedone"), Some(&2));
    assert_eq!(overrides.dynamic_vis.unique_objects.get("linkedtwo"), Some(&2));

    match &overrides.dynamic_vis.groups[1].kind {
        DynamicVisKind::UniqueObject { source_id, linked_ids } => {
            assert_eq!(source_id, "door");
            assert!(linked_ids.contains(&"door".to_string()));
            assert!(linked_ids.contains(&"linkedone".to_string()));
            assert!(linked_ids.contains(&"linkedtwo".to_string()));
        }
        _ => panic!("expected unique object group"),
    }
}

/// Verifies discovery of plugin `-metadata.toml` files through generation and that
/// a same-size content edit with a pinned mtime sets the output status to stale.
#[test]
fn test_metadata_discovery_and_staleness_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("Data Files");
    std::fs::create_dir_all(&data_dir).unwrap();

    let plugin_path = data_dir.join("Foo.esp");
    use tes3::esp::{FileType, Header, Plugin, TES3Object};
    let mut plugin = Plugin::new();
    let mut header = Header::default();
    header.file_type = FileType::Esp;
    plugin.objects.push(TES3Object::Header(header));
    plugin.save_path(&plugin_path).unwrap();

    let ini_path = temp.path().join("Morrowind.ini");
    std::fs::write(&ini_path, "").unwrap();

    let output_root = temp.path().join("output");

    let metadata_path = data_dir.join("Foo-metadata.toml");
    std::fs::write(
        &metadata_path,
        r#"
[tools.mge-xe.distantland]
include_objects = ["special_ref"]
"#,
    )
    .unwrap();

    let job = distantland::GenerationJob {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![data_dir.clone()]),
        plugins: Some(vec![std::path::PathBuf::from("Foo.esp")]),
        grass_plugins: None,
        output_root: Some(output_root.clone()),
        auto_sync_plugins: false,
        settings: distantland::GenerationSettings {
            use_override_list: false,
            use_plugin_metadata: true,
            generate_terrain: true,
            ..distantland::GenerationSettings::default()
        },
    };

    let mut reporter = distantland::NullProgressReporter;
    let outcome = distantland::ensure_generated(&job, &mut reporter).unwrap();
    assert!(matches!(outcome, distantland::GenerationOutcome::Generated { .. }));

    let status = distantland::check_output_status(&job).unwrap();
    assert_eq!(status.kind, distantland::OutputStatusKind::Valid);

    let report_path = output_root.join("distantland").join("generation_report.toml");
    assert!(report_path.exists());
    let report: toml::Value = toml::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert!(report["load_order_identity"].is_str());
    assert_eq!(report["units"]["state"]["status"].as_str(), Some("absent"));
    let generation_state_path = output_root.join("distantland").join("generation_state.bin");
    assert!(generation_state_path.is_file());
    assert!(!output_root.join("distantland").join("generation_index_a.bin").exists());
    assert!(!output_root.join("distantland").join("generation_index_b.bin").exists());
    assert!(!output_root.join("distantland").join("commit_journal.bin").exists());

    let outcome2 = distantland::ensure_generated(&job, &mut reporter).unwrap();
    assert!(matches!(outcome2, distantland::GenerationOutcome::AlreadyValid { .. }));

    let current_time = std::fs::metadata(&metadata_path).unwrap().modified().unwrap();
    std::fs::write(
        &metadata_path,
        r#"
[tools.mge-xe.distantland]
include_objects = ["changed_ref"]
"#,
    )
    .unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&metadata_path).unwrap();
    let times = std::fs::FileTimes::new().set_modified(current_time);
    file.set_times(times).unwrap();
    drop(file);

    let status2 = distantland::check_output_status(&job).unwrap();
    assert_eq!(
        status2.kind,
        distantland::OutputStatusKind::Stale,
        "expected a stale status, got {status2:?}"
    );
    let issues = status2.details.issues;
    assert!(
        issues
            .iter()
            .any(|issue| issue.code == "stale_generation_inputs" || issue.code == "stale_load_order_identity"),
        "issues: {issues:?}"
    );

    let outcome3 = distantland::ensure_generated(&job, &mut reporter).unwrap();
    assert!(matches!(outcome3, distantland::GenerationOutcome::Generated { .. }));

    let report: toml::Value =
        toml::from_str(&std::fs::read_to_string(output_root.join("distantland").join("generation_report.toml")).unwrap())
            .unwrap();
    assert_eq!(report["units"]["state"]["status"].as_str(), Some("loaded"));
    assert!(generation_state_path.is_file());

    let status3 = distantland::check_output_status(&job).unwrap();
    assert_eq!(status3.kind, distantland::OutputStatusKind::Valid);
}
