# MGE XE TOML configuration reference

MGE XE stores its settings in `mgeXE.toml` at the Morrowind root. The active
schema is `schema_version = 3`. The executable components never read, migrate,
overwrite, or delete the obsolete `MGE3\MGE.ini`.

The machine-readable authority is `mge-config`: typed structures live in
`src/schema.rs`, validation in `src/validation.rs`, and the commented
defaults in `src/default.toml`. Targeted saves preserve comments and valid
values; mge-config retains unknown root tables for other components.

## Load and write rules

- Missing known keys resolve to their schema defaults without a warning.
- Unknown keys, wrong types, unknown enum spellings, out-of-range values, and
  schema-version mismatches produce warnings. Valid sibling values still load;
  invalid values use defaults, and the loader discards malformed collection
  elements individually where it can.
- Saving a parseable document writes current version numbers, removes unknown
  keys inside owned tables, and removes or defaults values reported during load.
  Saves preserve completely unknown root tables.
- Malformed UTF-8 or TOML syntax uses embedded defaults and disables TOML writes
  until a successful explicit reload. Independent `Morrowind.ini` and registry
  writes remain available.
- Saves compare an exact-byte revision and use a unique adjacent temporary file
  plus an atomic Windows move.
- The GUI refuses revision conflicts. The runtime reloads the latest valid
  document and replays only its owned non-`DONT_SAVE` scalar values.
- First-run creation never replaces a file that appeared after the process
  loaded its defaults.

## Component ownership

`mge-config` owns the persistent schema, defaults, validation,
diagnostics, parsing, and document mutation. Each executable consumes it
directly:

- `MGEXEgui.exe` uses the typed Rust API and targeted document saves.
- `mgeHost64.exe` projects only the settings required by startup generation and
  host-side culling.
- `d3d8.dll` calls a narrow Rust C ABI, then applies scalar values to
  the live C++ `Configuration` object through the binding table in
  `d3d8/cpp/mge/inidata.h`.

The C++ object remains the runtime authority because render code and the MWSE
API read or mutate it directly. `inidata.h` maps TOML paths to C++
storage; it is not a second schema or default table. Structured macros,
triggers, remaps, and the shader chain live in their Rust models, which render
them into the runtime's legacy buffers only at the boundary.

The DLL performs its read-only load during process initialization. It never
creates or saves a file from `DllMain`; first-run creation and later writes use
explicit save paths. The host is not a configuration service and remains
optional when its distant-land responsibilities are not needed.

## Enum spellings

| Path | Accepted values |
| --- | --- |
| `graphics.anti_aliasing` | `none`, `x2`, `x4`, `x8` |
| `graphics.z_buffer_format` | `d24s8`, `d24x8`, `d32`, `d16`, `d16l`, `d32fl`, `intz`, `rawz`, `df16`, `df24` |
| `graphics.vsync` | `immediate`, `one`, `two`, `three`, `four` |
| `graphics.anisotropy` | `off`, `x2`, `x4`, `x8`, `x16` |
| `render.fog_mode` | `depth_pixel`, `depth_vertex`, `range_vertex` |
| `render.screenshot_format` | `bmp`, `jpeg`, `dds`, `png`, `tga` |
| `render.screenshot_suffix` | `timestamp`, `ordinal`, `character_ordinal`, `character_game_time_ordinal` |
| `render.window_align_x`, `render.window_align_y` | `left`, `center`, `right` |
| `distant_land.per_pixel_mode` | `always`, `interiors_only` |
| `input.macros[].type` | `unused`, `console1`, `console2`, `hammer1`, `hammer2`, `unhammer`, `alternate_hammer1`, `alternate_hammer2`, `alternate_unhammer`, `press1`, `press2`, `unpress`, `begin_timer`, `end_timer`, `graphics` |

## Complete cutover inventory

The following inventory is deliberately expressed as old section/key to new
TOML path. An explicit name set plus a field pattern covers the repeated
weather rows, so all 50 generated bindings remain reviewable without copying
the same mapping ten times.

### Runtime and graphics

| Obsolete INI entry | TOML path |
| --- | --- |
| `[Misc] MGE Disabled` | `runtime.disabled` |
| `[Misc] Internal MWSE Disabled` | `runtime.mwse_disabled` |
| `[Misc] Only Proxy D3D8To9` | `runtime.proxy_only` |
| `[Misc] Skip Intro Movies` | `runtime.skip_intro` |
| `[Misc] Use Menu Background Caching` | `runtime.menu_caching` |
| `[Misc] Customize 3rd Person Camera` | `runtime.custom_camera` |
| `[Misc] Initial 3rd Person Camera X` | `runtime.camera_x` |
| `[Misc] Initial 3rd Person Camera Y` | `runtime.camera_y` |
| `[Misc] Initial 3rd Person Camera Z` | `runtime.camera_z` |
| `[Misc] Crosshair Autohide` | `runtime.crosshair_autohide` |
| `[Global Graphics] Antialiasing Level` | `graphics.anti_aliasing` |
| `[Global Graphics] Z-Buffer Format` | `graphics.z_buffer_format` |
| `[Global Graphics] VWait` | `graphics.vsync` |
| `[Global Graphics] Refresh Rate` | `graphics.refresh_rate` |
| `[Global Graphics] Borderless Window` | `graphics.borderless` |
| `[Render State] Anisotropic Filtering Level` | `graphics.anisotropy` |
| `[Render State] Transparency Antialiasing` | `graphics.transparency_antialiasing` |
| `[Render State] Horizontal Screen FOV` | `render.fov` |
| `[Render State] Fog Mode` | `render.fog_mode` |
| `[Render State] Hardware Shader` | `render.enable_shaders` |
| `[Render State] HDR Reaction Time` | `render.hdr_reaction_time` |
| `[Render State] MGE FPS Counter` | `render.fps_counter` |
| `[Render State] MGE Messages` | `render.messages` |
| `[Render State] MGE Messages Timeout` | `render.message_timeout_ms` |
| `[Render State] Screenshot Format` | `render.screenshot_format` |
| `[Render State] Screenshot Output Directory` | `render.screenshot_directory` |
| `[Render State] Screenshot Name Prefix` | `render.screenshot_name` |
| `[Render State] Screenshot Name Suffix` | `render.screenshot_suffix` |
| `[Render State] UI Scaling` | `render.ui_scale` |
| `[Render State] Window Align X` | `render.window_align_x` |
| `[Render State] Window Align Y` | `render.window_align_y` |

`render.indexed_skinning` has no obsolete INI predecessor. It enables the
indexed bone-palette skinning path: MGE XE rebuilds Morrowind's skin partitions
around an eight-entry palette so each skinned mesh draws in fewer partitions.
It defaults to `false` and is read once at process start, so changing it takes
effect only after a full game restart. It is never applied to partitions that
have already been built.

The feature also needs the matching custom DXVK `d3d9.dll`, which supplies the
eight-entry fixed-function matrix palette. MGE XE checks for it at runtime and
stays on the stock skinning path when it, or any of the engine patches, is
unavailable. Set `render.indexed_skinning = true` to opt in; leaving it at the
default `false` is the documented rollback and is safe with the custom DXVK
still installed.

### Distant land

| Obsolete `[Distant Land]` key | TOML path |
| --- | --- |
| `Distant Land` | `distant_land.enabled` |
| `Automatic Distant Land Rebuild` | `distant_land.automatic_rebuild` |
| `Distant Statics` | `distant_land.statics` |
| `Use Distant Water Without Distant Land` | `distant_land.water_without_land` |
| `Render Grass` | `distant_land.render_grass` |
| `Draw Distance` | `distant_land.draw_distance` |
| `Near Statics End` | `distant_land.near_static_end` |
| `Far Statics End` | `distant_land.far_static_end` |
| `Very Far Statics End` | `distant_land.very_far_static_end` |
| `Far Static Min Size` | `distant_land.far_static_min_size` |
| `Very Far Static Min Size` | `distant_land.very_far_static_min_size` |
| `Water Reflects Land` | `distant_land.water.reflect_land` |
| `Water Reflects Near Statics` | `distant_land.water.reflect_near_statics` |
| `Water Reflects Interiors` | `distant_land.water.reflect_interiors` |
| `Enable Sky Reflections` | `distant_land.water.reflect_sky` |
| `Dynamic Ripples` | `distant_land.water.dynamic_ripples` |
| `Blur Water Reflections` | `distant_land.water.blur_reflections` |
| `Water Wave Height` | `distant_land.water.wave_height` |
| `Water Caustics Intensity` | `distant_land.water.caustics_intensity` |
| `Use Exponential Fog` | `distant_land.fog.exponential` |
| `Use Atmosphere Scattering` | `distant_land.fog.atmosphere_scattering` |
| `Above Water Fog Start` | `distant_land.fog.above_water_start` |
| `Above Water Fog End` | `distant_land.fog.above_water_end` |
| `Below Water Fog Start` | `distant_land.fog.below_water_start` |
| `Below Water Fog End` | `distant_land.fog.below_water_end` |
| `Interior Fog Start` | `distant_land.fog.interior_start` |
| `Interior Fog End` | `distant_land.fog.interior_end` |
| `Sun Shadows` | `distant_land.shadows.enabled` |
| `Sun Shadow Map Resolution` | `distant_land.shadows.map_resolution` |
| `Per Pixel Shader` | `distant_land.per_pixel_lighting` |
| `Per Pixel Shader Flags` | `distant_land.per_pixel_mode` |
| `Terrain Horizon Culling` | `distant_land.horizon.culling` |
| `Horizon Height Bias` | `distant_land.horizon.height_bias` |
| `Horizon Object Bias` | `distant_land.horizon.object_bias` |
| `Horizon Near Exclude` | `distant_land.horizon.near_exclude` |
| `Horizon Ring Step` | `distant_land.horizon.ring_step` |
| `Horizon Max Range` | `distant_land.horizon.max_range` |
| `Horizon Azimuth Bins` | `distant_land.horizon.azimuth_bins` |
| `Horizon Sample Spacing` | `distant_land.horizon.sample_spacing` |
| `Horizon Adaptive Gate` | `distant_land.horizon.adaptive_gate` |
| `Horizon Hierarchical March` | `distant_land.horizon.hierarchical_march` |

The host-only horizon setting with no obsolete C++ binding is
`distant_land.horizon.rebuild_eye_threshold`.

`distant_land.grass.interior_wind` has no obsolete INI predecessor either. It
is the constant wind applied to grass placed in interior cells, which have no
weather to drive the per-weather wind factors. It uses the same units as those
factors and is clamped to `0.0..1.0`; `0.0` leaves interior grass with only the
shader's faint idle shimmer. The GUI edits it as the Interior row of the
Distant Land Weather Settings window, where the fog columns are disabled
because interiors have no weather fog to scale. Interior grass comes from the generator's
`grass_plugins` list, whose interior placements are baked into `usage.data`
like exterior ones.

### Weather and lighting patterns

For each weather name in `clear`, `cloudy`, `foggy`, `overcast`, `rain`,
`thunderstorm`, `ashstorm`, `blight`, `snow`, and `blizzard`:

| Obsolete entry pattern | TOML path pattern |
| --- | --- |
| `[Distant Land Weather] <Weather> Wind Ratio` | `distant_land.weather.<weather>.wind` |
| `[Distant Land Weather] <Weather> Fog Ratio` | `distant_land.weather.<weather>.fog_ratio` |
| `[Distant Land Weather] <Weather> Fog Offset` | `distant_land.weather.<weather>.fog_offset` |
| `[Per Pixel Lighting] <Weather> Sun Brightness` | `lighting.weather.<weather>.sun` |
| `[Per Pixel Lighting] <Weather> Ambient Brightness` | `lighting.weather.<weather>.ambient` |

### Structured and GUI-owned data

| Obsolete INI representation | TOML representation |
| --- | --- |
| Bare `[Shader Chain]` lines | `shaders.chain = ["Shader A", "Shader B"]` |
| `[Macros] M<index>=...` plus `[MacrosDesc]` | `[[input.macros]]` with `index`, `type`, and type-specific fields |
| `[InputTriggers] T<index>=...` | `[[input.triggers]]` with `index`, `active`, `interval_ms`, and `keys` |
| `[InputRemap] R<source>=<target>` | `[input.remap]` |
| `[Render State] Match FOV To Aspect Ratio` | `gui.match_fov_to_aspect_ratio` |
| `[Distant Land] Auto Distances` | `gui.auto_distances` |
| `[Distant Land] Auto Distances Choice` | `gui.auto_distance_mode` |
| `[Distant Land] Exponential Distance Multiplier` | `gui.exponential_distance_multiplier` |
| `[Main] GUI Language` + `[Main] Language Autodetection` | `gui.language = "auto"` or an embedded locale code; see [GUI localization](../gui/localization.md) |

Trigger intervals are milliseconds in both TOML and the runtime. This removes
the old GUI/runtime factor-of-1000 mismatch.

## Runtime ownership

The following bound values retain the legacy `DONT_SAVE` policy because the GUI,
registry, or structured Rust model owns them:

- startup gates: `runtime.disabled`, `runtime.mwse_disabled`, and
  `runtime.proxy_only`;
- graphics/display selection fields;
- screenshot format/path/name/suffix;
- `distant_land.water_without_land`;
- shader chain and all structured input collections.

`d3d8/crates/config-contract-test` exports the real C++ binding table and verifies all
131 rows against the Rust schema on the i686 target, including uniqueness,
storage widths, buffer capacities, `DONT_SAVE`, and default values.

## `Morrowind.ini`

`Morrowind.ini` remains game-owned and is not part of this TOML schema.
`MGEXEgui/src/morrowind_profile.rs` uses `GetPrivateProfileStringA` and
`WritePrivateProfileStringA` against the engine's exact
`.\\Morrowind.ini` path for the following scalar entries:

- `[General]`: `Max FPS`, `Screen Shot Enable`, `DontThreadLoad`,
  `AllowYesToAll`, `High Detail Shadows`, `Show FPS`, `Disable Audio`,
  `Subtitles`, and `ShowHitFader`.
- `[LightAttenuation]`: `UseConstant`, `ConstantValue`, `UseLinear`,
  `LinearValue`, `UseQuadratic`, and `QuadraticValue`.

`DontThreadLoad` remains the inverse of the GUI's thread-loading option. Reads
honor attenuation enable defaults false/true/false; saves intentionally preserve
the established behavior of enabling all three attenuation terms.
