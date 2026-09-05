#pragma once
#ifdef _CONF
#ifndef _INIDATA_H_
#define _INIDATA_H_

#include "configinternal.h"

#define NUM(variable, type, path, flags) {&(variable), type, 1, path, flags}
#define BIT(variable, bit, path, flags) {&(variable), t_bit, bit, path, flags}
#define BUFFER(variable, type, path) {&(variable), type, sizeof(variable), path, DONT_SAVE}

#define WEATHER_BIND(index, name) \
    NUM(Configuration.DL.Wind[index], t_float, "distant_land.weather." name ".wind", 0), \
    NUM(Configuration.DL.FogD[index], t_float, "distant_land.weather." name ".fog_ratio", 0), \
    NUM(Configuration.DL.FgOD[index], t_float, "distant_land.weather." name ".fog_offset", 0), \
    NUM(Configuration.Lighting.SunMult[index], t_float, "lighting.weather." name ".sun", 0), \
    NUM(Configuration.Lighting.AmbMult[index], t_float, "lighting.weather." name ".ambient", 0)

const iniSetting iniSettings[] = {
    BIT(Configuration.MGEFlags, MGE_DISABLED_BIT, "runtime.disabled", DONT_SAVE),
    BIT(Configuration.MGEFlags, MWSE_DISABLED_BIT, "runtime.mwse_disabled", DONT_SAVE),
    BIT(Configuration.MGEFlags, SKIP_INTRO_BIT, "runtime.skip_intro", 0),
    BIT(Configuration.MGEFlags, FPS_COUNTER_BIT, "render.fps_counter", 0),
    BIT(Configuration.MGEFlags, DISPLAY_MESSAGES_BIT, "render.messages", 0),
    BIT(Configuration.MGEFlags, USE_MENU_CACHING_BIT, "runtime.menu_caching", 0),
    NUM(Configuration.OnlyProxyD3D8To9, t_bool, "runtime.proxy_only", DONT_SAVE),

    BIT(Configuration.MGEFlags, USE_DISTANT_LAND_BIT, "distant_land.enabled", 0),
    NUM(Configuration.AutomaticDistantLandRebuild, t_bool, "distant_land.automatic_rebuild", 0),
    NUM(Configuration.EnableNativeDepthCapture, t_bool, "distant_land.native_depth_capture", 0),
    NUM(Configuration.EnableNativePplPackets, t_bool, "distant_land.native_ppl_packets", 0),
    NUM(Configuration.ExpandedLightLimit, t_bool, "distant_land.expanded_light_limit", 0),
    BIT(Configuration.MGEFlags, USE_DISTANT_STATICS_BIT, "distant_land.statics", 0),
    BIT(Configuration.MGEFlags, USE_DISTANT_WATER_BIT, "distant_land.water_without_land", DONT_SAVE),
    BIT(Configuration.MGEFlags, REFLECTIVE_WATER_BIT, "distant_land.water.reflect_land", 0),
    BIT(Configuration.MGEFlags, REFLECT_NEAR_BIT, "distant_land.water.reflect_near_statics", 0),
    BIT(Configuration.MGEFlags, REFLECT_INTERIOR_BIT, "distant_land.water.reflect_interiors", 0),
    BIT(Configuration.MGEFlags, REFLECT_SKY_BIT, "distant_land.water.reflect_sky", 0),
    BIT(Configuration.MGEFlags, DYNAMIC_RIPPLES_BIT, "distant_land.water.dynamic_ripples", 0),
    BIT(Configuration.MGEFlags, BLUR_REFLECTIONS_BIT, "distant_land.water.blur_reflections", 0),
    BIT(Configuration.MGEFlags, EXP_FOG_BIT, "distant_land.fog.exponential", 0),
    BIT(Configuration.MGEFlags, USE_ATM_SCATTER_BIT, "distant_land.fog.atmosphere_scattering", 0),
    BIT(Configuration.MGEFlags, USE_GRASS_BIT, "distant_land.render_grass", 0),
    BIT(Configuration.MGEFlags, USE_SHADOWS_BIT, "distant_land.shadows.enabled", 0),
    BIT(Configuration.MGEFlags, USE_FFESHADER_BIT, "distant_land.per_pixel_lighting", 0),

    NUM(Configuration.AALevel, t_uint8, "graphics.anti_aliasing", DONT_SAVE),
    NUM(Configuration.ZBufFormat, t_uint8, "graphics.z_buffer_format", DONT_SAVE),
    NUM(Configuration.VWait, t_uint8, "graphics.vsync", DONT_SAVE),
    NUM(Configuration.RefreshRate, t_uint8, "graphics.refresh_rate", DONT_SAVE),
    NUM(Configuration.Borderless, t_bool, "graphics.borderless", DONT_SAVE),
    NUM(Configuration.AnisoLevel, t_uint8, "graphics.anisotropy", DONT_SAVE),
    NUM(Configuration.ScreenFOV, t_float, "render.fov", 0),
    NUM(Configuration.FogMode, t_uint8, "render.fog_mode", 0),
    BIT(
        Configuration.MGEFlags,
        TRANSPARENCY_AA_BIT,
        "graphics.transparency_antialiasing",
        0),
    BIT(Configuration.MGEFlags, USE_HW_SHADER_BIT, "render.enable_shaders", 0),
    NUM(Configuration.EnableIndexedSkinning, t_bool, "render.indexed_skinning", 0),
    NUM(Configuration.HDRReactionSpeed, t_float, "render.hdr_reaction_time", 0),
    NUM(Configuration.PerPixelLightFlags, t_uint32, "distant_land.per_pixel_mode", 0),

    NUM(Configuration.SSFormat, t_uint8, "render.screenshot_format", DONT_SAVE),
    BUFFER(Configuration.SSDir, t_string, "render.screenshot_directory"),
    BUFFER(Configuration.SSName, t_string, "render.screenshot_name"),
    NUM(Configuration.SSSuffix, t_uint8, "render.screenshot_suffix", DONT_SAVE),
    NUM(Configuration.StatusTimeout, t_int32, "render.message_timeout_ms", 0),
    NUM(Configuration.Force3rdPerson, t_bool, "runtime.custom_camera", 0),
    NUM(Configuration.Offset3rdPerson.x, t_float, "runtime.camera_x", 0),
    NUM(Configuration.Offset3rdPerson.y, t_float, "runtime.camera_y", 0),
    NUM(Configuration.Offset3rdPerson.z, t_float, "runtime.camera_z", 0),
    BIT(Configuration.MGEFlags, CROSSHAIR_AUTOHIDE_BIT, "runtime.crosshair_autohide", 0),
    NUM(Configuration.UIScale, t_float, "render.ui_scale", 0),
    NUM(Configuration.WindowAlignX, t_int32, "render.window_align_x", 0),
    NUM(Configuration.WindowAlignY, t_int32, "render.window_align_y", 0),

    BUFFER(Configuration.ShaderChain, t_set, "shaders.chain"),

    NUM(Configuration.DL.DrawDist, t_float, "distant_land.draw_distance", 0),
    NUM(Configuration.DL.NearStaticEnd, t_float, "distant_land.near_static_end", 0),
    NUM(Configuration.DL.FarStaticEnd, t_float, "distant_land.far_static_end", 0),
    NUM(Configuration.DL.VeryFarStaticEnd, t_float, "distant_land.very_far_static_end", 0),
    NUM(Configuration.DL.FarStaticMinSize, t_float, "distant_land.far_static_min_size", 0),
    NUM(
        Configuration.DL.VeryFarStaticMinSize,
        t_float,
        "distant_land.very_far_static_min_size",
        0),
    NUM(Configuration.Horizon.Culling, t_bool, "distant_land.horizon.culling", 0),
    NUM(Configuration.Horizon.BiasZ, t_float, "distant_land.horizon.height_bias", 0),
    NUM(Configuration.Horizon.ObjectBiasZ, t_float, "distant_land.horizon.object_bias", 0),
    NUM(Configuration.Horizon.NearUnits, t_float, "distant_land.horizon.near_exclude", 0),
    NUM(Configuration.Horizon.RingStep, t_float, "distant_land.horizon.ring_step", 0),
    NUM(Configuration.Horizon.MaxRange, t_float, "distant_land.horizon.max_range", 0),
    NUM(Configuration.Horizon.Bins, t_uint32, "distant_land.horizon.azimuth_bins", 0),
    NUM(
        Configuration.Horizon.SampleSpacing,
        t_float,
        "distant_land.horizon.sample_spacing",
        0),
    NUM(Configuration.Horizon.AdaptiveGate, t_bool, "distant_land.horizon.adaptive_gate", 0),
    NUM(
        Configuration.Horizon.HierarchicalMarch,
        t_bool,
        "distant_land.horizon.hierarchical_march",
        0),
    NUM(
        Configuration.DL.AboveWaterFogStart,
        t_float,
        "distant_land.fog.above_water_start",
        0),
    NUM(
        Configuration.DL.AboveWaterFogEnd,
        t_float,
        "distant_land.fog.above_water_end",
        0),
    NUM(
        Configuration.DL.BelowWaterFogStart,
        t_float,
        "distant_land.fog.below_water_start",
        0),
    NUM(
        Configuration.DL.BelowWaterFogEnd,
        t_float,
        "distant_land.fog.below_water_end",
        0),
    NUM(Configuration.DL.InteriorFogStart, t_float, "distant_land.fog.interior_start", 0),
    NUM(Configuration.DL.InteriorFogEnd, t_float, "distant_land.fog.interior_end", 0),
    NUM(Configuration.Grass.InteriorWind, t_float, "distant_land.grass.interior_wind", 0),
    NUM(Configuration.DL.WaterWaveHeight, t_uint8, "distant_land.water.wave_height", 0),
    NUM(
        Configuration.DL.WaterCaustics,
        t_uint8,
        "distant_land.water.caustics_intensity",
        0),
    NUM(
        Configuration.DL.ShadowResolution,
        t_uint32,
        "distant_land.shadows.map_resolution",
        0),

    WEATHER_BIND(0, "clear"),
    WEATHER_BIND(1, "cloudy"),
    WEATHER_BIND(2, "foggy"),
    WEATHER_BIND(3, "overcast"),
    WEATHER_BIND(4, "rain"),
    WEATHER_BIND(5, "thunderstorm"),
    WEATHER_BIND(6, "ashstorm"),
    WEATHER_BIND(7, "blight"),
    WEATHER_BIND(8, "snow"),
    WEATHER_BIND(9, "blizzard"),

    BUFFER(Configuration.Input.Macros, t_set, "input.macros"),
    BUFFER(Configuration.Input.Triggers, t_set, "input.triggers"),
    BUFFER(Configuration.Input.Remap, t_set, "input.remap"),
};

#undef WEATHER_BIND
#undef BUFFER
#undef BIT
#undef NUM

#endif /* _INIDATA_H_ */
#endif /* _CONF */
