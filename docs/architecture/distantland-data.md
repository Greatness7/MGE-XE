# Distant land data and generation

The generated data set under `Data Files\distantland`, who produces and consumes it, and
the formats involved. Companion to [`ARCHITECTURE.md`](../../ARCHITECTURE.md) §7.

## 1. File inventory

| File | Format | Producer | Consumers |
| --- | --- | --- | --- |
| `version` | 1 byte; must equal 16. Every other value is rejected | generator | d3d8.dll (`dlshare.cpp`), host (`loading.rs`), MGEXEgui |
| `generation_state.bin` | complete-or-absent state and required-artifact inventory | generator | Rust host (`open_output_snapshot`) |
| `generation_report.toml` | advisory metrics, warnings, contract results, and bounded stage timings | generator | users and profiling tools only |
| `terrain.bin` | `XELAND02`, see [`terrain-bin.md`](terrain-bin.md) | generator | d3d8.dll (geometry upload), host (quadtree metadata) |
| `statics\static_meshes_000..127` | 128 fixed `XESTAT06` v6 shards | generator | d3d8.dll (geometry upload + static LOD tier construction), host via IPC metadata |
| `.writer.lock` | writer/session lock | generator, host | generator (exclusive), host (shared session pin) |
| `terrain_occlusion.bin` | `XEOCCL02` v2, see [`horizon-occlusion-asset.md`](horizon-occlusion-asset.md) | generator | host (horizon occluder) |
| `terrain_atlas.dds` | DXT1 mip chain (atlas of baked terrain tiles) | generator | d3d8.dll |
| `terrain_material.dds` | A8B8G8R8, point-sampled material index map | generator | d3d8.dll |
| `terrain_material_flags.dds` | A8B8G8R8, point-sampled material flag map | generator | d3d8.dll |
| `terrain_patch_albedo.dds` | DXT1 low-frequency albedo, mipped | generator | d3d8.dll |
| `terrain_blend_patterns.dds` | A8B8G8R8 blend-pattern tile sheet | generator | d3d8.dll |
| `statics\usage.data` | placements + vis groups (§4) | generator | d3d8.dll (vis groups), host (placements), MGEXEgui (trailing stat) |

All payloads use fixed paths. `generation_state.bin` is the sole publication authority; the
fixed shard set is complete only when all 128 files are inventoried and validated. Routine startup
does not parse `generation_report.toml`, so a missing, malformed, or edited report cannot change
serveability.

Runtime format declarations live in `d3d8/cpp/mge/dlformat.h` (C++, with `static_assert` layout
guards). Terrain has a Rust ABI mirror in `mgeHost64/src/abi/terrain.rs`; static mesh
runtime metadata crosses IPC as `DistantStatic` / `DistantSubset` in
`mgeHost64/src/abi/render.rs`. The file writers live in the sibling
`distantland` crate. **All relevant mirrors and their format-specific versions move together.**
`MGE_DL_VERSION` is the whole-output-tree gate; use it when a breaking change has no narrower
asset-specific interlock.

The Rust host validates `generation_state.bin` and its inventory under a shared session pin. The
client reads the `version` byte, refuses anything but 16, and opens `terrain.bin` plus the 128 fixed
shard names directly. The host uses the same terrain path for quadtree metadata. MGEXEgui
checks the current version, state, all fixed shards, and the required files only; it is
not a storage parser.

## 2. Terrain (`terrain.bin` + DDS set)

`terrain.bin` carries a 116-byte header (world origin/size, atlas/tile/gutter/material/
blend-pattern geometry, vertex stride, index format, mesh count), per-mesh headers
(bounding sphere/box, vertex/triangle counts), then a geometry blob of 20-byte vertices
(position, packed normal, colour) and 16- or 32-bit indices (chosen per mesh by vertex
count). The byte-level contract is [`terrain-bin.md`](terrain-bin.md).

At load (`DistantLand::initLandscape` / `initLandscapeClientFromTerrainFile` in `distantterrain.cpp`), the path is
the fixed `Data Files\distantland\terrain.bin`:

- The client memory-maps the file, validates header/mesh layout, creates one VB/IB pair
  per terrain mesh (validating and copying the serialized index width verbatim), and loads the five
  DDS textures, validating the texture contract (formats, dimensions divisible by the
  tile layout, mip counts). `validateTerrainTextureContract` logs precise failures.
- The client copies header-derived shader constants into `DistantLand::terrainConstants`
  (`TerrainRuntimeConstants`, 64-byte ABI-checked struct) and binds them to `XE Main.fx`
  (`ehTerrain*` handles) when terrain renders; it forces sampler states for the terrain
  textures explicitly (`renderexterior.cpp`).
- The client registers buffer pointers with the host (`InitLandscape` RPC); the host re-parses
  `terrain.bin` itself for bounds and builds the land quadtree (`state/loading.rs::init_landscape`).
  When terrain horizon culling is enabled, the host loads `terrain_occlusion.bin` as the required
  occluder source; a missing or invalid asset leaves horizon culling inactive without failing
  the load.

## 3. Distant statics (`statics\static_meshes_000..127`)

[static-lod.md](static-lod.md) describes the component-provenance and cumulative
face-count mechanism end-to-end.

`XESTAT06` v6 has a 160-byte header, `static_count` 52-byte static records (type, bounds,
subset range), `subset_count` 152-byte subset records, a component table, a 16-byte-per-entry
UV-bound palette table, a texture-path string table, then the geometry blob (16-bit indices).
Both vertex layouts are 20 bytes and the stride is still selected per static via
`grass_vertex_stride`: for regular statics `position.w` is an ordinal into the subset's palette,
for grass (`STATIC_GRASS`) it is a constant `1.0` and no palette is stored.

Every file is an independent complete container; empty shards are valid. The producer assigns a
normalized static key with the ratified BLAKE3 recipe. Global ordinal order is shard id first,
then key bytes within a shard. `usage.data` indexes this logical concatenation.

Header fields through offset 108 keep the v4 meanings: record sizes, vertex/index
strides, counts, table offsets/sizes, texture blob, geometry blob, `grass_vertex_stride`,
and reserved bytes. v5 appends:

| Offset | Field | Meaning |
| --- | --- | --- |
| 112 | `component_table_offset: u64` | File-absolute, 8-byte aligned start of the component table. |
| 120 | `component_table_size: u64` | `component_count * component_record_size`. |
| 128 | `component_record_size: u32` | Always 16. |
| 132 | `component_count: u32` | Total component records across all subsets. |

`SubsetRecord` still carries bounds, texture path offset, flags
`hasAlpha`/`hasUVController`, vertex/face counts, geometry offsets, and the 56-byte
`HorizonFootprint` at offset 80. v5 appends `first_component_index: u32` at offset 136
and `component_count: u32` at offset 140. A component-less subset is valid and means all
tiers draw the full subset.

`ComponentRecord` is 16 bytes:

```
u32 first_triangle
u32 triangle_count
f32 radius          // source model radius * placement scale; building doubling is not baked
u8  classification  // source StaticType; STATIC_GRASS is invalid here
u8  reserved[3]     // zero
```

The generator emits component records for merged synthetic static subsets. Their triangle
ranges must tile the owning subset exactly, in serialized index order, with no overlaps or
gaps. The generator may coalesce adjacent compatible component records. The client
validates record sizes, section order/alignment, reserved bytes, finite non-negative
radii, known non-grass classifications, and component tiling.

Client load (`beginStaticsPhase` / `stepStaticsPhase` / `finishStaticsPhase`):

- Before any per-static D3D allocation, the client opens and validates all 128 headers/tables,
  checked-sums their static counts, and requires the sum to equal the leading `usage.data` count.
  A missing or malformed shard names that exact path and fails closed.
- Each shard keeps its bounded metadata prefix mapped. One sliding geometry window stays active
  while the client consumes shards in id order and translates local subset ordinals to the global
  concatenated subset vector. Per subset the client creates a VB and IB and
  resolves the texture through `BSA::loadTexture` (Morrowind BSAs + loose files, cached;
  an error texture is the fallback). This is the dominant load cost, so the loader is resumable.
    The `StaticsLoader` state lives outside the loop and the upload pump (see
  `ARCHITECTURE.md` §4.5) advances it in time-budgeted slices across menu frames.
- The client classifies component-bearing subsets at load using the active
  `Configuration.DL.FarStaticMinSize` / `VeryFarStaticMinSize` thresholds. Forced
  `STATIC_NEAR/FAR/VERY_FAR` classifications override radius; `STATIC_BUILDING` doubles
  radius before comparison; automatic/tree classifications use the stored radius. The
  client gather-copies the subset's index data into one GPU index buffer ordered as
  `[very-far-capable][far-only][near-only]`, producing cumulative counts
  `veryFarFaces <= farFaces <= faces`. Component-less subsets upload unchanged and set all
  three counts equal.
- The client pushes the completed metadata (`DistantStatic[]` + `DistantSubset[]`,
  containing the 32-bit D3D pointers, generated horizon footprints, and cumulative face
  counts) through shared vectors to the host (`InitDistantStatics` RPC). The same RPC also sends the
  min-size thresholds so the host's whole-static quadtree classification matches the
  loader.
- Static `type` (`STATIC_NEAR/FAR/VERY_FAR/GRASS/TREE/BUILDING`) decides which host
  quadtrees instances land in, together with the client-sent size thresholds.

## 4. Placements (`statics\usage.data`)

Sequential layout, read as a stream:

```
u32   total used-static count            (client reads; host skips)
u32   dynamic vis group count
       └─ 130-byte records: u8 source (Journal|Global|UniqueObject),
          char[64] id, u8 rangeCount, 8 × {i32 begin, i32 end}
worldspace blocks, repeated:
   u32 usedCount        (block 0 = the exterior, empty name;
   char[64] cellName     name present only for later, interior blocks;
   usedCount × UsageRecord                a 0 count after block 0 terminates)
       └─ {u32 staticIndex, u16 visIndex, vec3 pos, vec3 rot, f32 scale}
trailing metadata (GUI reads the last 4 bytes as the generated "near size" float)
```

- Both `char[64]` fields hold the engine's own single-byte name bytes, not UTF-8: on a
  localized install they are that install's codepage. They are NUL-padded when shorter and
  carry no terminator when a name fills all 64 bytes, so readers must bound at the field
  width and compare raw bytes — decoding them lossily merges distinct non-ASCII names. The
  generator recovers the original bytes by encoding through WINDOWS-1252, which round-trips
  because every high byte maps to a distinct codepoint; an embedded NUL or a name wider than
  64 bytes is rejected rather than truncated.
- The host consumes the placement blocks. It expands each record to a world transform
  (legacy D3DX rotation order), computes post-transform bounds against the registered
  static metadata, and inserts them into per-worldspace quadtrees (near/far/very-far/grass).
- The client consumes the dynamic-vis group definitions (`loadVisGroupsClient`).
  It resolves each id against game objects via MWBridge (dialogue/journal index, global
  variable value, or object-disabled flag), re-resolves on every save load, scans for
  changes on cell change, and sends toggles to the host (`UpdateDynVis`). Group index 0 is
  reserved; `visIndex` on each placement links instances to groups.

## 5. Generation pipeline

Generation lives in the `distantland/` subtree and has two entry points.

The configuration GUI (`MGEXEgui.exe`, `MGEXEgui/src/ui/generate/`) is a child window that
discovers the plugin load order from `Morrowind.ini` + data directories, edits the
`distantland` `GenerationSettings`/`GenerationJob`, and runs `ensure_generated` on a
worker thread directly against `Data Files`. The generator holds the exclusive writer lock
through complete-or-absent state publication, validation, and garbage collection. The GUI saves
the strict version-3 job in the generator-owned `[generation]` table of game-root
`mgeXE.toml` and surfaces invalid existing data instead of silently overwriting it.

At `Direct3DCreate8` time, `d3d8.dll` attempts to launch `mgeHost64.exe` when MGE is enabled,
proxy-only mode is disabled, distant land is enabled, and the executable exists. This attempt
is not gated on whether automatic generation is configured; the host policy
(`mgeHost64/src/startup_generation.rs`) checks the saved job against the install, inspects the
current output status, and regenerates if needed. A per-install named mutex
serializes regeneration, the host logs progress to `mgeHost64.log`, and a session fallback
disables distant land in-memory if generation fails.
After a successful GUI run, MGEXEgui re-reads the outputs and auto-enables the distant-land
checkboxes. Its live-session probe is advisory and bounded; an active game session surfaces the
shared "close Morrowind first" message instead of attempting generation, and the exclusive
writer lock inside `ensure_generated` remains the authority either way.

### Mod-author inputs

- Per-plugin `-metadata.toml` files shipped next to `.esp`/`.esm` files configure
  generation (statics classification etc.). See
  [`mod-metadata-guide.md`](../../mod-metadata-guide.md).
- Explicit override-source files (`.ovr`, `.txt`, or TOML using the mod-metadata schema)
  provide ordered global classification rules. New jobs enable
  `MGE3\MGE XE Default Statics Classifiers.toml` by default.

## 6. Runtime load sequencing

The complete state machine and overlap rules are in
[distantland-lifecycle.md](distantland-lifecycle.md). At a data-contract level:

1. The client waits asynchronously for the host and its session-pinned output
   snapshot, then allocates the long-lived shared vectors.
2. Upload start reads the one-byte version and fails closed unless it is 16.
3. `Landscape` uploads terrain and starts `InitLandscape`; the host
   parses the same file, builds the land quadtree, and loads the horizon
   occluder asset (degrading to an inactive field if it is missing or invalid).
4. Resumable `Statics` preflights and uploads all fixed shards while the
   landscape RPC is in flight. It then reads dynamic-vis groups and starts
   `InitDistantStatics`; the host reads placements and builds static/grass
   quadtrees.
5. `Grass` creates the client instance resources while the host finishes that
   static initialization.
6. Rendering enables only after upload completes and the save's world data has
   resolved.

A version or format-validation failure anywhere logs to `mgeXE.log` / `mgeHost64.log`,
tears down distant land, clears `USE_DISTANT_LAND` for the session, and surfaces a status
overlay error. The game keeps running without distant land.
