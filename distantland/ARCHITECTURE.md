# Architecture

`distantland` is a Rust 2024 library (with a minimal CLI front end) that generates
Morrowind distant-land output compatible with MGE-XE. Given a Morrowind installation and an
active load order, it produces everything MGE-XE's runtime needs to render distant terrain
and distant statics: the packed static-mesh cache, the static usage table, generated texture
atlases, and the terrain package (`terrain.bin` plus five companion DDS files), together
with a machine-readable manifest describing the run.

This document is the primary architectural reference. Deep dives into individual subsystems
live in [docs/architecture/](docs/architecture/):

- [docs/architecture/vfs.md](docs/architecture/vfs.md) — the virtual filesystem (data dirs, BSAs, asset resolution)
- [docs/architecture/statics.md](docs/architecture/statics.md) — distant statics: scanning, NIF extraction, atlasing, optimization, merging
- [docs/architecture/terrain.md](docs/architecture/terrain.md) — the terrain package: layout, textures, meshes, control maps
- [docs/architecture/caching-and-startup.md](docs/architecture/caching-and-startup.md) — fingerprints, domain gates, complete-or-absent startup
- [docs/architecture/binary-formats.md](docs/architecture/binary-formats.md) — the MGE-XE on-disk formats this crate owns
- [docs/architecture/storage-foundation.md](docs/architecture/storage-foundation.md) — the normative complete-or-absent storage contract

## High-level data flow

```
Morrowind.ini / data dirs / load order
        │
        ▼
   Vfs (crates/vfs/src/lib.rs) ──────────────── case-normalized asset maps over loose files + BSAs
        │
        ▼
   StaticOverrides (.ovr files + plugin -metadata.toml)
        │
        ▼
   UsageInfo (crates/usage/src/info.rs) ─── objects, references, LAND heightmaps, LTEX tables,
        │                            interiors, per-mesh max scales
        ├──────────────────────────────────────────────┐
        ▼                                            ▼
   DistantStatics (NIF extraction)              TerrainAtlasLayout
        │                                              │
        ▼                                             │
   Static texture analysis + atlas packing             │
        │                                              │
        ▼                                            ▼
   Statics bundle stage                         Terrain package stage
   (optimize → filter → merge →                 (textures → meshes → control maps)
    convert → serialize)                              │
        │                                              │
        ▼                                            ▼
   statics\static_meshes_000..127               terrain.bin + 5 DDS files
   statics\usage.data                                  │
   statics\textures\_mge_xe_atlas*.dds                 │
        │                                              │
        └──────────────────┬───────────────────────────┘
                           ▼
             generation_report.toml + contract validation
```

## Entry points

There are two distinct ways generation is driven:

1. **`generate(&GenerationJob, &mut dyn ProgressReporter)`** ([src/generation.rs](src/generation.rs)) —
   one full pipeline run writing directly into the job's resolved output root. This is what the
   CLI ([src/main.rs](src/main.rs)) calls. Incremental gates inside the pipeline still let
   unchanged heavyweight stages be skipped.

2. **`ensure_generated(&GenerationJob, &mut dyn ProgressReporter)`** ([src/generation/startup.rs](src/generation/startup.rs)) —
   the startup handoff intended for a GUI/host. Under one exclusive writer session it classifies
   the store, then runs a single ensure/continue generation call: the front half reaches
   the unit-diff boundary once and either returns already-current (true no-op) or continues with
   the same live locals into planning and publication. Final on-disk Routine validation runs under
   that same exclusive session before release. Independent `check_output_status` remains a separate
   shared-snapshot read path.

The CLI is intentionally minimal — `--job <FILE>` (a TOML document containing the
`[generation]` namespace) plus `--force-rebuild` — and exists chiefly for profiling and
development runs. The loader ignores unrelated root tables and treats a missing namespace as
unconfigured defaults. Unknown owned keys and invalid values warn and fall back locally; malformed
TOML syntax remains an error. MGEXEgui is the only product writer and persists this namespace inside
`mgeXE.toml`.

## Pipeline walkthrough

`generate_with_output_root` ([src/generation.rs](src/generation.rs)) is a thin sequencer. Each
stage is wrapped in `run_stage`, which notifies the `ProgressReporter` and times the stage, and
each pipeline stage opens a `tracing` span whose elapsed time lands in the report's bounded,
flat `trace_summary.stage_timings` list (see [Tracing and metrics](#tracing-metrics-and-the-generation-report)).

Stages, in order (enum `GenerationStage` in [src/generation/progress.rs](src/generation/progress.rs)):

| Stage | What happens | Code |
|---|---|---|
| `InitializeVfs` | Resolve INI/data dirs/plugins/BSAs and build the caller-owned asset maps | [crates/vfs/src/loader.rs](crates/vfs/src/loader.rs) |
| `ParseOverrides` | Parse `.ovr` override files, then merge plugin `-metadata.toml` directives (in load order, after `.ovr` so mods win) | [crates/statics/src/overrides.rs](crates/statics/src/overrides.rs), [crates/statics/src/metadata.rs](crates/statics/src/metadata.rs) |
| `ParsePlugins` | Scan and merge active plugins into `UsageInfo`; capture canonical DL-relevant projections before reference remapping/interior filtering; then derive the filtered usage view and per-mesh scale maxima. Dedicated grass placements join the projection as statics-domain globals after their separate classification pass. | [crates/usage/src/info/load.rs](crates/usage/src/info/load.rs), [src/generation/projection.rs](src/generation/projection.rs) |
| *(inline)* | Build `TerrainAtlasLayout` from landscape cells — done once, reused by terrain texture and mesh stages | [crates/terrain/src/layout.rs](crates/terrain/src/layout.rs) |
| `GenerateStatics` | Extract a `DistantStatic` from each unique referenced NIF mesh | [crates/statics/src/extract.rs](crates/statics/src/extract.rs) |
| *(inline)* | Prune usage: drop references to filtered-out meshes, deep-water statics, and terrain-buried (low-visibility) statics | [crates/usage/src/info/filter.rs](crates/usage/src/info/filter.rs) |
| `AnalyzeTextureDensity` | Single hoisted read pass over all atlas-eligible source textures: fingerprints, geometry-informed sizing plan, dedupe reconciliation, and bounded aggregate sizing metrics | [crates/statics/src/atlas/sizing/](crates/statics/src/atlas/sizing/) |
| *(inline decide)* | Prepare terrain inputs, build terrain state plus the terrain-domain and DDS-domain digests, and serialize/hash occlusion without writing; the package gate is decided after the unit diff | [src/generation/terrain_stage.rs](src/generation/terrain_stage.rs) |
| `ComputeUnitDiff` | Compare five immutable unit tables and two domain globals with the previous generation state, producing the bounded unit report plus the complete typed dirty-chunk set terrain planning consumes | [crates/foundation/src/state_db.rs](crates/foundation/src/state_db.rs), [src/generation/unit_fingerprint.rs](src/generation/unit_fingerprint.rs) |
| `CreateTextureAtlas` | Validate v3 shared/family page-slot evidence, exact-carry or reconcile opaque and alpha independently with stable reservations and seeded free space, emit binding deltas, mark pages `Carry` or `Build`, bake final page ids and UV bounds into statics, and retain normalized pixels required by dirty pages; no atlas output is written | [crates/statics/src/atlas/manager.rs](crates/statics/src/atlas/manager.rs), [crates/statics/src/atlas/reconcile.rs](crates/statics/src/atlas/reconcile.rs), [crates/statics/src/atlas/plan.rs](crates/statics/src/atlas/plan.rs) |
| *(inline decide)* | Feed the atlas-binding digest into the statics-domain gate; on an eligible miss, close typed owner dirt over binding deltas, meshopt only dirty meshes and dirty-cell partners while restoring cached post-optimize bounds for clean meshes, then globally filter/group; otherwise meshopt globally. Validate/decode affected prior shards, build dirty merge geometry, splice the authoritative typed-key assembly, and serialize usage without writing | [src/generation/statics_stage.rs](src/generation/statics_stage.rs), [src/generation/statics_stage/plan.rs](src/generation/statics_stage/plan.rs), [src/generation/unit_fingerprint.rs](src/generation/unit_fingerprint.rs) |
| *(inline)* | Form one `CommitPlan` containing atlas/statics/terrain decisions, unit reporting, deferred small outputs, cumulative preflight, and an empty `PublicationWrites` ledger; a fully clean plan exits as a true no-op | [src/generation.rs](src/generation.rs) (`CommitPlan`), [crates/foundation/src/commit.rs](crates/foundation/src/commit.rs) (`PublicationWrites`) |
| *(dirty)* | Invalidate existing valid state in place; carry clean atlas inventory entries and stream only `Build` pages; write version when required | [crates/foundation/src/storage/authority.rs](crates/foundation/src/storage/authority.rs), [crates/statics/src/atlas/plan.rs](crates/statics/src/atlas/plan.rs) |
| `WriteVersionFile` | Consume the precomputed byte-equality decision and write the single-byte version marker only when changed | [src/generation.rs](src/generation.rs) |
| Statics bundle | Publish deferred `usage.data`; carry all 128 static shards on a coarse hit, or serialize only dirty `statics\static_meshes_000..127` shards sequentially on one background thread | [src/generation/statics_stage.rs](src/generation/statics_stage.rs) |
| `WriteTerrainPackage` | Publish occlusion only when its prepared bytes changed; carry the whole package on a hit, otherwise rebuild only mesh work items with a dirty chunk dependency, carry the five DDS products when their domain digest is unchanged, and stream canonical `terrain.bin` on a background thread | [src/generation/terrain_stage.rs](src/generation/terrain_stage.rs) |
| *(publish)* | Join payload writers, run the sync barrier, check the write ledger against the inventory, publish complete `generation_state.bin`, prune superseded owned files, write the advisory generation report, and Routine-validate under the exclusive lock | [crates/foundation/src/storage/authority.rs](crates/foundation/src/storage/authority.rs) |
| `WriteGenerationReport` | Assemble trace/metrics observability and write the advisory `generation_report.toml` after state publication; it is absent from the committed inventory | [src/generation/output/manifest.rs](src/generation/output/manifest.rs) |

Two scheduling details matter:

- **Dirty static-shard serialization and disk writes run on one background thread.** The fixed
  128-shard set can be multi-GB; dirty shards are serialized and durably written sequentially after
  the thread is spawned, while unchanged shards carry their inventory entries untouched. This work
  starts before terrain materialization so it overlaps terrain CPU work. Terrain file I/O is then spawned on
  its own background thread. The orchestrator joins both — including on error paths — so an error
  never unwinds past an in-flight write. Each writer hashes while streaming; payload durability is
  established by the pre-state sync barrier, which syncs every buffered payload handle immediately
  before `generation_state.bin` is published.
- **The 128 static shards and `usage.data` form one logical bundle.** `usage.data` stores indices
  into the global shard-major/key-byte order. A coarse domain hit carries the whole bundle; on a
  miss, usage is reserialized from one typed-key assembly while dirty owners are packed and only
  affected shards are validated, decoded, spliced, and conditionally rewritten. Clean shards stay
  unopened. Fail-closed planner/splice trips complete the full build before publication.
- **Publication is evidence-checked.** Every authoritative output writer records its path, byte
  length, and BLAKE3 into the run's `PublicationWrites` ledger. Before the state write,
  `finish_publish` requires each recorded write to match the inventory entry it is about to
  publish, so an accidental overwrite of a carried artifact or a write missing from the inventory
  fails while state is still invalid. State invalidation/publication and pruning remain exclusively
  owned by `WriterSession`.

## Repository layout

```
src/
  lib.rs              public facade and compatibility re-exports
  main.rs             minimal --job CLI (profiling/dev); GUI hosts call the library directly
  generation.rs       pipeline orchestration            generation/   job, progress, metrics,
                                                                      identity, cache, output, startup,
                                                                      statics_stage, terrain_stage
                      mge_xe, vfs, usage, statics, terrain, and nif are crate aliases in lib.rs
crates/
  formats/            MGE-XE binary codecs and format records
  texture/            shared DDS, decode/resize, and exact-dedupe primitives
  diagnostics/        tracing subscriber setup and the in-process stage-timing collector
  foundation/         storage authority, state DB, identities, units, output contract, output_index
  vfs/                data-dir/BSA resolution and normalized asset maps
  usage/              load-order scanning and plain world/reference/terrain source data
  statics/            NIF extraction, atlasing, optimization, merging, and serialization
  terrain/            layout, source textures, meshes, and package construction
  job/                generation job schema, validation, and path resolution
  test-support/       dev-only hermetic fixtures and targeted static-output comparison
tests/                integration tests + the bundled MGE-XE default classifier .ovr asset
```

Dependencies point upward through the list only as shown by the domain graph: formats, texture, and
diagnostics are leaf utilities with no workspace dependencies of their own (diagnostics is domain-free
and compiles in parallel with everything else); foundation depends on formats; VFS depends on
foundation; usage depends on VFS;
statics and terrain are sibling domains above those shared crates; job sits above them, depending on
statics, terrain, usage, vfs, and foundation for the constants and types its settings validate
against; the root package alone owns
orchestration, publication, metrics, and the public compatibility facade. External hosts can
therefore continue to use `distantland::{generate, GenerationJob, Vfs, ...}` while Cargo
recompiles only the affected lower crate.

The job schema is a leaf crate rather than a root module so `crates/test-support` can build jobs
without depending on the root package, which dev-depends on test-support in turn. Cargo tolerates
that cycle; rust-analyzer cannot represent it, and it forced test-support's `GenerationJob` and the
root crate's unit-test `GenerationJob` to be distinct types bridged by a serde round-trip.

## Configuration: jobs and settings

`GenerationJob` ([crates/job/src/lib.rs](crates/job/src/lib.rs), re-exported as
`distantland::generation::job`) is the full request:

- `morrowind_ini` — optional; auto-discovered (registry / default install) when omitted.
- `data_dirs` — optional explicit data-directory layers; later dirs override earlier ones.
- `plugins` — optional explicit load order (preserved exactly); otherwise read from the INI's
  `[Game Files]`, sorted masters-first then by modification time.
- `grass_plugins` — optional generator-only grass/groundcover plugin list, in load order. Bare
  names resolve against `data_dirs`. These files never enter the active game load order, but they
  do form an override chain among themselves: a later entry can override or delete an earlier
  entry's placements, and can place references to grass statics an earlier entry defines.
- `output_root` — outputs are written under `<output_root>\distantland\`; defaults to the
  active VFS data directory.
- `settings` — `GenerationSettings`.

On disk the job is the root `[generation]` table of an `mgeXE.toml` document
(`GENERATION_JOB_NAMESPACE`), versioned by `GENERATION_JOB_FILE_VERSION`. A version mismatch warns
but does not gate known fields; the current version is emitted on save. The job's fields sit
directly in `[generation]` alongside `version`, with only `settings` descending into
`[generation.settings]`:

```text
[generation]
version = 3
plugins = [...]
output_root = "Data Files"

[generation.settings]
...
[generation.settings.static_texture_sizing]
```

`GenerationJobFile` (read) and `GenerationJobFileRef` (write) mirror `GenerationJob`'s fields by
hand. Unknown owned keys are collected as warnings and disappear when MGEXEgui serializes the current
table. Adding a job field fails compilation until mirrored on both sides:
`GenerationJobFileRef::new` destructures `GenerationJob` exhaustively, and
`GenerationJobFile::into_job` constructs it exhaustively.

`serialize_generation_job_document` emits the table with a one-line comment above each key, to
match the house style of the rest of `mgeXE.toml`. MGE-XE's `mge-config` splices that string into
the live document verbatim and positions it after all other tables, so the formatting chosen here
is what reaches disk. Relative paths keep process-current-directory semantics unless the host
calls `resolve_generation_job_paths` to rebase them against the job file's directory.

`GenerationSettings` highlights (all serde-defaulted; `validate()` enforces ranges):

- **Statics**: `min_static_size`, `door_size_multiplier` (inflate doors' effective size so they
  stay with their buildings), inclusion toggles (activators, misc, interior categories),
  `grass_density`, static mesh simplifier knobs (`static_mesh_target_error`, normal/color
  weights), `merge_group_radius` (BVH merge-group spatial bound),
  `static_mesh_merge_error_multiplier`
  (per-subset cap on additional merge-stage simplification; `1.0` disables the repeated pass),
  `exclude_script_disable_targets` (drop persistent references named by a script's
  `SomeId->Disable`; the object's own attached-script disable is unconditional and has no toggle), and
  `deep_water_static_cull_depth` (distance in game units below water level at which non-grass references are culled).
- **Static textures**: `max_static_texture_long_axis` / `max_static_texture_short_axis`
  (independent per-axis caps, resolved per texture into one longest-side limit and floored by
  what fits one atlas page — see `TextureAxisCaps`), `static_texture_sizing`
  (geometry-informed downscaling —
  `Off`/`Report`/`DownscaleOpaque`/`Downscale` with a calibrated `protected_density`),
  `texture_dedupe_mode` (exact dedupe, default `Exact`).
- **Terrain**: `generate_terrain` toggle, `terrain_detail` preset (maps to a meshopt absolute
  target error), smoothed-normal and vertex-color mesh attribute weights,
  `max_terrain_texture_size` (logical tile size), control-map guards
  (`max_terrain_control_texture_size`, `max_terrain_control_texture_bytes`).
- **Overrides**: `use_override_list` + `override_files` (legacy `.ovr`), `use_plugin_metadata`
  (auto-discover `-metadata.toml` next to active plugins; see
  [Overrides and plugin metadata](docs/architecture/statics.md#overrides-and-plugin-metadata)).
- **Cache control**: `force_rebuild` bypasses every fingerprint gate and cache read.

Defaults for most numeric settings live as documented constants in their domain crate roots ([crates/statics/src/lib.rs](crates/statics/src/lib.rs), [crates/terrain/src/lib.rs](crates/terrain/src/lib.rs)) and are gathered in `GenerationSettings::default` ([crates/job/src/job.rs](crates/job/src/job.rs)).

## Output contract

`OutputPaths` ([crates/foundation/src/output.rs](crates/foundation/src/output.rs)) defines
the canonical layout beneath the output root. The MGE-XE contract (checked by
`validate_mge_xe_contract`) comprises:

```
distantland\version                          single byte = MGE_DL_VERSION
distantland\generation_state.bin             sole publication authority (TES3GCS1)
distantland\terrain.bin                      world-space terrain payload (when enabled)
distantland\terrain_atlas.dds                BC1 source-texture atlas (per-tile mips, wrap gutters)
distantland\terrain_material.dds             RGBA8 control map: base/decal material ids per patch
distantland\terrain_material_flags.dds       RGBA8 control map: blend-pattern id + flags per patch
distantland\terrain_patch_albedo.dds         BC1+mips averaged per-patch albedo
distantland\terrain_blend_patterns.dds       RGBA8 atlas of distinct 5×5 blend-alpha patterns
distantland\terrain_occlusion.bin            horizon occlusion base grid
distantland\generation_report.toml           optional advisory observability record of the run
distantland\statics\usage.data               static usage table + dynamic vis groups + interiors
distantland\statics\static_meshes_000..127   fixed packed static-mesh shards (XESTAT05 v5)
distantland\statics\textures\_mge_xe_atlas*.dds        opaque atlas pages
distantland\statics\textures\_mge_xe_atlas_alpha*.dds  alpha atlas pages
distantland\.writer.lock                     exclusive-writer/shared-reader ownership
```

The [storage facade](crates/foundation/src/storage.rs) owns the lock, durable/fault writers, path
grammar, state codec, and lean writer session. The public read-only
[output-index facade](crates/foundation/src/output_index.rs) (re-exported at `src/lib.rs`) opens a state-backed `OutputSnapshot` under a shared
pin and validates its inventory; every other version is rejected. Explicit Full validation hashes
payloads. The host requests Routine header/length validation, retains the pin for its session, and
loads the fixed canonical payload paths. The C++ client checks the version byte and opens those
same canonical paths.

No legacy `world`, `world.dds`, or `world_n.dds` files are emitted. Geometry-informed texture
sizing publishes no report of its own: the decisions reach output only through atlas dimensions,
and the run's proposed-reduction count is visible on the `stage.analyze_texture_density` span. An older tree is never adopted as cache: after a successful
publication, superseded journal/index/epoch evidence and cleanup targets are pruned. This includes
the former JSON observability files, but cleanup happens only on a real publication, not an
already-valid no-op.

## Caching and incremental rebuilds (summary)

Full details in [docs/architecture/caching-and-startup.md](docs/architecture/caching-and-startup.md).
The design has four cooperating layers:

1. **Domain digests** ("would regeneration produce the same thing?") — canonical hashes of the
   unit model. The statics digest covers sorted mesh/merge tables, global statics
   facts, serializer version, and atlas bindings. The terrain digest covers sorted cell/chunk
   tables plus complete prepared global facts. Atlas evidence is part of the embedded generation state.
2. **Committed state inventory** — required-artifact entries bind canonical paths, lengths, kinds,
   and content hashes; the state body carries unit tables, reverse indexes, domain globals, and
   atlas metadata used by the next run.
3. **Startup input validation** — request and load-order identities provide early diagnostics.
   `check_output_status` runs the full VFS → projections → referenced mesh/texture and
   resolution-fact → unit-diff front half under a shared snapshot. `ensure_generated` runs that
   same front half once under the exclusive writer and continues into publish when dirty, without a
   second front-half reconstruction.
4. **Complete-or-absent authority** — one exclusive writer invalidates, writes dirty artifacts, then
   publishes the complete state and Routine-validates the reloaded inventory before release; shared
   runtime pins keep a Routine-valid tree stable until the session ends. A complete no-change run
   writes nothing.

All content hashes are BLAKE3. `force_rebuild` defeats reuse gates but still publishes through the
same state-file authority.

## Tracing, metrics, and the generation report

Three complementary observability mechanisms:

- **`tracing` spans** — `TraceReportLayer`
  ([crates/diagnostics/src/tracing_report.rs](crates/diagnostics/src/tracing_report.rs)) captures
  elapsed time for `generation` and the existing `stage.*` pipeline spans. The report embeds total
  elapsed milliseconds plus at most 128 stage entries in opening order; arbitrary fields and
  parentage are intentionally not persisted. On Windows, a completed stage entry also carries
  optional process-wide `private_bytes_at_end` (`PrivateUsage`) and
  `peak_working_set_bytes_at_end` (`PeakWorkingSetSize`) samples taken when the span closes. Active
  snapshot entries and non-Windows reports omit memory. Nested `stage.*` spans are inclusive, so
  compare private-byte deltas between siblings; the working-set field is the process-lifetime high
  water mark as of close, not a per-stage or private-byte peak.
- **`GenerationMetrics`** ([src/generation/metrics.rs](src/generation/metrics.rs)) — structured
  per-stage counters (reference counts before/after each prune, static/subset/vertex/triangle
  totals, merge-simplification target/cap/cache distributions, atlas family layout hits plus
  carried/built page and byte counts, terrain sizes, and storage I/O accounting) plus
  `CacheMetadata` recording each output's `OutputWriteDecision`
  (`Written` / `SkippedUnchanged` / copy variants) and gate hit flags. The report is advisory and
  developer-facing, so a counter earns its place either by having a reader — MGEXEgui reads
  `gpu_memory`, `atlas_size`, the two terrain byte estimates and `usage.burial`, and the
  incrementality and interruption suites assert on much of the reuse accounting — or by answering a
  question someone debugging a run actually asks, like the per-record carried/rebuilt/added/removed
  breakdown. Byte tallies, wall-clock microseconds, and peak-memory estimates are not that: stage
  timings live in `TraceSummary.stage_timings`, and VFS sizes, dedupe stats, sizing aggregates, and
  unit-model counts are reported through `stage.*` span fields instead.
- **`GenerationWarning`s** — stable-coded, human-readable warnings (e.g.
  `terrain_package_disabled`) surfaced in both the TOML report and the `GenerationReport`.

The TOML generation report ([src/generation/output/manifest.rs](src/generation/output/manifest.rs))
is human-facing advisory observability: report version, effective settings, request and load-order
identity, metrics, bounded `units` dirty sets, warnings, flat stage timings, and
contract-validation results. It contains no authoritative artifact inventory. Routine startup
does not parse it; `generation_state.bin` alone owns complete-or-absent publication state,
canonical artifact paths, lengths, hashes, and cache state.

## Concurrency, allocation, and determinism

- **Parallelism** is `rayon` throughout: plugin parsing, NIF extraction, loose-file scanning,
  per-cell merge grouping, atlas page encoding, BCn compression bands, terrain mesh chunks, and
  texture loading all fan out over the thread pool. The one *thread* (not pool) spawn is the
  background static-shard writes described above.
- **Allocators**: the binary uses mimalloc as the global allocator;
  `init_meshopt_allocator` routes meshopt's internal C++ scratch allocations through mimalloc
  too (the Rust `#[global_allocator]` doesn't cover `operator new`).
- **Hashing**: `IndexMap`/`IndexSet` aliases in [src/lib.rs](src/lib.rs) use hashbrown's fast
  non-DOS-resistant `DefaultHashBuilder` — keys are trusted game data; throughput wins.
- **Determinism is a hard requirement**: `usage.data` indexes into the concatenated static shards
  by position; global order is shard id first and normalized key bytes second,
  and fingerprint gates hash ordered content. Therefore parallel stages must not let traversal
  order leak into output: `UsageInfo::sort_for_deterministic_output` and
  `finalize_distant_statics` applies shard-major/key-byte order before serialization, merge groups sort members by
  reference key, and the sizing plan/report use sorted entries. When touching parallel code,
  preserve this property. The request, committed-settings, and load-order identities use separate
  versioned `CanonicalWriter` domains rather than serialized TOML. Their coordinated conversion is
  an intentional one-time full distant-land rebuild boundary for this pre-release format.

- **Caller-owned VFS**: the generation pipeline retains the `Vfs` loaded by `InitializeVfs` and
  passes `&Vfs` through every asset-consuming stage. Applications that need hot reload own that
  replacement policy outside the asset library.

## Key dependencies

| Crate | Role |
|---|---|
| `tes3` (git, with `bytes_io`) | ESM/ESP plugin parsing, NIF streams, BSA archive reading (`tes3::bsa`) |
| `meshopt` | mesh simplification (attribute-aware), vertex-cache optimization, remapping |
| `obvhs` (git) | BVH construction for exterior-reference merge grouping |
| `minsphere` (git) | minimum bounding spheres for subsets |
| `tex-packer-core` | rectangle packing for static atlas pages (pinned to 0.1.0; 0.3.0 is an API break) |
| `intel_tex_2` | BC1/BC3 block compression kernels (parallelized in [crates/texture/src/dds.rs](crates/texture/src/dds.rs)) |
| `image`, `image_dds`, `fast_image_resize` | texture decode, DDS parsing, high-quality resize |
| `blake3` | every fingerprint in the caching system |
| `rayon`, `mimalloc` | parallelism and allocation |
| `tracing` + `tracing-subscriber` (+ optional `tracing-tracy`) | observability; owned by `crates/diagnostics`, whose `trace_tracy` feature the root forwards to enable Tracy capture |

The workspace requires nightly Rust (the statics crate uses `trim_prefix_suffix`, and shared
dependencies enable nightly features). `missing_docs` is denied workspace-wide — every public item
needs a doc comment.

## Development workflow

- `cargo check --workspace` / `cargo fmt --all` / `cargo clippy --workspace` /
  `cargo test -p distantland -p 'distantland_*'` — the standard loop. That scope keeps the
  32-bit crates out; see BUILD.md for the full-workspace sweep and its four exclusions.
- Profiling: `cargo run --release --features trace_tracy -- --job <job.toml>` for Tracy
  timelines (set `$env:RUST_LOG = "distantland=info"` first so INFO spans are emitted);
  the generation report `trace_summary` for quick span-level timing without external tools.
- Tests live in `#[cfg(test)] mod tests` files colocated with their subjects (e.g.
  `crates/terrain/src/package/tests/`), plus end-to-end checks in
  [tests/integration_tests.rs](tests/integration_tests.rs). For local end-to-end runs, pass a
  job file with `--job <file.toml>`; job files carry absolute plugin paths, so keep yours
  untracked.

## MGE-XE compatibility references

The MGE-XE runtime is the ground truth for binary format expectations, shader-coupled
layouts, and override semantics. Behavioral parity notes worth knowing:

- The minimum-size static filter runs *after* mesh optimization (final bounds, not raw source
  geometry), matching MGE-XE.
- The `version` file byte must equal `MGE_DL_VERSION`, matching
  `d3d8/cpp/mge/mgeversion.h` and `mgeHost64/src/abi/constants.rs` (paths from the repo
  root). The GUI keeps no copy of its own — it reads this crate's `MGE_DL_VERSION`.
- The packed vertex `uv_bound` lane order and the terrain vertex packing are shader-coupled;
  see [docs/architecture/binary-formats.md](docs/architecture/binary-formats.md).
