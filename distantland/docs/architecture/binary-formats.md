# MGE-XE binary formats

`crates/formats/src/` owns the on-disk structures for the four custom binary outputs. These formats
are contracts with the MGE-XE runtime. Field order, packing, and lane order are coupled
to MGE-XE's vertex declarations and shaders. The MGE-XE source is the authority when in doubt;
this crate's structs mirror it.

All four use little-endian layouts via the `bytes_io` reader/writer or explicit POD tables. Deserializers validate
magics, versions, and length invariants defensively (`load_terrain_file` etc. return
`io::Error` on malformed input rather than panicking).

## Version-16 publication authority

The version-16 output store uses one generator-owned authority file outside `crates/formats/src/`:

- `generation_state.bin` ([state.rs](../../crates/foundation/src/storage/state.rs)) uses the shared
  framed-archive envelope with magic `TES3GCS1`, schema version 1, reserved zeroes, and a BLAKE3
  checksum of the complete body. The body is a `CommittedState`: opaque `state_db` bytes
  plus a sorted unique inventory of `RequiredArtifact` entries (kind, relative path, length,
  content BLAKE3).

Inventory entries bind the version marker, `usage.data`, all canonical `statics\static_meshes_000..127` shards,
terrain companions when enabled, and atlas pages. Routine runtime validation checks existence,
length, and fixed current headers; Full validation also hashes every listed artifact. There is
no journal, generation index, or epoch-named payload directory. See
[storage-foundation.md](storage-foundation.md).

## `static_meshes` ([distant_statics.rs](../../crates/formats/src/distant_statics.rs))

The packed static-mesh cache is a fixed set of 128 canonical files at
`distantland\statics\static_meshes_000..127`, each with magic `XESTAT05` and version 5. Empty
shards are valid complete containers. Shard assignment hashes the normalized static key with BLAKE3 over the
domain `tes3-distantland-static-shard-assignment-v2\0`, shard count `u32_le(128)`, key length
`u64_le`, and key bytes; the low seven bits of the digest choose the shard. Global runtime order is
shard id, then key bytes within the shard. Each file has this layout:

- **Header** (`StaticMeshesFileHeader`, 136 bytes): magic, version, counts, record sizes,
  section offsets/sizes for the static table, subset table, component table, texture-path
  blob, and geometry blob. The header also carries both vertex strides, `vertex_stride`
  (regular statics, 28) and `grass_vertex_stride` (grass, 20), plus a `reserved` u32
  (must be 0). Sections are alignment-padded (`serialize_static_meshes` in
  [crates/statics/src/write.rs](../../crates/statics/src/write.rs)). v5 appends
  `component_table_offset`, `component_table_size`, `component_record_size` (16), and
  `component_count` after the v4 fields.
- **`StaticRecord`** (52 bytes/entry): whole-static bounding sphere + AABB, `StaticType`
  classification, subset range.
- **`SubsetRecord`** (144 bytes/entry): per-subset bounds, vertex/index ranges, alpha and
  UV-controller flags, the NUL-terminated texture path (atlas page name or passthrough path),
  a 56-byte generated `HorizonFootprint` at offset 80, then `first_component_index` at
  offset 136 and `component_count` at offset 140.
- **`ComponentRecord`** (16 bytes/entry): one merged source-component range in subset
  triangle units: `first_triangle: u32`, `triangle_count: u32`, `radius: f32`
  (source-model radius times placement scale; building doubling is not baked),
  `classification: u8` (source `StaticType`; grass is invalid here), and three zero
  reserved bytes. Component records tile their owning subset exactly. Component-less
  subsets are valid and render full geometry in all runtime tiers.
- **`HorizonFootprint`** (56 bytes, optional): `max_z: f32`, `vertex_count: u8`, three zero
  padding bytes, and up to six subset-local XY vertices as `[[f32; 2]; 6]`. `vertex_count == 0`
  means no generated footprint and the host falls back to box-derived horizon bounds.
- **`PackedVertex`** (28-byte stride, `#[repr(C)]` POD) for regular (non-grass) statics:

  | Field | Format | Notes |
  |---|---|---|
  | `position` | `[f16; 4]` | homogeneous position |
  | `normal` | `[u8; 4]` | packed normalized normal |
  | `color` | `[u8; 4]` | vertex color |
  | `uv` | `[f16; 2]` | primary UV |
  | `uv_bound` | `[f16; 4]` | atlas clamp rect, lane order `[min_y, max_x, min_x, max_y]` |

  The `uv_bound` lane order is shader-coupled. See the packing in
  [crates/statics/src/model/pack.rs](../../crates/statics/src/model/pack.rs); do not "fix" it.
  Fields here stay `[f32; N]`-style arrays rather than `glam` vector types on purpose.
  `Vec4`'s 16-byte alignment would change the wire layout.
- **`PackedGrassVertex`** (20-byte stride, `#[repr(C)]` POD) for grass (`StaticType::StaticGrass`)
  subsets. Identical to `PackedVertex` minus the trailing `uv_bound` field (`position`,
  `normal`, `color`, `uv`); grass never atlases so it does not need the clamp rect. The writer
  selects the stride per static via `vertex_stride_for`, and the reader/host pick it via
  `grass_vertex_stride`.
- **Index blob**: `u16` triangle indices (subsets are split so 16-bit indexing always
  suffices).

`deserialize_static_meshes` is the bounds-checked production reader. It validates fixed sizes and
strides, aligned/non-overlapping section ranges, checked count products, file-absolute offsets,
UTF-8 NUL-terminated texture paths, static/subset/component coverage, flags and reserved fields,
the regular/grass vertex stride choice, and every triangle index. Because source mesh keys are
not serialized, it returns statics in file-table order. It reconstructs grass `uv_bound` as zero
because those bytes do not exist on disk.

## `usage.data` ([usage_data.rs](../../crates/formats/src/usage_data.rs))

The static usage table MGE-XE loads at startup contains, in order: the header with
`min_static_size`, the dynamic visibility groups (64-byte fixed-width NUL-padded ids,
kind + ranges), the per-cell reference lists (`UsageDataReference`: position, rotation,
scale, and the positional index of the static in the shard-major logical concatenation), and the interior cell
entries. Writer: [crates/usage/src/write.rs](../../crates/usage/src/write.rs).

`deserialize_usage_data` wraps the existing `UsageData::load` implementation, rejects trailing
bytes, and bounds dynamic-visibility enabled ranges at the format maximum of eight.

Because references address statics by global position, `usage.data` and the complete fixed shard
set are one logical bundle. A coarse miss may still carry individual unchanged shards (see
[caching-and-startup.md](caching-and-startup.md)).

## `terrain.bin` ([distant_terrain.rs](../../crates/formats/src/distant_terrain.rs))

The world-space terrain file (`TerrainFile`), with format magic/version validation:

- **Header** (`TerrainFileHeader`): magic, version, world-space and atlas geometry the shader
  needs (control-map region, atlas tile geometry/LOD info), and the mesh count. The manifest's
  terrain section mirrors much of this for diagnostics.
- **`TerrainMesh`** entries: AABB + bounding sphere, vertex buffer, and triangle indices.
  Meshes with ≤ `u16::MAX` vertices use `u16` indices, larger ones `u32`
  (`mesh_uses_u16_indices`).
- **`TerrainVertex`**: world position (`[f32; 3]`), normal packed via
  `pack_ubyte4n_bias_normal` (UBYTE4N biased encoding), color packed via `pack_d3dcolor_vclr`
  (D3DCOLOR byte order).

## `terrain_occlusion.bin` ([terrain_occlusion.rs](../../crates/formats/src/terrain_occlusion.rs))

The terrain max-height occlusion base grid has its own magic/version, world/cell extent, spacing,
dimensions, and one row-major `f32` max-height payload. `EMPTY_OCCLUSION_HEIGHT` marks uncovered
samples. The host builds the runtime mip pyramid from this base grid, parsing it with its own
C++-mirroring reader. There is no Rust deserializer; instead, a hand-built fixture in
`terrain_occlusion/tests.rs` pins the writer.

## The `version` file and DDS outputs

- `distantland\version` is a single byte equal to `MGE_DL_VERSION` (currently 16, defined in
  [crates/foundation/src/output.rs](../../crates/foundation/src/output.rs)); it must match
  the runtime's `MGE_DL_VERSION` in `d3d8/cpp/mge/mgeversion.h` (path from the repo root),
  which `mgeHost64/src/abi/constants.rs` asserts at test time.
- All DDS outputs use the legacy D3D9 header (non-DX10) written by
  [crates/texture/src/dds.rs](../../crates/texture/src/dds.rs) `write_legacy_dds_header`, since
  MGE-XE is a D3D9 runtime.
  Static atlas pages are BC1 (opaque) / BC3 (alpha) with full mips; terrain outputs are
  described in [terrain.md](terrain.md). Terrain DDS surfaces are written top-down
  ("unflipped").
