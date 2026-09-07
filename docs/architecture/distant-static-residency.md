# Merged-static VRAM residency

Merged distant statics are the largest generated payload — 2-8 GiB on a modded install, more than
the rest of the distant-land set combined. They are streamed in and out of VRAM under a runtime
cap instead of being uploaded whole at load time. Nothing else streams: ordinary (unmerged)
statics, static textures, UV-bound palettes, terrain geometry and terrain textures are uploaded
once and stay resident for the device session.

Data formats are in [distantland-data.md](distantland-data.md); the upload pump that precedes
residency is in [distantland-lifecycle.md](distantland-lifecycle.md); the transport is in
[ipc.md](ipc.md); the DXVK budget interface is in [dxvk-ppl-interop.md](dxvk-ppl-interop.md).

## File map

| Where | What |
| --- | --- |
| `d3d8/cpp/mge/distantstatics.cpp` | The whole client runtime: resource catalog, I/O worker, admission, eviction, ledger, transition instrumentation. |
| `d3d8/cpp/mge/distantinit.cpp` | Cap selection and resampling, the pump's drain/bootstrap phases, the per-frame `tickResidency`. |
| `d3d8/cpp/mge/distantland.h` | Budgets, radii, timeouts, and the cap state (lines ~123-141). |
| `mgeHost64/src/state/distant_land.rs` | Residency resource index, cell buckets, admission-order partition, `plan_residency`. |
| `mgeHost64/src/ipc/server.rs` | `update_residency`, `plan_residency` command handlers. |
| `mgeHost64/src/abi/protocol.rs` | `ResidencyPlan`, `ResidencyCommit`, `PlanResidencyParameters`. |

## The cap

One cap governs merged bytes for the device session. It is selected once fixed resources exist
(`selectInitialMergedStreamingCap`), from two sources in priority order:

1. `IDxvkMorrowindMemoryInterop1::GetDeviceLocalMemoryBudgetV1`, when our DXVK fork provides it.
2. Otherwise: an infinite cap.

The automatic candidate is `heap_budget - memory_used - headroom + logical_gpu_merged`. The merged
bytes are added back because `memory_used` already contains them; reducing this to `budget - used`
would let MGE's own admissions shrink its own cap. Headroom is 12.5% of the heap budget clamped to
256 MiB - 1 GiB — the clamp is on the headroom, not on the cap, which is a common misreading.

`heap_budget` is a live DXVK allocator-policy value, not card capacity. An 11 GB card reports about
9.94 GiB and the figure drifts as other GPU clients come and go, so the cap is resampled every
500 ms and ratcheted **downward only**, after four consecutive lower candidates. The fourth
candidate is applied, not the minimum of the window, so one transient spike cannot pin the session
low. A ratchet calls `wakeResidencyForCapDebt`.

The cap is intentionally not persistent configuration. A knob, if one is ever wanted, should be a
reserve ("leave N MB free for other applications") with the cap still derived, landed schema-first
in `mge-config`. A read-only display of the selected cap and current residency needs no schema
change and is what users actually ask for when diagnosing.

## Two schedules

`beginResidency` picks one and logs which:

- **`full_drain`** — the cap clears `merged_total`, or there is no budget interface. Every merged
  subset is uploaded in shard-file order through a budgeted mapped-window phase, the planner is
  disabled outright, and the session renders exactly as it did before streaming existed. This is
  the ordinary case on a roomy GPU.
- **`capped_bootstrap`** — the cap binds. The nearest ring is drained before `RenderReady`, then
  gameplay admission takes over. Bootstrap gives up after `kResidencyBootstrapTimeoutMs` (5 s)
  rather than hanging the load screen, entering with temporary pop-in instead.

A dataset with no merged geometry leaves residency idle in either case.

## Resource lifetime

```text
Unloaded -> IoQueued -> ReadyForGpu -> CommitPending -> Resident
                                                          |
                                            EvictQueued <-+
                                                  |
                                        RemovalInFlight -> Unloaded

Any creation or lock failure -> Unavailable (terminal for the session)
```

Interlocks that are not visible at the call site:

- **The client is the sole byte-ledger authority.** `logicalReservedMergedBytes` covers io-queued
  through removal-in-flight and is decremented only after `Release`. The host tracks spatial
  priority and committed resident state — no mirrored byte total, no admission reservation.
- **`Release` is forbidden until the removal RPC returns `Complete` and the host reports success.**
  On timeout, server loss, or host rejection the buffers, palette entry and ledger bytes stay in
  removal-in-flight; three failures freeze residency for the device session rather than risk
  handing a live host a stale pointer.
- **`update_residency` is all-or-none on both sides.** The client retains every buffer in a batch
  the host reports failed, so the host validates the whole batch before mutating anything
  (`DistantLand::validate_residency_commit` decides every error the apply path can report).
- **A readmitted resource leaves the removal batch before it is built.** A retry only reaches a
  resource whose earlier removal RPC did not complete, so the planner has had a chance to set
  `readmitRequested` in between. Those ids move straight to `CommitPending` and are never named in
  the `Unloaded` batch, so the host is never told a resource is gone while the client keeps its
  buffers.
- **D3D calls stay on Morrowind's main thread.** The I/O worker may read and reorder bytes; it may
  not create, lock, unlock, or release a D3D resource.
- **`staticUvBoundPalettes` shares its VB's lifetime** — insert after a streamed VB is created,
  erase before it is released. A palette miss on a non-null static VB is a counter and a debug
  failure, never a silently accepted identity palette.
- **Precise LOD traversal overwrites `render_mesh.faces` from `far_faces`/`very_far_faces` before
  its zero-face guard.** Residency must zero all three counts; clearing only `faces` leaves the far
  and very-far queries unsafe.
- **Device recreation is a full reset.** It cancels I/O, stops the host, releases every resource and
  rebuilds from scratch. No residency state survives `DistantLand::release()`.

## Admission and eviction

Residency **membership** is radial and never frustum-aware: which cells are eligible, and which
resident is evicted, depend only on distance. `ensure_residency_offsets` sorts cell offsets by
`(x*x + y*y, y, x)`, `admissionRadius` is `DrawDist * kCellSize + kCellSize`, and the retain radius
is one cell beyond it, giving the hysteresis that stops a boundary from thrashing.

Admission **order** within that set is camera-biased. `plan_residency` walks `planner_order` — a
mutable permutation of indices into `residency_offsets`, partitioned so that every cell in front of
the camera precedes every cell that is not — through `planner_offset_cursor`. Rotation therefore
changes which eligible candidate is admitted next, and can change which resident an admission
displaces, but it can never admit a cell outside the admission radius nor evict one inside the
retain radius. See [Camera-biased admission order](#camera-biased-admission-order) below.

When a candidate does not fit the client's headroom, `farthest_replaceable` picks the resident
resource furthest from the player to displace it. It searches `resident_streamable`, not
`residency_resources`: the latter carries one entry per subset — hundreds of thousands on a full
load order, nearly all ordinary statics that are never streamed — while the former is bounded by
the streaming byte cap. The planner then leaves its bucket cursor on the candidate the eviction
was made for, so the next sweep call admits it into the freed budget.

The planner is asked to run when the player's quantized cell changes, or when a cap ratchet leaves
debt. Each run bumps `residencyPlanEpoch`; the host keys its sweep cursor on the epoch as well as
the cell, so a save loaded into the cell the player already occupies restarts the sweep instead of
continuing the previous save's cursor.

Two boundaries do the work, both budgeted:

| Boundary | When | Budget |
| --- | --- | --- |
| Admission | end of frame, in `Present` | 2 ms / 2 MiB / 16 resources |
| Eviction | entry of `renderStage0`, after both persistent visible vectors are cleared | 1 ms / 2 resources |

Eviction has an end-of-frame fallback for menu and load frames that never reach stage0
(`stage0Complete` is captured before its per-frame reset). Both skip the tick outright while a
render RPC is outstanding, because every residency RPC begins with a blocking wait.

`kResidencyAdmitBudgetResources` was 2 and starved admission: the count budget bound every frame
against a byte allowance it never approached, and the far band never became resident at draw
distance 24. 16 is sized so a frame's admissions (~29 KB mean subset) still sit well inside the
2 MiB guard.

### Camera-biased admission order

The partition is absolute rather than weighted: a forward cell at maximum radius precedes a
non-forward cell one step away. That is what makes the ordering visible in game, and also why a
camera pointed opposite its travel direction pays the most for it.

- `residency_offsets` is canonical and stays radial. `planner_order` indexes into it and is always a
  permutation of `0..len` with `planner_order.len() == residency_offsets.len()`;
  `planner_order_scratch` is the reused stable-partition workspace.
- Heading arrives as `PlanResidencyParameters::view_heading_bin`: `0` means no valid hint, `1..=32`
  encode bins `0..=31`, and any other value is treated as `0`. C++ quantizes
  `atan2(eyeVec.y, eyeVec.x)` and sends `0` when the horizontal component is under epsilon, so
  looking straight up or down does not encode a false east heading.
- A heading change mid-sweep repartitions only `planner_order[min(cursor + 1, len)..]`. The current
  offset is pinned unconditionally, because `planner_bucket_cursor == 0` cannot distinguish an
  untouched cell from a candidate deliberately rolled back after an eviction, and moving an
  eviction-funded candidate wastes that eviction.
- Rotation must not repartition once `planner_offset_cursor >= len`. A parked planner has no pending
  tail, and this guard is what keeps a session whose cap does not bind from doing needless work.
- A new epoch or cell adopts the supplied heading (radial when it is zero) and rebuilds the whole
  order. A zero hint *within* an epoch keeps the last valid heading, so menu and non-Stage0 ticks do
  not discard gameplay order. These canonical rebuilds bound any radial segmentation left by
  repeated mid-sweep rotations; it never survives a cell crossing or an admitting rewind.
- `rebuild_residency_index` retains `residency_offsets`, so it must explicitly clear
  `planner_heading_bin` and restore `planner_order` to canonical radial indices.
- Bootstrap stays radial. `eyeVec` is not reliable before Stage0, so the hint is supplied only from
  end-of-frame planning with `stage0RanThisFrame` set.

Rejected shapes: a heap or balanced tree still needs every heading-dependent key updated; a
two-phase forward-then-all traversal cannot promote cells already swept into the consumed prefix
after a 180-degree turn; precomputed per-heading permutations need visited bookkeeping to switch
mid-sweep.

Measured 2026-09-03 on `fix/vram-usage` at a forced 320 MB cap, comparing two host builds that
differ only in whether the partition runs, over byte-identical position streams:

| Camera vs. travel | Evictions | Median frame time |
| --- | --- | --- |
| Aligned | -8.6% | +10.2% |
| 90 degrees | +12.4% | +4.1% |
| Opposed | +35.0% | -1.5% |
| Spinning, 30 deg/s | +12 to +20% | +1.9% |

Divergence cost is close to linear in angle, with break-even somewhere around 35-50 degrees. The
eviction rise is not thrash: a blind side-by-side A/B on the aligned and spinning routes picked the
biased build in all three pairs, and the spinning route has both the worst churn numbers and a clear
visual win. Resident bytes sit at the cap in every arm, so the frame-time cost is distant geometry
actually being drawn rather than planner overhead.

## Reading the logs

`mgeXE.log` carries the whole story; each launch truncates it, so archive it before relaunching.

- `Merged-static cap selected:` — source, cap, `merged_total`, and whether the cap binds.
- `Merged-static residency schedule:` — `full_drain` or `capped_bootstrap`.
- `Merged-static cap sample:` / `cap ratchet:` — the automatic cap tracking `memory_used`.
- `Distant static transition summary:` — one per epoch: trigger, destination, whether an admission
  landed and how many Presents behind the sample, which eviction boundary ran, planner and upload
  time maxima. `lead_frames` is the count of frames rendered without the geometry; its tail is the
  acceptable case, a stall is not.
- `Distant static residency summary:` at teardown — admitted/evicted bytes and counts, `peak_gpu`
  against the cap, and the fault counters. `unavailable`, `palette_misses`, `removal_non_complete`
  and `frozen` should all be zero.

## Deployment interlocks

- **Deploy `d3d8.dll` and `mgeHost64.exe` together.** `DistantSubset` is 144 B and there is no ABI
  handshake; a mismatched pair misreads the catalog.
- **`cargo xtask deploy` installs the bundled stock DXVK `d3d9.dll` over a custom build.** Restore
  the custom one afterwards or the budget interop is silently absent and every session takes the
  infinite-cap full drain.
- **Regenerate distant land if generation state predates format 9.** `MGE_DL_VERSION` is 18 and does
  not identify that mismatch.

## Known limits

Understood and accepted; each fails soft.

- **Residency plans around the player; rendering follows the camera.** All three planner sites use
  `MWBridge::tryGetPlayerPosition`, while `eyePos` comes from the inverse view matrix. Camera
  *facing* does reach the planner, since it orders admission; camera *position* does not. Vanity,
  third-person and MGE zoom offsets are absorbed by the `+ kCellSize` term in `admissionRadius` —
  hundreds of units against 8192 — but an MWSE cutscene camera diverges without bound and can frame
  merged statics the planner never admitted. Substituting `eyePos` is not the fix: it is unset
  during load-screen drain and bootstrap, and a wandering camera would evict around the player and
  re-admit on return. The shape that works is admission around the camera with retention anchored to
  the player, which needs a real triggering mod to size against.
- **`Unavailable` is terminal for the session.** A buffer that fails creation is never re-attempted
  when memory later frees. Never observed in practice, but it is why the cap must be conservative:
  guessing high once costs geometry for the session rather than a few frames of pop-in. Making it
  retryable is what would let the cap policy be looser.
- **Killing `mgeHost64.exe` mid-transition leaves Morrowind in a permanent busy hang** rather than
  crashing or recovering. It needs the host to be killed deliberately. If revisited, the wanted
  shape is a fatal log plus intentional termination, not recovery machinery.
- **Host RAM is a separate, unaddressed scaling problem.** `read_distant_statics` builds quadtrees
  for the exterior and every interior worldspace at once, and `set_current_world_space` only
  switches an index. One `QuadTreeMesh` per (placement x subset), each with a full `RenderMesh` and
  its 64-byte matrix. The host is 64-bit so this is not fatal, but it grows with mod count the way
  VRAM does and streaming does nothing for it.
