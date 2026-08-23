# Terrain horizon culling

This document is the architecture and implementation reference for MGE XE terrain horizon
culling. It covers the generated terrain occlusion asset that supplies the occluder height field,
hierarchical horizon construction, asynchronous rebuilds, adaptive self-suspension, quadtree
integration, generated static footprints, configuration, and validation results.

When this document and the implementation differ, source code and ABI layout assertions take
precedence.

## 1. Status and scope

The 64-bit host implements terrain horizon culling for distant static visibility queries. The feature
defaults to enabled, as do its adaptive gate and hierarchical builder. The host's own
`Configuration::default` leaves culling off, but that applies only when no `mgeXE.toml` is present;
the shipped schema default is `distant_land.horizon.culling = true`.

The current implementation:

- loads a generated LAND-source terrain max-height grid from `terrain_occlusion.bin` at distant-land
  load time; a missing or invalid asset leaves the height field cleared so culling self-no-ops;
- builds a camera-relative, distance-layered polar horizon table;
- accelerates table construction with a max-height pyramid while retaining a bit-identical linear
  implementation as the permanent test oracle and escape hatch;
- rebuilds moved-eye tables asynchronously when possible and bounds the use of stale tables;
- applies horizon rejection to Near, Far, and VeryFar static quadtrees before the host returns
  `RenderMesh` records to the 32-bit renderer;
- prunes whole quadtree nodes using conservative XY bounds and maximum height;
- tests individual meshes with a cheap capped-disc path, a cheap early-accept path, and a precise
  convex-footprint fallback;
- consumes generator-emitted static footprints when their instance transform is safe, otherwise
  derives conservative bounds from the runtime oriented box;
- suspends itself in low-benefit and sustained-fast-motion regimes; and
- exposes live tuning through the existing 32-bit runtime and MWSE options menu.

The implementation does not use GPU occlusion queries, DXVK-specific behavior, same-frame GPU
readback, or general building-to-building occlusion. Terrain is the only occluder.

## 2. Goals and non-goals

### Goals

- Reject distant statics only when terrain provably hides them before draw submission.
- Prefer false negatives (drawing a hidden object) over false positives (hiding a visible object).
- Remove entire quadtree subtrees when conservative node bounds prove them hidden.
- Keep camera movement rebuild work off the blocking IPC path whenever possible.
- Bound the visual risk of stale data and fail open whenever required data or bounds are invalid.
- Keep the feature live-tunable and easy to disable.
- Preserve an exact linear reference path for correctness testing.

### Non-goals

- General-purpose occlusion culling between arbitrary meshes or buildings.
- Horizon culling for terrain, grass, water, sky, or shadow maps.
- A DXVK/Vulkan Hi-Z implementation.
- Aggressive inference from incomplete, degenerate, or invalid bounds.
- Treating fewer rendered meshes as sufficient evidence of a performance win.

## 3. End-to-end architecture

```text
distantland output
  terrain.bin ------------------------------+
  terrain_occlusion.bin -------------------+|
  generated static HorizonFootprint -------+|
                                            ||
32-bit d3d8.dll                             ||       64-bit mgeHost64
  renderexterior.cpp                        ||
  cullDistantStatics                        ||
  SetHorizonConfig -------------------------||-----> Configuration / live config
  GetVisibleMeshes(camera, frustum, flags) -||-----> prepare_horizon
                                            ||         |
                                            ||         +-> TerrainHeightField
                                            ||         +-> HorizonBuilder worker
                                            ||         +-> cached HorizonTable
                                            ||
                                            |+-------> static loading / HorizonMeshBounds
                                            |
                                            +--------> quadtree traversal
                                                       node rect prune
                                                       mesh disc cull
                                                       mesh early accept
                                                       mesh footprint fallback
                                                          |
                                                          v
                                                   filtered RenderMesh set
                                                          |
                                                          v
                                                   32-bit renderer
```

The 32-bit client blocks on the host RPC result for each visibility query. Quadtree traversal is
therefore on the critical path. Horizon table construction normally is not. Moved-eye rebuilds run
on a dedicated host worker, and traversal continues with the previous table while the new one is in
flight.

The table is omnidirectional. Camera rotation does not invalidate it; only eye translation,
configuration changes, terrain/world changes, or loss of valid terrain coverage do.

## 4. Runtime data model

### 4.1 Terrain occluder: `TerrainHeightField`

`mgeHost64/src/state/horizon.rs` owns the terrain occluder.

```text
TerrainHeightField
  origin, size
  spacing
  nx, ny
  max_z[]              base max-height grid
  covered_cells
  global_max_z
  levels[]             2x2 max-reduction pyramid
```

At landscape load, the host already reads and parses `Data Files\distantland\terrain.bin`. When
configuration enables horizon culling, the host loads
`Data Files\distantland\terrain_occlusion.bin`, which supplies the base max-height grid directly and
the host rebuilds the mip pyramid from it. The asset is required: if it is missing, stale, or
invalid, the host logs a warning, leaves the field cleared so horizon culling self-no-ops, and
distant-land loading continues; regenerating distant land restores the asset. Deriving the grid from
`terrain.bin` survives only as a test oracle.

The generator builds the asset base grid from decoded LAND heights, not the simplified
render mesh. It is bit-identical to the test-only derivation on undecimated fixtures. In production
it can be slightly higher because mesh simplification drops vertices, so asset mode may cull the
same or slightly more candidates. The default `Horizon Height Bias` keeps the drawn distant mesh
conservative for current terrain detail presets. Missing asset coverage fails open. Empty cells use
an internal sentinel and never contribute a terrain sample.

The asset's serialized `base_spacing` fixes the grid spacing at 512 world units. It remains
independent of the horizon ray-march step. This distinction is a correctness requirement:

- coarsening the march step samples the same fixed grid less often and can only miss occluders,
  which loses culling but cannot invent a higher horizon;
- coarsening the grid attributes a cell maximum to a larger area and can inflate the horizon,
  which can over-cull visible geometry.

`sample_max_z(x, y)` returns the maximum of the 2x2 base-cell block around the sample anchor. The
hierarchical builder accounts for that footprint when constructing upper bounds.

### 4.2 Max-height pyramid

The host builds an immutable max-height pyramid immediately after the base grid:

```text
level_n[x, y] = max(valid children in level_(n-1)[2x..2x+1, 2y..2y+1])
```

The reduction handles odd dimensions and continues the chain to 1x1. A coarse cell is always an
upper bound for every populated base cell it covers. The host uses the pyramid only to prove that a
range of ray samples cannot raise the running horizon. It never supplies an approximate replacement
sample.

The hierarchical builder therefore remains exactly equivalent to the linear builder. Visual
similarity alone is insufficient.

### 4.3 Camera-relative `HorizonTable`

The horizon is a polar table indexed by azimuth bin and distance ring:

```text
HorizonTable
  eye
  bin_count
  ring_count
  ring_step
  r_near
  bias_z
  bias_obj_z
  max_slope[bin][ring]
```

Each entry is a prefix maximum of terrain elevation slope:

```text
slope = (terrain_z - terrain_bias - eye_z) / horizontal_distance
```

A ring therefore represents the highest terrain horizon observed along a bin from the near exclusion
radius through that ring. The implementation caps the ring count at 32. If the requested range would
exceed that count, it enlarges the effective ring step to preserve the range.

### 4.4 Static occludee bounds

Every quadtree mesh stores:

```text
HorizonMeshBounds
  max_z
  footprint_xy[0..vertex_count]    convex polygon, 3..6 vertices
  footprint_center
  footprint_radius                 minimum enclosing circle
```

The host derives the ordinary fallback from the world-space oriented bounding box. Projecting the
eight box corners onto XY and taking their convex hull produces at most six vertices. The host marks
degenerate point or line projections unusable and fails open.

The minimum enclosing circle supports the cheap cull and early-accept stages. The true maximum Z
prevents a wide, low merged mesh from inheriting the unrealistically high apex of its 3D bounding
sphere.

### 4.5 Generated static footprints

The distant-land generator can emit one optional subset-local footprint:

```text
HorizonFootprint (56 bytes, repr(C))
  max_z: f32
  vertex_count: u8
  padding[3]
  footprint_xy[6][2]: f32
```

`DistantSubset` embeds it. C++ and Rust mirror the struct, and layout assertions protect it.
The host accepts generated footprints only for non-building subsets that use translation-only,
unit-scale transforms. Rotation, scale, malformed polygons, non-finite values, or degenerate area
cause the host to fall back to conservative OBB-derived bounds.

Calculating the minimum circle in subset-local coordinates before translation avoids floating-point
cancellation far from the world origin.

## 5. Horizon construction

### 5.1 Linear reference algorithm

For each azimuth bin, the linear path evaluates the exact radial sequence
`sample_start + i * march_step`, updates a running maximum slope, and writes that prefix maximum to
the corresponding ring.

```text
for each bin:
  running = -infinity
  for each radial sample i:
    r = sample_start + i * march_step
    if terrain sample exists:
      running = max(running, (terrain_z - bias_z - eye_z) / r)
    table[bin][ring(r)] = max(table[bin][ring(r)], running)
  prefix-fill any later ring slots
```

The implementation retains the linear path permanently as:

- the exact behavioral oracle for hierarchical tests; and
- a load-time escape hatch through `Horizon Hierarchical March=False`.

### 5.2 Hierarchical algorithm

The hierarchical path recursively examines contiguous ranges of the same sample indices used by the
linear path. For a range `[lo, hi)`, it computes a conservative upper bound on any slope in that
range:

1. Convert the first and last radial samples to a world-space segment.
2. Pad its XY AABB enough to cover every base sample's 2x2 read footprint.
3. Select a max-height pyramid level that covers the segment with a small number of cells.
4. Query the maximum terrain height over that padded AABB.
5. Convert that maximum to a slope bound using the nearest radius when the height delta is positive,
   or the farthest radius when it is negative.

If the upper bound is no greater than the running horizon, no sample in the segment can change the
output, so the builder skips the entire range. Otherwise, it splits the range. It directly evaluates
ranges of eight samples or fewer.

The safety invariant is:

```text
hierarchical max_slope vector == linear max_slope vector, bit for bit
```

Randomized terrain/eye/parameter tests and fixed ridge/bias fixtures enforce this invariant.

### 5.3 Why the table is distance-layered

A single horizon per azimuth would allow terrain behind an object to hide it. Rings avoid that error.
Object tests use only a fully completed ring strictly nearer than the object's nearest footprint
point. Terrain sampled at or behind the object cannot contribute to that ring.

## 6. Cache, asynchronous rebuilds, and invalidation

### 6.1 Cache matching

A cached table is reusable when:

- its horizon parameters match; and
- its eye remains within `Horizon Rebuild Eye Threshold` of the current eye.

The default threshold is 16 world units. Because the table covers 360 degrees, yaw and pitch do not
participate in the cache key.

### 6.2 Worker design

The host spawns `HorizonBuilder` lazily. It contains:

- a mutex/condition-variable mailbox with one replaceable request slot;
- an `ArcSwapOption` result slot; and
- one named `horizon-builder` thread.

Posting overwrites the pending mailbox request, so movement coalesces toward the newest eye instead
of creating an unbounded queue. The worker performs only CPU work on immutable shared terrain data,
publishes the finished table, and returns to the mailbox.

A result carries both a structural generation and a request ID. The host always discards
wrong-generation results. It may still adopt a superseded request ID when its table matches the
current eye and parameters. Without this rule, the host would discard a one-frame-late but spatially
current result and post another request. Adoption never replaces a valid table or makes the current
cache farther from the current eye.

### 6.3 Synchronous fallback

On a cache miss, the host builds synchronously only for these reasons, in precedence order:

| Reason | Trigger |
| --- | --- |
| `ForceSync` | test-only override |
| `ColdOrParamChange` | no usable table, or parameters changed |
| `StaleBeyondCap` | current table eye is more than 64 units from the current eye |
| `WorkerStarved` | an async request has remained outstanding for 8 prepare frames |
| `BuilderUnavailable` | the worker could not be spawned |

Otherwise, the host posts a new request and keeps the stale table active. The 64-unit stale cap
bounds spatial error; the 8-frame pending cap bounds a stalled worker.

### 6.4 Structural invalidation

Terrain replacement, world changes, disabling the feature, and horizon parameter changes invalidate
the horizon epoch and cached table. A live parameter-only update keeps the existing height field,
because march parameters do not alter the load-time grid.

Changing `Horizon Rebuild Eye Threshold` or `Horizon Hierarchical March`
requires a restart/reload because those settings are host load-time controls and are not in the live
IPC payload.

## 7. Adaptive self-suspension gate

Horizon traversal has a fixed CPU/IPC cost even when it removes little geometry. The adaptive gate
turns the feature into a scene- and movement-sensitive optimization rather than always paying that
cost.

### 7.1 States

| State code | State | Behavior |
| ---: | --- | --- |
| 0 | Inactive | no valid horizon context |
| 1 | `Active { probing }` | horizon traversal enabled; `probing` distinguishes a resume-benefit window |
| 2 | Warming | one asynchronous table is requested; no stale table is exposed |
| 3 | SuspendedLowBenefit | no build and no horizon traversal |
| 4 | SuspendedFast | no build and no horizon traversal during sustained fast motion |

The gate therefore has five internal states. Ordinary operation uses `Active { probing: false }`;
adopting a warm table uses `Active { probing: true }` until that probe window receives a verdict.
Both forms expose active mode and state code 1. Disabling the adaptive gate leaves horizon culling
continuously active and also reports code 1.

### 7.2 Low-benefit policy

An evaluation window is 120 rendered frames. A window is beneficial when either:

```text
average meshes culled per frame >= 16
or
average quadtree nodes pruned per frame >= 8
```

Ordinary active mode requires two consecutive low-benefit windows before suspension. A resume probe
uses `Active { probing: true }`. One beneficial window clears `probing`, while one failed window
returns immediately to suspension.

Suspended low-benefit mode probes again after 2 seconds, with exponential backoff up to 16 seconds.
Accumulating 512 units of eye movement triggers an immediate probe and resets the backoff.

### 7.3 Fast-motion policy

The gate tracks the last 64 per-frame eye displacements. A displacement greater than 64 units is a
fast sample.

- enter `SuspendedFast` when at least 32 of 64 samples are fast;
- leave it when at most 8 of 64 samples are fast;
- leave through Warming so a fresh table is built before culling resumes.

This avoids repeated synchronous stale-cap builds during flight or scripted high-speed movement.

### 7.4 Warming

Warming posts exactly one asynchronous build and never falls back to a synchronous build. On
adoption, the gate enters `Active { probing: true }`. If the gate cannot adopt a table within 32
frames, it returns to low-benefit suspension with increased probe backoff.

While horizon culling is enabled, the gate ticks once at the real render-frame boundary, after the
main static query and any water reflection query. The host accumulates per-query counters so one
rendered frame counts only once.

### 7.5 Known policy limitation

Pure rotation while low-benefit suspended does not trigger a movement probe. Turning from an open
view toward an occluded view can therefore leave culling suspended until the next timer probe, up to
the 16-second backoff cap. This loses potential performance but cannot hide visible geometry.

An isolated movement burst can also make the single warm result too stale to adopt. That causes one
timeout and one backoff increase without a benefit measurement. Sustained movement cannot compound
the increase because 512 units of movement forces a new probe and resets the backoff.

## 8. Quadtree traversal and rejection tests

### 8.1 Scope

The host applies a prepared horizon to the static buckets that these flags select:

- `VIS_NEAR`
- `VIS_FAR`
- `VIS_VERY_FAR`

It does not apply horizon culling to:

- `VIS_GRASS`
- `VIS_LAND`

Terrain is the occluder, not an occludee. Grass remains outside the feature scope.

### 8.2 Node traversal order

For each quadtree node:

1. Test its bounding sphere against the frustum.
2. Test its sphere against the query distance limit.
3. If a horizon is present, test the node's conservative XY rectangle and maximum Z.
4. Prune the entire subtree when that rectangle is hidden.
5. Recurse into children.
6. Process leaf meshes.

The quadtree volume calculation recomputes node `max_z`, `xy_min`, and `xy_max` bottom-up with the
node sphere. Each mesh sphere's XY extent supplies leaf XY bounds. These bounds may be loose but
cannot exclude mesh geometry.

The implementation has no node-level early accept. Accepting a whole subtree would require a proof
that every member is visible and would add complexity for limited expected benefit.

### 8.3 Mesh test order

After the existing enabled, frustum, OBB refinement, and distance tests, the horizon path is:

1. Capped-disc cull. Test the minimum enclosing XY circle using the geometry's true `max_z`.
2. Capped-disc early accept. Prove the object's top is above the highest possible horizon across
   its span; if so, skip the precise footprint test.
3. Convex-footprint fallback. Test the real projected polygon and top height.
4. Emit the mesh only when none of the cull stages proves it hidden.

The cheap circle over-covers the true footprint. A successful cull is therefore a subset of what the
precise hull test would cull. By design, a successful early accept is more pessimistic than the hull
test. These stages change cost, not the visible result.

## 9. Correctness contract

These rules are non-negotiable.

### 9.1 Conservative terrain and object slopes

- Subtracting terrain height bias from the terrain sample lowers the horizon.
- Adding object bias to object top height raises the object.
- An object above the eye uses its nearest footprint distance for the highest possible top slope.
- An object below the eye uses its farthest footprint distance, because that is the least-negative
  and therefore highest possible top slope.
- A cull requires the object top to be below the minimum horizon over every covered azimuth bin.

### 9.2 Only complete nearer rings may cull

The object test selects the last complete ring before the nearest point of the object. Objects inside
the first complete ring cannot be horizon-culled. Terrain at or behind the object cannot participate.

### 9.3 Fail open

The host returns visible, not culled, when any required input is invalid or inconclusive, including:

- missing height field or cached table;
- eye outside terrain coverage;
- non-finite bounds or parameters;
- degenerate footprints;
- eye inside the footprint/disc;
- incomplete nearer ring;
- ambiguous angular coverage; or
- rejected generated footprint transforms.

### 9.4 Fixed-grid tuning distinction

`Horizon Sample Spacing` is safe to coarsen only because the asset's occluder grid is independent and
fixed. Do not couple these settings. A coarse march can under-cull; a coarse max-height grid can
over-cull.

### 9.5 Hierarchical exactness

A pyramid skip is legal only when its conservative upper bound proves that no exact linear sample in
the skipped index range can raise the running horizon. The hierarchical output must remain bitwise
equal to the linear output for the same field, eye, and parameters.

### 9.6 ABI synchronization

Mirror changes to `HorizonFootprint`, `DistantSubset`, the live IPC payload, or exposed parameter IDs
across generator, C++, Rust, Lua, layout tests, and generated-data compatibility versions as
applicable.

## 10. Configuration and adaptive defaults

### 10.1 Host configuration

All keys live under `[distant_land.horizon]` in game-root `mgeXE.toml`.

| Key | Default | Range | Live | Meaning |
| --- | ---: | ---: | :---: | --- |
| `culling` | true | bool | yes | master feature switch (`Terrain Horizon Culling`); the host's no-config fallback is `false` |
| `height_bias` | 512 | 0..32768 | yes | lowers terrain horizon |
| `object_bias` | 256 | 0..32768 | yes | raises tested object top |
| `near_exclude` | 2048 | 0..65536 | yes | terrain radius excluded from construction |
| `ring_step` | 4096 | 1..65536 | yes | radial output-ring width |
| `max_range` | 49152 | 1..1048576 | yes | maximum occluder distance |
| `azimuth_bins` | 512 | 64..4096 | yes | full-circle angular resolution |
| `sample_spacing` | 512 | 1..8192 | yes | exact radial sample step |
| `adaptive_gate` | true | bool | yes | self-suspend when unprofitable or too fast |
| `rebuild_eye_threshold` | 16 | 0..8192 | no | translation tolerated by cache |
| `hierarchical_march` | true | bool | no | pyramid builder; false selects linear oracle |

The live `SetHorizonConfig` payload contains fields through Adaptive Gate. The rebuild eye
threshold and hierarchical selection remain load-time host controls.

### 10.2 MWSE options integration

The MGE XE Options Lua module binds plain exports from `d3d8.dll` through LuaJIT FFI. Older DLLs that
do not expose them simply hide the controls.

Parameter IDs are:

| ID | Parameter | Access |
| ---: | --- | --- |
| 0 | culling enabled | read/write |
| 1 | terrain height bias | read/write |
| 2 | object bias | read/write |
| 3 | near exclude | read/write |
| 4 | ring step | read/write |
| 5 | max range | read/write |
| 6 | azimuth bins | read/write |
| 7 | sample spacing | read/write |
| 8 | adaptive gate | read/write |
| 9 | hierarchical march | read-only |

Both C++ and the host clamp runtime values. A dirty flag causes the render thread to push changed
values to the host on the next exterior frame.

### 10.3 Draw-distance adaptive policy

When the menu updates automatic render distances, it derives horizon defaults from draw distance:

```text
maxRange = min(drawDistanceCells * 0.75, 30.0) * 8192
ringStep = 4096 when maxRange <= 12 cells, otherwise 8192
bins = 512
budgetSpacing = maxRange * bins / 48000
sampleSpacing = min(1024, budgetSpacing) when hierarchical marching is enabled
sampleSpacing = budgetSpacing when the linear escape hatch is active
```

This policy preserves 512 bins for angular safety, caps useful horizon reach at 30 cells, and keeps
ring resolution at one cell or finer. The hierarchical speedup recovers near-field sampling at high
draw distances without forcing the linear builder into an expensive fine step.

Representative hierarchical defaults:

| Draw distance | Max range | Ring step | Sample spacing |
| ---: | ---: | ---: | ---: |
| 10 cells | 7.5 cells | 4096 | about 655 |
| 16 cells | 12 cells | 4096 | 1024 |
| 20 cells | 15 cells | 8192 | 1024 |
| 32 cells | 24 cells | 8192 | 1024 |
| 40 cells | 30 cells | 8192 | 1024 |

## 11. Diagnostics

The current implementation omits these development-time telemetry items: per-query counters returned
over IPC, Lua statistic exports, per-build and per-traversal log lines, rolling rebuild summaries,
and the benchmark harness. Only a small set remains:

- Internal counters. Traversal still counts culled meshes and pruned nodes per frame, but only
  host-side, to feed the adaptive gate's benefit windows. `HorizonCullStats` carries additional
  fields (candidates, early accepts, footprint-fallback tests) that unit tests assert as behavioral
  probes; nothing exports them.
- Host logging. One-time load summaries (generated-footprint acceptance counts; height-field source,
  dimensions, spacing, coverage, and pyramid size),
  adaptive-gate state transitions (emitted at `debug` level, which release builds compile out; only
  the gate-toggle log is `info`), and the worker-spawn failure warning. Nothing horizon-related
  logs per frame or per build.
- Test observability. `HorizonTable::build_with_stats` produces `MarchStats` (leaf samples, bound
  probes, skipped segments). Only tests that prove the hierarchical march does strictly less work
  than the linear oracle consume it.

When new tuning work needs richer measurement, re-add instrumentation locally rather than shipping
it. The removal was deliberate.

Adaptive transition logs expose the internal Debug values. Resume-probe transitions therefore use
`Active { probing: true }` and `Active { probing: false }` rather than the former `Evaluating` and
`Active` names; the external mode and numeric state code remain unchanged.

## 12. Validation and tuning evidence

This document omits the detailed session logs and handoffs but retains the decisions below because
they explain current constants and defaults.

Before its removal, a waypoint-playback harness advanced one recorded camera waypoint per rendered
frame for movement comparisons. That made culling counts spatially comparable across
configurations. Playback did not reproduce the route's wall-clock timing, so real-time runs validated
gate probe timing. When re-measuring, always compare both output work and frame-time behavior. A
configuration that culls more meshes can still lose performance through traversal cost, memory
contention, or synchronous rebuilds.

### 12.1 Angular and grid safety

- 256 azimuth bins produced visible angular-leakage pop-in. Production policy keeps 512.
- Coarsening the terrain grid inflated slopes and produced over-cull. Grid spacing remains fixed at
  512 and is not live-tunable.
- Coarsening only the march step on that fixed grid is conservative. It may miss peaks and draw more,
  but cannot raise the computed horizon.
- Ring steps at or below one terrain cell retained the useful nearby ridges in tested scenes; larger
  steps lost substantial culling.
- Useful occluder range saturated near 30 cells in the tested locations. This result motivated the
  adaptive cap.

### 12.2 Hierarchical builder and spacing cap

The hierarchical builder matched the linear output across fixed and randomized tests. At DD32, the
current budget-range configuration evaluated roughly 28% of the linear leaf samples. A 1024-unit
step evaluated roughly 18% while retaining almost all culling obtained at 512.

In a DD40 Balmora stationary comparison, the final 1024 cap rendered about 91 meshes at 185.8 FPS,
versus about 130 meshes at 167.2 FPS for the coarse 48k-budget step. A fixed 512 step rendered only
about two fewer meshes than 1024 but reduced motion-run P10 FPS in the measured route. The current
policy caps at 1024 under the hierarchical builder and retains budget spacing for the linear fallback.

These measurements are tuning evidence, not universal performance guarantees.

### 12.3 Adaptive gate

Measurements in open, dense, moving, and fast-motion regimes validate retaining both low-benefit
and fast-motion suspension:

- an open Tel Vos DD50 view reached full low-benefit suspension with zero precise horizon work and
  performance within ordinary noise of culling disabled;
- dense Sunad Mora remained active and retained roughly a 41% FPS improvement over disabled in the
  validated stationary comparison;
- a dense Balmora movement route stayed Active for the full run and improved average FPS by roughly
  16% and P10 by roughly 40% versus disabled;
- on a high-altitude flight path averaging about 410 units per frame, always-on culling was roughly
  12% slower than disabled, while the adaptive gate spent about 81% of the route in Fast suspension
  and recovered performance to within noise of disabled.

These measurements support the two policies, not every tuning constant. The 120-frame window,
two-window confirmation, 16-mesh / 8-node benefit floors, 32/8-of-64 motion hysteresis, 2-16 second
backoff, 512-unit movement probe, and 32-frame warm timeout remain tuning. The gate's goal is to avoid
cases where culling hurts performance, not guarantee a gain in every scene.

## 13. Testing contract

The Rust host test suite covers at least these invariants:

- terrain base-grid construction, empty cells, odd/even mip reduction, and conservative AABB maxima;
- `terrain_occlusion.bin` parsing, validation, and degradation to an inactive field on asset failure;
- asset-derived and test-oracle-derived field equality on undecimated LAND fixtures, including linear
  and hierarchical horizon-table equality;
- linear/hierarchical bitwise equivalence on deterministic and randomized cases;
- terrain behind an object cannot cull it;
- incomplete nearer rings cannot cull;
- height and object biases only shrink the culled set;
- below-eye distance handling;
- disc cull and early-accept equivalence relative to the footprint fallback;
- generated-footprint validation and far-origin translation stability;
- node max-Z and XY-bound containment;
- node-prune visible-output equivalence;
- cache reuse and parameter invalidation;
- stale-result generation/request acceptance rules;
- async worker lifecycle and starvation fallback;
- adaptive warming, suspension, probing, fast-motion entry/exit, and frame-boundary accumulation; and
- absence of horizon effects when data is unavailable or the feature is disabled.

ABI layout tests protect the mirrored protocol and render structs. Changes to generated footprints
also require generator serialization tests and end-to-end regeneration validation.

## 14. Source map

| Area | Primary source |
| --- | --- |
| terrain field, pyramid, table builders, object tests | `mgeHost64/src/state/horizon.rs` |
| terrain occlusion asset parser | `mgeHost64/src/abi/occlusion.rs` |
| cache, worker, and gate runtime | `mgeHost64/src/state/horizon/runtime.rs` (builder thread in `runtime/worker.rs`) |
| gate integration, traversal dispatch | `mgeHost64/src/state/distant_land.rs` |
| adaptive state machine | `mgeHost64/src/state/horizon/gate.rs` |
| node/mesh storage and traversal | `mgeHost64/src/state/quadtree.rs` |
| terrain/static loading and footprint acceptance | `mgeHost64/src/state/loading.rs` |
| host defaults and ranges | `mgeHost64/src/config.rs` |
| generator occlusion asset writer | `distantland/crates/formats/src/terrain_occlusion.rs` |
| Rust IPC payload | `mgeHost64/src/abi/protocol.rs` |
| Rust generated footprint ABI | `mgeHost64/src/abi/render.rs` |
| C++ shared ABI | `d3d8/cpp/ipc/bridge.h`, `d3d8/cpp/mge/dlformat.h` |
| live C++ FFI | `d3d8/cpp/mge/apiffi.cpp` |
| per-frame client integration | `d3d8/cpp/mge/distantland.cpp`, `renderexterior.cpp`, `renderwater.cpp`, and `d3d8/cpp/ipc/client.*` |
| MWSE options and adaptive defaults | `assets/Data Files/mwse/mods/MGE XE Options/gui.lua` |
| terrain byte format | [`terrain-bin.md`](terrain-bin.md) |
| serialized terrain occluder | [`horizon-occlusion-asset.md`](horizon-occlusion-asset.md) |

## 15. Known limitations

### 15.1 Terrain occlusion asset considerations

The terrain occluder comes from generated `terrain_occlusion.bin`; deriving it from `terrain.bin`
survives only as a test oracle for the generator. Remaining ideas are operational rather than
required for correctness:
memory-mapped validation for very large worlds, sparse or tiled payloads if full dense grids become
too large, and additional release telemetry around asset load time.

### 15.2 Release QA scenarios

The automated contracts are extensive, but these integration scenarios remain useful release QA:

- dense-to-open-to-dense transitions, including resume latency and hitching;
- a live GUI parameter change while the gate is enabled; and
- pure-rotation recovery from a long low-benefit probe backoff.

### 15.3 Partial-azimuth construction

Building less than 360 degrees could reduce worker work, but would make rotation part of the cache
key and complicate edge safety, guard bands, and gate resume behavior. The current full-circle table
excludes rotation from the cache key. Retain it unless measurement shows construction cost again
dominates.

### 15.4 Additional occludees

Grass, terrain, shadows, and general meshes remain outside scope. Any expansion requires a separate
correctness and performance case rather than reusing the static policy by assumption.

## 16. Why the implementation remains host-side

The host owns the information required for conservative rejection: static bucket identity,
world-space bounds, generated footprints, dynamic-visibility state, quadtree hierarchy, terrain data,
and the final visible `RenderMesh` set.

DXVK receives lower-level D3D9 draw calls after most of that context has been lost. A DXVK path would
need explicit metadata, proxy bounds, query or predicate lifetime management, and synchronization
policy. Same-frame query consumption risks stalls; previous-frame results risk lag; draw-call
fingerprinting is brittle. A GPU Hi-Z design may become worthwhile only if the host-side path proves
insufficient and MGE XE supplies the missing metadata.

## 17. Maintenance rules

- Treat this file as the single current horizon-culling design document.
- Update it when defaults, state-machine thresholds, ABI fields, or correctness invariants change.
- Keep the Rust configuration ranges authoritative and mirror them in C++ and Lua.
- Keep the linear builder and bit-equivalence tests while hierarchical marching ships.
- Do not remove fail-open behavior to gain culling without a new proof and regression tests.
- Do not infer a production default from one benchmark scene or from mesh counts alone.
- Keep generated-occluder format changes in
  [`horizon-occlusion-asset.md`](horizon-occlusion-asset.md).
