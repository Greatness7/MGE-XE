mod bindings;
mod document;
pub mod ffi;
mod schema;
mod validation;

#[cfg(feature = "contract-test")]
pub use bindings::RuntimeNumberRepr;
pub use document::{ConfigDocument, ConfigError, DEFAULT_DOCUMENT, FILE_NAME, OpenState};
pub use schema::*;
pub use validation::{Warning, normalize_distant_land};

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::fs;
    use std::ptr;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn embedded_document_matches_typed_defaults() {
        let root = test_root("embedded-defaults");
        let path = root.join(FILE_NAME);
        let document = ConfigDocument::open(&path);
        assert_eq!(document.state(), OpenState::MissingDefaults);
        assert_eq!(document.settings(), &Settings::default());
        assert_eq!(document.settings().gui.language, "auto");
    }

    #[test]
    fn native_ppl_packets_round_trips_through_toml_and_bindings() {
        let root = test_root("native-ppl-packets");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, DEFAULT_DOCUMENT).unwrap();

        let mut document = ConfigDocument::open(&path);
        assert!(document.settings().distant_land.native_ppl_packets);
        assert_eq!(document.get_number("distant_land.native_ppl_packets"), Some(1.0));

        document.set_number("distant_land.native_ppl_packets", 0.0).unwrap();
        document.save().unwrap();
        assert!(!ConfigDocument::open(&path).settings().distant_land.native_ppl_packets);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn weather_clamp_warnings_carry_the_path_of_the_clamped_field() {
        let root = test_root("weather-clamp-warnings");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, DEFAULT_DOCUMENT).unwrap();
        let mut document = ConfigDocument::open(&path);

        // Each setter re-runs bounds validation and replaces the warning list, so the label
        // under test is the only warning present after its own call.
        document.set_number("distant_land.weather.blight.fog_offset", 500.0).unwrap();
        assert_eq!(
            document
                .warnings()
                .iter()
                .map(|warning| warning.path.as_str())
                .collect::<Vec<_>>(),
            ["distant_land.weather.blight.fog_offset"]
        );

        document.set_number("lighting.weather.snow.ambient", 50.0).unwrap();
        assert_eq!(
            document
                .warnings()
                .iter()
                .map(|warning| warning.path.as_str())
                .collect::<Vec<_>>(),
            ["lighting.weather.snow.ambient"]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn weather_labels_match_names() {
        // The clamp labels are spelled out at compile time, so an edit to WEATHER_NAMES that
        // does not move the table with it would silently mislabel warnings.
        let fields = [
            ("distant_land.weather", "wind"),
            ("distant_land.weather", "fog_ratio"),
            ("distant_land.weather", "fog_offset"),
            ("lighting.weather", "sun"),
            ("lighting.weather", "ambient"),
        ];
        for (slot, name) in WEATHER_NAMES.iter().enumerate() {
            for (offset, (prefix, field)) in fields.iter().enumerate() {
                assert_eq!(validation::WEATHER_LABELS[slot][offset], format!("{prefix}.{name}.{field}"));
            }
        }
    }

    #[test]
    fn every_weather_binding_path_round_trips_and_neighbours_are_rejected() {
        let mut settings = Settings::default();
        let fields = [
            ("distant_land.weather", "wind"),
            ("distant_land.weather", "fog_ratio"),
            ("distant_land.weather", "fog_offset"),
            ("lighting.weather", "sun"),
            ("lighting.weather", "ambient"),
        ];

        let mut expected = Vec::new();
        for (slot, name) in WEATHER_NAMES.iter().enumerate() {
            for (offset, (prefix, field)) in fields.iter().enumerate() {
                let path = format!("{prefix}.{name}.{field}");
                // Quarters are exact in f32, so the widened read back compares equal.
                let value = (slot * fields.len() + offset) as f64 / 4.0;
                settings.set_number(&path, value).unwrap();
                expected.push((path, value));
            }
        }
        assert_eq!(expected.len(), 50);
        for (path, value) in &expected {
            assert_eq!(settings.get_number(path), Some(*value), "{path}");
        }

        for path in [
            "distant_land.weather.clear.sun",
            "lighting.weather.clear.wind",
            "distant_land.weather.sunny.wind",
            "distant_land.weather.clear",
            "distant_land.weather.clear.wind.extra",
        ] {
            assert_eq!(settings.get_number(path), None, "{path}");
            assert!(settings.set_number(path, 0.0).is_err(), "{path}");
        }
    }

    #[test]
    fn enable_shaders_round_trips_through_toml_and_bindings() {
        let root = test_root("enable-shaders");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            DEFAULT_DOCUMENT.replace("enable_shaders = false", "enable_shaders = true"),
        )
        .unwrap();

        let mut document = ConfigDocument::open(&path);
        assert!(document.settings().render.enable_shaders);
        assert_eq!(document.get_number("render.enable_shaders"), Some(1.0));

        document.set_number("render.enable_shaders", 0.0).unwrap();
        document.save().unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("enable_shaders = false"));
        assert!(!ConfigDocument::open(&path).settings().render.enable_shaders);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gui_language_round_trips_through_the_targeted_document_save() {
        let root = test_root("gui-language");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        let mut document = ConfigDocument::open(&path);
        let mut settings = document.settings().clone();
        settings.gui.language = "fr".into();
        document.replace_settings(settings).unwrap();
        document.save().unwrap();

        let reopened = ConfigDocument::open(&path);
        assert_eq!(reopened.settings().gui.language, "fr");
        assert!(fs::read_to_string(&path).unwrap().contains("language = \"fr\""));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn typed_gui_save_removes_unknown_owned_keys() {
        let root = test_root("preserve-gui-tooltip-speed");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            "schema_version = 1\n\n[gui]\nlanguage = \"auto\"\ntooltip_speed = 11\n",
        )
        .unwrap();

        let mut document = ConfigDocument::open(&path);
        let mut settings = document.settings().clone();
        settings.gui.language = "fr".into();
        document.replace_settings(settings).unwrap();
        document.save().unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("language = \"fr\""));
        assert!(!written.contains("tooltip_speed"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn targeted_updates_remove_unknown_root_scalars_but_preserve_root_tables() {
        let root = test_root("preserve");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            "schema_version = 1\nunknown = \"remove\"\n\n[render]\n# FOV comment\nfov = 80.0\n\n[extension]\nvalue = \"keep\"\n",
        )
        .unwrap();
        let mut document = ConfigDocument::open(&path);
        document.set_number("render.fov", 90.0).unwrap();
        document.save().unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(!written.contains("unknown ="));
        assert!(written.contains("[extension]"));
        assert!(written.contains("value = \"keep\""));
        assert!(written.contains("# FOV comment"));
        assert!(written.contains("fov = 90.0"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writing_a_copy_preserves_the_source_and_live_document_state() {
        let root = test_root("write-copy");
        let path = root.join(FILE_NAME);
        let target = root.join("export.toml");
        fs::create_dir_all(&root).unwrap();
        let source = "schema_version = 1\n\n[render]\n# FOV comment\nfov = 80.0\n\n[extension]\nvalue = \"keep\"\n";
        fs::write(&path, source).unwrap();

        let mut document = ConfigDocument::open(&path);
        document.set_number("render.fov", 90.0).unwrap();
        let exported_settings = document.settings().clone();
        document.write_copy(&target, exported_settings).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        let exported = fs::read_to_string(&target).unwrap();
        assert!(exported.contains("[extension]"));
        assert!(exported.contains("# FOV comment"));
        assert!(exported.contains("fov = 90.0"));
        assert_eq!(document.get_number("render.fov"), Some(90.0));

        document.save().unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("fov = 90.0"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn structured_arrays_replace_whole_only_when_changed() {
        let root = test_root("structured-contract");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            "# root comment\nschema_version = 1\nunknown = \"keep\"\n\n[input]\n\n[[input.macros]]\nindex = 3\ntype = \"graphics\"\nfunction = 1\n# extension comment\nextension = \"entry-local\"\n",
        )
        .unwrap();
        let mut document = ConfigDocument::open(&path);

        document.set_number("render.fov", 90.0).unwrap();
        document.save().unwrap();
        let unchanged = fs::read_to_string(&path).unwrap();
        assert!(!unchanged.contains("# extension comment"));
        assert!(!unchanged.contains("extension = \"entry-local\""));

        let mut settings = document.settings().clone();
        settings.input.macros[0].function = 2;
        document.replace_settings(settings).unwrap();
        document.save().unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("function = 2"));
        assert!(!written.contains("# extension comment"));
        assert!(!written.contains("extension = \"entry-local\""));
        assert!(written.contains("# root comment"));
        assert!(!written.contains("unknown ="));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_values_default_locally_and_malformed_array_elements_are_discarded() {
        let root = test_root("tolerant-fields");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            r#"schema_version = 1

[render]
fov = "wide"
ui_scale = 2.0

[[input.macros]]
index = 1
type = "graphics"
function = 1

[[input.macros]]
index = "broken"
type = "graphics"
function = 2

[[input.macros]]
index = 3
type = "graphics"
function = 255

[[input.macros]]
index = 4
type = "graphics"
function = 3
"#,
        )
        .unwrap();

        let mut document = ConfigDocument::open(&path);
        assert_eq!(document.state(), OpenState::Valid);
        assert_eq!(document.settings().render.fov, Settings::default().render.fov);
        assert_eq!(document.settings().render.ui_scale, 2.0);
        assert_eq!(
            document
                .settings()
                .input
                .macros
                .iter()
                .map(|item| item.index)
                .collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert!(document.warnings().iter().any(|warning| warning.path == "render.fov"));
        assert!(
            document
                .warnings()
                .iter()
                .any(|warning| warning.path.starts_with("input.macros[1]"))
        );

        document.save().unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(!written.contains("wide"));
        assert!(!written.contains("broken"));
        assert!(!written.contains("function = 255"));
        assert!(ConfigDocument::open(&path).warnings().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_files_are_not_overwritten_and_external_edits_conflict() {
        let root = test_root("invalid-conflict");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, "schema_version = 1\n[render\n").unwrap();
        let malformed_bytes = fs::read(&path).unwrap();
        let mut invalid = ConfigDocument::open(&path);
        assert_eq!(invalid.state(), OpenState::InvalidDefaults);
        assert!(matches!(invalid.save(), Err(ConfigError::WritesDisabled)));
        assert_eq!(fs::read(&path).unwrap(), malformed_bytes);

        fs::write(&path, DEFAULT_DOCUMENT).unwrap();
        let mut valid = ConfigDocument::open(&path);
        valid.set_number("render.fov", 91.0).unwrap();
        let external = format!("{DEFAULT_DOCUMENT}\nexternal = true\n");
        fs::write(&path, &external).unwrap();
        assert!(matches!(valid.save(), Err(ConfigError::RevisionConflict)));
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_utf8_is_blocked_but_newer_schema_is_normalized() {
        let root = test_root("invalid-documents");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();

        let invalid_utf8 = [0xff, 0xfe, 0xfd];
        fs::write(&path, invalid_utf8).unwrap();
        let mut document = ConfigDocument::open(&path);
        assert_eq!(document.state(), OpenState::InvalidDefaults);
        assert!(document.diagnostic().unwrap().contains("UTF-8"));
        assert!(document.save().is_err());
        assert_eq!(fs::read(&path).unwrap(), invalid_utf8);

        let newer = "schema_version = 999\n";
        fs::write(&path, newer).unwrap();
        let mut document = ConfigDocument::open(&path);
        assert_eq!(document.state(), OpenState::Valid);
        assert!(!document.writes_disabled());
        assert!(document.warnings().iter().any(|warning| warning.path == "schema_version"));
        document.save().unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains(&format!("schema_version = {SCHEMA_VERSION}"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repairing_and_reloading_an_invalid_file_reenables_saves() {
        let root = test_root("repair-reload");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, "schema_version = 1\n[render\n").unwrap();
        let mut document = ConfigDocument::open(&path);
        assert!(document.writes_disabled());

        fs::write(&path, DEFAULT_DOCUMENT).unwrap();
        document.reload().unwrap();
        assert!(!document.writes_disabled());
        document.set_number("render.fov", 88.0).unwrap();
        document.save().unwrap();
        assert_eq!(ConfigDocument::open(&path).get_number("render.fov"), Some(88.0));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_reload_leaves_live_values_and_write_policy_untouched() {
        let root = test_root("failed-reload");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, DEFAULT_DOCUMENT).unwrap();
        let mut document = ConfigDocument::open(&path);
        document.set_number("render.fov", 87.0).unwrap();

        fs::write(&path, "schema_version = 1\n[render\n").unwrap();
        assert!(document.reload().is_err());
        assert_eq!(document.get_number("render.fov"), Some(87.0));
        assert!(!document.writes_disabled());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creation_collision_never_overwrites_the_new_file() {
        let root = test_root("creation-collision");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        let mut document = ConfigDocument::open(&path);
        assert!(document.needs_creation());

        let external = format!("{DEFAULT_DOCUMENT}\ncreated_by_gui = true\n");
        fs::write(&path, &external).unwrap();
        assert!(matches!(document.save(), Err(ConfigError::CreationCollision)));
        assert!(document.writes_disabled());
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn out_of_range_values_use_defaults_and_normalize_on_save() {
        let root = test_root("persist-clamp");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            "schema_version = 1\n\n[render]\nfov = 999.0\n\n[distant_land]\nnear_static_end = 15.0\nfar_static_end = 4.0\n",
        )
        .unwrap();
        let mut document = ConfigDocument::open(&path);
        let default = Settings::default().render.fov as f64;
        assert_eq!(document.get_number("render.fov"), Some(default));
        assert_eq!(document.settings().distant_land.near_static_end, 15.0);
        assert_eq!(document.settings().distant_land.far_static_end, 15.0);
        assert!(document.warnings().iter().any(|warning| warning.path == "render.fov"));
        document.save().unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(!written.contains("fov = 999.0"));
        assert!(written.contains("far_static_end = 15.0"));
        assert!(ConfigDocument::open(&path).warnings().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saved_floats_are_written_at_f32_precision() {
        let root = test_root("persist-float-precision");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        // A stale document carrying widened values a previous save produced.
        fs::write(
            &path,
            "schema_version = 2\n\n[distant_land]\nvery_far_static_end = 7.840000152587891\n\n[distant_land.fog]\nabove_water_start = 2.319999933242798\n",
        )
        .unwrap();
        let mut document = ConfigDocument::open(&path);
        // 8.0 * 0.67 is exactly 5.36 as an f32 and 5.360000133514404 as an f64.
        document
            .set_number("distant_land.far_static_end", (8.0f32 * 0.67) as f64)
            .unwrap();
        document.save().unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("far_static_end = 5.36"), "{written}");
        assert!(!written.contains("5.360000133514404"), "{written}");
        // Untouched keys are rewritten too, without changing the value they load as.
        assert!(written.contains("very_far_static_end = 7.84"), "{written}");
        assert!(written.contains("above_water_start = 2.32"), "{written}");

        let reloaded = ConfigDocument::open(&path);
        assert_eq!(reloaded.settings().distant_land.far_static_end, 8.0 * 0.67);
        assert_eq!(reloaded.settings().distant_land.very_far_static_end, 7.84);
        assert_eq!(reloaded.settings().distant_land.fog.above_water_start, 2.32);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn structured_input_serializes_and_renders_the_cpp_spelling() {
        let mut settings = Settings::default();
        settings.input.macros.push(MacroSettings {
            index: 3,
            kind: MacroKind::Console1,
            key_events: vec![KeyEvent { code: 41, down: true }],
            description: "console".into(),
            ..Default::default()
        });
        settings.input.triggers.push(TriggerSettings {
            index: 0,
            active: true,
            interval_ms: 250,
            keys: vec![30],
        });
        settings.input.remap.insert(30, 31);

        let encoded = toml_edit::ser::to_string(&settings).unwrap();
        let mut decoded: Settings = toml_edit::de::from_str(&encoded).unwrap();
        assert_eq!(decoded, settings);
        assert_eq!(decoded.input.render_macros(), ["M3=Console1,41,True"]);
        assert_eq!(decoded.input.render_triggers(), ["T0=True,250,30"]);
        assert_eq!(decoded.input.render_remap(), ["R30=31"]);

        decoded.input.macros.extend([
            MacroSettings {
                kind: MacroKind::Hammer1,
                keys: vec![0, u16::MAX],
                ..Default::default()
            },
            MacroSettings {
                kind: MacroKind::BeginTimer,
                timer_id: u8::MAX,
                ..Default::default()
            },
            MacroSettings {
                kind: MacroKind::Graphics,
                function: u8::MAX,
                ..Default::default()
            },
            MacroSettings {
                kind: MacroKind::Unused,
                keys: vec![u16::MAX],
                ..Default::default()
            },
        ]);
        decoded.input.triggers[0] = TriggerSettings {
            index: u8::MAX,
            active: false,
            interval_ms: u32::MAX,
            keys: vec![0, u16::MAX],
        };
        decoded.input.remap.clear();
        decoded.input.remap.extend([(0, 0), (u16::MAX, u8::MAX)]);

        let macros = decoded.input.render_macros();
        assert!(decoded.input.rendered_macro_line_lengths().eq(macros.iter().map(String::len)));
        let triggers = decoded.input.render_triggers();
        assert!(
            decoded
                .input
                .rendered_trigger_line_lengths()
                .eq(triggers.iter().map(String::len))
        );
        let remap = decoded.input.render_remap();
        assert!(decoded.input.rendered_remap_line_lengths().eq(remap.iter().map(String::len)));
    }

    #[test]
    fn structured_input_rejects_unsafe_runtime_indices() {
        let mut settings = Settings::default();
        settings.input.macros.push(MacroSettings {
            index: 2,
            kind: MacroKind::BeginTimer,
            timer_id: TRIGGER_COUNT as u8,
            ..Default::default()
        });
        assert!(validation::validate(&mut settings).is_err());

        settings.input.macros[0] = MacroSettings {
            index: 2,
            kind: MacroKind::Graphics,
            function: GRAPHICS_FUNCTION_COUNT,
            ..Default::default()
        };
        assert!(validation::validate(&mut settings).is_err());
    }

    #[test]
    fn ffi_getters_never_modify_outputs_on_failure_or_truncation() {
        let root = test_root("ffi");
        fs::create_dir_all(&root).unwrap();
        let file = root.join(FILE_NAME);
        fs::write(&file, DEFAULT_DOCUMENT.replace("chain = []", "chain = [\"Bloom\"]")).unwrap();
        let path = CString::new(file.to_string_lossy().as_bytes()).unwrap();
        let mut document = ptr::null_mut();
        let status = unsafe { ffi::open(path.as_ptr(), &mut document) };
        assert_eq!(status, ffi::FfiStatus::Ok);
        assert!(!document.is_null());

        let unknown = CString::new("unknown.path").unwrap();
        let mut number = 123.0;
        assert_eq!(
            unsafe { ffi::get_number(document, unknown.as_ptr(), &mut number) },
            ffi::FfiStatus::UnknownPath
        );
        assert_eq!(number, 123.0);

        let string_path = CString::new("render.screenshot_name").unwrap();
        let mut string_output = [b'X' as i8; 1];
        assert_eq!(
            unsafe {
                ffi::get_string(
                    document,
                    string_path.as_ptr(),
                    string_output.as_mut_ptr(),
                    string_output.len(),
                )
            },
            ffi::FfiStatus::BufferTooSmall
        );
        assert_eq!(string_output, [b'X' as i8; 1]);

        let lines_path = CString::new("shaders.chain").unwrap();
        let mut lines_output = [b'Y' as i8; 1];
        assert_eq!(
            unsafe { ffi::get_lines(document, lines_path.as_ptr(), lines_output.as_mut_ptr(), lines_output.len(),) },
            ffi::FfiStatus::BufferTooSmall
        );
        assert_eq!(lines_output, [b'Y' as i8; 1]);

        let empty_lines_path = CString::new("input.remap").unwrap();
        let mut empty_lines_output = [b'Z' as i8; 1];
        assert_eq!(
            unsafe {
                ffi::get_lines(
                    document,
                    empty_lines_path.as_ptr(),
                    empty_lines_output.as_mut_ptr(),
                    empty_lines_output.len(),
                )
            },
            ffi::FfiStatus::BufferTooSmall
        );
        assert_eq!(empty_lines_output, [b'Z' as i8; 1]);
        unsafe { ffi::close(document) };
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_numeric_setters_reject_unrepresentable_values() {
        let mut settings = Settings::default();

        assert!(settings.set_number("graphics.anti_aliasing", 256.0).is_err());
        assert!(settings.set_number("distant_land.shadows.map_resolution", -1.0).is_err());
        assert!(settings.set_number("render.window_align_x", i32::MAX as f64 + 1.0).is_err());
        assert!(settings.set_number("render.enable_shaders", 0.5).is_err());
        assert!(settings.set_number("render.fov", f64::NAN).is_err());
        assert!(settings.set_number("render.fov", f64::INFINITY).is_err());
        assert!(settings.set_number("render.fov", f64::MAX).is_err());
    }

    #[test]
    fn validation_rejects_non_finite_floats() {
        let mut settings = Settings::default();
        settings.render.fov = f32::NAN;

        assert_eq!(
            super::validation::validate(&mut settings).unwrap_err().to_string(),
            "render.fov must be finite"
        );
    }

    #[test]
    fn validation_normalizes_distant_land_relationships() {
        let mut settings = Settings::default();
        let distant = &mut settings.distant_land;
        distant.draw_distance = 20.0;
        distant.near_static_end = 15.0;
        distant.far_static_end = 4.0;
        distant.very_far_static_end = 5.0;
        distant.far_static_min_size = 900.0;
        distant.very_far_static_min_size = 300.0;
        distant.fog.above_water_start = 40.0;
        distant.fog.above_water_end = 250.0;
        distant.fog.below_water_start = 8.0;
        distant.fog.below_water_end = 100.0;
        distant.fog.interior_start = 30.0;
        distant.fog.interior_end = 10.0;

        let warnings = super::validation::validate(&mut settings).unwrap();
        let distant = &settings.distant_land;
        assert_eq!(distant.far_static_end, 15.0);
        assert_eq!(distant.very_far_static_end, 15.0);
        assert_eq!(distant.very_far_static_min_size, 900.0);
        // Both above/below fog ends are capped at the draw distance, which then
        // leaves both starts inverted.
        assert_eq!(distant.fog.above_water_end, 20.0);
        assert_eq!(distant.fog.below_water_end, 20.0);
        assert_eq!(distant.fog.above_water_start, 19.9);
        assert_eq!(distant.fog.below_water_start, 8.0);
        assert_eq!(distant.fog.interior_start, 9.9);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.path == "distant_land.very_far_static_end"),
            "relationship corrections are reported like clamps: {warnings:?}"
        );
    }

    #[test]
    fn coherent_distant_land_distances_are_left_alone() {
        let mut settings = Settings::default();
        let before = settings.distant_land.clone();

        let warnings = super::validation::validate(&mut settings).unwrap();
        assert_eq!(settings.distant_land, before);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn incremental_writes_do_not_apply_cross_field_normalization() {
        let root = test_root("incremental-writes");
        let path = root.join(FILE_NAME);
        let mut document = ConfigDocument::open(&path);

        // A writer pushing a whole fog range outward sets the start while the
        // document still holds the old, lower end. Correcting the inversion here
        // would discard the incoming start before its end ever arrives.
        assert_eq!(document.settings().distant_land.fog.above_water_end, 5.0);
        document.set_number("distant_land.fog.above_water_start", 6.0).unwrap();
        assert_eq!(document.settings().distant_land.fog.above_water_start, 6.0);

        document.set_number("distant_land.fog.above_water_end", 12.0).unwrap();
        document.set_number("distant_land.draw_distance", 20.0).unwrap();
        let distant = &document.settings().distant_land;
        assert_eq!(distant.fog.above_water_start, 6.0);
        assert_eq!(distant.fog.above_water_end, 12.0);
    }

    #[test]
    fn obsolete_mge_ini_has_no_effect() {
        let root = test_root("ignore-ini");
        fs::create_dir_all(root.join("MGE3")).unwrap();
        fs::write(
            root.join("MGE3").join("MGE.ini"),
            b"\xff\xfe[Global Graphics]\nMGE Disabled=1\n",
        )
        .unwrap();
        let document = ConfigDocument::open(root.join(FILE_NAME));
        assert_eq!(document.state(), OpenState::MissingDefaults);
        assert!(!document.settings().runtime.disabled);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reset_preserves_generator_table_and_export_omits_it() {
        let root = test_root("generator-table-lifecycle");
        let path = root.join(FILE_NAME);
        let export = root.join("export.toml");
        fs::create_dir_all(&root).unwrap();
        let generator = "\n# keep generator comment\n[generation]\nversion = 3\nplugins = [\"Morrowind.esm\"]\n";
        fs::write(&path, format!("{DEFAULT_DOCUMENT}{generator}")).unwrap();

        let mut document = ConfigDocument::open(&path);
        document.reset_to_defaults(true);
        document.save().unwrap();
        let reset = fs::read_to_string(&path).unwrap();
        assert!(reset.contains(generator.trim()));

        document.write_copy(&export, document.settings().clone()).unwrap();
        assert!(!fs::read_to_string(export).unwrap().contains("[generation]"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_table_replacement_uses_live_document_save() {
        let root = test_root("generator-table-replace");
        let path = root.join(FILE_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, DEFAULT_DOCUMENT).unwrap();
        let mut document = ConfigDocument::open(&path);

        document
            .replace_root_table_from_document(
                "generation",
                "[generation]\nversion = 3\nplugins = [\"Morrowind.esm\"]\n[generation.settings]\ngrass_density = 1.0\n",
            )
            .unwrap();
        document.save().unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[generation]"));
        assert!(written.contains("plugins = [\"Morrowind.esm\"]"));

        // The spliced table must land after every pre-existing table and stay contiguous.
        // A parsed replacement carries doc_position values from its own standalone parse, so
        // without a position fixup the encoder interleaves it into the front of the document.
        let headers: Vec<&str> = written.lines().map(str::trim).filter(|line| line.starts_with('[')).collect();
        let first_spliced = headers
            .iter()
            .position(|header| header.starts_with("[generation"))
            .expect("spliced table header is present");
        assert!(
            headers[first_spliced..]
                .iter()
                .all(|header| header.starts_with("[generation")),
            "spliced table is not contiguous at the end of the document: {headers:?}"
        );
        assert!(
            written.contains("\n\n[generation]\n"),
            "spliced header is not separated from the preceding table: {written}"
        );

        fs::remove_dir_all(root).unwrap();
    }

    fn test_root(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("mge-config-{label}-{}-{unique}", std::process::id()))
    }
}
