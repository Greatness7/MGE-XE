use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mge_config::{
    Anisotropy, AntiAliasing, ConfigDocument, DRAW_DISTANCE_RANGE, FogMode, PerPixelMode, STATIC_MIN_SIZE_RANGE,
    ScreenshotFormat, ScreenshotSuffix, Settings, VSync,
};
use rust_i18n::t;

use crate::{
    input::InputSettings,
    morrowind_profile::{IniSettings, MorrowindProfile},
};

pub const VERSION_NUMBER: &str = env!("CARGO_PKG_VERSION");
pub const VERSION_STRING: &str = concat!("MGE-XE G7 Fork v", env!("CARGO_PKG_VERSION"));
pub const CONFIG_SCHEMA_VERSION: u32 = mge_config::SCHEMA_VERSION;

/// Localization keys for the weather rows, in `WeatherSet::as_array` order.
pub const WEATHER_NAMES: [&str; 10] = [
    "weather.clear",
    "weather.cloudy",
    "weather.foggy",
    "weather.overcast",
    "weather.rain",
    "weather.thunderstorm",
    "weather.ashstorm",
    "weather.blight",
    "weather.snow",
    "weather.blizzard",
];

/// The GUI's live edit state.
///
/// `mge` is the `mgeXE.toml` schema itself, edited in place by the widgets.
/// There is no second copy of it and nothing to translate. The other two fields
/// are here because they are genuinely not that: `ini` is a different file, and
/// `input` is the dense per-key form the keyboard widgets index, folded back
/// into `mge.input` by [`AppSettings::persistable`].
#[derive(Clone, Debug, Default)]
pub struct AppSettings {
    pub mge: Settings,
    pub ini: IniSettings,
    pub input: InputSettings,
}

impl AppSettings {
    fn new(mge: &Settings) -> Self {
        Self {
            mge: mge.clone(),
            ini: IniSettings::default(),
            input: InputSettings::from_config(&mge.input),
        }
    }

    /// The only conversion left before a write: the keyboard editors work on
    /// dense per-key tables, the schema persists the sparse assignments.
    fn persistable(&self) -> Settings {
        let mut mge = self.mge.clone();
        mge.input = self.input.to_config();
        mge
    }
}

/// Horizontal FOV that preserves Morrowind's vertical view at a given screen
/// size. The engine's stock 75° is horizontal at 4:3, so a wider screen has to
/// widen the horizontal angle to keep the same vertical extent.
pub fn auto_fov_degrees(width: u32, height: u32) -> f32 {
    const BASE_FOV: f64 = 75.0;
    if width == 0 || height == 0 {
        return BASE_FOV as f32;
    }
    let aspect = f64::from(width) / f64::from(height);
    let half_base = 0.5 * BASE_FOV.to_radians();
    let fov = 2.0 * ((aspect / (4.0 / 3.0)) * half_base.tan()).atan();
    // The spinner and the schema both bound FOV to 5..=150; an extreme
    // resolution must not park an out-of-range value in the TOML.
    (fov.to_degrees() as f32).clamp(FOV_MIN, FOV_MAX)
}

pub const FOV_MIN: f32 = mge_config::FOV_RANGE.0;
pub const FOV_MAX: f32 = mge_config::FOV_RANGE.1;

/// Recompute the FOV from the screen size when Auto FOV is on. Call after
/// anything that changes either the tick or the resolution it derives from.
pub fn refresh_auto_fov(settings: &mut Settings, width: u32, height: u32) {
    if settings.gui.match_fov_to_aspect_ratio {
        settings.render.fov = auto_fov_degrees(width, height);
    }
}

/// Derive the distance and fog tiers the automatic modes own, then normalize.
pub fn update_auto_distances(settings: &mut Settings, min_static_size: Option<f32>) {
    let mode = settings.gui.auto_distance_mode;
    if settings.gui.auto_distances {
        let distant = &mut settings.distant_land;
        // Modes 1 and 2 are fog-driven: the user sets the fog range and draw distance
        // follows. Mode 0 is the inverse.
        let by_fog_end = matches!(mode, 1 | 2);
        if by_fog_end {
            distant.draw_distance = distant
                .fog
                .above_water_end
                .clamp(DRAW_DISTANCE_RANGE.0, DRAW_DISTANCE_RANGE.1);
        }
        let draw_distance = distant.draw_distance;
        let minimum_static_distance = draw_distance.min(4.0);
        if !by_fog_end {
            distant.fog.above_water_start = draw_distance * 0.24 + 0.4;
            distant.fog.above_water_end = draw_distance;
        }
        distant.fog.below_water_start = -0.5;
        distant.fog.below_water_end = 0.3;
        distant.fog.interior_start = 0.0;
        distant.fog.interior_end = draw_distance * 0.5 + 0.5;
        if mode == 2 {
            // No pop-in: every tier runs to the draw distance (which equals the
            // fog end here), and the renderer clamps statics to
            // `cullDist = fogEnd` anyway, so statics are culled exactly where
            // fog has already hidden them and the per-tier LOD bands never
            // fire. Costs fill rate; that is the trade.
            distant.near_static_end = draw_distance;
            distant.far_static_end = draw_distance;
            distant.very_far_static_end = draw_distance;
        } else {
            distant.near_static_end = (draw_distance * 0.3).max(minimum_static_distance);
            distant.far_static_end = (draw_distance * 0.67).max(minimum_static_distance);
            distant.very_far_static_end = (draw_distance * 0.98).max(minimum_static_distance);
        }
    }
    normalize_distances(settings, min_static_size);
}

/// Immediate feedback while a spinner is being dragged. The cross-field
/// relationships live in `mge-config`, which is also what runs them on save, so
/// there is one copy. Per-field bounds are already on the spinners themselves.
///
/// `min_static_size` is the size the distant-land data was generated with; it
/// floors both tier minimums and is GUI-only (not in the schema).
pub fn normalize_distances(settings: &mut Settings, min_static_size: Option<f32>) {
    let distant = &mut settings.distant_land;
    if let Some(floor) = min_static_size {
        let floor = floor.clamp(STATIC_MIN_SIZE_RANGE.0, STATIC_MIN_SIZE_RANGE.1);
        distant.far_static_min_size = distant.far_static_min_size.max(floor);
        distant.very_far_static_min_size = distant.very_far_static_min_size.max(floor);
    }
    mge_config::normalize_distant_land(distant, &mut Vec::new());
}

pub struct SettingsStore {
    root: PathBuf,
    mge: ConfigDocument,
    morrowind: MorrowindProfile,
}

pub(crate) struct SaveResults {
    pub(crate) toml: Result<()>,
    pub(crate) morrowind_ini: Result<()>,
}

impl SettingsStore {
    pub fn load(root: impl Into<PathBuf>) -> Result<(Self, AppSettings)> {
        let root = root.into();
        let mge = ConfigDocument::open(root.join(mge_config::FILE_NAME));
        let morrowind = MorrowindProfile::open(&root)?;
        let mut settings = AppSettings::new(mge.settings());
        morrowind.load(&mut settings.ini)?;
        Ok((Self { root, mge, morrowind }, settings))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.mge.diagnostic()
    }

    pub fn warnings(&self) -> &[mge_config::Warning] {
        self.mge.warnings()
    }

    pub fn reload(&mut self) -> Result<AppSettings> {
        self.mge.reload()?;
        let mut settings = AppSettings::new(self.mge.settings());
        self.morrowind.load(&mut settings.ini)?;
        Ok(settings)
    }

    pub fn save(&mut self, settings: &AppSettings) -> SaveResults {
        let toml = self
            .mge
            .replace_settings(settings.persistable())
            .and_then(|()| self.mge.save())
            .context("save mgeXE.toml");
        let morrowind_ini = self.morrowind.save(&settings.ini);
        SaveResults { toml, morrowind_ini }
    }

    /// Atomically replaces and saves the generator-owned root table.
    pub fn save_generation_job(&mut self, document: &str) -> Result<()> {
        let mut candidate = self.mge.clone();
        candidate.replace_root_table_from_document(distantland::GENERATION_JOB_NAMESPACE, document)?;
        candidate.save().context("save distant-land settings in mgeXE.toml")?;
        self.mge = candidate;
        Ok(())
    }

    pub fn reset(&mut self, clear: bool) -> Result<AppSettings> {
        let mut candidate = self.mge.clone();
        candidate.reset_to_defaults(clear);
        let mut settings = AppSettings::new(candidate.settings());
        self.morrowind.load(&mut settings.ini)?;
        candidate.save().context("restore mgeXE.toml defaults")?;
        self.mge = candidate;
        Ok(settings)
    }

    pub fn import_mge(&mut self, source: &Path) -> Result<AppSettings> {
        let imported = ConfigDocument::open(source);
        if imported.state() != mge_config::OpenState::Valid {
            let detail = imported
                .diagnostic()
                .map(str::to_owned)
                .unwrap_or_else(|| t!("messages.file_missing_invalid").into_owned());
            bail!(
                "{}",
                t!(
                    "messages.import_source_failed",
                    path = source.display().to_string(),
                    error = detail
                )
            );
        }
        let mut candidate = self.mge.clone();
        candidate.replace_settings(imported.settings().clone())?;
        let mut settings = AppSettings::new(candidate.settings());
        self.morrowind.load(&mut settings.ini)?;
        candidate.save().context("import mgeXE.toml settings")?;
        self.mge = candidate;
        Ok(settings)
    }

    pub fn export_mge(&self, target: &Path, settings: &AppSettings) -> Result<()> {
        self.mge
            .write_copy(target, settings.persistable())
            .context("export mgeXE.toml")
    }
}

pub const AA_VALUES: [(AntiAliasing, &str); 4] = [
    (AntiAliasing::None, "graphics.choices.none"),
    (AntiAliasing::X2, "graphics.choices.2x"),
    (AntiAliasing::X4, "graphics.choices.4x"),
    (AntiAliasing::X8, "graphics.choices.8x"),
];
pub const VSYNC_VALUES: [(VSync, &str); 5] = [
    (VSync::Immediate, "common.choices.off"),
    (VSync::One, "common.choices.on"),
    (VSync::Two, "graphics.choices.x2"),
    (VSync::Three, "graphics.choices.x3"),
    (VSync::Four, "graphics.choices.x4"),
];
pub const ANISO_VALUES: [(Anisotropy, &str); 5] = [
    (Anisotropy::Off, "common.choices.off"),
    (Anisotropy::X2, "graphics.choices.2x"),
    (Anisotropy::X4, "graphics.choices.4x"),
    (Anisotropy::X8, "graphics.choices.8x"),
    (Anisotropy::X16, "graphics.choices.16x"),
];
pub const FOG_VALUES: [(FogMode, &str); 3] = [
    (FogMode::DepthPixel, "graphics.fog_mode.depth_pixel"),
    (FogMode::DepthVertex, "graphics.fog_mode.depth_vertex"),
    (FogMode::RangeVertex, "graphics.fog_mode.range_vertex"),
];
pub const SS_FORMAT_VALUES: [(ScreenshotFormat, &str); 5] = [
    (ScreenshotFormat::Bmp, "BMP"),
    (ScreenshotFormat::Jpeg, "JPEG"),
    (ScreenshotFormat::Dds, "DDS"),
    (ScreenshotFormat::Png, "PNG"),
    (ScreenshotFormat::Tga, "TGA"),
];
pub const SS_SUFFIX_VALUES: [(ScreenshotSuffix, &str); 4] = [
    (ScreenshotSuffix::Timestamp, "graphics.screenshots.suffix.timestamp"),
    (ScreenshotSuffix::Ordinal, "graphics.screenshots.suffix.ordinal"),
    (
        ScreenshotSuffix::CharacterOrdinal,
        "graphics.screenshots.suffix.character_ordinal",
    ),
    (
        ScreenshotSuffix::CharacterGameTimeOrdinal,
        "graphics.screenshots.suffix.character_time_ordinal",
    ),
];
pub const SS_SUFFIX_PREVIEWS: [&str; 4] = ["2020-07-04 12.50.11.808", "0048", "Player 0119", "Player, Day 71, 07.38 0030"];
pub const PPL_VALUES: [(PerPixelMode, &str); 2] = [
    (PerPixelMode::Always, "distant.lighting.always"),
    (PerPixelMode::InteriorsOnly, "distant.lighting.interiors_only"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn store_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("mge-gui-{name}-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Morrowind.ini"), "[General]\n").unwrap();
        root
    }

    #[test]
    fn auto_fov_matches_the_legacy_formula() {
        // 4:3 is the reference aspect and must return the stock FOV unchanged.
        assert!((auto_fov_degrees(1024, 768) - 75.0).abs() < 1e-3);
        // Expected values are the legacy WinForms expression evaluated in
        // double precision for 16:9, 16:10 and 21:9.
        assert!((auto_fov_degrees(1920, 1080) - 91.308_51).abs() < 1e-3);
        assert!((auto_fov_degrees(1920, 1200) - 85.277_27).abs() < 1e-3);
        assert!((auto_fov_degrees(2560, 1080) - 107.512377).abs() < 1e-3);
        // Degenerate input must not produce NaN.
        assert_eq!(auto_fov_degrees(1920, 0), 75.0);
        // Extremes stay inside the spinner/schema range.
        assert!((FOV_MIN..=FOV_MAX).contains(&auto_fov_degrees(15360, 1080)));
    }

    #[test]
    fn refresh_auto_fov_only_applies_when_enabled() {
        let mut settings = Settings::default();
        settings.gui.match_fov_to_aspect_ratio = false;
        settings.render.fov = 60.0;
        refresh_auto_fov(&mut settings, 1920, 1080);
        assert_eq!(settings.render.fov, 60.0);

        settings.gui.match_fov_to_aspect_ratio = true;
        refresh_auto_fov(&mut settings, 1920, 1080);
        assert_eq!(settings.render.fov, auto_fov_degrees(1920, 1080));
    }

    #[test]
    fn the_dense_input_tables_fold_back() {
        let mut settings = AppSettings::default();
        settings.input.remap[42] = 7;
        settings.input.triggers[1].keys[200] = true;

        let persisted = settings.persistable();
        assert_eq!(persisted.input.remap.get(&42), Some(&7));
        assert_eq!(persisted.input.triggers[0].index, 1);
    }

    #[test]
    fn generated_min_static_size_only_bounds_tier_minimums_from_below() {
        // The post-generation reconcile hands the tree's baked-in minimum to
        // `normalize_distances`, which must clamp the way the C# GUI's
        // `ValidateDistances` did: a higher user setting survives, a lower one
        // is raised. The generated value is never adopted outright.
        let mut settings = Settings::default();
        settings.distant_land.far_static_min_size = 600.0;
        settings.distant_land.very_far_static_min_size = 900.0;
        normalize_distances(&mut settings, Some(150.0));
        assert_eq!(settings.distant_land.far_static_min_size, 600.0);
        assert_eq!(settings.distant_land.very_far_static_min_size, 900.0);

        settings.distant_land.far_static_min_size = 100.0;
        normalize_distances(&mut settings, Some(150.0));
        assert_eq!(settings.distant_land.far_static_min_size, 150.0);
        // Very Far is still dragged up to at least Far.
        assert!(settings.distant_land.very_far_static_min_size >= settings.distant_land.far_static_min_size);
    }

    #[test]
    fn automatic_distances_write_concrete_renderer_values() {
        let mut settings = Settings::default();
        settings.distant_land.enabled = true;
        settings.distant_land.draw_distance = 10.0;
        update_auto_distances(&mut settings, None);
        assert_eq!(settings.distant_land.near_static_end, 4.0);
        assert!((settings.distant_land.far_static_end - 6.7).abs() < 0.00001);
        assert!((settings.distant_land.very_far_static_end - 9.8).abs() < 0.00001);
    }

    #[test]
    fn the_no_pop_mode_holds_every_static_tier_at_the_fog_end() {
        // Mode 2 exists so statics are culled only where fog has already hidden them.
        // The renderer clamps them to `cullDist = fogEnd`, so every tier ending at the
        // fog end means no tier boundary is ever reached while a static is still visible.
        let mut settings = Settings::default();
        settings.distant_land.enabled = true;
        settings.gui.auto_distances = true;
        settings.gui.auto_distance_mode = 2;
        settings.distant_land.fog.above_water_end = 7.0;
        update_auto_distances(&mut settings, None);

        // Fog-driven, like mode 1: the user's fog end survives and drives draw distance.
        assert_eq!(settings.distant_land.fog.above_water_end, 7.0);
        assert_eq!(settings.distant_land.draw_distance, 7.0);
        assert_eq!(settings.distant_land.near_static_end, 7.0);
        assert_eq!(settings.distant_land.far_static_end, 7.0);
        assert_eq!(settings.distant_land.very_far_static_end, 7.0);
    }

    #[test]
    fn reset_and_import_failures_leave_the_live_store_unchanged() {
        for action in ["reset", "import"] {
            let root = store_root(action);
            let live_path = root.join(mge_config::FILE_NAME);
            fs::write(&live_path, mge_config::DEFAULT_DOCUMENT).unwrap();
            let (mut store, displayed) = SettingsStore::load(&root).unwrap();
            let before = store.mge.settings().clone();

            let external = format!("{}\nexternal = true\n", mge_config::DEFAULT_DOCUMENT);
            fs::write(&live_path, &external).unwrap();
            let result = if action == "reset" {
                store.reset(true)
            } else {
                let import_path = root.join("import.toml");
                let mut imported = ConfigDocument::open(&import_path);
                imported.set_number("render.fov", 100.0).unwrap();
                imported.save().unwrap();
                store.import_mge(&import_path)
            };

            assert!(result.is_err());
            assert_eq!(store.mge.settings(), &before);
            assert_eq!(displayed.mge.render.fov, before.render.fov);
            assert_eq!(fs::read_to_string(&live_path).unwrap(), external);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn export_writes_unsaved_gui_edits_without_touching_the_live_document() {
        let root = store_root("export");
        let live_path = root.join(mge_config::FILE_NAME);
        let target = root.join("export.toml");
        fs::write(&live_path, mge_config::DEFAULT_DOCUMENT).unwrap();
        let live_before = fs::read(&live_path).unwrap();
        let (store, mut settings) = SettingsStore::load(&root).unwrap();
        let stored_fov = store.mge.settings().render.fov;
        settings.mge.render.fov = 96.0;

        store.export_mge(&target, &settings).unwrap();

        assert_eq!(fs::read(&live_path).unwrap(), live_before);
        assert_eq!(store.mge.settings().render.fov, stored_fov);
        assert_eq!(ConfigDocument::open(&target).settings().render.fov, 96.0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_toml_does_not_block_independent_morrowind_ini_updates() {
        let root = store_root("independent-ini-save");
        let live_path = root.join(mge_config::FILE_NAME);
        let malformed = "schema_version = 1\n[render\n";
        fs::write(&live_path, malformed).unwrap();

        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("config::tests::invalid_toml_does_not_block_independent_morrowind_ini_updates_child")
            .arg("--nocapture")
            .env("MGE_XE_TEST_INDEPENDENT_SAVE_ROOT", &root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(&live_path).unwrap(), malformed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_toml_does_not_block_independent_morrowind_ini_updates_child() {
        let Some(root) = std::env::var_os("MGE_XE_TEST_INDEPENDENT_SAVE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        std::env::set_current_dir(&root).unwrap();
        let (mut store, mut settings) = SettingsStore::load(&root).unwrap();
        settings.ini.screenshots = true;

        let results = store.save(&settings);

        assert!(results.toml.is_err());
        assert!(results.morrowind_ini.is_ok());
        let mut reloaded = AppSettings::default();
        store.morrowind.load(&mut reloaded.ini).unwrap();
        assert!(reloaded.ini.screenshots);
    }
}
