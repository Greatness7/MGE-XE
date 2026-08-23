# `terrain.bin` binary format

This is the byte-level contract shared by the distant-land generator,
the 32-bit runtime loader, and the 64-bit host. It is the Stage 1 contract for the
runtime-side terrain ABI.

## Constants

| Name | Value |
| --- | --- |
| magic | `XELAND02` |
| version | `2` |
| fixed header size | `116` bytes |
| mesh header size | `48` bytes |
| vertex stride | `20` bytes |
| file index format | `2` = per-mesh little-endian `u16[3]` or `u32[3]` triangles, inferred from `vertex_count` |

## External required files

The runtime loads `terrain.bin` alongside these terrain textures:

1. `terrain_atlas.dds`
2. `terrain_material.dds`
3. `terrain_material_flags.dds`
4. `terrain_patch_albedo.dds`
5. `terrain_blend_patterns.dds`

## Endianness and scalar rules

- All scalar fields in `terrain.bin` are little-endian.
- The runtime/parser must read and validate fields one scalar at a time; it must not dump native structs directly to or from disk.
- `BoundingSphere` on disk is radius, then center.xyz.
- `BoundingBox` on disk is min.xyz, then max.xyz.
- There is no serialized `header_size` field. The contract is a fixed 116-byte header prefix followed immediately by `mesh_count` mesh payloads.

## Header byte layout

Offsets are relative to the beginning of `terrain.bin`.

| Offset | Size | Type | Field |
| --- | ---: | --- | --- |
| 0 | 8 | `u8[8]` | `magic` (`XELAND02`) |
| 8 | 4 | `u32` | `version` |
| 12 | 4 | `f32` | `cell_size` |
| 16 | 4 | `f32` | `patch_size` |
| 20 | 4 | `i32` | `origin_cell_x` |
| 24 | 4 | `i32` | `origin_cell_y` |
| 28 | 4 | `u32` | `cell_size_x` |
| 32 | 4 | `u32` | `cell_size_y` |
| 36 | 4 | `f32` | `world_origin_x` |
| 40 | 4 | `f32` | `world_origin_y` |
| 44 | 4 | `f32` | `world_size_x` |
| 48 | 4 | `f32` | `world_size_y` |
| 52 | 4 | `u32` | `atlas_size` |
| 56 | 4 | `u32` | `logical_tile_size` |
| 60 | 4 | `u32` | `gutter_size` |
| 64 | 4 | `u32` | `physical_tile_size` |
| 68 | 4 | `u32` | `tiles_per_row` |
| 72 | 4 | `u32` | `atlas_max_lod` |
| 76 | 4 | `u32` | `material_size_x` |
| 80 | 4 | `u32` | `material_size_y` |
| 84 | 4 | `u32` | `pattern_count` |
| 88 | 4 | `u32` | `pattern_tile_size` |
| 92 | 4 | `u32` | `pattern_gutter_size` |
| 96 | 4 | `u32` | `pattern_physical_size` |
| 100 | 4 | `u32` | `patterns_per_row` |
| 104 | 4 | `u32` | `vertex_stride` |
| 108 | 4 | `u32` | `file_index_format` |
| 112 | 4 | `u32` | `mesh_count` |

## Mesh payload layout

Each mesh payload uses this exact layout:

### Mesh header (`48` bytes)

Offsets are relative to the beginning of one mesh payload.

| Offset | Size | Type | Field |
| --- | ---: | --- | --- |
| 0 | 4 | `f32` | `bounding_sphere_radius` |
| 4 | 4 | `f32` | `bounding_sphere_center_x` |
| 8 | 4 | `f32` | `bounding_sphere_center_y` |
| 12 | 4 | `f32` | `bounding_sphere_center_z` |
| 16 | 4 | `f32` | `bounding_box_min_x` |
| 20 | 4 | `f32` | `bounding_box_min_y` |
| 24 | 4 | `f32` | `bounding_box_min_z` |
| 28 | 4 | `f32` | `bounding_box_max_x` |
| 32 | 4 | `f32` | `bounding_box_max_y` |
| 36 | 4 | `f32` | `bounding_box_max_z` |
| 40 | 4 | `u32` | `vertex_count` |
| 44 | 4 | `u32` | `triangle_count` |

### Vertex payload

Immediately after the 48-byte mesh header:

`vertex_count * 20` bytes of tightly packed vertices.

| Vertex offset | Size | Type | Field |
| --- | ---: | --- | --- |
| 0 | 12 | `f32[3]` | `position.xyz` |
| 12 | 4 | `u8[4]` | `normal` (`UBYTE4N` bias-encoded) |
| 16 | 4 | `u32` | `color` (`D3DCOLOR`, logical `0xAARRGGBB`) |

Color byte rule:

- logical D3DCOLOR value: `0xAARRGGBB`
- raw little-endian vertex-buffer bytes: `[BB, GG, RR, AA]`

### Triangle payload

Immediately after the vertex payload, the file stores tightly packed
little-endian triangles. The index width is selected independently for each
mesh from its vertex count:

- when `vertex_count <= 65535`, `triangle_count * 6` bytes of
  `[u16 i0][u16 i1][u16 i2]`;
- otherwise, `triangle_count * 12` bytes of
  `[u32 i0][u32 i1][u32 i2]`.

The runtime validates the indices at the serialized width and copies them
verbatim into a matching 16-bit or 32-bit D3D index buffer.

## Validation rules

The Stage 1 parser and note agree on these checks:

1. `magic == XELAND02`
2. `version == 2`
3. `vertex_stride == 20`
4. `file_index_format == 2`
5. `cell_size` and `patch_size` are finite and positive
6. `cell_size / patch_size` is an integer patch-grid size
7. `world_origin == origin_cell * cell_size`
8. `world_size == cell_size_xy * cell_size`
9. `material_size_xy == cell_size_xy * (cell_size / patch_size)`
10. `physical_tile_size == logical_tile_size + 2 * gutter_size`
11. `tiles_per_row * physical_tile_size <= atlas_size`
12. `logical_tile_size` is one of `64`, `128`, `256`, `512`
13. `atlas_max_lod` matches tile size: `64 -> 0`, `128 -> 1`, `256 -> 2`, `512 -> 3`
14. `pattern_count` is `1..=256`
15. `pattern_physical_size == pattern_tile_size + 2 * pattern_gutter_size`
16. each mesh payload must fit exactly in the file; trailing bytes are invalid

## Texture-side byte contracts referenced by the runtime

- `terrain_material.dds`: `A8B8G8R8`, `R/G = base_id lo/hi`, `B/A = decal_id lo/hi`
- `terrain_material_flags.dds`: `A8B8G8R8`, `R = pattern_id`, `G = flags`, `B = 0`, `A = 255`
- `terrain_blend_patterns.dds`: `A8B8G8R8`, alpha/pattern value written to `R`, sampled from `.r`
- `terrain_patch_albedo.dds`: `DXT1`, one texel per 512-unit patch plus mips
- `terrain_atlas.dds`: DXT1 source terrain atlas; the deepest stored/sampled mip is `atlas_max_lod`

## Runtime-facing D3D9 declaration

```cpp
const D3DVERTEXELEMENT9 TerrainElem[] = {
    {0,  0, D3DDECLTYPE_FLOAT3,  D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
    {0, 12, D3DDECLTYPE_UBYTE4N, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_NORMAL,   0},
    {0, 16, D3DDECLTYPE_D3DCOLOR,D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR,    0},
    D3DDECL_END()
};
```
