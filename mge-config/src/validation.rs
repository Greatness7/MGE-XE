use std::collections::BTreeSet;
use std::fmt;

use crate::schema::{
    DRAW_DISTANCE_RANGE, DistantLandSettings, FAR_STATIC_END_RANGE, FOG_ABOVE_END_RANGE, FOG_ABOVE_START_RANGE,
    FOG_BELOW_END_RANGE, FOG_BELOW_START_RANGE, FOG_INTERIOR_END_RANGE, FOG_INTERIOR_START_RANGE, FOV_RANGE,
    GRAPHICS_FUNCTION_COUNT, GRASS_INTERIOR_WIND_RANGE, HORIZON_BIAS_Z_RANGE, HORIZON_BINS_RANGE, HORIZON_MAX_RANGE_RANGE,
    HORIZON_NEAR_UNITS_RANGE, HORIZON_OBJECT_BIAS_Z_RANGE, HORIZON_REBUILD_EYE_THRESHOLD_RANGE, HORIZON_RING_STEP_RANGE,
    HORIZON_SAMPLE_SPACING_RANGE, INPUT_COUNT, MacroKind, NEAR_STATIC_END_RANGE, SCHEMA_VERSION, STATIC_MIN_SIZE_RANGE,
    Settings, TRIGGER_COUNT, VERY_FAR_STATIC_END_RANGE, WEATHER_NAMES,
};

/// Clamp warning labels for every per-weather scalar, spelled out at compile time so that
/// `validate_bounds` does not `format!` 50 paths on every scalar setter call.
///
/// The outer index is the weather's position in [`WEATHER_NAMES`]; `weather_labels_match_names`
/// pins that correspondence.
macro_rules! weather_labels {
    ($($name:literal),+ $(,)?) => {
        [$([
            concat!("distant_land.weather.", $name, ".wind"),
            concat!("distant_land.weather.", $name, ".fog_ratio"),
            concat!("distant_land.weather.", $name, ".fog_offset"),
            concat!("lighting.weather.", $name, ".sun"),
            concat!("lighting.weather.", $name, ".ambient"),
        ]),+]
    };
}

pub(crate) const WEATHER_LABELS: [[&str; 5]; WEATHER_NAMES.len()] = weather_labels![
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Warning {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidationError {
    pub path: String,
    pub message: String,
}

impl ValidationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.path, self.message)
    }
}

fn clamp_f32(value: &mut f32, min: f32, max: f32, path: &str, warnings: &mut Vec<Warning>) -> Result<(), ValidationError> {
    if !value.is_finite() {
        return Err(ValidationError::new(path, "must be finite"));
    }
    let original = *value;
    *value = (*value).clamp(min, max);
    if *value != original {
        warnings.push(Warning {
            path: path.into(),
            message: format!("{original} was clamped to {}", *value),
        });
    }
    Ok(())
}

fn clamp_u32(value: &mut u32, min: u32, max: u32, path: &str, warnings: &mut Vec<Warning>) {
    let original = *value;
    *value = (*value).clamp(min, max);
    if *value != original {
        warnings.push(Warning {
            path: path.into(),
            message: format!("{original} was clamped to {}", *value),
        });
    }
}

fn clamp_u8(value: &mut u8, min: u8, max: u8, path: &str, warnings: &mut Vec<Warning>) {
    let original = *value;
    *value = (*value).clamp(min, max);
    if *value != original {
        warnings.push(Warning {
            path: path.into(),
            message: format!("{original} was clamped to {}", *value),
        });
    }
}

fn clamp_i32(value: &mut i32, min: i32, max: i32, path: &str, warnings: &mut Vec<Warning>) {
    let original = *value;
    *value = (*value).clamp(min, max);
    if *value != original {
        warnings.push(Warning {
            path: path.into(),
            message: format!("{original} was clamped to {}", *value),
        });
    }
}

fn raise_to(value: &mut f32, floor: f32, path: &str, warnings: &mut Vec<Warning>) {
    if *value < floor {
        let original = *value;
        *value = floor;
        warnings.push(Warning {
            path: path.into(),
            message: format!("{original} was clamped to {floor}"),
        });
    }
}

fn lower_to(value: &mut f32, ceiling: f32, path: &str, warnings: &mut Vec<Warning>) {
    if *value > ceiling {
        let original = *value;
        *value = ceiling;
        warnings.push(Warning {
            path: path.into(),
            message: format!("{original} was clamped to {ceiling}"),
        });
    }
}

/// Push an inverted range's start back below its end, no lower than the start
/// field's own bound. Only an inverted range is touched, so a start that merely
/// sits close to the end keeps the value the user chose.
fn precede(start: &mut f32, end: f32, floor: f32, path: &str, warnings: &mut Vec<Warning>) {
    if *start >= end {
        lower_to(start, (end - 0.1).max(floor), path, warnings);
    }
}

/// Distant-land relationships that no single field's bounds can express, and the sole
/// authority for them: `validate` runs it over a parsed or replaced document, and
/// `MGEXEgui` calls it per interaction so a spinner shows the corrected value
/// immediately rather than at save time.
///
/// Per-field bounds belong to `validate_bounds`, not here. The GUI additionally floors
/// both minimum sizes at the size the distant-land data was generated with; that floor
/// is not part of the settings schema.
pub fn normalize_distant_land(distant: &mut DistantLandSettings, warnings: &mut Vec<Warning>) {
    raise_to(
        &mut distant.far_static_end,
        distant.near_static_end,
        "distant_land.far_static_end",
        warnings,
    );
    raise_to(
        &mut distant.very_far_static_end,
        distant.far_static_end,
        "distant_land.very_far_static_end",
        warnings,
    );
    raise_to(
        &mut distant.very_far_static_min_size,
        distant.far_static_min_size,
        "distant_land.very_far_static_min_size",
        warnings,
    );

    // Fog ends are capped first so the start corrections see the final end.
    lower_to(
        &mut distant.fog.above_water_end,
        distant.draw_distance,
        "distant_land.fog.above_water_end",
        warnings,
    );
    lower_to(
        &mut distant.fog.below_water_end,
        distant.draw_distance,
        "distant_land.fog.below_water_end",
        warnings,
    );
    precede(
        &mut distant.fog.above_water_start,
        distant.fog.above_water_end,
        FOG_ABOVE_START_RANGE.0,
        "distant_land.fog.above_water_start",
        warnings,
    );
    precede(
        &mut distant.fog.below_water_start,
        distant.fog.below_water_end,
        FOG_BELOW_START_RANGE.0,
        "distant_land.fog.below_water_start",
        warnings,
    );
    // Interior fog is independent of draw distance.
    precede(
        &mut distant.fog.interior_start,
        distant.fog.interior_end,
        FOG_INTERIOR_START_RANGE.0,
        "distant_land.fog.interior_start",
        warnings,
    );
}

pub fn validate(settings: &mut Settings) -> Result<Vec<Warning>, ValidationError> {
    let mut warnings = validate_bounds(settings)?;
    normalize_distant_land(&mut settings.distant_land, &mut warnings);
    Ok(warnings)
}

/// Per-field bounds only, for incremental single-field edits.
///
/// `set_number` and `set_string` cannot see the rest of the values their caller
/// is about to write, so running the cross-field pass here would let a stale
/// companion field overwrite the value being set. The document is normalized in
/// full whenever it is parsed or replaced wholesale.
pub(crate) fn validate_bounds(settings: &mut Settings) -> Result<Vec<Warning>, ValidationError> {
    if settings.schema_version != SCHEMA_VERSION {
        return Err(ValidationError::new(
            "schema_version",
            format!(
                "{} is not supported; this build supports {}",
                settings.schema_version, SCHEMA_VERSION
            ),
        ));
    }

    let mut warnings = Vec::new();
    clamp_u8(
        &mut settings.graphics.refresh_rate,
        0,
        240,
        "graphics.refresh_rate",
        &mut warnings,
    );
    clamp_f32(
        &mut settings.render.fov,
        FOV_RANGE.0,
        FOV_RANGE.1,
        "render.fov",
        &mut warnings,
    )?;
    clamp_f32(
        &mut settings.render.hdr_reaction_time,
        0.01,
        30.0,
        "render.hdr_reaction_time",
        &mut warnings,
    )?;
    clamp_i32(
        &mut settings.render.message_timeout_ms,
        1_000,
        10_000,
        "render.message_timeout_ms",
        &mut warnings,
    );
    clamp_f32(&mut settings.render.ui_scale, 0.5, 5.0, "render.ui_scale", &mut warnings)?;
    if settings.render.screenshot_directory.len() >= 208 {
        return Err(ValidationError::new(
            "render.screenshot_directory",
            "exceeds the 207-byte runtime capacity",
        ));
    }
    if settings.render.screenshot_name.len() >= 32 {
        return Err(ValidationError::new(
            "render.screenshot_name",
            "exceeds the 31-byte runtime capacity",
        ));
    }

    clamp_f32(
        &mut settings.runtime.camera_x,
        -250.0,
        250.0,
        "runtime.camera_x",
        &mut warnings,
    )?;
    clamp_f32(
        &mut settings.runtime.camera_y,
        -2500.0,
        2500.0,
        "runtime.camera_y",
        &mut warnings,
    )?;
    clamp_f32(
        &mut settings.runtime.camera_z,
        -250.0,
        250.0,
        "runtime.camera_z",
        &mut warnings,
    )?;

    let distant = &mut settings.distant_land;
    clamp_f32(
        &mut distant.draw_distance,
        DRAW_DISTANCE_RANGE.0,
        DRAW_DISTANCE_RANGE.1,
        "distant_land.draw_distance",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.near_static_end,
        NEAR_STATIC_END_RANGE.0,
        NEAR_STATIC_END_RANGE.1,
        "distant_land.near_static_end",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.far_static_end,
        FAR_STATIC_END_RANGE.0,
        FAR_STATIC_END_RANGE.1,
        "distant_land.far_static_end",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.very_far_static_end,
        VERY_FAR_STATIC_END_RANGE.0,
        VERY_FAR_STATIC_END_RANGE.1,
        "distant_land.very_far_static_end",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.far_static_min_size,
        STATIC_MIN_SIZE_RANGE.0,
        STATIC_MIN_SIZE_RANGE.1,
        "distant_land.far_static_min_size",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.very_far_static_min_size,
        STATIC_MIN_SIZE_RANGE.0,
        STATIC_MIN_SIZE_RANGE.1,
        "distant_land.very_far_static_min_size",
        &mut warnings,
    )?;
    clamp_u8(
        &mut distant.water.wave_height,
        0,
        250,
        "distant_land.water.wave_height",
        &mut warnings,
    );
    clamp_u8(
        &mut distant.water.caustics_intensity,
        0,
        100,
        "distant_land.water.caustics_intensity",
        &mut warnings,
    );
    clamp_f32(
        &mut distant.fog.above_water_start,
        FOG_ABOVE_START_RANGE.0,
        FOG_ABOVE_START_RANGE.1,
        "distant_land.fog.above_water_start",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.fog.above_water_end,
        FOG_ABOVE_END_RANGE.0,
        FOG_ABOVE_END_RANGE.1,
        "distant_land.fog.above_water_end",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.fog.below_water_start,
        FOG_BELOW_START_RANGE.0,
        FOG_BELOW_START_RANGE.1,
        "distant_land.fog.below_water_start",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.fog.below_water_end,
        FOG_BELOW_END_RANGE.0,
        FOG_BELOW_END_RANGE.1,
        "distant_land.fog.below_water_end",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.fog.interior_start,
        FOG_INTERIOR_START_RANGE.0,
        FOG_INTERIOR_START_RANGE.1,
        "distant_land.fog.interior_start",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.fog.interior_end,
        FOG_INTERIOR_END_RANGE.0,
        FOG_INTERIOR_END_RANGE.1,
        "distant_land.fog.interior_end",
        &mut warnings,
    )?;
    clamp_f32(
        &mut distant.grass.interior_wind,
        GRASS_INTERIOR_WIND_RANGE.0,
        GRASS_INTERIOR_WIND_RANGE.1,
        "distant_land.grass.interior_wind",
        &mut warnings,
    )?;
    clamp_u32(
        &mut distant.shadows.map_resolution,
        1024,
        2048,
        "distant_land.shadows.map_resolution",
        &mut warnings,
    );

    let horizon = &mut distant.horizon;
    clamp_f32(
        &mut horizon.height_bias,
        HORIZON_BIAS_Z_RANGE.0,
        HORIZON_BIAS_Z_RANGE.1,
        "distant_land.horizon.height_bias",
        &mut warnings,
    )?;
    clamp_f32(
        &mut horizon.object_bias,
        HORIZON_OBJECT_BIAS_Z_RANGE.0,
        HORIZON_OBJECT_BIAS_Z_RANGE.1,
        "distant_land.horizon.object_bias",
        &mut warnings,
    )?;
    clamp_f32(
        &mut horizon.near_exclude,
        HORIZON_NEAR_UNITS_RANGE.0,
        HORIZON_NEAR_UNITS_RANGE.1,
        "distant_land.horizon.near_exclude",
        &mut warnings,
    )?;
    clamp_f32(
        &mut horizon.ring_step,
        HORIZON_RING_STEP_RANGE.0,
        HORIZON_RING_STEP_RANGE.1,
        "distant_land.horizon.ring_step",
        &mut warnings,
    )?;
    clamp_f32(
        &mut horizon.max_range,
        HORIZON_MAX_RANGE_RANGE.0,
        HORIZON_MAX_RANGE_RANGE.1,
        "distant_land.horizon.max_range",
        &mut warnings,
    )?;
    clamp_u32(
        &mut horizon.azimuth_bins,
        HORIZON_BINS_RANGE.0,
        HORIZON_BINS_RANGE.1,
        "distant_land.horizon.azimuth_bins",
        &mut warnings,
    );
    clamp_f32(
        &mut horizon.sample_spacing,
        HORIZON_SAMPLE_SPACING_RANGE.0,
        HORIZON_SAMPLE_SPACING_RANGE.1,
        "distant_land.horizon.sample_spacing",
        &mut warnings,
    )?;
    clamp_f32(
        &mut horizon.rebuild_eye_threshold,
        HORIZON_REBUILD_EYE_THRESHOLD_RANGE.0,
        HORIZON_REBUILD_EYE_THRESHOLD_RANGE.1,
        "distant_land.horizon.rebuild_eye_threshold",
        &mut warnings,
    )?;

    for (index, weather) in distant.weather.as_mut_array().into_iter().enumerate() {
        let labels = WEATHER_LABELS[index];
        clamp_f32(&mut weather.wind, 0.0, 1.0, labels[0], &mut warnings)?;
        clamp_f32(&mut weather.fog_ratio, 0.001, 2.0, labels[1], &mut warnings)?;
        clamp_f32(&mut weather.fog_offset, 0.0, 200.0, labels[2], &mut warnings)?;
    }
    for (index, lighting) in settings.lighting.weather.as_mut_array().into_iter().enumerate() {
        let labels = WEATHER_LABELS[index];
        clamp_f32(&mut lighting.sun, 0.0, 10.0, labels[3], &mut warnings)?;
        clamp_f32(&mut lighting.ambient, 0.0, 10.0, labels[4], &mut warnings)?;
    }

    clamp_u8(
        &mut settings.gui.auto_distance_mode,
        0,
        2,
        "gui.auto_distance_mode",
        &mut warnings,
    );
    clamp_f32(
        &mut settings.gui.exponential_distance_multiplier,
        2.5,
        5.0,
        "gui.exponential_distance_multiplier",
        &mut warnings,
    )?;

    let mut macro_indices = BTreeSet::new();
    for (position, item) in settings.input.macros.iter().enumerate() {
        if item.index as usize >= INPUT_COUNT {
            return Err(ValidationError::new(
                format!("input.macros[{position}]"),
                format!("index {} is out of range", item.index),
            ));
        }
        if !macro_indices.insert(item.index) {
            return Err(ValidationError::new(
                format!("input.macros[{position}]"),
                format!("duplicates index {}", item.index),
            ));
        }
        if item.key_events.iter().any(|event| event.code as usize >= INPUT_COUNT)
            || item.keys.iter().any(|key| *key as usize >= INPUT_COUNT)
        {
            return Err(ValidationError::new(
                format!("input.macros[{position}]"),
                "contains an out-of-range key",
            ));
        }
        if item.kind.is_console()
            && (item.key_events.len() > u8::MAX as usize || item.key_events.iter().any(|event| event.code > u8::MAX as u16))
        {
            return Err(ValidationError::new(
                format!("input.macros[{position}]"),
                "console key sequence exceeds the runtime byte capacity",
            ));
        }
        if item.kind.is_timer() && item.timer_id as usize >= TRIGGER_COUNT {
            return Err(ValidationError::new(
                format!("input.macros[{position}]"),
                format!("timer_id {} is out of range", item.timer_id),
            ));
        }
        if item.kind == MacroKind::Graphics && item.function >= GRAPHICS_FUNCTION_COUNT {
            return Err(ValidationError::new(
                format!("input.macros[{position}]"),
                format!("function {} is out of range", item.function),
            ));
        }
    }
    let mut trigger_indices = BTreeSet::new();
    for (position, item) in settings.input.triggers.iter_mut().enumerate() {
        if item.index as usize >= TRIGGER_COUNT {
            return Err(ValidationError::new(
                format!("input.triggers[{position}]"),
                format!("index {} is out of range", item.index),
            ));
        }
        if !trigger_indices.insert(item.index) {
            return Err(ValidationError::new(
                format!("input.triggers[{position}]"),
                format!("duplicates index {}", item.index),
            ));
        }
        if item.keys.iter().any(|key| *key as usize >= INPUT_COUNT) {
            return Err(ValidationError::new(
                format!("input.triggers[{position}]"),
                "contains an out-of-range key",
            ));
        }
        clamp_u32(
            &mut item.interval_ms,
            1,
            60_000,
            &format!("input.triggers[{position}].interval_ms"),
            &mut warnings,
        );
    }
    if let Some(source) = settings.input.remap.keys().find(|source| **source > 255) {
        return Err(ValidationError::new(
            format!("input.remap.{source}"),
            "source key is greater than 255",
        ));
    }

    check_multisz("shaders.chain", settings.shaders.chain.iter().map(String::len), 512)?;
    check_multisz("input.macros", settings.input.rendered_macro_line_lengths(), 4096)?;
    check_multisz("input.triggers", settings.input.rendered_trigger_line_lengths(), 4096)?;
    check_multisz("input.remap", settings.input.rendered_remap_line_lengths(), 4096)?;

    Ok(warnings)
}

fn check_multisz(path: &str, line_lengths: impl Iterator<Item = usize>, capacity: usize) -> Result<(), ValidationError> {
    let content_bytes = line_lengths.map(|len| len + 1).sum::<usize>();
    let bytes = content_bytes + if content_bytes == 0 { 2 } else { 1 };
    if bytes > capacity {
        Err(ValidationError::new(
            path,
            format!("needs {bytes} bytes but the runtime capacity is {capacity}"),
        ))
    } else {
        Ok(())
    }
}
