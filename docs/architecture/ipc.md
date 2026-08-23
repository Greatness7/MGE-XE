# IPC: d3d8.dll ↔ mgeHost64.exe

The 32-bit runtime offloads distant-land state and visibility culling to the 64-bit host
over shared memory. Companion to [`ARCHITECTURE.md`](../../ARCHITECTURE.md) §6.

Code:

- 32-bit client (C++): `d3d8/cpp/ipc/`. `client.*` (RPC client + process launch), `bridge.h`
  (wire types), `vecbase.*`/`view.*`/`vecwrap.*` (shared-vector views), `dlshare.*`
  (distant-land file helpers + shared state).
- 64-bit server (Rust): `mgeHost64/src/ipc/`. `server.rs` (dispatch loop),
  `shared_vec.rs` (vector implementation); ABI mirrors in `mgeHost64/src/abi/`.

## 1. Transport and lifecycle

`IPC::Client::launchServer` creates:

- a shared-memory mapping for one `IPC::Parameters` block,
- two auto-reset events (`rpcStart`, `rpcComplete`),
- a duplicated handle to the client process (so the host can detect client death).

It then spawns `mgeHost64.exe` with those four handles formatted as hex on the command
line (`"%p %p %p %p"` = sharedMem, clientProcess, rpcStartEvent, rpcCompleteEvent),
inheritable handles enabled. The host parses them back in `win::parse_startup_handles`.

RPC is strictly one-at-a-time, client-initiated:

```
server: SetEvent(rpcComplete) once at the top of listen(), before the first wait
client: write Parameters { command, params union } → SetEvent(rpcStart) → wait
server: WaitForMultipleObjects(rpcStart, clientProcess)
        → dispatch on Command → write OUT fields → SetEvent(rpcComplete)
client: wake on rpcComplete (WakeReason::Complete) or server death (ServerLost)
```

Most client calls come in async pairs (`allocVec`/`awaitAllocVec`, `getVisibleMeshes` …
`waitForCompletion`) so the render thread can overlap host-side culling with its own work;
`*Blocking` variants exist for setup paths. If the client process dies the host returns
from `listen()` cleanly; if the host dies the client gets `ServerLost` and distant land
fails gracefully.

The client launches the host on two paths. `FakeDirect3DCreate` calls
`StartupGeneration::launchEarlyHost` unconditionally for Morrowind; that function skips only
when MGE is disabled, proxy-only mode is on, distant land is off, or `mgeHost64.exe` is
missing. Whether generation is configured is not consulted here — the host's own worker
thread decides that. The same process then stays around as the IPC host. If the early launch
was skipped or failed, the upload pump's `HostWait` phase launches it lazily.

The host starts its RPC loop before its startup-generation worker publishes a
session output snapshot. Bootstrap readiness therefore means the server can
answer commands; `QueryOutputStatus` separately reports `Pending`, `Ready`, or
`Failed`. The client polls both states without blocking ordinary menu/load
frames. See [distantland-lifecycle.md](distantland-lifecycle.md).

## 2. Command set

`IPC::Command` (`d3d8/cpp/ipc/bridge.h`) / `abi/protocol.rs`:

| Command | Parameters | Purpose |
| --- | --- | --- |
| `AllocVec` | element size, window size, max capacity, initial capacity → `VecId`, mapping handle, sizes | Create a shared vector. |
| `FreeVec` | `VecId` → `wasFreed` | Request release of a shared vector. The host keeps it mapped and returns `wasFreed = 0` while the 32-bit side still holds users or host ownership is not sole; the slot ID is recycled only once both sides have released. |
| `Exit` | none | Shut the host down. |
| `UpdateDynVis` | `VecId` of `DynVisFlag{groupIndex, enable}` | Apply dynamic-visibility group toggles to mesh instances. |
| `InitDistantStatics` | `VecId`s of `DistantStatic[]` + `DistantSubset[]`, plus far/very-far min-size thresholds → `success` | Register static mesh metadata (with 32-bit D3D pointers, generated horizon footprints, and cumulative static LOD face counts); host re-reads `usage.data` itself and builds per-worldspace quadtrees using the client-sent thresholds. |
| `InitLandscape` | `VecId` of `LandscapeBuffers{vb, ib}` + sort token → `success` | Register terrain buffers; host re-reads `terrain.bin` and builds the land quadtree. |
| `SetWorldSpace` | cell name (64 chars) → `cellFound` | Switch the active worldspace for queries. |
| `GetVisibleMeshesCoarse` | visible-set `VecId`, frustum, set flags, sort | Frustum-only query (used for `VIS_LAND`, `VIS_GRASS`, and shadow casters). |
| `GetVisibleMeshes` | + view sphere and raw near/far static band endpoints | Frustum + distance/size query for statics bands; precise static queries use the endpoints to select the cumulative face count emitted for each mesh. |
| `SortVisibleSet` | `VecId`, sort order (`ByState`/`ByTexture`) | Sort an already-filled set. |
| `SetHorizonConfig` | live terrain horizon-culling tuning parameters | Push live horizon-culling config (see [horizon-culling.md](horizon-culling.md)). |
| `FinishHorizonFrame` | none | Close the current render frame for the adaptive horizon gate: ticks the gate once with this frame's accumulated precise-static stats. Sent once per rendered frame while horizon culling is enabled (after both the main distant-static pass and any water-reflection static pass), independent of `SortVisibleSet`, so a frame with only a reflection query still ticks and stats never bleed across frames. |
| `QueryOutputStatus` | status out parameter | Poll the startup-generation worker's published output state (`Pending`, `Ready`, or `Failed`). |

`Command::None = 0` is the default placeholder, not an operation; the host dispatches it as a
no-op. RPC completion means only that dispatch finished — both init commands report their real
outcome in the OUT `success` field, which the client reads through
`lastInitDistantStaticsSucceeded()` / `lastInitLandscapeSucceeded()`.

Set flags select which quadtrees the host walks: `VIS_NEAR/FAR/VERY_FAR` (statics bands),
`VIS_GRASS`, `VIS_LAND`. The host appends results to the given shared vector as `RenderMesh`
records: `{enabled, hasAlpha, animateUV, tex, transform, verts, vBuffer, faces, iBuffer}`.
The pointers are the 32-bit D3D9 resources the client registered at upload time, so the
client can draw a result row directly (`VisibleSet::Render`).

`hasAlpha` and `animateUV` describe the subset, not the texture, and `VisibleSet::Render`
tracks each one separately for that reason. `tex` does not imply either: subsets that skip
the atlas keep their source texture path, which carries no alpha or opaque prefix, and
`BSA::loadTexture` caches by path, so two subsets can share one texture pointer and still
disagree on both flags. The generator's merge step keeps such subsets separate
(`statics/src/model.rs`), so they reach the client as distinct meshes.

For precise
static queries, the host copies the mesh record, replaces `faces` with the selected
cumulative count (`faces`, `farFaces`, or `veryFarFaces`), and skips zero-count results.
Coarse queries have no band endpoints and emit full face counts.

Version 16 has no payload-selection RPC. The Rust host is the sole state/inventory/checksum
authority and retains the shared snapshot pin for server lifetime. The C++ client opens the fixed
terrain and 128 static-shard paths after checking the version byte, and retains the
payload-specific binary validation; it does not parse the storage envelope or implement BLAKE3.

## 3. Shared vectors

The bulk-data primitive is a growable typed array in a named shared mapping, identified by
`VecId`.

- The 64-bit side (`SharedVec`) owns the allocation, header, and full view. Borrows are
  size-checked, not type-tagged: `is_type::<T: Pod>()` compares `size_of::<T>()` against the
  negotiated element size, so two POD types of equal size are indistinguishable at runtime.
- The 32-bit side (`IPC::VecBase`/`VecView<T>`, `view.*`) has scarce address space, so the
  client maps a fixed-size sliding window over the vector rather than the whole thing,
  using `VirtualAlloc2`-reserved placeholder regions remapped with `MapViewOfFile3` /
  `UnmapViewOfFileEx` as access moves. The client resolves these APIs dynamically
  (`IPC::initImports`); missing APIs disable distant land (pre-Win10 systems).
  The 64-bit host has no such constraint. It maps whole `SEC_RESERVE` mappings with
  plain `MapViewOfFile`/`VirtualAlloc` (`win::map_view`, `win::commit_pages`).
- `IpcClientVector` + `vecwrap.*` wrap a `VecView<RenderMesh>` with a forward cursor
  (`restart`/`first`/`next`) that never dereferences across a window remap; `VisibleSet`
  (`mge/visibleset.*`) drives it as the D3D draw loop.

The client keeps five long-lived vectors (allocated in the upload pump's `IpcSetup` phase): visible
sets for land, distant statics, grass, and an extra set (reflections/shadows), plus the
`DynVisFlag` update vector. Setup-time vectors (terrain `LandscapeBuffers`, statics, subsets) are allocated and filled by
the client, then retained until the init RPC that consumes them completes — the host may read
them incrementally while the client continues — and only then released with `freeVecBlocking`
(`finishLandscapeUpload`).

## 4. ABI rules

The 32-bit and 64-bit sides compile the same logical structs from different codebases.
Keep them bit-identical:

- Wire payload structs: `#pragma pack(push, 4)` in C++, `#[repr(C)]` + bytemuck `Pod` in
  Rust, mirrored field-for-field between `d3d8/cpp/ipc/bridge.h` and `mgeHost64/src/abi/`.
  Exceptions: `SetWorldSpaceParameters` is `#[repr(C, packed(4))]`, and the containers
  `ParameterUnion`, `Parameters`, and `VecShare` are `#[repr(C)]` without `Pod` (a union, a
  block with a discriminant, and a header holding atomics respectively).
- Pointers use `ptr32<T>` / `ptr64<T>`. The pointer is real on the owning side and an
  opaque integer on the other (`bridge.h` flips the definitions on `MGE64_HOST`). Handles
  use `HANDLE32` (`__ptr32`).
- Selected ABI sizes and offsets are asserted in `mgeHost64/src/abi/layout_tests.rs` —
  coverage is deliberate, not exhaustive (`DynVisParameters` and `ParameterUnion` have none).
  On the C++ side `bridge.h` carries `static_assert(sizeof(Parameters) == 136)` and
  `dlformat.h`/`distantland.h` carry the format and runtime-constant assertions. **Any change
  is a lockstep change to both halves plus an assertion covering it.**
- `DistantSubset` is 128 bytes in the current ABI. The original 64-byte subset metadata
  comes first, then a 56-byte generated `HorizonFootprint` at offset 64, `farFaces`
  at offset 120, and `veryFarFaces` at offset 124.
- `DistantStaticParameters` is 20 bytes: the static/subset vector IDs, then
  `farStaticMinSize` at offset 8 and `veryFarStaticMinSize` at offset 12, then the OUT
  `success` field. The host uses the client-sent thresholds for whole-static quadtree
  classification.
- `GetMeshesParameters` is 132 bytes. Precise static queries include `nearStaticEnd` at
  offset 124 and `farStaticEnd` at offset 128.
- `abi/math.rs` re-implements the D3DX math types (`D3DXMATRIX`, `D3DXPLANE`,
  `D3DXVECTOR4`) layout-compatibly, along with the `ViewFrustum` containment tests, so culling
  math agrees across the boundary. Plane extraction and normalization are C++-only
  (`ViewFrustum::ViewFrustum` in `bridge.cpp`); Rust only consumes the planes it receives.
