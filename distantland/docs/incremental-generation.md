# Incremental generation (version 16)

The version-16 pipeline scales generation work to the changed inputs. It keeps one
complete-or-absent publication authority, and its output is logically equivalent to a forced clean
rebuild. Domain gates, unit fingerprints, fixed static shards, and canonical payload paths are in
place. Terrain-record reuse, static-shard reuse, atlas family reconciliation with binding deltas,
and owner-partial static CPU reuse on a coarse miss are all active. The atlas compaction and
post-optimization reuse investigation is retired, and its script,
`scripts/atlas_binding_cohort.py`, is deleted rather than left to rot.

## Core modules

- `crates/foundation/src/storage/`: lock, durable writes, path grammar, state codec, and lean writer session
- `src/generation/statics_stage.rs` and `terrain_stage.rs`: domain planning and canonical publication
- `crates/foundation/src/state_db.rs` and `src/generation/unit_fingerprint.rs`: generation state and unit digests
- `src/generation.rs`: sequencer, `CommitPlan`, and publication orchestration
- `src/generation/startup.rs`: lock-first ensure / status paths
- `crates/foundation/src/output_index.rs`: shared state-backed `OutputSnapshot`

## Authority set

- `generation_state.bin`: framed `TES3GCS1` state with a state body and required-artifact inventory
- `version`: single-byte format marker (16)
- all canonical `statics\static_meshes_000..127` shards and, when enabled, `terrain.bin` plus terrain companions
- generator-owned atlas pages under `statics\textures\`
- exclusive/shared `.writer.lock`

A file is live only when a Routine-valid state inventories it with matching length (and Full-mode
hash when requested). There is no journal, index slot, or epoch name.

## Decide before mutate

Every run completes the front half before authorizing writes. The no-op path performs no
state invalidation, state publication, durability flush, payload write, or pruning cycle.

## Dirty path summary

1. Exclusive lock held from classification through final validation.
2. Complete write/reuse plan and free-space preflight.
3. Invalidate existing valid state in place.
4. Write dirty required artifacts via `PublicationWrites`.
5. Publish complete checksummed state (commit point).
6. Prune superseded owned outputs; treat prune failures as cleanup warnings.
7. Write the optional non-authoritative generation report while the exclusive lock is still held.
8. Reload/Routine-validate the on-disk state under the exclusive lock, then release.

Interrupted runs after invalidation leave the cache absent. The run rebuilds older trees to
canonical version 16, and removes their journal/index/epoch evidence only after version-16
publication succeeds.

## Domain carry rules

- The statics domain digest is the coarse fast path for `usage.data` plus all 128 static shards.
- After a coarse statics miss, the global optimize/filter/merge-grouping prefix runs once. Complete
  typed unit dirt plus atlas binding deltas select dirty mesh owners and merge cells. Packing covers
  only their geometry. The run validates affected prior shards for length, hash, and decode, then
  splices them against the typed-key assembly. Clean shards are never opened. Any missing
  evidence or reproduction disagreement falls back to a complete rebuild. Global order is shard id
  followed by rendered key bytes, and the same assembly supplies `usage.data` ordinals.
- Atlas binding digest and locally versioned v5 atlas evidence (`ATLAS_CACHE_VERSION`) live in
  generation state. Shared config
  validates globally. Each family retains stable pages, monotonic active slots and reservations,
  logical-key relations, provider/content evidence, and complete bindings. Exact source/config
  equality carries without decoding. Otherwise a valid prior family reconciles membership, sizing,
  trim, and exact-dedupe changes: compatible slots survive, new groups reuse free space before page
  append, middle empty pages remain reusable, and trailing empty pages truncate. Only pages with a
  new slot or changed active placement/visible content rebuild; removal alone carries. A zero-write
  reconciliation commits new evidence and is distinct from an exact cache hit. Comparable families
  emit exhaustive binding-delta counts that feed the statics owner planner. After state commit, the
  run prunes recognized absent atlas paths and preserves unknown files.
- Terrain domain digest gates the whole package; a hit carries `terrain.bin` and all five DDS
  products untouched.
- On a terrain miss, the run decides mesh records, the DDS bundle, and occlusion independently:
  - Mesh records carry per work item. Each item declares the absolute terrain chunks it
    depends on and rebuilds only when one of them is dirty. Otherwise it carries its committed
    record, or its prior absence. Reuse first validates the committed payload's hash, header facts,
    and record count against persisted metadata. If any check fails it falls back to a full mesh
    rebuild with a stable reason.
  - The five DDS products form one coarse bundle gated by `terrain_dds_domain_digest`, which
    covers exactly their inputs and excludes height/normal/color. A height-only edit carries all
    five as `skipped_unchanged`.
  - Occlusion stays coarse. Its builder always scans the whole height domain, and its own
    byte-level content gate decides the write.
- `force_rebuild` performs no terrain-record, DDS, static-owner/shard, or atlas-page reuse, but publishes
  reusable metadata for the next run.
- Terrain-disabled states are valid without terrain files and carry no record metadata.

## Design constraints retained

1. Complete the full decision before the first dirty mutation.
2. Publish authority only after all required artifacts are durable.
3. Prefer full clean regeneration over partial recovery of torn trees.
4. Keep fingerprints byte-based and recipe-versioned.
5. Keep generator and current MGE-XE runtime as one supported format pair.
