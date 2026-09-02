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
| `mgeHost64/src/state/distant_land.rs` | Residency resource index, cell buckets, `plan_residency`. |
| `mgeHost64/src/ipc/server.rs` | `update_residency`, `plan_residency` command handlers. |
| `mgeHost64/src/abi/protocol.rs` | `ResidencyPlan`, `ResidencyCommit`, `PlanResidencyParameters`. |

## The cap

One cap governs merged bytes for the device session. It is selected once fixed resources exist
(`selectInitialMergedStreamingCap`), from three sources in priority order:

1. `MGE_DL_STREAMING_CAP_MB`, a development override in MB read once from Morrowind's process
   environment. It bypasses automatic selection entirely.
2. `IDxvkMorrowindMemoryInterop1::GetDeviceLocalMemoryBudgetV1`, when our DXVK fork provides it.
3. Neither: an infinite cap.

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
- **`Release` is forbidden until the removal RPC returns `Complete`.** On timeout or server loss the
  buffers, palette entry and ledger bytes stay in removal-in-flight; three failures freeze residency
  for the device session rather than risk handing a live host a stale pointer.
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

Residency is **radial and never frustum-aware**: camera rotation causes no admission and no
eviction. Admission is nearest-cell-first with ring completion — `ensure_residency_offsets` sorts
cell offsets by `(x*x + y*y, y, x)` and `plan_residency` walks that order through
`planner_offset_cursor` — so every cell at radius R is admitted before any cell at R+1, including
cells behind the player. `admissionRadius` is `DrawDist * kCellSize + kCellSize`; the retain radius
is one cell beyond it, giving the hysteresis that stops a boundary from thrashing.

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
  `MWBridge::tryGetPlayerPosition`, while `eyePos` comes from the inverse view matrix. Vanity,
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
- **Readmission during removal-in-flight** briefly reports `Unloaded` to the host while the buffers
  are still live. Safe direction: missing geometry, not a dangling pointer.
- **`update_residency` stops applying a batch after the first error.** The client sees failure and
  retains buffers or freezes.
- **Killing `mgeHost64.exe` mid-transition leaves Morrowind in a permanent busy hang** rather than
  crashing or recovering. It needs the host to be killed deliberately. If revisited, the wanted
  shape is a fatal log plus intentional termination, not recovery machinery.
- **Host RAM is a separate, unaddressed scaling problem.** `read_distant_statics` builds quadtrees
  for the exterior and every interior worldspace at once, and `set_current_world_space` only
  switches an index. One `QuadTreeMesh` per (placement x subset), each with a full `RenderMesh` and
  its 64-byte matrix. The host is 64-bit so this is not fatal, but it grows with mod count the way
  VRAM does and streaming does nothing for it.
