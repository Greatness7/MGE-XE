use crate::schema::{
    Alignment, Anisotropy, AntiAliasing, FogMode, PerPixelMode, ScreenshotFormat, ScreenshotSuffix, Settings, VSync,
    WEATHER_NAMES, ZBufferFormat,
};

#[cfg(feature = "contract-test")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeNumberRepr {
    Boolean,
    U8,
    U32,
    I32,
    F32,
}

enum RuntimeNumber {
    Boolean(bool),
    U8(u8),
    U32(u32),
    I32(i32),
    F32(f32),
}

impl RuntimeNumber {
    fn value(&self) -> f64 {
        match *self {
            Self::Boolean(value) => u8::from(value) as f64,
            Self::U8(value) => value as f64,
            Self::U32(value) => value as f64,
            Self::I32(value) => value as f64,
            Self::F32(value) => value as f64,
        }
    }

    #[cfg(feature = "contract-test")]
    fn repr(&self) -> RuntimeNumberRepr {
        match self {
            Self::Boolean(_) => RuntimeNumberRepr::Boolean,
            Self::U8(_) => RuntimeNumberRepr::U8,
            Self::U32(_) => RuntimeNumberRepr::U32,
            Self::I32(_) => RuntimeNumberRepr::I32,
            Self::F32(_) => RuntimeNumberRepr::F32,
        }
    }
}

macro_rules! runtime_number_from {
    ($type:ty, $variant:ident) => {
        impl From<$type> for RuntimeNumber {
            fn from(value: $type) -> Self {
                Self::$variant(value)
            }
        }
    };
}

runtime_number_from!(u8, U8);
runtime_number_from!(u32, U32);
runtime_number_from!(i32, I32);
runtime_number_from!(f32, F32);

fn bool_number(value: bool) -> RuntimeNumber {
    RuntimeNumber::Boolean(value)
}

fn number_bool(value: f64, path: &str) -> Result<bool, String> {
    match value {
        0.0 => Ok(false),
        1.0 => Ok(true),
        _ => Err(format!("{path} requires numeric boolean 0 or 1, got {value}")),
    }
}

fn integer<T>(value: f64, path: &str) -> Result<T, String>
where
    T: TryFrom<i64>,
{
    if value.is_finite() && value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        T::try_from(value as i64).map_err(|_| format!("{path} integer value {value} is out of range"))
    } else {
        Err(format!("{path} requires an integer value, got {value}"))
    }
}

/// The five per-weather scalars reachable from the runtime binding table. Weather paths are
/// the only binding paths with a variable segment, so they are parsed rather than matched.
enum WeatherField {
    Wind,
    FogRatio,
    FogOffset,
    Sun,
    Ambient,
}

/// Splits `distant_land.weather.<name>.<field>` or `lighting.weather.<name>.<field>` into the
/// index of `<name>` in [`WEATHER_NAMES`] and the field it selects. The two prefixes own
/// disjoint field names, so a mismatched pair such as `lighting.weather.clear.wind` is rejected.
fn parse_weather_path(path: &str) -> Option<(usize, WeatherField)> {
    let (rest, distant) = match path.strip_prefix("distant_land.weather.") {
        Some(rest) => (rest, true),
        None => (path.strip_prefix("lighting.weather.")?, false),
    };
    let (name, field) = rest.split_once('.')?;
    let index = WEATHER_NAMES.iter().position(|candidate| *candidate == name)?;
    let field = match (distant, field) {
        (true, "wind") => WeatherField::Wind,
        (true, "fog_ratio") => WeatherField::FogRatio,
        (true, "fog_offset") => WeatherField::FogOffset,
        (false, "sun") => WeatherField::Sun,
        (false, "ambient") => WeatherField::Ambient,
        _ => return None,
    };
    Some((index, field))
}

fn finite_f32(value: f64, path: &str) -> Result<f32, String> {
    let converted = value as f32;
    if converted.is_finite() {
        Ok(converted)
    } else {
        Err(format!("{path} requires a finite 32-bit float, got {value}"))
    }
}

impl Settings {
    fn runtime_number(&self, path: &str) -> Option<RuntimeNumber> {
        let value = match path {
            "graphics.anti_aliasing" => self.graphics.anti_aliasing.runtime_value().into(),
            "graphics.z_buffer_format" => self.graphics.z_buffer_format.runtime_value().into(),
            "graphics.vsync" => self.graphics.vsync.runtime_value().into(),
            "graphics.refresh_rate" => self.graphics.refresh_rate.into(),
            "graphics.borderless" => bool_number(self.graphics.borderless),
            "graphics.anisotropy" => self.graphics.anisotropy.runtime_value().into(),
            "graphics.transparency_antialiasing" => bool_number(self.graphics.transparency_antialiasing),
            "render.fov" => self.render.fov.into(),
            "render.fog_mode" => self.render.fog_mode.runtime_value().into(),
            "render.enable_shaders" => bool_number(self.render.enable_shaders),
            "render.indexed_skinning" => bool_number(self.render.indexed_skinning),
            "render.hdr_reaction_time" => self.render.hdr_reaction_time.into(),
            "render.fps_counter" => bool_number(self.render.fps_counter),
            "render.messages" => bool_number(self.render.messages),
            "render.message_timeout_ms" => self.render.message_timeout_ms.into(),
            "render.screenshot_format" => self.render.screenshot_format.runtime_value().into(),
            "render.screenshot_suffix" => self.render.screenshot_suffix.runtime_value().into(),
            "render.ui_scale" => self.render.ui_scale.into(),
            "render.window_align_x" => self.render.window_align_x.runtime_value().into(),
            "render.window_align_y" => self.render.window_align_y.runtime_value().into(),
            "runtime.disabled" => bool_number(self.runtime.disabled),
            "runtime.mwse_disabled" => bool_number(self.runtime.mwse_disabled),
            "runtime.proxy_only" => bool_number(self.runtime.proxy_only),
            "runtime.skip_intro" => bool_number(self.runtime.skip_intro),
            "runtime.menu_caching" => bool_number(self.runtime.menu_caching),
            "runtime.custom_camera" => bool_number(self.runtime.custom_camera),
            "runtime.camera_x" => self.runtime.camera_x.into(),
            "runtime.camera_y" => self.runtime.camera_y.into(),
            "runtime.camera_z" => self.runtime.camera_z.into(),
            "runtime.crosshair_autohide" => bool_number(self.runtime.crosshair_autohide),
            "distant_land.enabled" => bool_number(self.distant_land.enabled),
            "distant_land.automatic_rebuild" => bool_number(self.distant_land.automatic_rebuild),
            "distant_land.native_depth_capture" => bool_number(self.distant_land.native_depth_capture),
            "distant_land.native_ppl_packets" => bool_number(self.distant_land.native_ppl_packets),
            "distant_land.expanded_light_limit" => bool_number(self.distant_land.expanded_light_limit),
            "distant_land.statics" => bool_number(self.distant_land.statics),
            "distant_land.water_without_land" => bool_number(self.distant_land.water_without_land),
            "distant_land.render_grass" => bool_number(self.distant_land.render_grass),
            "distant_land.draw_distance" => self.distant_land.draw_distance.into(),
            "distant_land.near_static_end" => self.distant_land.near_static_end.into(),
            "distant_land.far_static_end" => self.distant_land.far_static_end.into(),
            "distant_land.very_far_static_end" => self.distant_land.very_far_static_end.into(),
            "distant_land.far_static_min_size" => self.distant_land.far_static_min_size.into(),
            "distant_land.very_far_static_min_size" => self.distant_land.very_far_static_min_size.into(),
            "distant_land.water.reflect_land" => bool_number(self.distant_land.water.reflect_land),
            "distant_land.water.reflect_near_statics" => bool_number(self.distant_land.water.reflect_near_statics),
            "distant_land.water.reflect_interiors" => bool_number(self.distant_land.water.reflect_interiors),
            "distant_land.water.reflect_sky" => bool_number(self.distant_land.water.reflect_sky),
            "distant_land.water.dynamic_ripples" => bool_number(self.distant_land.water.dynamic_ripples),
            "distant_land.water.blur_reflections" => bool_number(self.distant_land.water.blur_reflections),
            "distant_land.water.wave_height" => self.distant_land.water.wave_height.into(),
            "distant_land.water.caustics_intensity" => self.distant_land.water.caustics_intensity.into(),
            "distant_land.fog.exponential" => bool_number(self.distant_land.fog.exponential),
            "distant_land.fog.atmosphere_scattering" => bool_number(self.distant_land.fog.atmosphere_scattering),
            "distant_land.fog.above_water_start" => self.distant_land.fog.above_water_start.into(),
            "distant_land.fog.above_water_end" => self.distant_land.fog.above_water_end.into(),
            "distant_land.fog.below_water_start" => self.distant_land.fog.below_water_start.into(),
            "distant_land.fog.below_water_end" => self.distant_land.fog.below_water_end.into(),
            "distant_land.fog.interior_start" => self.distant_land.fog.interior_start.into(),
            "distant_land.fog.interior_end" => self.distant_land.fog.interior_end.into(),
            "distant_land.grass.interior_wind" => self.distant_land.grass.interior_wind.into(),
            "distant_land.shadows.enabled" => bool_number(self.distant_land.shadows.enabled),
            "distant_land.shadows.map_resolution" => self.distant_land.shadows.map_resolution.into(),
            "distant_land.per_pixel_lighting" => bool_number(self.distant_land.per_pixel_lighting),
            "distant_land.per_pixel_mode" => self.distant_land.per_pixel_mode.runtime_value().into(),
            "distant_land.horizon.culling" => bool_number(self.distant_land.horizon.culling),
            "distant_land.horizon.height_bias" => self.distant_land.horizon.height_bias.into(),
            "distant_land.horizon.object_bias" => self.distant_land.horizon.object_bias.into(),
            "distant_land.horizon.near_exclude" => self.distant_land.horizon.near_exclude.into(),
            "distant_land.horizon.ring_step" => self.distant_land.horizon.ring_step.into(),
            "distant_land.horizon.max_range" => self.distant_land.horizon.max_range.into(),
            "distant_land.horizon.azimuth_bins" => self.distant_land.horizon.azimuth_bins.into(),
            "distant_land.horizon.sample_spacing" => self.distant_land.horizon.sample_spacing.into(),
            "distant_land.horizon.adaptive_gate" => bool_number(self.distant_land.horizon.adaptive_gate),
            "distant_land.horizon.hierarchical_march" => bool_number(self.distant_land.horizon.hierarchical_march),
            _ => return self.weather_number(path),
        };
        Some(value)
    }

    pub fn get_number(&self, path: &str) -> Option<f64> {
        self.runtime_number(path).map(|number| number.value())
    }

    #[cfg(feature = "contract-test")]
    pub fn runtime_number_repr(&self, path: &str) -> Option<RuntimeNumberRepr> {
        self.runtime_number(path).map(|number| number.repr())
    }

    fn weather_number(&self, path: &str) -> Option<RuntimeNumber> {
        let (index, field) = parse_weather_path(path)?;
        let value = match field {
            WeatherField::Wind => self.distant_land.weather.as_array()[index].wind,
            WeatherField::FogRatio => self.distant_land.weather.as_array()[index].fog_ratio,
            WeatherField::FogOffset => self.distant_land.weather.as_array()[index].fog_offset,
            WeatherField::Sun => self.lighting.weather.as_array()[index].sun,
            WeatherField::Ambient => self.lighting.weather.as_array()[index].ambient,
        };
        Some(value.into())
    }

    pub fn set_number(&mut self, path: &str, value: f64) -> Result<(), String> {
        match path {
            "graphics.anti_aliasing" => {
                self.graphics.anti_aliasing = AntiAliasing::from_runtime(integer::<u8>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "graphics.z_buffer_format" => {
                self.graphics.z_buffer_format = ZBufferFormat::from_runtime(integer::<u8>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "graphics.vsync" => {
                self.graphics.vsync = VSync::from_runtime(integer::<u8>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "graphics.refresh_rate" => self.graphics.refresh_rate = integer::<u8>(value, path)?,
            "graphics.borderless" => self.graphics.borderless = number_bool(value, path)?,
            "graphics.anisotropy" => {
                self.graphics.anisotropy = Anisotropy::from_runtime(integer::<u8>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "graphics.transparency_antialiasing" => self.graphics.transparency_antialiasing = number_bool(value, path)?,
            "render.fov" => self.render.fov = finite_f32(value, path)?,
            "render.fog_mode" => {
                self.render.fog_mode = FogMode::from_runtime(integer::<u8>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "render.enable_shaders" => self.render.enable_shaders = number_bool(value, path)?,
            "render.indexed_skinning" => self.render.indexed_skinning = number_bool(value, path)?,
            "render.hdr_reaction_time" => self.render.hdr_reaction_time = finite_f32(value, path)?,
            "render.fps_counter" => self.render.fps_counter = number_bool(value, path)?,
            "render.messages" => self.render.messages = number_bool(value, path)?,
            "render.message_timeout_ms" => self.render.message_timeout_ms = integer::<i32>(value, path)?,
            "render.screenshot_format" => {
                self.render.screenshot_format = ScreenshotFormat::from_runtime(integer::<u8>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "render.screenshot_suffix" => {
                self.render.screenshot_suffix = ScreenshotSuffix::from_runtime(integer::<u8>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "render.ui_scale" => self.render.ui_scale = finite_f32(value, path)?,
            "render.window_align_x" => {
                self.render.window_align_x = Alignment::from_runtime(integer::<i32>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "render.window_align_y" => {
                self.render.window_align_y = Alignment::from_runtime(integer::<i32>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "runtime.disabled" => self.runtime.disabled = number_bool(value, path)?,
            "runtime.mwse_disabled" => self.runtime.mwse_disabled = number_bool(value, path)?,
            "runtime.proxy_only" => self.runtime.proxy_only = number_bool(value, path)?,
            "runtime.skip_intro" => self.runtime.skip_intro = number_bool(value, path)?,
            "runtime.menu_caching" => self.runtime.menu_caching = number_bool(value, path)?,
            "runtime.custom_camera" => self.runtime.custom_camera = number_bool(value, path)?,
            "runtime.camera_x" => self.runtime.camera_x = finite_f32(value, path)?,
            "runtime.camera_y" => self.runtime.camera_y = finite_f32(value, path)?,
            "runtime.camera_z" => self.runtime.camera_z = finite_f32(value, path)?,
            "runtime.crosshair_autohide" => self.runtime.crosshair_autohide = number_bool(value, path)?,
            "distant_land.enabled" => self.distant_land.enabled = number_bool(value, path)?,
            "distant_land.automatic_rebuild" => self.distant_land.automatic_rebuild = number_bool(value, path)?,
            "distant_land.native_depth_capture" => self.distant_land.native_depth_capture = number_bool(value, path)?,
            "distant_land.native_ppl_packets" => self.distant_land.native_ppl_packets = number_bool(value, path)?,
            "distant_land.expanded_light_limit" => self.distant_land.expanded_light_limit = number_bool(value, path)?,
            "distant_land.statics" => self.distant_land.statics = number_bool(value, path)?,
            "distant_land.water_without_land" => self.distant_land.water_without_land = number_bool(value, path)?,
            "distant_land.render_grass" => self.distant_land.render_grass = number_bool(value, path)?,
            "distant_land.draw_distance" => self.distant_land.draw_distance = finite_f32(value, path)?,
            "distant_land.near_static_end" => self.distant_land.near_static_end = finite_f32(value, path)?,
            "distant_land.far_static_end" => self.distant_land.far_static_end = finite_f32(value, path)?,
            "distant_land.very_far_static_end" => self.distant_land.very_far_static_end = finite_f32(value, path)?,
            "distant_land.far_static_min_size" => self.distant_land.far_static_min_size = finite_f32(value, path)?,
            "distant_land.very_far_static_min_size" => self.distant_land.very_far_static_min_size = finite_f32(value, path)?,
            "distant_land.water.reflect_land" => self.distant_land.water.reflect_land = number_bool(value, path)?,
            "distant_land.water.reflect_near_statics" => {
                self.distant_land.water.reflect_near_statics = number_bool(value, path)?
            }
            "distant_land.water.reflect_interiors" => self.distant_land.water.reflect_interiors = number_bool(value, path)?,
            "distant_land.water.reflect_sky" => self.distant_land.water.reflect_sky = number_bool(value, path)?,
            "distant_land.water.dynamic_ripples" => self.distant_land.water.dynamic_ripples = number_bool(value, path)?,
            "distant_land.water.blur_reflections" => self.distant_land.water.blur_reflections = number_bool(value, path)?,
            "distant_land.water.wave_height" => self.distant_land.water.wave_height = integer::<u8>(value, path)?,
            "distant_land.water.caustics_intensity" => {
                self.distant_land.water.caustics_intensity = integer::<u8>(value, path)?
            }
            "distant_land.fog.exponential" => self.distant_land.fog.exponential = number_bool(value, path)?,
            "distant_land.fog.atmosphere_scattering" => {
                self.distant_land.fog.atmosphere_scattering = number_bool(value, path)?
            }
            "distant_land.fog.above_water_start" => self.distant_land.fog.above_water_start = finite_f32(value, path)?,
            "distant_land.fog.above_water_end" => self.distant_land.fog.above_water_end = finite_f32(value, path)?,
            "distant_land.fog.below_water_start" => self.distant_land.fog.below_water_start = finite_f32(value, path)?,
            "distant_land.fog.below_water_end" => self.distant_land.fog.below_water_end = finite_f32(value, path)?,
            "distant_land.fog.interior_start" => self.distant_land.fog.interior_start = finite_f32(value, path)?,
            "distant_land.fog.interior_end" => self.distant_land.fog.interior_end = finite_f32(value, path)?,
            "distant_land.grass.interior_wind" => self.distant_land.grass.interior_wind = finite_f32(value, path)?,
            "distant_land.shadows.enabled" => self.distant_land.shadows.enabled = number_bool(value, path)?,
            "distant_land.shadows.map_resolution" => self.distant_land.shadows.map_resolution = integer::<u32>(value, path)?,
            "distant_land.per_pixel_lighting" => self.distant_land.per_pixel_lighting = number_bool(value, path)?,
            "distant_land.per_pixel_mode" => {
                self.distant_land.per_pixel_mode = PerPixelMode::from_runtime(integer::<u32>(value, path)?)
                    .ok_or_else(|| format!("{path} has unknown runtime enum value {value}"))?
            }
            "distant_land.horizon.culling" => self.distant_land.horizon.culling = number_bool(value, path)?,
            "distant_land.horizon.height_bias" => self.distant_land.horizon.height_bias = finite_f32(value, path)?,
            "distant_land.horizon.object_bias" => self.distant_land.horizon.object_bias = finite_f32(value, path)?,
            "distant_land.horizon.near_exclude" => self.distant_land.horizon.near_exclude = finite_f32(value, path)?,
            "distant_land.horizon.ring_step" => self.distant_land.horizon.ring_step = finite_f32(value, path)?,
            "distant_land.horizon.max_range" => self.distant_land.horizon.max_range = finite_f32(value, path)?,
            "distant_land.horizon.azimuth_bins" => self.distant_land.horizon.azimuth_bins = integer::<u32>(value, path)?,
            "distant_land.horizon.sample_spacing" => self.distant_land.horizon.sample_spacing = finite_f32(value, path)?,
            "distant_land.horizon.adaptive_gate" => self.distant_land.horizon.adaptive_gate = number_bool(value, path)?,
            "distant_land.horizon.hierarchical_march" => {
                self.distant_land.horizon.hierarchical_march = number_bool(value, path)?
            }
            _ => return self.set_weather_number(path, value),
        }
        Ok(())
    }

    fn set_weather_number(&mut self, path: &str, value: f64) -> Result<(), String> {
        let Some((index, field)) = parse_weather_path(path) else {
            return Err(format!("unknown numeric configuration path {path}"));
        };
        let value = finite_f32(value, path)?;
        match field {
            WeatherField::Wind => self.distant_land.weather.as_mut_array()[index].wind = value,
            WeatherField::FogRatio => self.distant_land.weather.as_mut_array()[index].fog_ratio = value,
            WeatherField::FogOffset => self.distant_land.weather.as_mut_array()[index].fog_offset = value,
            WeatherField::Sun => self.lighting.weather.as_mut_array()[index].sun = value,
            WeatherField::Ambient => self.lighting.weather.as_mut_array()[index].ambient = value,
        }
        Ok(())
    }

    pub fn get_string(&self, path: &str) -> Option<&str> {
        match path {
            "render.screenshot_directory" => Some(&self.render.screenshot_directory),
            "render.screenshot_name" => Some(&self.render.screenshot_name),
            _ => None,
        }
    }

    pub fn set_string(&mut self, path: &str, value: &str) -> Result<(), String> {
        match path {
            "render.screenshot_directory" => self.render.screenshot_directory = value.into(),
            "render.screenshot_name" => self.render.screenshot_name = value.into(),
            _ => return Err(format!("unknown string configuration path {path}")),
        }
        Ok(())
    }
}
