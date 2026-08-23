# Terrain package

The terrain path produces the terrain runtime set: `terrain.bin` (world-space meshes) plus
five DDS files (`terrain_atlas`, `terrain_material`, `terrain_material_flags`,
`terrain_patch_albedo`, `terrain_blend_patterns`).
[src/generation/terrain_stage.rs](../../src/generation/terrain_stage.rs) orchestrates it across a
no-write planning phase and a ledger-recorded publication phase. Preparation runs before
static-atlas work: `prepare_terrain_package_inputs` captures everything needed for the domain
digests, gate, and metrics. The stage builds, serializes, hashes, and compares terrain occlusion
with the existing bytes.
Plan selection follows the unit diff, and all of those decisions enter the single `CommitPlan`.
On a package hit the stage skips the heavy build entirely; on a miss it rebuilds only the work
the diff selects (see [Terrain publication decisions](#terrain-publication-decisions)).

All layout, sampling, mesh, and package construction code lives in `crates/terrain`. It depends on
usage, VFS, foundation, formats, and the shared texture crate, and returns in-memory plans/results.
The root `generation/terrain_stage.rs` alone owns durable files, publication permits, and inventory
updates.

Disabling `generate_terrain` skips package publication and records the
`terrain_package_disabled` manifest warning. The committed output remains a valid statics-only
tree, but it does not provide the full terrain runtime set.

## Atlas layout (`crates/terrain/src/layout.rs`)

`build_terrain_atlas_layout` packs the set of landscape cell coordinates into
`TerrainAtlasRegion`s, rectangular groups of cells with an offset into a shared logical
atlas space. The layout depends only on *which* cells exist, so the orchestrator computes it
once, right after plugin parsing, and shares it between the texture and mesh stages. Regions
also define the world-space chunking used by mesh generation.

## Source textures (`crates/terrain/src/texture/`)

- `cell.rs`: owns terrain-domain sampling over the decoded `TerrainCell` values supplied by
  `distantland_usage`. Cross-cell helpers (`sample_vertex_texture`, `mod_cell`,
  `vertex_to_patch`) handle Morrowind's off-by-one patch addressing at cell borders. LAND decoding,
  LTEX-table resolution, and `TerrainCell::is_default` remain in the usage crate.
- `texture_cache.rs`: `TerrainTextureCache` loads every unique referenced texture plus the
  default fallback, capped at `max_terrain_texture_size`. The cache splits loading into two
  phases: `load_inputs` only reads each asset's bytes and computes `bytes_hash` (the identity
  fingerprint the unit fingerprint and tier-0 dedupe need), leaving `levels`/`bc1_mips` empty.
  The cache calls `decode` later, in parallel, and only when a terrain package rebuild actually
  needs pixels; a clean terrain diff decodes nothing. For DDS sources, decode starts from the
  best-fitting source mip rather than the top level. When the source is BC1/BC2/BC3 with safe
  block encodings, decode also extracts ready-made BC1 blocks per mip
  (`Bc1CompatibleSourceExtractor`), enabling a lossless block-copy fast path when building the
  atlas. Each texture carries a full RGBA mip chain generated with sRGB-correct (linear-light)
  2×2 downsampling. Lookups never fail: missing keys fall back to the default texture.
- `sampler.rs`: LOD-blended bilinear sampling primitives over precomputed sample grids
  (`SampleGrid`, `PreparedTextureSlot`), used by patch-albedo averaging and anywhere terrain
  texels are resampled into output space.
- `color.rs`: shared sRGB ↔ linear conversion helpers.
- `dds.rs`: the terrain-specific DDS encoders (`write_bc1_dds_from_chain_unflipped`,
  `write_bc1_dds_unflipped`, `write_rgba8_dds_unflipped`), all top-down (unflipped) layouts on
  top of the shared encoders in
  [crates/texture/src/dds.rs](../../crates/texture/src/dds.rs).

## Terrain meshes (`crates/terrain/src/mesh.rs`)

`TerrainMeshBuilder` builds one `TerrainMesh` per selected mesh chunk (a fixed number of cells per
side within a region), in parallel. Full rebuilds select every enumerated work item; incremental
rebuilds select only work affected by dirty terrain chunks.

### Work keys and deterministic assembly

Mesh work is enumerated up front by `enumerate_terrain_mesh_work` into descriptors, each carrying a
`TerrainMeshWorkKey`, the absolute starting LAND cell plus `cells_per_side`. The key names the
*nominal square* the builder processes. It encodes neither a region vector ordinal nor an atlas
placement, so it stays stable for any layout that decomposes the world into the same squares, and
it also names edge work whose owned-cell rectangle is clipped by its region. Squares are anchored
at their region's origin, not on the absolute chunk grid, so they are stable only while the layout
is. A changed layout is a full-terrain invalidator.

Each descriptor also declares the absolute `TerrainChunkUnitKey`s it depends on
(`overlapping_absolute_chunks`). The declaration covers every chunk overlapping the nominal
square, unclipped by the region. That is deliberate and load-bearing: a work item's read set is
the nominal square plus a one-cell fringe, and those reads can fall outside the region and still
change output. `default_chunk_uniform_height` scans one cell past the square in `+x`/`+y`
regardless of the region, and the dense grid's far edge vertices land on the next cell. Since a
chunk's fingerprint already covers its 4×4 area plus a one-cell halo, every cell within distance
one of a nominal cell is covered by that cell's own chunk, which makes the nominal-square set
sufficient. Clipping to the region would not make it sufficient. A narrow region can leave fringe
reads several cells outside every declared chunk's halo.

Building returns one `TerrainMeshWorkResult` per descriptor, retaining `None` for work that emits
no record. `assemble_terrain_mesh_set` then sorts by key, rejects duplicates, and only then drops
absent results and lowers to `TerrainFile.meshes`. Emitted record order is therefore a property of
the work keys alone rather than of the parallel iterator's collection order. The same path serves
forced-clean and incremental runs. MGE-XE pairs each record with the buffer at the same
ordinal and then uses spatial bounds, so a deterministic file order is all the runtime requires.

### Per-chunk build

Per populated chunk:

1. **Dense grid.** The builder samples vertices from the 65×65 height grids at full resolution
   (64 quads per cell edge, two triangles each), each carrying position, raw normal, a *smoothed*
   normal (averaged across cell borders so simplification doesn't see seams), and vertex color.
2. **Simplification.** meshopt `simplify_with_attributes` with configurable smoothed-normal and
   vertex-color weights (`terrain_mesh_{smoothed_normal,color}_weight`) and the absolute target
   error selected by the `TerrainDetail` preset (Ultra High 15.0 … Low 256.0 world units).
   The builder locks chunk-border vertices so neighboring chunks stay watertight.
3. **Fast path.** Chunks consisting entirely of default cells at a uniform height collapse to
   a trivial two-triangle quad (`build_default_terrain_mesh`).
4. **Output.** `TerrainVertex` (position + normal packed UBYTE4N-biased + color packed as
   D3DCOLOR), `u16` or `u32` indices chosen by vertex count, per-mesh AABB and bounding
   sphere. The builder drops deep-water-only chunks.

`MeshSimplifierConfig` ([mesh/config.rs](../../crates/terrain/src/mesh/config.rs)) bundles the
target error, simplifier options, and weights; it is hashed into the terrain package fingerprint,
so any tuning change invalidates the cache. Chunk width is not part of it: `MESH_CHUNK_CELLS_PER_SIDE`
([mesh.rs](../../crates/terrain/src/mesh.rs)) is a fixed layout invariant, hashed into the same
fingerprint so a source change still invalidates cached terrain.

## Package assembly (`crates/terrain/src/package.rs`)

`prepare_terrain_package_inputs` derives, without doing heavy work:

- The source atlas spec ([package/atlas.rs](../../crates/terrain/src/package/atlas.rs),
  `choose_source_atlas`): square atlas of logical tiles (one per distinct texture), each
  surrounded by wrap gutters (the tile repeated into the border) so bilinear sampling and
  mip generation never bleed across tiles. Physical tile = logical + 2·gutter; the max LOD is
  bounded so gutters survive downsampling.
- The material plan ([package/material.rs](../../crates/terrain/src/package/material.rs)):
  terrain texture ids (dedupe-aware, `collect_terrain_texture_ids`), then per landscape patch
  (the 16×16 VTEX grid per cell) a `PatchMaterial`: base texture id, decal texture id, and a
  blend pattern. Blend patterns are 5×5 alpha grids (`BlendAlphaGrid`) describing how the decal
  blends over the base within the patch. `choose_blend_pattern_atlas` collects the distinct
  patterns and packs them into their own small atlas.
- The control region (`terrain_control_region`): the patch-space rectangle covering all
  cells. The planner validates it against `max_terrain_control_texture_size` / `_bytes`,
  failing with clear errors instead of producing textures the runtime can't allocate.
- The terrain gate inputs (`TerrainGateInputs` in
  [package.rs](../../crates/terrain/src/package.rs)): layout, atlas and control-map geometry, ordered
  texture identities, detail and mesh-simplifier settings, and explicit algorithm versions.
  [generation/unit_fingerprint.rs](../../src/generation/unit_fingerprint.rs) combines those global
  facts with the per-cell and per-chunk fingerprint tables to produce `terrain_domain_digest`.

Package materialization is split so that height-independent work can be skipped. The material plan
determines the five DDS products' content and carries the blend-pattern facts the `terrain.bin`
header needs. `plan_terrain_package` computes it and retains the loaded texture cache, atlas
spec, and control region, all without reading LAND heights.
`TerrainPackagePlan::build_dds_images` is the heavy half: it builds the source atlas chain and
rasterizes/encodes the control images. A mesh-only rebuild computes the plan for its header facts
and never calls it.

The package produces:

| Output | Content | Encoding |
|---|---|---|
| `terrain.bin` | `TerrainFile`: assembled terrain meshes plus atlas/control-map geometry the shader needs | custom binary, see [binary-formats.md](binary-formats.md) |
| `terrain_atlas.dds` | all source textures as gutter-wrapped tiles, full per-tile mip chain; BC1 blocks block-copied from source DDS when eligible, re-encoded otherwise | BC1, per-mip built by `build_terrain_atlas_bc1_chain` |
| `terrain_material.dds` | one texel per patch: `pack_material_texel(base_id, decal_id)` | RGBA8, no mips |
| `terrain_material_flags.dds` | one texel per patch: `pack_material_flags_texel(pattern_id, flags)` | RGBA8, no mips |
| `terrain_patch_albedo.dds` | per-patch average of the blended base+decal albedo (LOD-correct sampling, linear-light averaging), the far-distance fallback color | BC1 + mips |
| `terrain_blend_patterns.dds` | the distinct 5×5 blend patterns rasterized into a small atlas with clamped gutters | RGBA8, no mips |

### Terrain publication decisions

The stage runs as prepare → diff → plan → publish, so that plan selection can consume the complete
unit diff (`prepare_terrain_stage` populates state and digests; `plan_terrain_stage` decides).

`terrain_domain_digest` still gates the whole package: an unchanged terrain domain carries the
payload and DDS inventory without copying or re-hashing, and every write is `skipped_unchanged`.
When it misses, the stage makes three decisions independently:

- **Mesh records.** A work item rebuilds iff any declared chunk dependency is in the complete dirty
  set. Otherwise it carries its committed record, or carries its prior *absence* when its key
  emitted no record. Rebuilding may add or remove a record (a deep-water threshold, say), so the
  file is reassembled from the current complete work-key set and a new ordered key list published.
  The deep-water threshold is the reachable case: a work item whose cells are all default and flat
  at `DEEP_WATER_Z` takes the fast path and emits nothing, but perturbing one height makes that cell
  non-default, forces the dense path, and inserts a record at that key's position, not at the end.
  Reuse requires validating the committed `terrain.bin` first: matching terrain-global fingerprint,
  no terrain cell added or removed, every persisted key present in the current work set, matching
  header facts, header `mesh_count` equal to the persisted key count, and a full payload hash match
  against the inventory. Any failure falls back to a complete mesh rebuild with a stable reason.
  A regenerable run is never failed just because reuse was unavailable.
- **The five DDS products.** These stay one coarse bundle, gated by `terrain_dds_domain_digest`
  over the exact union of their inputs (control region and layout facts, atlas sizing and dedupe
  recipe, ordered source texture identities, per-cell material/VTEX data, and rasterizer versions).
  The gate excludes height, normal, and vertex-color data because none of the five builders reads
  them, so a height-only edit carries all five.
- **`terrain_occlusion.bin`.** It is explicitly coarse. Its builder scans the complete height domain
  and produces a global max-Z base grid; localizing that would introduce another unit model without
  serving the mesh result. It keeps its independent byte-level content gate: unchanged bytes are
  `skipped_unchanged`; the stage rewrites and inventories changed bytes. This is a known
  limitation, not a claim of occlusion unit incrementality.

`force_rebuild` bypasses record reuse and DDS carry, but still publishes complete current metadata
so the next ordinary run can reuse it.

Invalidate-first / state-last ordering is unchanged. The stage assigns record metadata to the
current state only after the terrain writer joins, so state records what was actually committed,
and any failure after `begin_dirty` leaves the cache absent. `finish_publish` checks every changed
destination against the `PublicationWrites` ledger it drains.

## Shadow units

Terrain-cell units are keyed by absolute LAND cell. Their fingerprints combine the cell's own
height/normal/color/material identity with the precise current/north/west/northwest material
sources and the content identities of textures those sources select. This makes one source
material edit propagate to existing south/east/southeast consumers without making unrelated
neighbor geometry an input. The same facts build `texture -> terrain consumer cells`.

Terrain-chunk units use the absolute four-cell grid (`div_euclid(4)`) and fingerprint the full
4×4 ownership area plus a one-cell halo, including explicit missing-cell markers and mesh
simplifier/version inputs. That halo is what makes the work-key dependency declaration above
sufficient, and it is also what makes one edit reach its neighbours: editing cell `3,0` dirties
chunk `0,0`, which owns it, *and* chunk `1,0`, whose halo starts at x = 3.

Mesh work items remain region-relative for byte compatibility; a work item is dirty iff any
absolute chunk it declares is dirty. Terrain-disabled runs persist no terrain unit tables and no
record metadata, so re-enabling safely reports all terrain units added.

The complete dirty-chunk set is typed and unbounded (`GenerationDiff::dirty_terrain_chunks`).
The `UnitsReport.dirty_keys` list in the manifest is bounded to 64 entries for readability and is
diagnostic only. It must never drive builder decisions.
