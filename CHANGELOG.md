# Changelog

## Unreleased

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
