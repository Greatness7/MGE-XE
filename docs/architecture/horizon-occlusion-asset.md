# Terrain occlusion asset (`terrain_occlusion.bin`)

`terrain_occlusion.bin` is the generated terrain horizon-occluder asset consumed by
`mgeHost64` when terrain horizon culling is enabled. It replaces the expensive startup path that
derives the base max-height grid by parsing `terrain.bin` and scanning every terrain mesh vertex.
The host builds the runtime max-height pyramid from the base grid.

The asset is the only production source of the occluder height field. A missing, stale,
incompatible, or corrupt asset never fails distant-land loading; the host logs the rejection, leaves
the height field cleared so horizon culling self-no-ops, and regenerating distant land restores it.
Deriving the grid from `terrain.bin` survives only as a test oracle.

## 1. Runtime role

The asset loads into the existing `TerrainHeightField` shape:

```text
TerrainHeightField
  origin, size
  spacing
  nx, ny
  max_z[]              level 0 base grid
  covered_cells
  global_max_z
  levels[]             2x2 max-reduction pyramid, excluding base
```

Only the load source changes. The horizon table, hierarchical builder, worker, adaptive gate,
quadtree traversal, and object tests are unchanged.

## 2. File inventory and contract

The generator writes:

```text
Data Files\distantland\terrain_occlusion.bin
```

The file is part of the required `distantland` output contract. `check_output_status` treats an
output tree without it as incomplete, which causes one automatic regeneration for users with startup
generation enabled after upgrading.

The asset has its own version interlock. A format bump rejects only this asset and invalidates
the generator's terrain-stage fingerprint (the occlusion version feeds the terrain package
fingerprint); it does not require an `MGE_DL_VERSION` bump.

## 3. Source semantics

The generator builds level 0 from decoded LAND height grids for populated
exterior cells:

```rust
x = cell_x as f32 * 8192.0 + i as f32 * 128.0
y = cell_y as f32 * 8192.0 + j as f32 * 128.0
z = heights[j][i]
```

The source is not the simplified distant terrain mesh. This is intentional:

- the game renders nearby active-grid terrain from native LAND heights, and nearby ridges
  dominate the horizon;
- future terrain LOD work may change simplified mesh vertices, while LAND heights are stable;
- LAND coverage is denser than the decimated mesh and preserves true ridge peaks.

The asset can therefore cull slightly more than the test-only derivation from simplified
`terrain.bin` vertices. The direction is conservative at default settings: the asset grid is greater than or equal to
the derived grid at matching LAND samples, and the default `Horizon Height Bias` of 512
world units lowers the occluder by more than the highest current mesh simplification target error
(`Low` detail: 256 units). Bias 0 with low terrain detail is the least conservative configuration.

At cell seams, the asset bins both cells' border rows and keeps the maximum. Missing cells inside
otherwise populated regions are left uncovered (`f32::MIN` sentinel), which can only lower the
horizon and lose culling. The generator includes uniform default cells at their uniform height.

## 4. Binary format v2

All values are little-endian. The file has a fixed 56-byte header followed by the raw `f32` base-grid
payload. There is no padding or offset table.

| Offset | Size | Field | Type | Host validation |
| ---: | ---: | --- | --- | --- |
| 0 | 8 | `magic` | `[u8; 8]` | `XEOCCL02` |
| 8 | 4 | `version` | `u32` | `2` |
| 12 | 8 | `origin_cell` | `[i32; 2]` | exactly equals `terrain.bin` |
| 20 | 8 | `cell_size_xy` | `[u32; 2]` | exactly equals `terrain.bin` |
| 28 | 8 | `world_origin` | `[f32; 2]` | finite, exactly equals `terrain.bin` |
| 36 | 8 | `world_size` | `[f32; 2]` | finite, positive, exactly equals `terrain.bin` |
| 44 | 4 | `base_spacing` | `f32` | finite, positive; fixed at 512 by the generator |
| 48 | 4 | `base_nx` | `u32` | `ceil(world_size.x / base_spacing) + 1` |
| 52 | 4 | `base_ny` | `u32` | `ceil(world_size.y / base_spacing) + 1` |
| 56 | ... | base payload | `f32[]` | row-major, exact total size |

Missing cells use `f32::MIN`, matching host `EMPTY_HEIGHT`. That sentinel is finite and legal. The
host rejects only NaN and infinities during the base-height scan. The payload maps directly to
`TerrainHeightField::max_z`; the host derives `TerrainHeightField::levels` from it.

## 5. Builder parity rules

The generator mirrors the host's existing runtime math:

```rust
nx = (world_size.x / spacing).ceil() as u32 + 1;
ny = (world_size.y / spacing).ceil() as u32 + 1;

ix = ((x - origin.x) / spacing).floor().clamp(0.0, (nx - 1) as f32) as u32;
iy = ((y - origin.y) / spacing).floor().clamp(0.0, (ny - 1) as f32) as u32;
max_z[iy * nx + ix] = max(max_z[iy * nx + ix], z);
```

LAND coordinates are exact multiples of powers of two (`8192` and `128`), so the generator and the
runtime dense-vertex path produce identical `f32` coordinates at LAND sample points.

## 6. Host validation and degradation

The host parser (`mgeHost64/src/abi/occlusion.rs`) performs structural validation and copies the base
grid into owned `Vec<f32>` storage. It never casts the file byte buffer to `f32` slices.

`TerrainHeightField::from_occlusion` then:

1. cross-checks the asset header against the paired `terrain.bin` header;
2. scans the base grid for NaN/infinity, recomputing `covered_cells` and `global_max_z`;
3. builds the mip pyramid from the base grid with the existing runtime `build_pyramid`.

Failure matrix (all outcomes leave the height field cleared so horizon culling self-no-ops, and
distant-land loading continues):

| Condition | Outcome |
| --- | --- |
| file missing | `warn!`; field stays cleared |
| bad magic/version/dims/size/trailing bytes | `warn!`; field stays cleared |
| asset header differs from `terrain.bin` | `warn!`; field stays cleared |
| NaN or infinity in the base grid | `warn!`; field stays cleared |
| valid asset | use asset; log load timing and field summary |

## 7. Source map

| Area | Source |
| --- | --- |
| generator builder/serializer/tests | `distantland/crates/formats/src/terrain_occlusion.rs` |
| output contract path | `distantland/crates/foundation/src/output.rs` |
| generation stage write | `distantland/src/generation/terrain_stage.rs` |
| host parser/tests | `mgeHost64/src/abi/occlusion.rs` |
| host field constructor/tests | `mgeHost64/src/state/horizon/height_field.rs` |
| host load/degradation path | `mgeHost64/src/state/loading.rs` |

## 8. Tests

Generator tests cover:

- exact fixture bytes and byte-for-byte round trip;
- LAND-height binning, boundary clamping, missing-cell sentinels, seam max behavior, and uniform
  default-style cells;
- rejection of bad magic/version, truncation, trailing bytes, and dimension mismatch.

Host tests cover:

- header size and offsets;
- fixture parsing;
- parser rejections for bad magic/version, dimension mismatch, truncation, trailing bytes, and
  non-finite spacing/size;
- asset-derived and test-oracle-derived field bit equality on undecimated one-cell and two-cell LAND
  fixtures, including linear and hierarchical horizon-table equality;
- rejection of terrain-header mismatch and NaN heights;
- acceptance of a sentinel-only asset with zero covered cells;
- degradation to an inactive height field with `Ok` when the asset is absent or invalid.

In-game release QA should regenerate distant land, confirm `terrain.host64_occlusion_load` appears
in the host log, corrupt/delete the asset to verify horizon culling degrades cleanly without failing
the load, and spot-check known ridgeline routes for pop-in.
