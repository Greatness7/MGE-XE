# Batched static visibility LOD

MGE XE preserves per-object visibility tiers inside generator-merged distant
static batches. The implementation removes triangles that would not have
survived into a farther static band without splitting one batch into several
draw calls.

This is visibility-tier LOD. It removes complete source components according to
MGE XE's Near/Far/VeryFar classification. It does not geometrically simplify
the triangles that remain; any mesh reduction is a separate generation step.

## Why provenance is required

The generator merges many exterior references into one synthetic static to
reduce draw calls. Classifying only that merged static by its large combined
radius makes every member survive too far, especially small alpha-tested
objects such as vegetation.

`XESTAT06` v6 therefore stores component provenance for each merged subset.
Every 16-byte `ComponentRecord` identifies one contiguous source triangle range
and carries the classification inputs that existed before merging:

```text
u32 first_triangle
u32 triangle_count
f32 radius
u8  classification
u8  reserved[3]
```

`radius` is the source model radius multiplied by placement scale. The generator
intentionally does not bake the building multiplier into the file. Components tile their
owning subset's serialized triangle range exactly, without gaps or overlaps.
[distantland-data.md](distantland-data.md) documents the complete container layout.

## Loader classification and index layout

The 32-bit loader classifies components with the active
`FarStaticMinSize`/`VeryFarStaticMinSize` values:

- forced `STATIC_NEAR`, `STATIC_FAR`, and `STATIC_VERY_FAR` map directly to
  their named tier;
- buildings compare twice their stored radius;
- automatic and tree classifications compare their stored radius;
- radius at or below the far threshold is Near, at or below the very-far
  threshold is Far, and anything larger is VeryFar.

Because classification happens at load time, changing the thresholds takes
effect after renderer reinitialization without regenerating distant land.

For a component-bearing subset, the client gather-copies its triangle ranges
into one GPU index buffer:

```text
[very-far-capable] [far-only] [near-only]
```

It records three cumulative face counts:

```text
veryFarFaces = very-far-capable
farFaces     = very-far-capable + far-only
faces        = very-far-capable + far-only + near-only
```

Every tier shares the vertex buffer, and the index buffer stores each triangle once.
Component-less subsets preserve the original index order and set all three
counts to the full face count.

## Host selection

`DistantSubset` sends the three counts to the host. Precise static visibility
queries also send the live near and far band endpoints. Immediately before
emitting a visible mesh, the host selects:

```text
range² <= (near_end + radius)² -> faces
range² <= (far_end  + radius)² -> farFaces
otherwise                     -> veryFarFaces
```

A zero selected count suppresses the mesh rather than issuing a zero-primitive
draw. The host writes the chosen value into the existing `RenderMesh.faces`
field, so the renderer uses the same draw path and the batch still costs at
most one draw call per subset.

Whole-static quadtree ownership is unchanged. `InitDistantStatics` receives the
same min-size thresholds the client uses, so whole-static placement and component
filtering stay consistent.

## Correctness boundaries

- Near rendering is unchanged and uses the complete subset.
- Farther tiers are cumulative, so a VeryFar component is also present in Far
  and Near, and a Far component is also present in Near.
- Bounds and generated horizon footprints describe the complete batch. They
  remain conservative for reduced tiers.
- The host chooses one face count for an entire merged subset. A spatially large batch
  can therefore retain components longer than independently rendered source
  objects would. It never removes them earlier.
- The generator excludes dynamic-visibility references from merging, and grass uses
  its separate instancing path.

## Format and ABI maintenance

The on-disk component table, loader metadata, and IPC fields form one contract:

- static file declarations and validation:
  `d3d8/cpp/mge/dlformat.h`;
- client classification and index construction:
  `d3d8/cpp/mge/distantstatics.cpp`;
- C++ IPC mirror: `d3d8/cpp/ipc/bridge.h`;
- Rust IPC mirror and layout assertions:
  `mgeHost64/src/abi/render.rs`,
  `mgeHost64/src/abi/protocol.rs`, and
  `mgeHost64/src/abi/layout_tests.rs`;
- host tier selection: `mgeHost64/src/state/quadtree.rs`;
- generator writer: `distantland`.

Changes to component records or cumulative counts require coordinated format,
loader, host, generator, and layout-test updates. A breaking generated-data
change also requires an `MGE_DL_VERSION` bump and regeneration.
