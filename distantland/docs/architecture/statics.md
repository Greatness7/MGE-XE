# Distant statics

The statics pipeline turns plugin references and their NIF meshes into MGE-XE's
fixed `static_meshes_000..127` cache shards, the `usage.data` table that indexes their logical
concatenation, and the atlas pages those
meshes sample. It spans three source areas: `crates/usage/src/` (what should exist), `crates/statics/src/`
(how each mesh becomes distant geometry), and the orchestration in
[src/generation/statics_stage.rs](../../src/generation/statics_stage.rs).
The statics crate depends downward on usage, VFS, foundation, formats, and the shared texture
crate; it has no dependency on root generation settings, storage authority, or the terrain crate.

## 1. Usage scanning (`crates/usage/src/info/`)

`UsageInfo::setup_with_grass_plugins_and_capture` walks the active load order (parallel per plugin, merged in load order,
[load.rs](../../crates/usage/src/info/load.rs)) and collects:

- **Objects** (`ObjectDefinition`): STAT/ACTI/MISC/DOOR/etc. records mapped to their mesh
  paths, filtered by the inclusion settings (`include_activators`, `include_misc`; doors are
  always included, like statics) and by override directives (`build_object_definition`).
- **Script disable classification** ([script.rs](../../crates/usage/src/info/script.rs)): each
  `SCTX` is scanned for a bare whole-word `Disable` (the script disables its own object) and for
  `SomeId->Disable` targets. Both sets are load-order-wide. `merge` unions them, so
  classification cannot run per plugin; `classify_script_disables` applies it after the merge and
  before the projection capture. Rule A folds into `ignore_by_default`; `remap_references` applies
  Rule B per reference against `DistantReference::persistent`, and only when
  `exclude_script_disable_targets` is set. Persistence comes from `tes3::esp::Reference::persistent()`
  (including moved references and teleport doors), not the base object's `PERSISTENT` flag.
  `force_mesh_generation`, set by dynamic-visibility/unique-object groups or `include_objects`,
  overrides both rules; a mesh `no_script` / `ignore_script` override clears the attached-script
  input before classification. Outcome counts and the Rule B exclusion list land in
  `metrics.usage.script_disable`; buried-reference outcomes and work counters land in
  `metrics.usage.burial`.
- **References** (`DistantReference`): per-cell placed instances keyed by `(ref_index, mask)`,
  with translation/rotation/scale and the owning cell. Exterior cells key by grid coordinate;
  interiors keep their cell name.
- **Terrain cells** (`TerrainCell`): the sole decoded LAND store per exterior cell, dense
  `65×65` heights, vertex normals/colors, and the VTEX texture-index grid resolved through
  per-plugin LTEX tables. Height sampling for buried-static culling and terrain mesh Z uses
  this map ([terrain.rs](../../crates/usage/src/info/terrain.rs)); the same cells feed the terrain
  package.
- Interior metadata for MGE-XE's interior inclusion rules.
- **Mesh scale maximums**: the largest scale each mesh is placed at, so size filtering can use
  worst-case effective size.

`GenerationJob.grass_plugins` supplies a second, ordered generator-only plugin list. Its plugins
are resolved against the same data directories but never enter the active load order. They form
an override chain among themselves: later entries can override or delete earlier placements
through `MAST`, and later grass `STAT` definitions of the same id win. The dedicated loader
resolves exterior references across that chain before retaining placements whose final object is
grass-classified; it ignores interior and terrain content and warns when an addressed master is
absent from the grass list. Density thinning uses filename/cell/position/occurrence hashing. Grass
meshes still use the normal VFS and static extraction path; `StaticGrass` gates keep them out of
atlas packing and merge grouping while their placements are serialized into `usage.data`.

Filtering ([filter.rs](../../crates/usage/src/info/filter.rs)) happens in two waves:

- During parse: `filter_interiors` (behaves-like-exterior / water / large-interior rules; an
  interior is "large" when its references span ≥ 10,000 units), `remap_references`
  (normalize reference ids to mesh keys, apply per-object visibility overrides, and cull grass
  by `grass_density` using a stable per-reference pseudo-random sample so density changes are
  deterministic).
- After static generation: `discard_unused_references` (mesh was filtered out or failed),
  `discard_deep_water_references`, and `discard_low_visibility_references`. The latter drops
  references whose mesh is mostly buried under the terrain heightfield (`is_buried` samples the
  landscape grid against the static's world AABB).

Reference pruning must run *after* static generation because it needs to know which meshes
actually produced distant statics.

## 2. NIF extraction (`crates/statics/src/extract.rs`)

`DistantStatic::from_nif_with_identity` builds the intermediate representation for one mesh:

- Loads the NIF through the VFS and walks `visible_geometries()`
  ([crates/statics/src/nif.rs](../../crates/statics/src/nif.rs)): skips hidden nodes and editor markers, resolves
  accumulated NiProperty state (texturing, material, alpha) down the scene graph, clears root
  transforms, and normalizes texture paths.
- Classifies the static (`StaticType`: building/tree/etc.) from path-based inference
  (`inferred_static_type`) unless an override classifier says otherwise
  (`resolve_static_type`); overrides can also force-generate or suppress a mesh entirely.
- Produces one `Subset` per distinct texture/material combination: vertices (position, normal,
  color, UV), `u16` triangles, alpha/UV-controller flags, and emissive-driven color
  adjustments.
- Computes minimum bounding spheres (minsphere) and AABBs; applies the early size filter using
  the mesh's maximum placed scale, `min_static_size`, and `door_size_multiplier`.

The result is the `DistantStatics` map (`IndexMap<String, DistantStatic>` keyed by normalized
mesh path) defined in [crates/statics/src/model.rs](../../crates/statics/src/model.rs).

## 3. Texture analysis, sizing, and dedupe

`AtlasTextureSet::from_distant_statics` collects the opaque and alpha texture key sets (the
two atlas domains). The `AnalyzeTextureDensity` stage then makes a single hoisted read pass
over every source texture (`collect_static_texture_source_info`) producing per-texture size,
blake3 content hash, and probed dimensions. From that one pass come:

- **Atlas cache fingerprints.** `fingerprints_from_source_info` builds them, and the atlas cache
  consumes them.
- **The baseline.** `TextureAxisCaps::longest_for` provides the unconditional per-texture upper bound,
  stored in `SizingPlan::baselines`. `min(long_cap, short_cap * long / short)` satisfies both axis
  caps under aspect-preserving scaling; the result is then snapped down to a whole mip step of the
  source, so every downscale selects a DDS mip rather than resampling. Equal caps reproduce a plain
  longest-side clamp exactly. It cannot live in the override maps below: those are populated only
  when the mode reduces the domain, and `reconcile_domain` discards them wholesale for an alias
  group that needs full resolution.
- **The sizing plan** ([sizing/](../../crates/statics/src/atlas/sizing/)). When
  `static_texture_sizing.mode != Off`, `analyze_static_texture_usage` measures, per triangle,
  the texel density the geometry actually exhibits (singular values of the 2×2 UV→world
  Jacobian, [sizing/math.rs](../../crates/statics/src/atlas/sizing/math.rs)), aggregates the limiting
  (lowest-density) use per texture/domain, and `plan_static_texture_resolutions` selects
  whole-mip reductions that keep density above `protected_density`, bounded by
  `min_texture_size` and `max_mip_reduction`. The alpha domain is never downscaled; in
  `Report` mode nothing is downscaled. No sizing report is published: the pass returns only the
  plan plus three counters (proposed reductions, applied reductions, baseline fallbacks), and the
  proposed-reduction count is recorded on the `stage.analyze_texture_density` span.
- **Dedupe reconciliation.** Textures that exact-dedupe collapses into one canonical entry must
  share one resolution. `merge_dedupe_alias_requirements` lifts the group to the maximum selected
  size.

Exact texture deduplication
([crates/texture/src/texture_dedupe.rs](../../crates/texture/src/texture_dedupe.rs)) is a shared
core used by both static atlasing and terrain. It fingerprints each texture (file size + content
hash, or decoded dimensions + RGBA bytes), and then `build_alias_map` collapses identical
fingerprints onto the first key seen. Collapsing intentionally changes atlas layout / material
ids, which is why the mode participates in cache fingerprints.

## 4. Atlas packing (`crates/statics/src/atlas/`)

`AtlasManager` ([manager.rs](../../crates/statics/src/atlas/manager.rs)) owns both domains and
produces immutable render plans without holding storage authority:

- **Cache probe** ([cache.rs](../../crates/statics/src/atlas/cache.rs)): generation state embeds locally
  versioned v3 evidence. Shared packing, cap, and dedupe configuration is validated globally;
  sizing overrides are stored per family. Each family persists stable page dimensions, monotonic
  active slot ids, reserved and visible rectangles, provider identity, visible-content hashes,
  sorted logical-key-to-slot relations, source fingerprints, and final bindings. Validation checks
  those old relations internally rather than requiring current key coverage. Version/shared-config
  failures reject both families; malformed structure or committed inventory rejects only that
  family. No `atlas_cache.bin` is written or read.
- **Exact carry, reconciliation, or fresh plan** ([pack.rs](../../crates/statics/src/atlas/pack.rs),
  [reconcile.rs](../../crates/statics/src/atlas/reconcile.rs), and
  [plan.rs](../../crates/statics/src/atlas/plan.rs)): unchanged source fingerprints and family config exact-
  carry without decoding. Otherwise current exact-dedupe groups are prepared at planned dimensions
  (preferring an exact DDS source mip, see
  [crates/texture/src/texture_io.rs](../../crates/texture/src/texture_io.rs)) and matched
  to compatible prior slots by a deterministic maximum-weight assignment over surviving logical
  keys. Matched slots retain ids, reservations, and page ids; unmatched groups consume deterministic
  seeded MaxRects free space before new pages append. Empty middle pages remain available, while only
  the empty trailing suffix is truncated. Invalid reconciliation arithmetic or invariants fail
  closed to a fresh pack of that family. Planning builds no page canvases and writes nothing.
- **Streaming render + root publication** ([plan.rs](../../crates/statics/src/atlas/plan.rs)):
  page recipes are only `Carry`
  or `Build`. A page builds for a new slot or changed active placement/visible content; removal and
  same-content provider promotion alone carry. The renderer recomposes dirty pages from every current active
  slot on a fresh canvas. Retained empty middle pages remain explicit `Carry` inventory entries.
  `AtlasPublishPlan::render` returns encoded `AtlasPageWrite` values for only the build paths; the
  root pipeline writes those bytes through its `PublicationWrites` ledger. Opaque pages encode as
  BC1 and alpha pages as BC3, both with full mip chains via
  [crates/texture/src/dds.rs](../../crates/texture/src/dds.rs). Page names stay
  `_mge_xe_atlas{N}.dds` / `_mge_xe_atlas_alpha{N}.dds`. Publication never rereads VFS inputs.
- **UV rewrite** ([uv.rs](../../crates/statics/src/atlas/uv.rs)): each atlas-eligible subset's
  `texture` becomes the page filename and every vertex gets the frame's `UvBound` (the atlas
  sub-rectangle its wrapped UVs must be clamped to at render time).

After planning, the atlas stage bakes its state into `distant_statics`. The canonical binding-map
digest feeds the downstream statics-domain gate before the `CommitPlan` issues write authority.
The complete next inventory combines prior `RequiredArtifact` entries for carried pages with new
write evidence for built pages. After state commit, recognized atlas page paths (`_mge_xe_atlas*.dds`
/ `_mge_xe_atlas_alpha*.dds`) absent from that inventory are pruned; unrecognized files in the
atlas texture directory are left alone. A cache hit means both families exact-carried unchanged
evidence; a zero-page-write reconciliation still serializes its new v3 relation. Comparable priors
also produce exhaustive Added/Removed/Changed/Unchanged binding counts, while the complete current
binding digest drives UV lowering and the coarse statics gate. The generation report
records plan mode, slot/page lifecycle counts, integer area and fragmentation metrics, and
binding-delta counts in addition to the existing layout, byte, timing, and peak-memory fields. The statics owner planner consumes comparable binding
deltas to dirty only their mesh and merge-cell consumers.

Atlas areas are integer texel counts: active visible rectangles, active reservations, full pages,
and border-excluded usable pages. `fragmentation_ppm` is
`(usable_page_area - reserved_area) * 1_000_000 / usable_page_area` (zero with no pages).
Retained empty pages consume disk but no runtime VRAM: MGE-XE passes each static subset's serialized
texture filename to `BSA::loadTexture` (`d3d8/cpp/mge/distantinit.cpp`), and
`loadTextureExact` resolves that requested filename from the distant-land folder, loose textures, or
BSA (`d3d8/cpp/mge/morrowindbsa.cpp`). It does not enumerate and preload the atlas directory.

## 5. Component-level visibility-tier LOD

Exterior merging preserves each source subset as a component range through model processing and
packing. XESTAT05 v5 stores those ranges as `ComponentRecord` entries: a contiguous triangle range,
the source-model radius multiplied by placement scale, and the source `StaticType`. The building
size multiplier is deliberately not baked into the radius; MGE-XE applies it while classifying the
component. Component records must tile their subset exactly, use consumer-valid non-grass
classifications, carry finite non-negative radii, and keep reserved bytes zero. A component-less subset remains valid
and renders its complete geometry in every tier.

At load time MGE-XE classifies components against the live far and very-far minimum-size settings,
then copies their triangle ranges once into a cumulative index buffer ordered
`[very-far][far-only][near-only]`. `DistantSubset` carries the resulting cumulative
`veryFarFaces <= farFaces <= faces` counts. Visibility queries carry the near/far distance endpoints,
so the host selects the total, far, or very-far prefix without splitting a merged subset into three
draw-call-bearing resources. This cumulative layout replaced the rejected tier-split approach,
which multiplied merged subsets and their draw/sort/quadtree overhead.

## 6. The statics bundle stage

[src/generation/statics_stage.rs](../../src/generation/statics_stage.rs) first prepares everything
below under the static bundle input fingerprint (see
[caching-and-startup.md](caching-and-startup.md)). Preparation performs no output writes.

On a coarse-gate hit, the prepared plan carries all 128 shard entries and `usage.data` without
reading, hashing, or copying shard bodies.

On a coarse miss with comparable schema-v6 state and atlas binding deltas, the stage keeps a
behavior-identical global membership prefix while switching meshopt and packing to owner-granular
work (fail-closed full rebuild on any disagreement):

1. **`OptimizeMeshes`** (`optimize_statics_keys`, [model/process.rs](../../crates/statics/src/model/process.rs)).
   When owner-partial evidence is complete, optimize only content/binding-dirty meshes and every
   pre-optimize-eligible partner of a dirty merge cell. Clean meshes receive exact post-optimize
   sphere/AABB values from the sorted `optimized_mesh_bounds` state table; missing entries join the
   optimize set, and a set covering at least 75% of candidates uses the full pass. Per optimized
   static, reusable Rayon workspaces merge compatible subsets, simplify with meshopt, and
   vertex-cache optimize. Force, migration, incomparable state, recipe/binding failures, and other
   owner-partial precondition failures retain the full `optimize_statics` path.
2. **Size filter.** `passes_static_min_radius` runs *after* optimization (final bounds), matching
   MGE-XE. Doors use `door_size_multiplier` to inflate their effective (not rendered) size.
3. **Merge grouping + usage mutation** ([merge.rs](../../crates/statics/src/merge.rs)). Global and cheap:
   per exterior cell (parallel), take references to statics with scaled radius ≥ 32 and no
   dynamic-vis group, build a BVH (obvhs) over their world AABBs, and cut groups whose node
   half-diagonal stays under `merge_group_radius`. Plan synthetic keys and rewrite usage references
   for every planned group; do *not* yet bake dirty-cell geometry here.
4. **Owner planner** ([statics_stage/plan.rs](../../src/generation/statics_stage/plan.rs)). Closes
   complete typed mesh/merge dirt over unit diffs and atlas binding deltas using reverse indexes.
   Dirty ordinary owners and dirty merge cells define the rebuild set; clean owners must reproduce
   their prior record-key subsets.
5. **Dirty-shard decode** ([statics_stage/decode.rs](../../src/generation/statics_stage/decode.rs)).
   Before state invalidation, the stage opens and decodes only dirty prior shards
   concurrently on the global Rayon pool. Each shard keeps the same inventory-length + full-BLAKE3,
   XESTAT05, and positional-key validation order; after all workers join, the lowest failing shard
   id selects the deterministic fallback reason. Splice remains serial.
6. **Dirty merge geometry + pack.** Build merged geometry only for dirty cells (LOD identity is
   mesh key, not positional map index). Merged vertices are baked at absolute world positions, so
   `SubterrainCull` ([model.rs](../../crates/statics/src/model.rs)) trims triangles whose three
   corners all sit below `terrain_height - terrain_detail.target_error()` during the append, keeping
   component ranges tiled by construction. Ordinary records stay shared and instanced per placement,
   so the cull cannot apply to them. The margin exists because terrain is one mesh simplified with
   an absolute error budget, so the rendered ground can sit that far below the sampled LAND heights;
   sampling the simplified mesh instead would roughly double the saving (19.9% of merged triangles
   against 7.7% at `high`) but that geometry is not built until terrain publish, after statics.
   Once merged geometry no longer needs its member meshes,
   drop statics with no surviving exterior or interior placement. Pack only dirty surviving
   ordinary owners and emitted dirty-cell merged records (`finalize` /
   [model/pack.rs](../../crates/statics/src/model/pack.rs)).
7. **Authoritative typed-key assembly + splice.** One current ordered record-key set drives shard
   membership, `usage.data` ordinals, and metrics. Clean-owner packed records are retained from
   decoded shards; dirty/removed records are dropped; fresh records are inserted. Usage is
   reconciled against that final set before serialization, so every reported and emitted placement
   resolves to a packed record. Final per-shard key vectors must equal the assembly.
8. **Publication plan.** Clean shards keep committed inventory entries unopened; dirty shards
   serialize as independent XESTAT05 v5 containers. Usage is prepared from the assembly
   ([crates/usage/src/write.rs](../../crates/usage/src/write.rs)). During publication, usage is written first and
   one background thread serializes/writes dirty shards sequentially while terrain materializes.

When owner-partial evidence is missing or fails closed, the same stage falls back to building every
planned merge group's geometry and packing the complete final map before per-shard fingerprinting,
still writing only shards whose final packed-input digest changed.

Merge simplification diagnostics (`metrics.statics.merge_simplification`) still record group/member
counts, LOD-cache reuse, capped/second-pass subset counts, triangle totals, and extent/target
distributions when full or dirty-cell geometry runs.

Every changed destination is written through the same `PublicationWrites` ledger used by atlas and terrain writers.

## Overrides and plugin metadata

Two layered configuration sources, merged into one `StaticOverrides` by `OverridesBuilder`
([crates/statics/src/overrides.rs](../../crates/statics/src/overrides.rs)); later sources win per key:

- **Legacy `.ovr` override files** ([overrides/parse.rs](../../crates/statics/src/overrides/parse.rs)):
  MGE-XE's format, per-mesh classifier lines (keywords for static type, force/ignore, grass
  density, etc.), `[names]` (per-object include/exclude), `[interiors]`, and `[dynamic_vis]`
  group definitions. Files are merged in the order given by `override_files`. The bundled
  MGE-XE default classifier set is a test asset
  ([tests/assets](../../tests/assets)).
- **Plugin `-metadata.toml`** ([crates/statics/src/metadata.rs](../../crates/statics/src/metadata.rs)): mods
  ship `<Plugin>-metadata.toml` next to their plugin; only files for *active* plugins are read,
  and only the `[tools.mge-xe.distantland]` table is consumed (include/exclude objects,
  include/exclude interiors, structured per-mesh static entries, dynamic-vis groups). Parse
  errors are fail-soft (warn and skip). These merge *after* all `.ovr` sources, in load order,
  so mod-shipped directives override the legacy layer. IDs and cell names are case-insensitive;
  exclusion wins when the same value is both included and excluded. Per-mesh `ignore_script = true`
  is the metadata equivalent of the legacy `no_script` keyword.

`DynamicVisData` (named visibility groups with ranges, deduplicated across sources) flows into
`usage.data` so MGE-XE can toggle groups at runtime; references in a dynamic-vis group are
excluded from merging.

## Shadow units

Before atlas application replaces texture symbols and before merge mutation removes member
references, `generation::unit_fingerprint` captures mesh, merge-cell, and static-texture unit
fingerprints. Mesh units include unresolved/read-failed/filtered states, resolution facts, raw
content identity, settings/override/admission inputs, bounds, and ordered source textures. A
BVH-free enumerator applies the same merge eligibility filter on every run, including legacy
bundle hits, and fingerprints cells with at least two eligible stable-key members. Merge units are
the one statics-domain unit with terrain inputs: because merged geometry is culled against the
heightmap, each carries the `height_hash` of the 3x3 cell block around its own cell plus the
`terrain_detail` that sets the cull margin. That halo bounds how far a member may overhang its
owning cell before a terrain edit under the overhang stops dirtying the unit. Static
texture units capture source identity, opaque/alpha membership, sizing decisions, and dedupe
settings. Texture units are report-only and do not enter the front-half statics-domain
predicate.

These tables and `mesh -> merge cells` / `texture -> meshes` reverse indexes are encoded into
generation state. The statics-domain digest remains the coarse fast path; fixed-shard
input digests remain the final per-shard identity, while schema-v4 typed keys provide the
owner-partial assembly/splice boundary after a coarse miss.

## Settings input audit

Three sites exhaustively destructure `GenerationSettings`, so any new field fails to compile until
it is classified at each: `UnitSettingsPartitions::from_settings` (unit ownership, the table below),
`UsageSettingsProjection::from` (whether the setting itself reaches `statics_global`), and
`write_generation_settings_canonical` (`settings_identity`, which gates only the no-op check).

The classification is what makes owner-partial static reuse sound: every setting that can change a
packed static record's bytes must live in a mesh or merge owner fingerprint (so a change dirties
the affected owners), because the owner-partial path carries clean shards verbatim. A usage-global
setting has no owner, so it reaches the coarse `statics_domain_digest` gate only through
`statics_global`, either as itself via `UsageSettingsProjection`, or through the projection that
already materializes its effect. `settings_identity` is not folded into that digest and does not
substitute for either.

| Setting | Owner | Notes |
|---|---|---|
| `min_static_size` | mesh | Size-filter membership. |
| `door_size_multiplier` | mesh | Size filter + the only pack-time setting `into_distant_static` reads (door sphere radius). |
| `static_mesh_target_error`, `static_mesh_normal_weight`, `static_mesh_color_weight`, `static_mesh_merge_error_multiplier` | mesh + merge | Simplifier behavior; change record bytes. |
| `merge_group_radius` | merge | Merge grouping. |
| `terrain_detail` | terrain unit + merge | Also sets the below-terrain cull margin, so it changes merged record bytes. |
| `max_static_texture_long_axis`, `max_static_texture_short_axis`, `max_static_atlas_size`, `static_texture_sizing`, `texture_dedupe_mode` | texture (binding) | Reach records only via atlas UV/page bindings, handled by the binding-delta closure. |
| `max_terrain_texture_size`, `max_terrain_atlas_size`, `terrain_mesh_*` | terrain unit | Not statics. |
| `grass_density`, `include_*`, `exclude_script_disable_targets` | usage-global | Decide which references/placements exist (membership), caught by unit add/remove and clean-owner reproduction. Captured as themselves by `UsageSettingsProjection`, because not every effect is materialized at the pre-filter boundary. |
| `deep_water_static_cull_depth` | usage-global | Cull depth below water during usage pruning; captured by `UsageSettingsProjection` because interior membership is not otherwise materialized at the final usage boundary. |
| `use_override_list`, `override_files`, `use_plugin_metadata` | usage-global | Select which override sources are parsed; the whole effect is already in the resolved `StaticOverrides`, so `OverrideStateProjection` carries them and `UsageSettingsProjection` ignores them. |
| `generate_terrain`, `max_terrain_control_texture_size`, `max_terrain_control_texture_bytes` | terrain-global / guard | Not statics. |
| `force_rebuild` | execution policy | Excluded from `settings_identity`. |

No current setting belongs to the *record-global recipe* class: the record-global digest
(`unit_fingerprint::statics_record_global_digest`) folds only versioned behavior/layout constants
(`STATICS_RECIPE_VERSION`, the shard-assignment and input-fingerprint domain magics, and the shard
count). It answers only "could identical
inputs produce different record bytes"; it is compared directly by the owner-partial planner and is
folded into the statics-domain digest so a recipe bump busts whole-bundle carry as well.

State schema v4 records, per shard, the ordered typed record keys and geometry totals
(`StaticShardState { input_digest, record_count, subset_count, vertex_count, triangle_count,
records }`), plus the top-level `statics_record_global_digest`. Validation requires each shard's
`records` to be strictly ascending by rendered bytes, unique, correctly assigned, and counted.

On a coarse miss, `plan_static_owners` combines exhaustive unit dirt with Changed binding consumers,
verifies Added/Removed binding membership invariants, closes dirty meshes over consuming merge
cells, and derives the exact shard set containing prior or current dirty-owner records. Those
shards alone are full-hash validated and decoded concurrently before state invalidation. The serial
assembly reproduces clean ordinary membership and clean-cell group ids, inserts actual non-empty
dirty outputs, and is the single source for splice membership, final counts, and usage ordinals.
Stable fail-closed codes identify unavailable evidence, recipe changes, shard integrity/decode
failures, clean-owner model gaps, and final assembly disagreement.
