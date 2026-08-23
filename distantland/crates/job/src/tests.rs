use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    GENERATION_JOB_FILE_VERSION, GENERATION_JOB_NAMESPACE, GenerationJob, GenerationSettings, TerrainDetail,
    load_generation_job_file, load_generation_job_file_with_warnings, resolve_generation_job_paths,
    serialize_generation_job_document, sync_plugins_from_load_order,
};

fn write_generation_job_file(path: &Path, job: &GenerationJob) -> anyhow::Result<()> {
    fs::write(path, serialize_generation_job_document(job)?)?;
    Ok(())
}

#[test]
fn load_generation_job_file_reads_versioned_job() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(
        &path,
        r#"
[generation]
version = 3
plugins = ["Morrowind.esm"]
[generation.settings]
grass_density = 0.5
"#,
    )
    .unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert_eq!(job.plugins, Some(vec![PathBuf::from("Morrowind.esm")]));
    assert_eq!(job.grass_plugins, None);
    assert_eq!(job.settings.grass_density, 0.5);
}

#[test]
fn version_three_round_trips_grass_plugins_in_full_mge_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    let job = GenerationJob {
        plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
        grass_plugins: Some(vec![PathBuf::from("Groundcover.esp")]),
        ..GenerationJob::default()
    };

    write_generation_job_file(&path, &job).unwrap();
    let mut source = fs::read_to_string(&path).unwrap();
    source.push_str("\n[render]\nfov = 75.0\n");
    fs::write(&path, source).unwrap();
    let document: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        document[GENERATION_JOB_NAMESPACE]["version"].as_integer(),
        Some(GENERATION_JOB_FILE_VERSION.into())
    );
    assert_eq!(load_generation_job_file(&path).unwrap().grass_plugins, job.grass_plugins);
}

#[test]
fn serialized_document_is_one_flat_table_plus_settings() {
    let job = GenerationJob {
        plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
        output_root: Some(PathBuf::from("Data Files")),
        ..GenerationJob::default()
    };

    let document = serialize_generation_job_document(&job).unwrap();
    let headers: Vec<&str> = document.lines().map(str::trim).filter(|line| line.starts_with('[')).collect();

    // A nested `job` level would show up here as `[generation.job]`.
    assert_eq!(
        headers,
        vec![
            "[generation]",
            "[generation.settings]",
            "[generation.settings.static_texture_sizing]"
        ]
    );
    // `version` and the job fields share the one table; only `settings` descends.
    assert!(document.contains("\nversion = 3\n"), "{document}");
    assert!(document.contains("\nplugins = [\"Morrowind.esm\"]\n"), "{document}");
    assert!(document.contains("\noutput_root = \"Data Files\"\n"), "{document}");
}

#[test]
fn every_emitted_key_has_a_comment() {
    // Guards against a new setting landing without a matching entry in the comment tables.
    let document = serialize_generation_job_document(&GenerationJob::default()).unwrap();
    let lines: Vec<&str> = document.lines().map(str::trim).collect();

    let mut uncommented = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        // Only consider a key's first line; continuation lines of a multi-line array are indented
        // in the pretty output and so are already trimmed away from a `key = ` shape.
        let Some((key, _)) = line.split_once(" = ") else {
            continue;
        };
        let commented = index.checked_sub(1).is_some_and(|previous| lines[previous].starts_with('#'));
        if !commented {
            uncommented.push(key);
        }
    }

    assert!(uncommented.is_empty(), "keys emitted without a comment: {uncommented:?}");
}

#[test]
fn load_generation_job_file_warns_on_version_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(&path, "[generation]\nversion = 999\n").unwrap();

    let loaded = load_generation_job_file_with_warnings(&path).unwrap();

    assert_eq!(loaded.job.settings.grass_density, GenerationSettings::default().grass_density);
    assert!(loaded.warnings.iter().any(|warning| warning.path == "generation.version"));
}

#[test]
fn missing_namespace_returns_unconfigured_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(&path, "schema_version = 1\n[render]\nfov = 75.0\n").unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert!(job.plugins.is_none());
    assert_eq!(job.settings.grass_density, GenerationSettings::default().grass_density);
}

#[test]
fn owned_tables_warn_and_ignore_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    for (name, source) in [
        ("namespace", "[generation]\nversion = 3\nmisspelled = true\n"),
        ("job-field", "[generation]\nversion = 3\nplugns = []\n"),
        (
            "settings",
            "[generation]\nversion = 3\n[generation.settings]\ngrass_densty = 0.5\n",
        ),
        (
            "nested-settings",
            "[generation]\nversion = 3\n[generation.settings.static_texture_sizing]\nmod = \"off\"\n",
        ),
    ] {
        let path = dir.path().join(format!("{name}.toml"));
        fs::write(&path, source).unwrap();
        let loaded = load_generation_job_file_with_warnings(&path).unwrap();
        assert!(
            loaded.warnings.iter().any(|warning| warning.message.contains("unknown key")),
            "{name} typo was not reported: {:?}",
            loaded.warnings
        );
    }
}

#[test]
fn invalid_generation_values_default_locally() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(
        &path,
        r#"[generation]
version = 3
plugins = ["Morrowind.esm"]
[generation.settings]
grass_density = "dense"
max_terrain_texture_size = 96
terrain_detail = "medium"
"#,
    )
    .unwrap();

    let loaded = load_generation_job_file_with_warnings(&path).unwrap();

    assert_eq!(loaded.job.plugins, Some(vec![PathBuf::from("Morrowind.esm")]));
    assert_eq!(loaded.job.settings.grass_density, GenerationSettings::default().grass_density);
    assert_eq!(
        loaded.job.settings.max_terrain_texture_size,
        GenerationSettings::default().max_terrain_texture_size
    );
    assert_eq!(loaded.job.settings.terrain_detail, TerrainDetail::Medium);
    assert!(
        loaded
            .warnings
            .iter()
            .any(|warning| warning.path == "generation.settings.grass_density")
    );
    assert!(
        loaded
            .warnings
            .iter()
            .any(|warning| warning.path == "generation.settings.max_terrain_texture_size")
    );
}

#[test]
fn invalid_toml_remains_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(&path, "[generation\nversion = 3\n").unwrap();

    assert!(load_generation_job_file_with_warnings(&path).is_err());
}

#[test]
fn write_generation_job_file_writes_cli_compatible_job() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    let job = GenerationJob {
        plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
        output_root: Some(PathBuf::from("out")),
        ..GenerationJob::default()
    };

    write_generation_job_file(&path, &job).unwrap();
    let loaded = load_generation_job_file(&path).unwrap();

    assert_eq!(loaded.plugins, job.plugins);
    assert_eq!(loaded.output_root, job.output_root);
}

#[test]
fn settings_without_plugin_metadata_field_default_to_enabled() {
    let settings: GenerationSettings = serde_json::from_str("{}").unwrap();
    assert!(settings.use_plugin_metadata);
}

#[test]
fn generation_settings_defaults_include_current_texture_resolutions() {
    let settings = GenerationSettings::default();

    assert_eq!(
        settings.max_terrain_texture_size,
        distantland_terrain::DEFAULT_TERRAIN_TEXTURE_SIZE
    );
}

#[test]
fn load_generation_job_file_reads_max_terrain_texture_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("job.json");
    fs::write(
        &path,
        "[generation]\nversion = 3\n[generation.settings]\nmax_terrain_texture_size = 512\n",
    )
    .unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert_eq!(job.settings.max_terrain_texture_size, 512);
}

#[test]
fn validate_for_generation_rejects_duplicate_plugin_filenames() {
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
fn resolve_generation_job_paths_rebases_relative_paths() {
    let base_dir = Path::new(r"C:\Jobs\DistantLand");
    let mut job = GenerationJob {
        morrowind_ini: Some(PathBuf::from("Morrowind.ini")),
        data_dirs: Some(vec![PathBuf::from("Data Files")]),
        plugins: Some(vec![PathBuf::from("Morrowind.esm"), PathBuf::from(r"Plugins\Test.esp")]),
        grass_plugins: Some(vec![PathBuf::from("Groundcover.esp"), PathBuf::from(r"Plugins\Grass.esp")]),
        output_root: Some(PathBuf::from("out")),
        auto_sync_plugins: false,
        settings: GenerationSettings {
            override_files: vec![PathBuf::from("overrides.txt")],
            ..GenerationSettings::default()
        },
    };

    resolve_generation_job_paths(&mut job, base_dir);

    assert_eq!(job.morrowind_ini, Some(base_dir.join("Morrowind.ini")));
    assert_eq!(job.data_dirs, Some(vec![base_dir.join("Data Files")]));
    assert_eq!(
        job.plugins,
        Some(vec![PathBuf::from("Morrowind.esm"), base_dir.join(r"Plugins\Test.esp")])
    );
    assert_eq!(
        job.grass_plugins,
        Some(vec![PathBuf::from("Groundcover.esp"), base_dir.join(r"Plugins\Grass.esp")])
    );
    assert_eq!(job.output_root, Some(base_dir.join("out")));
    assert_eq!(job.settings.override_files, vec![base_dir.join("overrides.txt")]);
}

#[test]
fn validate_rejects_duplicate_override_files() {
    let settings = GenerationSettings {
        override_files: vec![
            PathBuf::from(r"C:\Games\Morrowind\MGE3\MGE XE Default Statics Classifiers.toml"),
            PathBuf::from(r"C:\Games\Morrowind\mge3\MGE XE Default Statics Classifiers.toml"),
        ],
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("duplicate"));
}

#[test]
fn validate_requires_positive_protected_density_when_sizing_enabled() {
    for mode in [
        distantland_statics::StaticTextureSizingMode::Report,
        distantland_statics::StaticTextureSizingMode::DownscaleOpaque,
    ] {
        let settings = GenerationSettings {
            static_texture_sizing: distantland_statics::StaticTextureSizingSettings {
                mode,
                protected_density: 0.0,
                ..distantland_statics::StaticTextureSizingSettings::default()
            },
            ..GenerationSettings::default()
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("protected_density"), "unexpected error: {error}");
    }
}

#[test]
fn validate_accepts_off_sizing_with_zero_protected_density() {
    // The default (`Off`, `0.0`) must remain valid so untouched jobs keep working.
    GenerationSettings::default().validate().unwrap();
}

#[test]
fn validate_rejects_non_finite_protected_density() {
    let settings = GenerationSettings {
        static_texture_sizing: distantland_statics::StaticTextureSizingSettings {
            mode: distantland_statics::StaticTextureSizingMode::DownscaleOpaque,
            protected_density: f32::NAN,
            ..distantland_statics::StaticTextureSizingSettings::default()
        },
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("protected_density"), "unexpected error: {error}");
}

#[test]
fn validate_rejects_unsupported_min_texture_size() {
    let settings = GenerationSettings {
        static_texture_sizing: distantland_statics::StaticTextureSizingSettings {
            min_texture_size: 100,
            ..distantland_statics::StaticTextureSizingSettings::default()
        },
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("min_texture_size"), "unexpected error: {error}");
}

#[test]
fn validate_rejects_excessive_max_mip_reduction() {
    let settings = GenerationSettings {
        static_texture_sizing: distantland_statics::StaticTextureSizingSettings {
            max_mip_reduction: 7,
            ..distantland_statics::StaticTextureSizingSettings::default()
        },
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("max_mip_reduction"), "unexpected error: {error}");
}

#[test]
fn load_generation_job_file_defaults_missing_static_texture_sizing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(
        &path,
        "[generation]\nversion = 3\n[generation.settings]\nterrain_detail = \"high\"\n",
    )
    .unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert_eq!(
        job.settings.static_texture_sizing,
        distantland_statics::StaticTextureSizingSettings::default()
    );
    assert_eq!(
        job.settings.static_texture_sizing.mode,
        distantland_statics::StaticTextureSizingMode::Downscale
    );
}

#[test]
fn validate_rejects_unsupported_terrain_texture_sizes() {
    for max_terrain_texture_size in [0, 96, 1024] {
        let settings = GenerationSettings {
            max_terrain_texture_size,
            ..GenerationSettings::default()
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("max_terrain_texture_size"));
    }
}

#[test]
fn validate_for_generation_rejects_duplicate_data_dirs() {
    let job = GenerationJob {
        data_dirs: Some(vec![
            PathBuf::from(r"C:\Games\Morrowind\Data Files"),
            PathBuf::from(r"C:\Games\morrowind\data files"),
        ]),
        ..GenerationJob::default()
    };
    let error = job.validate_for_generation().unwrap_err().to_string();
    assert!(error.contains("duplicate"));
}

#[test]
fn generation_settings_defaults_include_static_mesh_simplification_params() {
    let settings = GenerationSettings::default();

    assert_eq!(
        settings.static_mesh_target_error,
        distantland_statics::DEFAULT_STATIC_MESH_TARGET_ERROR
    );
    assert_eq!(
        settings.static_mesh_normal_weight,
        distantland_statics::DEFAULT_STATIC_MESH_NORMAL_WEIGHT
    );
    assert_eq!(
        settings.static_mesh_color_weight,
        distantland_statics::DEFAULT_STATIC_MESH_COLOR_WEIGHT
    );
    assert_eq!(
        settings.static_mesh_merge_error_multiplier,
        distantland_statics::DEFAULT_STATIC_MESH_MERGE_ERROR_MULTIPLIER
    );
    assert_eq!(
        settings.terrain_mesh_smoothed_normal_weight,
        distantland_terrain::DEFAULT_TERRAIN_MESH_SMOOTHED_NORMAL_WEIGHT
    );
    assert_eq!(
        settings.terrain_mesh_color_weight,
        distantland_terrain::DEFAULT_TERRAIN_MESH_COLOR_WEIGHT
    );
}

#[test]
fn static_mesh_simplifier_config_reflects_settings() {
    let settings = GenerationSettings {
        static_mesh_target_error: 0.1,
        static_mesh_normal_weight: 2.0,
        static_mesh_color_weight: 3.0,
        static_mesh_merge_error_multiplier: 4.0,
        ..GenerationSettings::default()
    };
    let config = settings.static_mesh_simplifier_config();

    assert_eq!(config.target_error, 0.1);
    assert_eq!(config.normal_weight, 2.0);
    assert_eq!(config.color_weight, 3.0);
    assert_eq!(config.merge_error_multiplier, 4.0);
}

#[test]
fn validate_rejects_negative_static_mesh_target_error() {
    let settings = GenerationSettings {
        static_mesh_target_error: -0.01,
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("static_mesh_target_error"));
}

#[test]
fn validate_rejects_non_finite_static_mesh_target_error() {
    let settings = GenerationSettings {
        static_mesh_target_error: f32::INFINITY,
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("static_mesh_target_error"));
}

#[test]
fn validate_rejects_negative_static_mesh_normal_weight() {
    let settings = GenerationSettings {
        static_mesh_normal_weight: -1.0,
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("static_mesh_normal_weight"));
}

#[test]
fn validate_rejects_negative_static_mesh_color_weight() {
    let settings = GenerationSettings {
        static_mesh_color_weight: -1.0,
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("static_mesh_color_weight"));
}

#[test]
fn validate_rejects_merge_error_multiplier_below_one_or_non_finite() {
    for static_mesh_merge_error_multiplier in [0.0, 0.5, f32::INFINITY, f32::NAN] {
        let settings = GenerationSettings {
            static_mesh_merge_error_multiplier,
            ..GenerationSettings::default()
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("static_mesh_merge_error_multiplier"));
    }
}

#[test]
fn validate_rejects_below_one_door_size_multiplier() {
    let settings = GenerationSettings {
        door_size_multiplier: 0.5,
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("door_size_multiplier"));
}

#[test]
fn validate_rejects_non_finite_door_size_multiplier() {
    let settings = GenerationSettings {
        door_size_multiplier: f32::INFINITY,
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("door_size_multiplier"));
}

#[test]
fn validate_accepts_door_size_multiplier_at_or_above_one() {
    let settings = GenerationSettings {
        door_size_multiplier: 1.0,
        ..GenerationSettings::default()
    };
    assert!(settings.validate().is_ok());

    let settings = GenerationSettings {
        door_size_multiplier: 6.0,
        ..GenerationSettings::default()
    };
    assert!(settings.validate().is_ok());
}

#[test]
fn validate_rejects_non_finite_terrain_mesh_smoothed_normal_weight() {
    let settings = GenerationSettings {
        terrain_mesh_smoothed_normal_weight: f32::INFINITY,
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("terrain_mesh_smoothed_normal_weight"));
}

#[test]
fn write_generation_job_file_round_trips_terrain_mesh_weights() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("job.json");
    let job = GenerationJob {
        settings: GenerationSettings {
            terrain_mesh_smoothed_normal_weight: 1.25,
            terrain_mesh_color_weight: 1.75,
            ..GenerationSettings::default()
        },
        ..GenerationJob::default()
    };

    write_generation_job_file(&path, &job).unwrap();
    let loaded = load_generation_job_file(&path).unwrap();

    assert_eq!(loaded.settings.terrain_mesh_smoothed_normal_weight, 1.25);
    assert_eq!(loaded.settings.terrain_mesh_color_weight, 1.75);
}

#[test]
fn load_generation_job_file_defaults_missing_terrain_mesh_weights() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(
        &path,
        "[generation]\nversion = 3\n[generation.settings]\nterrain_detail = \"high\"\n",
    )
    .unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert_eq!(
        job.settings.terrain_mesh_smoothed_normal_weight,
        distantland_terrain::DEFAULT_TERRAIN_MESH_SMOOTHED_NORMAL_WEIGHT
    );
    assert_eq!(
        job.settings.terrain_mesh_color_weight,
        distantland_terrain::DEFAULT_TERRAIN_MESH_COLOR_WEIGHT
    );
}

#[test]
fn write_generation_job_file_round_trips_door_size_multiplier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("job.json");
    let job = GenerationJob {
        settings: GenerationSettings {
            door_size_multiplier: 6.0,
            ..GenerationSettings::default()
        },
        ..GenerationJob::default()
    };

    write_generation_job_file(&path, &job).unwrap();
    let loaded = load_generation_job_file(&path).unwrap();

    assert_eq!(loaded.settings.door_size_multiplier, 6.0);
}

#[test]
fn load_generation_job_file_defaults_missing_door_size_multiplier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(
        &path,
        "[generation]\nversion = 3\n[generation.settings]\nterrain_detail = \"high\"\n",
    )
    .unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert_eq!(job.settings.door_size_multiplier, 2.0);
}

#[test]
fn write_generation_job_file_round_trips_merge_group_radius() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("job.json");
    let job = GenerationJob {
        settings: GenerationSettings {
            merge_group_radius: 4096.0,
            static_mesh_merge_error_multiplier: 2.0,
            ..GenerationSettings::default()
        },
        ..GenerationJob::default()
    };

    write_generation_job_file(&path, &job).unwrap();
    let loaded = load_generation_job_file(&path).unwrap();

    assert_eq!(loaded.settings.merge_group_radius, 4096.0);
    assert_eq!(loaded.settings.static_mesh_merge_error_multiplier, 2.0);
}

#[test]
fn load_generation_job_file_defaults_missing_merge_group_radius() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(
        &path,
        "[generation]\nversion = 3\n[generation.settings]\nterrain_detail = \"high\"\n",
    )
    .unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert_eq!(
        job.settings.merge_group_radius,
        distantland_statics::DEFAULT_MERGE_GROUP_RADIUS
    );
    assert_eq!(
        job.settings.static_mesh_merge_error_multiplier,
        distantland_statics::DEFAULT_STATIC_MESH_MERGE_ERROR_MULTIPLIER
    );
}

#[test]
fn validate_rejects_non_positive_merge_group_radius() {
    for merge_group_radius in [0.0, -1.0, f32::INFINITY, f32::NAN] {
        let settings = GenerationSettings {
            merge_group_radius,
            ..GenerationSettings::default()
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("merge_group_radius"));
    }
}

#[test]
fn validate_for_generation_rejects_invalid_grass_plugin_lists() {
    for grass_plugins in [
        vec![PathBuf::from("Groundcover.omwaddon")],
        vec![PathBuf::from("A/Groundcover.esp"), PathBuf::from("B/groundcover.ESP")],
    ] {
        let job = GenerationJob {
            grass_plugins: Some(grass_plugins),
            ..GenerationJob::default()
        };
        assert!(job.validate_for_generation().is_err());
    }
}

#[test]
fn validate_for_generation_accepts_an_explicit_empty_grass_list() {
    let job = GenerationJob {
        grass_plugins: Some(vec![]),
        ..GenerationJob::default()
    };
    job.validate_for_generation().unwrap();
}

#[test]
fn validate_for_generation_rejects_plugin_cross_list_duplicates() {
    let job = GenerationJob {
        plugins: Some(vec![PathBuf::from("Data/Groundcover.esp")]),
        grass_plugins: Some(vec![PathBuf::from("Other/GROUNDCOVER.ESP")]),
        ..GenerationJob::default()
    };

    let error = job.validate_for_generation().unwrap_err().to_string();
    assert!(error.contains("plugins and grass_plugins"));
}

#[test]
fn write_generation_job_file_round_trips_texture_dedupe_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("job.json");
    let job = GenerationJob {
        settings: GenerationSettings {
            texture_dedupe_mode: distantland_statics::TextureDedupeMode::Off,
            ..GenerationSettings::default()
        },
        ..GenerationJob::default()
    };

    write_generation_job_file(&path, &job).unwrap();
    let loaded = load_generation_job_file(&path).unwrap();

    assert_eq!(
        loaded.settings.texture_dedupe_mode,
        distantland_statics::TextureDedupeMode::Off
    );
}

#[test]
fn load_generation_job_file_defaults_missing_texture_dedupe_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(
        &path,
        "[generation]\nversion = 3\n[generation.settings]\nterrain_detail = \"high\"\n",
    )
    .unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert_eq!(
        job.settings.texture_dedupe_mode,
        distantland_statics::TextureDedupeMode::Exact
    );
}

#[test]
fn write_generation_job_file_round_trips_deep_water_static_cull_depth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("job.json");
    let job = GenerationJob {
        settings: GenerationSettings {
            deep_water_static_cull_depth: 256.0,
            ..GenerationSettings::default()
        },
        ..GenerationJob::default()
    };

    write_generation_job_file(&path, &job).unwrap();
    let loaded = load_generation_job_file(&path).unwrap();

    assert_eq!(loaded.settings.deep_water_static_cull_depth, 256.0);
}

#[test]
fn load_generation_job_file_defaults_missing_deep_water_static_cull_depth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mgeXE.toml");
    fs::write(
        &path,
        "[generation]\nversion = 3\n[generation.settings]\nterrain_detail = \"high\"\n",
    )
    .unwrap();

    let job = load_generation_job_file(&path).unwrap();

    assert_eq!(job.settings.deep_water_static_cull_depth, 128.0);
}

#[test]
fn validate_rejects_negative_deep_water_static_cull_depth() {
    let settings = GenerationSettings {
        deep_water_static_cull_depth: -1.0,
        ..GenerationSettings::default()
    };
    let error = settings.validate().unwrap_err().to_string();
    assert!(error.contains("deep_water_static_cull_depth"));
}

#[test]
fn validate_rejects_non_finite_deep_water_static_cull_depth() {
    for deep_water_static_cull_depth in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
        let settings = GenerationSettings {
            deep_water_static_cull_depth,
            ..GenerationSettings::default()
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("deep_water_static_cull_depth"));
    }
}

#[test]
fn auto_sync_plugins_defaults_to_true_on_every_path_that_produces_a_job() {
    let dir = tempfile::tempdir().unwrap();

    // Directly, which `load_generation_job_file` also returns for an absent namespace.
    assert!(GenerationJob::default().auto_sync_plugins);

    let absent = dir.path().join("absent.toml");
    fs::write(&absent, "schema_version = 1\n").unwrap();
    assert!(load_generation_job_file(&absent).unwrap().auto_sync_plugins);

    // A v3 file written before the key existed. Opting these in is deliberate: the
    // saved list is what goes stale, so leaving old configs out is the wrong default.
    let old = dir.path().join("old.toml");
    fs::write(&old, "[generation]\nversion = 3\nplugins = [\"Morrowind.esm\"]\n").unwrap();
    assert!(load_generation_job_file(&old).unwrap().auto_sync_plugins);

    let off = dir.path().join("off.toml");
    fs::write(&off, "[generation]\nversion = 3\nauto_sync_plugins = false\n").unwrap();
    assert!(!load_generation_job_file(&off).unwrap().auto_sync_plugins);
}

/// A stub install: `Data Files`, real plugin files, and a `[Game Files]` load order.
/// The parser only returns plugins that exist on disk, so the files have to be real.
fn stub_install(dir: &Path, plugins: &[&str], load_order: &[&str]) -> PathBuf {
    let data_files = dir.join("Data Files");
    fs::create_dir_all(&data_files).unwrap();
    for name in plugins {
        fs::write(data_files.join(name), b"plugin").unwrap();
    }
    let mut ini = String::from("[Game Files]\n");
    for (index, name) in load_order.iter().enumerate() {
        ini.push_str(&format!("GameFile{index}={name}\n"));
    }
    let ini_path = dir.join("Morrowind.ini");
    fs::write(&ini_path, ini).unwrap();
    ini_path
}

#[test]
fn sync_drops_grass_by_filename_case_insensitively_without_reordering() {
    let dir = tempfile::tempdir().unwrap();
    let ini = stub_install(
        dir.path(),
        &["Morrowind.esm", "Rem_GL.esp", "Mod.esp"],
        &["Morrowind.esm", "Rem_GL.esp", "Mod.esp"],
    );

    let mut job = GenerationJob {
        // A bare, differently-cased name, as a hand-written job may carry.
        grass_plugins: Some(vec![PathBuf::from("rem_gl.ESP")]),
        ..GenerationJob::default()
    };
    sync_plugins_from_load_order(&mut job, &ini, None).unwrap();

    let data_files = dir.path().join("Data Files");
    // Masters first, then the parser's order. This helper does not re-sort them.
    assert_eq!(
        job.plugins,
        Some(vec![data_files.join("Morrowind.esm"), data_files.join("Mod.esp")])
    );
}

#[test]
fn sync_derives_base_data_files_from_the_ini_when_no_dirs_are_configured() {
    let dir = tempfile::tempdir().unwrap();
    let ini = stub_install(dir.path(), &["Morrowind.esm"], &["Morrowind.esm"]);
    let data_files = dir.path().join("Data Files");

    let mut derived = GenerationJob::default();
    sync_plugins_from_load_order(&mut derived, &ini, None).unwrap();

    let mut explicit = GenerationJob::default();
    sync_plugins_from_load_order(&mut explicit, &ini, Some(std::slice::from_ref(&data_files))).unwrap();

    // `None` must mean "derive the base layer", never "no layers": an empty slice
    // resolves nothing, and the two callers have to agree on the same list.
    assert_eq!(derived.plugins, Some(vec![data_files.join("Morrowind.esm")]));
    assert_eq!(derived.plugins, explicit.plugins);
}

#[test]
fn sync_propagates_an_unreadable_ini_instead_of_emptying_the_selection() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("Data Files")).unwrap();
    let missing = dir.path().join("Morrowind.ini");

    let mut job = GenerationJob {
        plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
        ..GenerationJob::default()
    };
    assert!(sync_plugins_from_load_order(&mut job, &missing, None).is_err());
    // The caller aborts on that error, so the job it still holds is untouched.
    assert_eq!(job.plugins, Some(vec![PathBuf::from("Morrowind.esm")]));
}
