# Virtual filesystem (VFS)

The VFS ([crates/vfs/src/lib.rs](../../crates/vfs/src/lib.rs) plus `crates/vfs/src/`) reproduces the game engine's data-layer
behavior: a stack of data directories plus BSA archives, resolved case-insensitively, with
loose files overriding archive entries. Plugin loading, NIF extraction, and texture
loading all resolve assets exclusively through it.

The implementation is the standalone `distantland_vfs` crate. It depends only on foundation
and third-party archive/path utilities; usage, statics, terrain, and the root pipeline depend on it,
never the reverse.

## Construction

`Vfs::load(&VfsLoadOptions)` ([crates/vfs/src/loader.rs](../../crates/vfs/src/loader.rs)) performs the full
startup sequence:

1. **INI discovery.** Uses the provided `morrowind_ini` path, or `find_morrowind_ini()`
   (Windows registry / default install location).
2. **Data directories.** Uses the explicit `data_dirs` list when provided (validated to exist),
   otherwise the list derived from the INI (`morrowind_data_dirs`). Order matters: later
   directories have higher priority.
3. **Plugin selection.** Uses the explicit `plugins` list when provided (bare filenames resolved
   against data dirs, highest-priority dir first; order preserved exactly), otherwise the INI
   `[Game Files]` section parsed by
   [crates/vfs/src/config_parsers.rs](../../crates/vfs/src/config_parsers.rs) and sorted the way the engine
   does: masters (`.esm`) first, then by file modification time.
4. **BSA archives.** `Morrowind.bsa` implicitly plus the INI `[Archives]` entries, resolved
   against data dirs and memory-mapped via `tes3::bsa::Archive::from_path`. Archives that fail
   to open are skipped with a warning. The loader keeps each archive's modification time for
   precedence checks.
5. **Asset maps.** `build_vfs_maps` indexes every `Meshes\` and `Textures\` entry from each
   BSA, then overlays loose files (`overlay_loose_files`). An archive that stores no file names
   is warned about and skipped, since every lookup here is by name.

`Vfs::load_metadata_only` skips step 5 for cheap startup status checks that only need the
load order (used by `check_output_status`).

## Asset maps and precedence

`DirectoryMaps` ([crates/vfs/src/directory_map/map.rs](../../crates/vfs/src/directory_map/map.rs)) holds two
`AssetMap`s (meshes, textures) from normalized key → `AssetSource` (loose path, BSA entry, or
embedded). Precedence rules:

- BSAs are indexed first, in archive order.
- Loose files overlay them following Morrowind's timestamp rule: a loose file replaces a BSA
  entry only when the loose file's write time is strictly newer than the archive's
  (`insert_loose_normalized`; `LooseInsertOutcome` captures each decision). Among loose files,
  later data directories win because they are overlaid later.
- The embedded error texture is generator-owned, force-inserted last
  (`insert_embedded_error_texture`), and reserved: static-pipeline texture lookups that miss
  remap to it (`resolve_static_texture_key_or_error`) instead of failing.

Loose-file scanning ([crates/vfs/src/directory_map/scan.rs](../../crates/vfs/src/directory_map/scan.rs))
enumerates `Meshes`/`Textures` roots per data dir, fans the per-root walks out over rayon, and
delivers candidates in a deterministic order so map contents don't depend on scheduling.
The scanner follows symlinked roots.

## Key normalization

All keys are normalized once at insertion ([crates/vfs/src/normalize.rs](../../crates/vfs/src/normalize.rs)):
lowercase ASCII, `/` → `\` (see `normalize_byte`). `NormalizedStr` / `NormalizedString` are
newtypes that make "already normalized" a type-level fact; `AssetMap` lookups can hash multi-part
keys (`get_key_value_parts_normalized`) without allocating a joined string. Texture keys
additionally tolerate extension differences. Morrowind resolves `.tga`/`.bmp` references to
`.dds` files when present (`NormalizedString::texture_key`, `has_supported_texture_extension`).

Mesh override keys trim the leading `meshes\` prefix (`normalize_mesh_override_key`), so
override files and plugin records address meshes by the same relative key the maps use.

## Resolution APIs

- `resolve_mesh(path)` / `resolve_mesh_path`: applies MGE-XE's mesh override conventions in
  priority order before falling back to the plain key (see `resolve_mesh_key_value` for the
  exact ladder). `resolve_model_mesh_key` returns the normalized base key without override
  selection (used where identity, not content, matters).
- `resolve_texture(path)` / `resolve_texture_key`: texture lookup with extension fallback.
- `resolve_static_texture_key_or_error` / `resolve_static_texture_sym_or_error`: static
  pipeline variants that never fail (error-texture remap).
- `TextureSym`: a small index-based symbol for texture keys, valid for the `Vfs` instance that
  produced it; lets hot static-pipeline structures store a `u32`-sized handle instead of a
  string (`texture_key_for_sym` recovers the key).
- `read_asset_bytes`: reads a resolved asset from disk or decompresses it out of its BSA,
  returning `Cow` bytes.

## Ownership

The generation pipeline loads one `Vfs` during `InitializeVfs`, retains it for the full run, and
passes `&Vfs` to worker code. Library consumers and tests follow the same caller-owned model. An
application that needs hot reload should own and replace its current `Vfs`; the asset library does
not publish process-global state.
