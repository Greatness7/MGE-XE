# Changelog

## v0.21.0 beta

### Added

- Camera-relative rendering, on by default. Fixes the jittering that was caused by float precision
  loss in areas far from the world origin, primarily in the Project Tamriel mods. ([#20](https://github.com/Greatness7/MGE-XE/issues/20))
- Distant statics now stream in and out as you travel instead of all loading at startup, so VRAM
  use no longer scales with the size of your mod list.
- Grass rendering is new enabled in interiors. The interface now auto-detects interior grass mods. ([#21](https://github.com/Greatness7/MGE-XE/issues/21))
- The `mgeXE.toml` file now automatically updates itself to include new settings as they released.
- Water volumes: mods can place bodies of water at any height, apart from the cell's own water.
  Their surfaces are drawn with the water shading, near the player and in distant land. Meshes
  are marked as water by the names in them, which the default statics classifiers set, or
  through plugin metadata; see the mod metadata guide. Distant land must be generated again.
  A water volume can have a colour: the emissive colour of its material, and a look of its own:
  flowing ripples, their speed and size, glow, opacity, clarity (how fast the water hides
  what is under it), and a tint from the vertex
  colours of the mesh, which the mod that owns the volume reports through `MGE_WaterLookSet`.
  A mod can also ship a water shader of its own for its surfaces
  (`Data Files\shaders\water\<name>.fx`); see `docs/water-shaders.md`. The look and the
  shader of a mesh apply in distant land too. The `wv` key of a mesh entry in plugin
  metadata gives a look to a mesh that cannot be edited. A water shader also gets the second
  texture of the mesh (a decal, a detail or a dark map). In an interior a water volume
  reflects the light of the room in place of the sky, and a look can give that colour itself.
  A water volume keeps the stencil test and the depth test of its mesh, so that a mesh can
  show its water through a mask: the water in a well. A mod can report closed shapes inside
  which there is no water, the hold of a boat under the water line (`MGE_WaterMasksSet`):
  water volumes, the water of the cell and the caustics are not drawn inside them.
  Distant land carries these dry spaces too, for a boat that the game has not loaded: the
  generator keeps a shape named `WaterMask` with its distant static. The `dry_space` key of a
  mesh entry, or an empty `mask_names` list, switches that off. Distant land must be
  generated again (`MGE_DL_VERSION` 21).
  A water shader of a mod can give ripples of its own to the standard shading, from a normal
  map or along a flow map (`shadeWaterVolumeRippled`). Three new settings, off by default,
  put light on water volumes: the shadows of distant land, the glint of the lamps and fires
  of the game, and caustics under the surface (`distant_land.water.volume_shadows`,
  `volume_point_lights`, `volume_caustics`).
- The water of a cell can have a colour: the emissive colour of the game's water material,
  which a mod can set from Lua.

### Changed

- Distant statics use about 23% less video memory and disk space.
- Interiors with no name are ignored and no longer add their contents to the exterior world spaces.

### Fixed

- Fixed a regression that caused billboard shapes to be ignored when generating distant statics.
- Fixed a (MGE XE UF) crash caused by reading one entry past the end of the distant object list.
- Fixed a memory leak when loading textures that caused out-of-memory crashes on large installs.
- Fixed malformed assets causing crashes or render incorrectly with `indexed_skinning` enabled.
- Fixed distant land generation failing on mods that place cells very far from the world origin.
- Fixed enabling per-pixel lighting not updating light attenuation values to `Morrowind.ini`.
- Fixed the fog on water in an interior that has no distant land: the exponential fog kept
  the values of the last cell that had distant land.
- Fixed several inherited (MGE XE) bugs related to water ripple logic.

## v0.20.3 beta

### Fixed

- Fixed some distant objects receiving incorrect vertex colors.

## v0.20.2 beta

### Fixed

- Fixed distant land generation and loading for non-English interior cell names.
- Fixed handling of grass mods that place statics defined in a master file (such as Tamriel Data).

## v0.20.1 beta

### Fixed

- Reduced memory use while generating distant land, hopefully preventing crashes with very large
  mod lists ([#3](https://github.com/Greatness7/MGE-XE/issues/3)).
- The configuration utility no longer fails to launch when ReShade is installed
  ([#4](https://github.com/Greatness7/MGE-XE/issues/4)).
- The configuration utility no longer needs administrator rights to save display settings, and now
  shows the same resolution the game and the Morrowind launcher use.
- The configuration utility now detects groundcover mods that use grass only defined in master files.
- Fixed sun shadow flickering at the bottom screen corners and at the cascade intersection.
- Skinned meshes, such as the hanging banners in Vivec, are now included in distant statics
  ([#1](https://github.com/Greatness7/MGE-XE/issues/1)).

## v0.20.0 beta, G7 fork

First public release of the G7 fork, based on MGE XE 0.19.1. Requirements, installation and feature
detail are in [README.md](README.md).

### Added

- Batched distant land. The generator merges each cell's geometry into one mesh split by texture
  state and packs static textures into atlases, so a cell costs one or two draw calls rather than
  one per object.
- Incremental regeneration. Only dirty cells, changed shards and touched atlas pages rebuild.
- Plugin `-metadata.toml` files are detected and loaded, so mod authors ship distant-land overrides
  and dynamic visibility with the mod instead of documenting install steps.
- Distance-tiered detail for statics: three triangle budgets per object instead of one fixed mesh.
- Horizon culling for statics and terrain, on by default.
- Automatic texture sizing and deduplication, with VRAM estimates and warnings during generation.
- Native depth capture, on by default (`native_depth_capture`). MGE reads the real depth buffer
  instead of redrawing the scene, which also fixes objects vanishing from scenes that use darkmaps
  with transparency.
- Native per-pixel lighting, on by default (`native_ppl_packets`). Lighting data goes straight to
  the GPU instead of through the legacy shader path.
- Up to 32 lights per object, opt-in (`expanded_light_limit`). Needs `native_ppl_packets` to reach
  per-pixel-lit objects.
- BC7 texture support, for finer detail and compatibility with OpenMW mods.
- Indexed skinning for meshes, opt-in and undertested (`indexed_skinning`).
- Native GUI with a dark theme, a tabbed generator window, four languages (EN/FR/PL/RU), a built-in
  log viewer and automatic startup rebuilds. Grass mods are detected and listed separately from
  plugins, and the distant-land tuning controls are new. No .NET runtime required.

### Changed

- Settings live in `mgeXE.toml` in the game root, replacing MGE3 and `MGE.ini`. You can edit it by
  hand, and your comments and formatting survive a GUI save. Nothing migrates from 0.19.1.
- `MGE_DL_VERSION` 7 to 16. Distant land data is incompatible with upstream in both directions, and
  the runtime rejects it rather than rendering it.
- Landscape rewritten. Texture quality no longer degrades as landmass mods are added and no longer
  depends on world size, and the geometry is finer.
- Distant landscape uploads in frame-budgeted chunks during load and menu frames.
- Terrain index data uses the narrowest width each mesh allows, and grass vertices went from 28 to
  20 bytes. Both upload without a conversion pass.

### Removed

- The C# WinForms GUI, the C++ helper process and the MSBuild build system.
- The vendored `3rdparty/` tree: niflib, AMD Tootle and the 4 GB patch. NIF parsing and mesh
  optimization are Rust crates now (`tes3`, `meshopt`).
- The bundled MWSE and its updater, the installer and the uninstaller.
- The DirectX capabilities viewer, static override template generation, the tooltip reading-speed
  preference and the "reduce texture memory use" toggle.

### Requirements

- A custom DXVK build ships as `d3d9.dll` and is required. Do not substitute an official release.
  Stock DXVK lacks the memory-placement profile that prevents out-of-memory crashes in Morrowind's
  32-bit address space, and it cannot load BC7 textures.
- Vulkan 1.3 hardware (NVIDIA Maxwell, AMD Polaris, Intel Iris Xe or Arc and newer) on 2023 or newer
  drivers, 64-bit Windows 10 1809 or later, an SSE4.2 CPU, and 8 GB or more VRAM for large mod lists.
- The DirectX June 2010 redistributable and the Visual C++ runtime.
- An SSD holding your game data. Not strictly required, but generation is painful without one.
- Distant land must be regenerated after installing.
