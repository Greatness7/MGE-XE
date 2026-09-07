# Storage foundation

Normative contract for the complete-or-absent distant-land store. The current format is the only
supported writer/reader format. The storage layer treats older generated trees only as material that
needs a clean rebuild. It refuses future versions without modifying generated output.

The implementation lives in `crates/foundation/src/`. The crate owns storage authority, durable
writes, state framing and validation, canonical output paths, unit keys, and context-free content
identity primitives. This layer now owns three values formerly owned by upper layers:
`TerrainMeshWorkKey` with the unit keys, `OPAQUE_ATLAS_PREFIX` with output path grammar, and
`MeshResolutionRule` with mesh-resolution identity facts. The root facade re-exports the public
values needed for compatibility.

Foundation owns byte/file identities and canonical load-order assembly: `load_order_identity`
and `collected_load_order_identity` (in `crates/foundation/src/identity.rs`) compute the identity
from collected file identities, and the root pipeline gathers those identities through VFS access
and statics metadata discovery.

## Authority model

| Component | Rule |
|---|---|
| Output format version | single-byte `distantland\version` equal to `MGE_DL_VERSION` |
| Publication authority | `distantland\generation_state.bin` only |
| Writer lock | exclusive `distantland\.writer.lock` for the full decide/publish lifecycle |
| Reader lock | shared `.writer.lock` for the lifetime of an `OutputSnapshot` |
| Payload paths | canonical only, with no epoch aliasing |

A missing, invalid, or incomplete state means no cache is available. There is no journal,
A/B index, generation number, or host-to-client path descriptor. The Rust reader validates the
state and inventory under a shared lock; the C++ client opens the fixed canonical payload paths
after its own version-byte check and retains existing static/terrain header validators.

## Canonical layout

Under the selected output root:

- `distantland\version`
- `distantland\generation_state.bin`, the sole publication authority
- `distantland\statics\usage.data`
- all 128 `distantland\statics\static_meshes_000..127` shards
- generator-owned atlas pages under `distantland\statics\textures\`
- when terrain is enabled:
  - `distantland\terrain.bin`
  - `distantland\terrain_atlas.dds`
  - `distantland\terrain_material.dds`
  - `distantland\terrain_material_flags.dds`
  - `distantland\terrain_patch_albedo.dds`
  - `distantland\terrain_blend_patterns.dds`
  - `distantland\terrain_occlusion.bin`
- `distantland\.writer.lock`

Optional observability products (not authority, not required for serving):

- `distantland\generation_report.toml`

## State file

`generation_state.bin` uses the shared framed-archive envelope with magic `TES3GCS1`, schema
version 1, reserved zeroes, and a BLAKE3 checksum of the complete body. The body is:

```text
CommittedState {
    state_bytes: Vec<u8>,      // independent state_db::encode bytes
    artifacts: Vec<RequiredArtifact>,
}

RequiredArtifact {
    kind: ArtifactKind,
    relative_path: String,        // relative to distantland, backslash grammar
    byte_length: u64,
    content_blake3: [u8; 32],
}
```

Invariants:

- artifact entries appear in normalized relative-path byte order and are unique;
- paths are relative to `distantland`, use the canonical lower-case ASCII grammar, and cannot
  contain absolute or parent-directory components;
- exactly one version and usage entry plus all 128 canonical static-shard entries exist;
- atlas entries exactly describe the committed generator-owned atlas pages;
- terrain enablement in generation state agrees with terrain inventory: all six terrain package
  outputs plus occlusion when enabled, none otherwise;
- the state file, writer lock, and generation report never inventory themselves;
- header expectations derive from current `ArtifactKind` constants, not stored per-entry versions.

`ArtifactKind` distinguishes version, usage, static shard, atlas DDS, terrain payload, terrain
DDS, and terrain occlusion.

## Classification under the writer lock

1. Refuse a version greater than `MGE_DL_VERSION` immediately. Do not inspect, adopt, clean, or overwrite.
2. Treat a matching version byte plus a decodable state whose required artifacts pass Routine validation as a valid base.
3. Missing version/state, an older version, invalid/torn state, invalid inventory, or a missing,
   wrong-length, or wrong-header required artifact means the cache is absent. Force a full clean regeneration.

Do not recover a partially written tree. Invalid state is the recovery decision.

## Validation modes

- Routine serving validation checks existence, exact length, and fixed current headers
  (`version` byte, `XESTAT06`/v6, `XELAND02`/v2, `XEOCCL02`/v2, DDS signature/header). It binds
  every static-shard header count to generation state and the checked sum of all 128 counts to the
  leading `usage.data` static count, without hashing shard bodies.
- Full explicit verification also hashes every listed artifact and compares `content_blake3`.

Validation and serving ignore unknown unlisted files.

Full and `content_blake3` stay coupled. Committed hashes have production consumers in incremental
reuse (dirty static-shard decode, `usage.data` byte-identity, terrain carry/partial-record reuse),
and most of them fail soft. A wrong recorded hash degrades incremental generation into repeated
full rebuilds rather than crashing. Full is the only assertion that stored hashes describe the
published bytes. Removing it while the field remains would delete the checker but keep the checked
value. Reconsider both together after the coarse statics/terrain reuse redesign removes the
production consumers. The publication write ledger does not replace Full. It proves only that
artifacts *this run wrote* match the new inventory, never that carried entries still match their
bytes on disk.

## Publication lifecycle

One `WriterSession` owns the exclusive lock for `generate` and `ensure_generated`:

1. Resolve enough of the job to identify `output_root` and acquire the exclusive lock.
2. Read version and state under the lock; classify and retain a Routine-valid base if present.
3. Complete the front half, identity calculation, and domain decisions under the same lock.
4. Build the complete write/reuse plan before invalidation. Carried artifacts must come from the
   Routine-validated base; a clean rebuild carries nothing.
5. If the base is valid, the unit diff is clean, and `force_rebuild` is false, call `finish_noop`.
   No state, generation report, lock contents, or artifact write; no durability flush.
6. For every dirty path, including `force_rebuild`, invalidate an existing valid state in place by
   zeroing and syncing the first eight magic bytes. Missing or already-invalid state needs no
   invalidation marker.
7. Write every dirty canonical required artifact through `PublicationWrites`. Small artifacts
   (version, `usage.data`, occlusion) sync at write time; bulk payloads (static shards, atlas
   pages, `terrain.bin`, terrain DDS) use buffered writes. Their flushed handles register in the
   ledger's pending-sync registry. Every authoritative write also records its path, byte length,
   and BLAKE3. Reused artifacts remain untouched and carry their validated length/hash entries.
   Write the `MGE_DL_VERSION` byte when replacing an older/missing version.
8. Run the sync barrier. Sequentially `sync_all` every pending payload handle. Any failure
   aborts before the state write, leaving the state invalidated.
9. Check the write ledger. Every recorded write must have an inventory entry at its canonical
   relative path with exactly the recorded byte length and hash. A mismatch or a missing entry
   aborts before the state write. Then encode generation state plus the complete required-artifact
   inventory. Write and `sync_all` the whole `generation_state.bin` as the publication commit
   point.
10. While still holding the lock, prune stale generator-owned outputs not in the new inventory,
    including superseded version-13 journal/index/epoch evidence. Preserve unknown/user files and
    log a bounded sample of unrecognized regular files encountered by the cleanup scans.
11. Write the non-authoritative TOML generation report. It is generator-owned advisory output and
    is not added to the committed inventory.
12. Reload and decode `generation_state.bin`, Routine-validate the full inventory against the
    on-disk artifacts, and compare the reloaded state with the expected in-memory `CommittedState`
    before releasing the session. This is the final serveability step (one full on-disk validation,
    not a second in-memory-only pass).

The complete state flush is the publication boundary. Post-publication pruning failures are cleanup
warnings. A crash during pruning may leave harmless unlisted files that readers ignore. Any error
or interruption after invalidation and before the state flush leaves the cache absent.

`force_rebuild` changes reuse decisions only. It does not get a separate invalidation or publication
path, and remains excluded from settings identity so a later ordinary launch can still
be a true no-op.

Do not simplify away these two constraints:

- Durable invalidation must precede canonical mutation. Writing payloads first and
  `generation_state.bin` last is unsafe while the writer overwrites payloads in place. If the process
  dies mid-run, the old state still claims the tree. Routine cannot detect the resulting mixed
  tree, because atlas and terrain DDS outputs are deterministic-size for a given resolution, so a
  new-generation rewrite can share the old path, length, and header while carrying different bytes
  (`full_validation_rejects_same_length_atlas_page_corruption` demonstrates the distinction). The
  `version` byte cannot substitute. It identifies the format, not the generation instance.
  Rejected alternatives: a staging tree plus atomic rename, where the writer would have to copy or
  hardlink carried artifacts and defeat reuse, and a per-generation nonce in every payload header,
  which turns carry into mutation.
- After `begin_dirty`, do not read, hash, or decode any prior committed artifact's bytes on disk as
  reuse input. This run may already have overwritten them, so the base state's
  recorded length/hash no longer necessarily describes them. Read back all prior artifact bytes,
  including dirty static-shard decoding and terrain record reuse, before `begin_dirty`. This applies
  to on-disk bytes, not the in-memory base inventory. Carried `RequiredArtifact` entries stay
  consumable during publication because publication never rewrites carried paths. The write ledger
  enforces this constraint.

## Runtime snapshot

`open_output_snapshot` (in `crates/foundation/src/output_index.rs`):

- checks the version byte matches `MGE_DL_VERSION`;
- acquires the shared `.writer.lock`;
- rechecks the version under the lock;
- decodes/checksums `generation_state.bin` and validates invariants;
- validates every required artifact in Routine or Full mode;
- returns `OutputSnapshot` retaining the shared guard, decoded state, canonical `OutputPaths`,
  terrain availability, and atlas paths.

There is no generation number, selected index slot, epoch path, or pinned descriptor.

## Fault injection

Representative interruption points (exit code 42 when armed):

| `TES3_DL_FAULT_EXIT` value | Boundary |
|---|---|
| `after_state_invalidation` | after zeroing state magic |
| `mid_terrain_dds` | before the first terrain DDS write, with earlier payloads already on disk |

## Pruned superseded evidence

After successful publication, owned cleanup removes:

- superseded version-15 `static_meshes_00..31` shards
- `commit_journal.bin`, `generation_index_a.bin`, `generation_index_b.bin`
- `statics\static_meshes.e*`, `terrain.e*.bin`
- `atlas_cache.bin` and other recognized legacy sidecars

Never delete `.writer.lock`. Preserve files outside exact generator-owned grammars.
Post-publish cleanup scans only the `distantland` root, `statics`, and `statics\textures`. It is not
a recursive audit of the output root. Cleanup reports unrecognized files rather than removing them.
