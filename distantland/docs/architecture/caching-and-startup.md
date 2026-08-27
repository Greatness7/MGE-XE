# Caching and startup

The output store uses the complete-or-absent state authority and fixed static shards at
canonical payload paths. This document describes fingerprints, domain gates, startup handoff, and
how no-op versus dirty publication interact with that store.

## Overview

- Writers take the exclusive `.writer.lock` before identity/diff work and hold it through state
  publication, pruning, and final Routine validation.
- Runtime readers take a shared lock, validate `generation_state.bin` and its inventory, and retain
  the pin for the session.
- Domain gates (statics bundle, atlas, terrain package, occlusion) still decide reuse versus
  rebuild from unit fingerprints and the Routine-validated base inventory.
- `generation_report.toml` is advisory observability only; startup does not parse it and it cannot
  make a valid state invalid.

Implementation entry points:

- `crates/foundation/src/storage/authority.rs`, exclusive writer session
- `crates/foundation/src/storage/state.rs`, state codec, inventory, invalidate/publish, artifact checks
- `crates/foundation/src/output_index.rs`, shared snapshot reader
- `src/generation/startup.rs`, `check_output_status` / `ensure_generated`
- `src/generation/unit_fingerprint.rs` / `crates/foundation/src/state_db.rs`, generation state

## Decide phase (no writes)

Every run completes the front half (VFS, overrides, plugins, projections, statics,
texture analysis, terrain preparation, unit fingerprints) before authorizing mutation. The
result is a `CommitPlan`:

- atlas / statics / terrain publish modes (reuse versus rebuild).
- version carryability.
- unit report.
- cumulative free-space preflight for dirty canonical writes.
- an empty `PublicationWrites` write ledger.

If the base state is Routine-valid, domains are hits, units are clean, and `force_rebuild` is
false, generation takes the true no-op path. It performs no state write, durability flush, report
rewrite, or artifact mutation.

Metrics need one distinction. `generation_report.toml` describes the run that produced the current
output, not the most recent invocation. A true no-op leaves the previous run's report, including
its `trace_summary` and `metrics`, untouched. Do not read report timings as "the last run's"
without confirming that the run published. A no-op's real cost is observable only through tracing
output, not the report. The front half still runs in full before the no-op exit (see the decide-phase
sequence below). A no-op is not free even though it writes nothing.

## Dirty publication

1. Free-space preflight for the planned dirty set.
2. In-place state invalidation (zero first eight magic bytes + `sync_all`) when a valid state exists.
3. Write dirty required artifacts through `PublicationWrites` (version, usage, static shards, atlas
   pages, terrain package, occlusion as needed). Background threads may stream multi-gigabyte
   payloads while hashing. Small artifacts sync at write time. The writer buffers bulk payloads, and
   their flushed handles await the sync barrier. Every write records its path, length, and hash.
4. Run the sync barrier. Sequentially call `sync_all` on every pending payload handle. Check every
   recorded write against the inventory about to be published. Then encode and durably write the
   complete `generation_state.bin` at the publication boundary.
5. Write the non-authoritative generation report. It is absent from the committed inventory.
6. Prune superseded owned files (including version-13 evidence). Prune failures produce warnings.
   Preserve unrecognized regular files and report them with a bounded path sample.
7. Routine-validate under the exclusive lock.
8. Release the lock.

Interrupted runs after invalidation leave the cache absent. The next successful run fully
regenerates.

## Domain gates

### Statics bundle

`usage.data` and the 128 `static_meshes_000..127` files remain one logical bundle because usage
references address the global shard-major/key-byte order. A coarse statics-domain hit carries all
129 entries without reading shard bodies. On a miss with comparable schema-v8 evidence
(`STATE_FORMAT_VERSION`), the producer closes typed mesh/merge dirt over atlas binding deltas. It meshopts only dirty meshes and
dirty-cell partners, restores clean meshes' post-optimize bounds from generation state, and then runs
the global size-filter/grouping membership prefix. Missing bounds or a broad optimize set expand
meshopt work. Failed owner-partial preconditions use the full meshopt pass. The producer reads dirty
prior shards before invalidation. They must match inventory length/full BLAKE3, decode structurally,
and positionally match their persisted key lists. The producer splices them against an authoritative
typed-key assembly. Clean shards remain unopened. Any failed precondition or reproduction check
falls back to the complete build. Force rebuild and migration carry no shards. A shard-integrity
fallback also forbids carrying suspect bytes. The generation report field
`cache.static_shards.reuse_mode` reports
`carried_bundle`, `owner_partial`, `full_rebuild`, or `forced_full`.

### Atlas

Atlas cache evidence lives inside generation state (`atlas_cache` bytes and binding digest). A hit
means both opaque and alpha exact-carried unchanged v5 family evidence (`ATLAS_CACHE_VERSION`)
and committed pages. Schema,
version, and shared packing-config failures reject both priors. Structural or inventory failures
reject only the affected family. Family validation checks the persisted old key/slot/binding
relations internally. This lets membership, sizing, trim, and exact-dedupe changes reconcile against
stable reservations. New groups reuse reconstructed free space before pages append. Empty middle
pages carry for reuse, and only empty trailing pages truncate. The atlas builder creates pages only
for new slots or changed active placement/visible content. Removal and identical-content provider
promotion alone do not rewrite a page. A zero-write reconciliation is not a cache hit. It serializes
new evidence. The
following unchanged run can exact-carry and no-op. Only built paths are authorized. Post-commit
pruning removes recognized atlas names absent from the new inventory while preserving unknown files.
Complete final bindings continue to feed the coarse statics-domain gate.

### Terrain

When `generate_terrain` is false, the state has `terrain_enabled == false` and no terrain inventory
entries. This is a valid statics-only tree. When enabled, the package gate and occlusion gate may
diverge. Occlusion damage costs one file rewrite, not a full terrain rebuild. An invalid inventory
still fails the whole base and forces clean regeneration rather than partial recovery.

## Startup handoff

`ensure_generated`:

1. Resolves enough of the job to identify `output_root` (metadata-only VFS).
2. Acquires the exclusive writer lock.
3. Classifies the store under the lock and builds the pre-run `OutputStatus` (reusing the metadata
   VFS; no second metadata load).
4. Enters `run_generation` once in ensure/continue mode with the same writer session.
5. Executes the normal front half through the unit diff (`state_db::diff`).
6. If the base was eligible for `AlreadyValid` and the unit diff is clean, call `finish_noop` and
   return `AlreadyValid`. No plan, state, report, artifact, or durability write.
7. Otherwise retain the live front-half locals and continue into terrain/atlas/statics planning and
   publication (no second `run_generation` call).
8. A dirty commit runs invalidate → write artifacts/report → publish state → prune, then
   reload/Routine-validate `generation_state.bin` under the exclusive session before release.
9. Map `AlreadyCurrent` → `AlreadyValid` and `Published` → `Generated` without a final
   `check_output_status` scan. `previous_status` is the pre-run classification (including Stale when a Valid base was dirty).

`check_output_status` remains an independent read-only API. It opens a shared snapshot for version
16, then runs its own front half via `generation_inputs_are_current`. It does not share the
ensure continuation path. Older versions are `Invalid` with an incompatible-version code. Future
versions refuse without mutation.

Dirty inspection produces a schema-3 `UnitsReport`, including an explicit settings-identity change
flag. That report produces one deterministic human-readable rebuild cause. The info log, generation
report schema 3 (`GENERATION_REPORT_VERSION`), and `Stale` status issue use it. The structured report remains the source of truth,
and the text never drives builder decisions.

## Fingerprints

Fingerprints use the exact bytes consumed, not filesystem metadata. The request, committed-settings,
and load-order identities use explicit little-endian, length-prefixed `CanonicalWriter` encodings
with separate domain tags and versions. Settings identity excludes execution-only `force_rebuild`
so a forced clean rebuild does not alter later no-ops. Their coordinated format conversion causes
one intentional full rebuild. Per-unit recipe versions invalidate algorithms independently (see
`state_db` / `unit_fingerprint`).

## Metrics

Durability and storage-I/O metrics report:

- `payload`, canonical bulk payload writes (static shards, atlas pages, `terrain.bin`, terrain
  DDS); sync time accrues at the pre-state sync barrier;
- `small_artifact`, version, usage, occlusion (synced at write time);
- `state`, invalidation and final state publication;
- `prune`, post-publication owned cleanup.

Generation-report metrics are non-authoritative evidence only. `cache.static_shards` reports reuse mode,
fixed shard count, carried/written counts and bytes, and written shard ids. `metrics.atlas` reports
family layout hits, decoded sources, carried/built page counts, and carried/written bytes.
