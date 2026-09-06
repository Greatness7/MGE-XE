use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 3;
pub const WEATHER_NAMES: [&str; 10] = [
    "clear",
    "cloudy",
    "foggy",
    "overcast",
    "rain",
    "thunderstorm",
    "ashstorm",
    "blight",
    "snow",
    "blizzard",
];
pub const INPUT_COUNT: usize = 266;
pub const TRIGGER_COUNT: usize = 4;
pub const GRAPHICS_FUNCTION_COUNT: u8 = 54;
pub const HORIZON_BIAS_Z_RANGE: (f32, f32) = (0.0, 32768.0);
pub const HORIZON_OBJECT_BIAS_Z_RANGE: (f32, f32) = (0.0, 32768.0);
pub const HORIZON_NEAR_UNITS_RANGE: (f32, f32) = (0.0, 65536.0);
pub const HORIZON_RING_STEP_RANGE: (f32, f32) = (1.0, 65536.0);
pub const HORIZON_MAX_RANGE_RANGE: (f32, f32) = (1.0, 1_048_576.0);
pub const HORIZON_BINS_RANGE: (u32, u32) = (64, 4096);
pub const HORIZON_SAMPLE_SPACING_RANGE: (f32, f32) = (1.0, 8192.0);
pub const HORIZON_REBUILD_EYE_THRESHOLD_RANGE: (f32, f32) = (0.0, 8192.0);

// Bounds shared with the GUI controls that edit these fields. Validation and
// the widgets must agree, or a value the user just picked is clamped on save.
pub const FOV_RANGE: (f32, f32) = (5.0, 150.0);
pub const DRAW_DISTANCE_RANGE: (f32, f32) = (1.0, 300.0);
pub const NEAR_STATIC_END_RANGE: (f32, f32) = (2.0, 299.8);
pub const FAR_STATIC_END_RANGE: (f32, f32) = (2.1, 299.9);
pub const VERY_FAR_STATIC_END_RANGE: (f32, f32) = (2.2, 300.0);
pub const STATIC_MIN_SIZE_RANGE: (f32, f32) = (0.0, 9999.0);
pub const FOG_ABOVE_START_RANGE: (f32, f32) = (0.0, 299.9);
pub const FOG_ABOVE_END_RANGE: (f32, f32) = (0.1, 300.0);
pub const FOG_BELOW_START_RANGE: (f32, f32) = (-99.9, 299.9);
pub const FOG_BELOW_END_RANGE: (f32, f32) = (0.1, 300.0);
pub const FOG_INTERIOR_START_RANGE: (f32, f32) = (-0.9, 299.9);
pub const FOG_INTERIOR_END_RANGE: (f32, f32) = (0.1, 300.0);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub graphics: GraphicsSettings,
    pub render: RenderSettings,
    pub runtime: RuntimeSettings,
    pub distant_land: DistantLandSettings,
    pub lighting: LightingSettings,
    pub shaders: ShaderSettings,
    pub input: InputSettings,
    pub gui: GuiSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            graphics: GraphicsSettings::default(),
            render: RenderSettings::default(),
            runtime: RuntimeSettings::default(),
            distant_land: DistantLandSettings::default(),
            lighting: LightingSettings::default(),
            shaders: ShaderSettings::default(),
            input: InputSettings::default(),
            gui: GuiSettings::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AntiAliasing {
    #[default]
    None,
    X2,
    X4,
    X8,
}

impl AntiAliasing {
    pub fn runtime_value(self) -> u8 {
        match self {
            Self::None => 0,
            Self::X2 => 2,
            Self::X4 => 4,
            Self::X8 => 8,
        }
    }

    pub fn from_runtime(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::None,
            2 => Self::X2,
            4 => Self::X4,
            8 => Self::X8,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZBufferFormat {
    #[default]
    D24s8,
    D24x8,
    D32,
    D16,
    D16l,
    D32fl,
    Intz,
    Rawz,
    Df16,
    Df24,
}

impl ZBufferFormat {
    pub fn runtime_value(self) -> u8 {
        match self {
            Self::D24s8 => 75,
            Self::D24x8 => 77,
            Self::D32 => 71,
            Self::D16 => 80,
            Self::D16l => 70,
            Self::D32fl => 82,
            Self::Intz => 1,
            Self::Rawz => 2,
            Self::Df16 => 3,
            Self::Df24 => 4,
        }
    }

    pub fn from_runtime(value: u8) -> Option<Self> {
        Some(match value {
            75 => Self::D24s8,
            77 => Self::D24x8,
            71 => Self::D32,
            80 => Self::D16,
            70 => Self::D16l,
            82 => Self::D32fl,
            1 => Self::Intz,
            2 => Self::Rawz,
            3 => Self::Df16,
            4 => Self::Df24,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VSync {
    #[default]
    Immediate,
    One,
    Two,
    Three,
    Four,
}

impl VSync {
    pub fn runtime_value(self) -> u8 {
        match self {
            Self::Immediate => 255,
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 4,
            Self::Four => 8,
        }
    }

    pub fn from_runtime(value: u8) -> Option<Self> {
        Some(match value {
            255 => Self::Immediate,
            1 => Self::One,
            2 => Self::Two,
            4 => Self::Three,
            8 => Self::Four,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anisotropy {
    Off,
    X2,
    X4,
    X8,
    #[default]
    X16,
}

impl Anisotropy {
    pub fn runtime_value(self) -> u8 {
        match self {
            Self::Off => 1,
            Self::X2 => 2,
            Self::X4 => 4,
            Self::X8 => 8,
            Self::X16 => 16,
        }
    }

    pub fn from_runtime(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Off,
            2 => Self::X2,
            4 => Self::X4,
            8 => Self::X8,
            16 => Self::X16,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FogMode {
    DepthPixel,
    DepthVertex,
    #[default]
    RangeVertex,
}

impl FogMode {
    pub fn runtime_value(self) -> u8 {
        self as u8
    }

    pub fn from_runtime(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::DepthPixel,
            1 => Self::DepthVertex,
            2 => Self::RangeVertex,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotFormat {
    Bmp,
    Jpeg,
    Dds,
    #[default]
    Png,
    Tga,
}

impl ScreenshotFormat {
    pub fn runtime_value(self) -> u8 {
        self as u8
    }

    pub fn from_runtime(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Bmp,
            1 => Self::Jpeg,
            2 => Self::Dds,
            3 => Self::Png,
            4 => Self::Tga,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotSuffix {
    #[default]
    Timestamp,
    Ordinal,
    CharacterOrdinal,
    CharacterGameTimeOrdinal,
}

impl ScreenshotSuffix {
    pub fn runtime_value(self) -> u8 {
        self as u8
    }

    pub fn from_runtime(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Timestamp,
            1 => Self::Ordinal,
            2 => Self::CharacterOrdinal,
            3 => Self::CharacterGameTimeOrdinal,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Left,
    #[default]
    Center,
    Right,
}

impl Alignment {
    pub fn runtime_value(self) -> i32 {
        self as i32
    }

    pub fn from_runtime(value: i32) -> Option<Self> {
        Some(match value {
            0 => Self::Left,
            1 => Self::Center,
            2 => Self::Right,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerPixelMode {
    #[default]
    Always,
    InteriorsOnly,
}

impl PerPixelMode {
    pub fn runtime_value(self) -> u32 {
        self as u32
    }

    pub fn from_runtime(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Always,
            1 => Self::InteriorsOnly,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsSettings {
    pub anti_aliasing: AntiAliasing,
    pub z_buffer_format: ZBufferFormat,
    pub vsync: VSync,
    pub refresh_rate: u8,
    pub borderless: bool,
    pub anisotropy: Anisotropy,
    pub transparency_antialiasing: bool,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            anti_aliasing: AntiAliasing::None,
            z_buffer_format: ZBufferFormat::D24s8,
            vsync: VSync::Immediate,
            refresh_rate: 0,
            borderless: true,
            anisotropy: Anisotropy::X8,
            transparency_antialiasing: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderSettings {
    pub fov: f32,
    pub fog_mode: FogMode,
    pub enable_shaders: bool,
    pub indexed_skinning: bool,
    pub camera_relative: bool,
    pub camera_relative_probe: bool,
    pub hdr_reaction_time: f32,
    pub fps_counter: bool,
    pub messages: bool,
    pub message_timeout_ms: i32,
    pub screenshot_format: ScreenshotFormat,
    pub screenshot_directory: String,
    pub screenshot_name: String,
    pub screenshot_suffix: ScreenshotSuffix,
    pub ui_scale: f32,
    pub window_align_x: Alignment,
    pub window_align_y: Alignment,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            fov: 75.0,
            fog_mode: FogMode::RangeVertex,
            enable_shaders: false,
            indexed_skinning: false,
            camera_relative: false,
            camera_relative_probe: false,
            hdr_reaction_time: 2.0,
            fps_counter: false,
            messages: true,
            message_timeout_ms: 2_000,
            screenshot_format: ScreenshotFormat::Png,
            screenshot_directory: String::new(),
            screenshot_name: "Morrowind".into(),
            screenshot_suffix: ScreenshotSuffix::Timestamp,
            ui_scale: 1.0,
            window_align_x: Alignment::Center,
            window_align_y: Alignment::Center,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeSettings {
    pub disabled: bool,
    pub mwse_disabled: bool,
    pub proxy_only: bool,
    pub skip_intro: bool,
    pub menu_caching: bool,
    pub custom_camera: bool,
    pub camera_x: f32,
    pub camera_y: f32,
    pub camera_z: f32,
    pub crosshair_autohide: bool,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            disabled: false,
            mwse_disabled: false,
            proxy_only: false,
            skip_intro: true,
            menu_caching: false,
            custom_camera: false,
            camera_x: 0.0,
            camera_y: -160.0,
            camera_z: 0.0,
            crosshair_autohide: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DistantLandSettings {
    pub enabled: bool,
    pub automatic_rebuild: bool,
    pub native_depth_capture: bool,
    pub native_ppl_packets: bool,
    pub expanded_light_limit: bool,
    pub statics: bool,
    pub water_without_land: bool,
    pub render_grass: bool,
    pub draw_distance: f32,
    pub near_static_end: f32,
    pub far_static_end: f32,
    pub very_far_static_end: f32,
    pub far_static_min_size: f32,
    pub very_far_static_min_size: f32,
    pub water: WaterSettings,
    pub fog: FogSettings,
    pub shadows: ShadowSettings,
    pub per_pixel_lighting: bool,
    pub per_pixel_mode: PerPixelMode,
    pub horizon: HorizonSettings,
    pub weather: WeatherSet,
}

impl Default for DistantLandSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            automatic_rebuild: false,
            native_depth_capture: true,
            native_ppl_packets: true,
            expanded_light_limit: false,
            statics: false,
            water_without_land: true,
            render_grass: true,
            draw_distance: 5.0,
            near_static_end: 2.0,
            far_static_end: 4.0,
            very_far_static_end: 5.0,
            far_static_min_size: 600.0,
            very_far_static_min_size: 800.0,
            water: WaterSettings::default(),
            fog: FogSettings::default(),
            shadows: ShadowSettings::default(),
            per_pixel_lighting: false,
            per_pixel_mode: PerPixelMode::Always,
            horizon: HorizonSettings::default(),
            weather: WeatherSet::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WaterSettings {
    pub reflect_land: bool,
    pub reflect_near_statics: bool,
    pub reflect_interiors: bool,
    pub reflect_sky: bool,
    pub dynamic_ripples: bool,
    pub blur_reflections: bool,
    pub wave_height: u8,
    pub caustics_intensity: u8,
}

impl Default for WaterSettings {
    fn default() -> Self {
        Self {
            reflect_land: true,
            reflect_near_statics: true,
            reflect_interiors: true,
            reflect_sky: true,
            dynamic_ripples: false,
            blur_reflections: false,
            wave_height: 50,
            caustics_intensity: 50,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FogSettings {
    pub exponential: bool,
    pub atmosphere_scattering: bool,
    pub above_water_start: f32,
    pub above_water_end: f32,
    pub below_water_start: f32,
    pub below_water_end: f32,
    pub interior_start: f32,
    pub interior_end: f32,
}

impl Default for FogSettings {
    fn default() -> Self {
        Self {
            exponential: true,
            atmosphere_scattering: true,
            above_water_start: 2.0,
            above_water_end: 5.0,
            below_water_start: -0.5,
            below_water_end: 0.3,
            interior_start: 0.0,
            interior_end: 2.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShadowSettings {
    pub enabled: bool,
    pub map_resolution: u32,
    /// Cells from the eye within which distant statics cast shadows; 0 means no limit.
    pub static_range: f32,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            map_resolution: 2048,
            static_range: 4.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HorizonSettings {
    pub culling: bool,
    pub height_bias: f32,
    pub object_bias: f32,
    pub near_exclude: f32,
    pub ring_step: f32,
    pub max_range: f32,
    pub azimuth_bins: u32,
    pub sample_spacing: f32,
    pub adaptive_gate: bool,
    pub rebuild_eye_threshold: f32,
    pub hierarchical_march: bool,
}

impl Default for HorizonSettings {
    fn default() -> Self {
        Self {
            culling: true,
            height_bias: 512.0,
            object_bias: 256.0,
            near_exclude: 2048.0,
            ring_step: 4096.0,
            max_range: 49152.0,
            azimuth_bins: 512,
            sample_spacing: 512.0,
            adaptive_gate: true,
            rebuild_eye_threshold: 16.0,
            hierarchical_march: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeatherSettings {
    pub wind: f32,
    pub fog_ratio: f32,
    pub fog_offset: f32,
}

impl Default for WeatherSettings {
    fn default() -> Self {
        Self {
            wind: 0.0,
            fog_ratio: 1.0,
            fog_offset: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeatherSet {
    pub clear: WeatherSettings,
    pub cloudy: WeatherSettings,
    pub foggy: WeatherSettings,
    pub overcast: WeatherSettings,
    pub rain: WeatherSettings,
    pub thunderstorm: WeatherSettings,
    pub ashstorm: WeatherSettings,
    pub blight: WeatherSettings,
    pub snow: WeatherSettings,
    pub blizzard: WeatherSettings,
}

impl Default for WeatherSet {
    fn default() -> Self {
        let values = [
            (0.1, 1.0, 0.0),
            (0.2, 0.9, 0.0),
            (0.0, 0.2, 30.0),
            (0.2, 0.7, 0.0),
            (0.3, 0.5, 10.0),
            (0.5, 0.5, 20.0),
            (0.8, 0.25, 45.0),
            (0.9, 0.25, 50.0),
            (0.0, 0.5, 40.0),
            (0.9, 0.16, 100.0),
        ]
        .map(|(wind, fog_ratio, fog_offset)| WeatherSettings {
            wind,
            fog_ratio,
            fog_offset,
        });
        Self {
            clear: values[0],
            cloudy: values[1],
            foggy: values[2],
            overcast: values[3],
            rain: values[4],
            thunderstorm: values[5],
            ashstorm: values[6],
            blight: values[7],
            snow: values[8],
            blizzard: values[9],
        }
    }
}

impl WeatherSet {
    pub fn as_array(&self) -> [&WeatherSettings; 10] {
        [
            &self.clear,
            &self.cloudy,
            &self.foggy,
            &self.overcast,
            &self.rain,
            &self.thunderstorm,
            &self.ashstorm,
            &self.blight,
            &self.snow,
            &self.blizzard,
        ]
    }

    pub fn as_mut_array(&mut self) -> [&mut WeatherSettings; 10] {
        [
            &mut self.clear,
            &mut self.cloudy,
            &mut self.foggy,
            &mut self.overcast,
            &mut self.rain,
            &mut self.thunderstorm,
            &mut self.ashstorm,
            &mut self.blight,
            &mut self.snow,
            &mut self.blizzard,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeatherLighting {
    pub sun: f32,
    pub ambient: f32,
}

impl Default for WeatherLighting {
    fn default() -> Self {
        Self { sun: 1.0, ambient: 1.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightingSettings {
    pub weather: LightingWeatherSet,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightingWeatherSet {
    pub clear: WeatherLighting,
    pub cloudy: WeatherLighting,
    pub foggy: WeatherLighting,
    pub overcast: WeatherLighting,
    pub rain: WeatherLighting,
    pub thunderstorm: WeatherLighting,
    pub ashstorm: WeatherLighting,
    pub blight: WeatherLighting,
    pub snow: WeatherLighting,
    pub blizzard: WeatherLighting,
}

impl LightingWeatherSet {
    pub fn as_array(&self) -> [&WeatherLighting; 10] {
        [
            &self.clear,
            &self.cloudy,
            &self.foggy,
            &self.overcast,
            &self.rain,
            &self.thunderstorm,
            &self.ashstorm,
            &self.blight,
            &self.snow,
            &self.blizzard,
        ]
    }

    pub fn as_mut_array(&mut self) -> [&mut WeatherLighting; 10] {
        [
            &mut self.clear,
            &mut self.cloudy,
            &mut self.foggy,
            &mut self.overcast,
            &mut self.rain,
            &mut self.thunderstorm,
            &mut self.ashstorm,
            &mut self.blight,
            &mut self.snow,
            &mut self.blizzard,
        ]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShaderSettings {
    pub chain: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacroKind {
    #[default]
    Unused,
    Console1,
    Console2,
    Hammer1,
    Hammer2,
    Unhammer,
    AlternateHammer1,
    AlternateHammer2,
    AlternateUnhammer,
    Press1,
    Press2,
    Unpress,
    BeginTimer,
    EndTimer,
    Graphics,
}

impl MacroKind {
    pub fn legacy_name(self) -> &'static str {
        match self {
            Self::Unused => "Unused",
            Self::Console1 => "Console1",
            Self::Console2 => "Console2",
            Self::Hammer1 => "Hammer1",
            Self::Hammer2 => "Hammer2",
            Self::Unhammer => "Unhammer",
            Self::AlternateHammer1 => "AHammer1",
            Self::AlternateHammer2 => "AHammer2",
            Self::AlternateUnhammer => "AUnhammer",
            Self::Press1 => "Press1",
            Self::Press2 => "Press2",
            Self::Unpress => "Unpress",
            Self::BeginTimer => "BeginTimer",
            Self::EndTimer => "EndTimer",
            Self::Graphics => "Graphics",
        }
    }

    pub fn is_console(self) -> bool {
        matches!(self, Self::Console1 | Self::Console2)
    }

    pub fn is_press(self) -> bool {
        matches!(
            self,
            Self::Hammer1
                | Self::Hammer2
                | Self::Unhammer
                | Self::AlternateHammer1
                | Self::AlternateHammer2
                | Self::AlternateUnhammer
                | Self::Press1
                | Self::Press2
                | Self::Unpress
        )
    }

    pub fn is_timer(self) -> bool {
        matches!(self, Self::BeginTimer | Self::EndTimer)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyEvent {
    pub code: u16,
    pub down: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MacroSettings {
    pub index: u16,
    #[serde(rename = "type")]
    pub kind: MacroKind,
    pub key_events: Vec<KeyEvent>,
    pub description: String,
    pub keys: Vec<u16>,
    pub timer_id: u8,
    pub function: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerSettings {
    pub index: u8,
    pub active: bool,
    pub interval_ms: u32,
    pub keys: Vec<u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputSettings {
    pub macros: Vec<MacroSettings>,
    pub triggers: Vec<TriggerSettings>,
    pub remap: BTreeMap<u16, u8>,
}

fn decimal_len(value: u32) -> usize {
    if value == 0 { 1 } else { value.ilog10() as usize + 1 }
}

fn bool_len(value: bool) -> usize {
    if value { "True" } else { "False" }.len()
}

impl InputSettings {
    pub fn render_macros(&self) -> Vec<String> {
        self.macros
            .iter()
            .filter(|item| item.kind != MacroKind::Unused)
            .map(|item| {
                let mut line = format!("M{}={}", item.index, item.kind.legacy_name());
                match item.kind {
                    MacroKind::Console1 | MacroKind::Console2 => {
                        for event in &item.key_events {
                            line.push_str(&format!(",{},{}", event.code, if event.down { "True" } else { "False" }));
                        }
                    }
                    MacroKind::Hammer1
                    | MacroKind::Hammer2
                    | MacroKind::Unhammer
                    | MacroKind::AlternateHammer1
                    | MacroKind::AlternateHammer2
                    | MacroKind::AlternateUnhammer
                    | MacroKind::Press1
                    | MacroKind::Press2
                    | MacroKind::Unpress => {
                        for key in &item.keys {
                            line.push_str(&format!(",{key}"));
                        }
                    }
                    MacroKind::BeginTimer | MacroKind::EndTimer => {
                        line.push_str(&format!(",{}", item.timer_id));
                    }
                    MacroKind::Graphics => line.push_str(&format!(",{}", item.function)),
                    MacroKind::Unused => {}
                }
                line
            })
            .collect()
    }

    pub fn render_triggers(&self) -> Vec<String> {
        self.triggers
            .iter()
            .filter(|item| !item.keys.is_empty())
            .map(|item| {
                let keys = item.keys.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
                format!(
                    "T{}={},{}{}{}",
                    item.index,
                    if item.active { "True" } else { "False" },
                    item.interval_ms,
                    if keys.is_empty() { "" } else { "," },
                    keys
                )
            })
            .collect()
    }

    pub fn render_remap(&self) -> Vec<String> {
        self.remap
            .iter()
            .filter(|(_, target)| **target != 0)
            .map(|(source, target)| format!("R{source}={target}"))
            .collect()
    }

    pub(crate) fn rendered_macro_line_lengths(&self) -> impl Iterator<Item = usize> + '_ {
        self.macros.iter().filter(|item| item.kind != MacroKind::Unused).map(|item| {
            let mut len = 2 + decimal_len(item.index.into()) + item.kind.legacy_name().len();
            match item.kind {
                MacroKind::Console1 | MacroKind::Console2 => {
                    len += item
                        .key_events
                        .iter()
                        .map(|event| 2 + decimal_len(event.code.into()) + bool_len(event.down))
                        .sum::<usize>();
                }
                MacroKind::Hammer1
                | MacroKind::Hammer2
                | MacroKind::Unhammer
                | MacroKind::AlternateHammer1
                | MacroKind::AlternateHammer2
                | MacroKind::AlternateUnhammer
                | MacroKind::Press1
                | MacroKind::Press2
                | MacroKind::Unpress => {
                    len += item.keys.iter().map(|key| 1 + decimal_len((*key).into())).sum::<usize>();
                }
                MacroKind::BeginTimer | MacroKind::EndTimer => {
                    len += 1 + decimal_len(item.timer_id.into());
                }
                MacroKind::Graphics => {
                    len += 1 + decimal_len(item.function.into());
                }
                MacroKind::Unused => {}
            }
            len
        })
    }

    pub(crate) fn rendered_trigger_line_lengths(&self) -> impl Iterator<Item = usize> + '_ {
        self.triggers.iter().filter(|item| !item.keys.is_empty()).map(|item| {
            3 + decimal_len(item.index.into())
                + bool_len(item.active)
                + decimal_len(item.interval_ms)
                + item.keys.iter().map(|key| 1 + decimal_len((*key).into())).sum::<usize>()
        })
    }

    pub(crate) fn rendered_remap_line_lengths(&self) -> impl Iterator<Item = usize> + '_ {
        self.remap
            .iter()
            .filter(|(_, target)| **target != 0)
            .map(|(source, target)| 2 + decimal_len((*source).into()) + decimal_len((*target).into()))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    pub language: String,
    pub match_fov_to_aspect_ratio: bool,
    pub auto_distances: bool,
    pub auto_distance_mode: u8,
    pub exponential_distance_multiplier: f32,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            match_fov_to_aspect_ratio: true,
            auto_distances: true,
            auto_distance_mode: 0,
            exponential_distance_multiplier: 4.0,
        }
    }
}
