# Dedicated grass-plugin list

The `GenerationJob.grass_plugins` field of the generation-job schema (file version 3) is an
optional ordered list of
generator-only `.esm`/`.esp` paths. Older job versions remain readable and imply an empty list;
`null`, omission, and `[]` are equivalent, and writers always emit version 3. Bare filenames
resolve against `data_dirs` exactly like active
plugins, but a filename cannot appear in both lists.

These plugins never enter the VFS active load order and contribute no plugin metadata or override
layers. They share the normal data directories so their meshes and textures resolve through the
existing VFS.

## Load order within the list

The list is its own load order, self-contained: grass plugins can override each other but can never
reach the game's load order. Both reference implementations work this way. OpenMW loads
groundcover files into an index space separate from its content list, and MGE-XE's legacy generator
gave grass mods ordinary override rules inside the single plugin list it had.

- A later entry overrides or deletes an earlier entry's placements, resolved through its `MAST`
  table exactly as the game resolves plugin overrides.
- Static definitions are unioned across the whole list, so an addon can place references to grass
  statics another entry defines. Later definitions of the same id win.
- A `MAST` target that is not itself in the list (`Morrowind.esm`, typically) cannot be addressed;
  such references are kept as new placements and reported. OpenMW's `resolveParentFileIndices`
  degrades the same way.

Placement identity is `(cell, resolved source plugin, refr_index)`. The cell is part of it because
groundcover generators restart `refr_index` at 0 in every cell, technically invalid, universal in
practice, and harmless under per-cell scoping. A plugin-global refnum would collapse every cell onto
the first. Identity is run-local: nothing downstream addresses a grass placement, so it never
reaches the output format.

`plugins` is not a supported home for groundcover. There, identity is `(plugin, refr_index)` with no
cell component, so a groundcover file's reused indices collapse and the lost placements are reported
as `plugin_duplicate_reference_indices`. Use this list instead.

## Warning codes

- `grass_plugin_master_not_in_list`

Density thinning hashes the normalized grass-plugin filename, exterior cell coordinates,
position bits, and a deterministic coincident-placement salt. Grass geometry remains an
ordinary static mesh, while `StaticGrass` keeps it structurally outside atlas packing and merge
grouping.

## Classification helpers

`is_grass_plugin` answers a single path with a bool; `classify_grass_plugins` is the same answer
across a list, rayon-parallel, with unreadable files classified `false`. Both decode only `STAT`
and `CELL`, the same record set OpenMW accepts from a groundcover file, and require both a
grass-prefixed `STAT` and more than 50 new exterior placements of one. A plugin defining no grass
static is rejected before any `CELL` is decoded. The heuristic is path-only, recognizing the
conventional `grass\` mesh prefix; generation remains authoritative because it additionally applies
VFS resolution and normal static overrides.

## Requirements on the configuring GUI

`MGEXEgui.exe` (`MGEXEgui/` in the MGE-XE repo) is the only writer of `grass_plugins`. The former
`config_ui/` wizard this section used to address no longer exists.

1. Emit `grass_plugins` in an order that resolves correctly. MGEXEgui sorts by its `load_order_key`,
   which is masters before plugins, then mtime, then name. That places a grass ESM ahead of every
   grass ESP. Do not name-sort. Name order is not order-independent, and an esp-patches-esp pair
   sorted by name resolves against the wrong source and silently drops the delete.
2. Keep the grass selection out of the game load order. The two lists are mutually exclusive, so a
   plugin claimed by one is inert in the other rather than hidden.
3. Use `classify_grass_plugins` to *annotate*, not to select. Nothing is pre-checked: a suggestion
   the user never made is not a selection. Groundcover left in `plugins` loses placements, so the
   annotation is a correctness hint worth surfacing on both lists.
4. Reject duplicate filenames within the grass list and overlaps with `plugins`, matching the
   library's fail-closed validation.

The residual case the ordering rule does not cover, an ESP patching another ESP, still reports
`grass_plugin_master_not_in_list`. Explicit reorder controls are the fix if that ever shows up in
practice. The on-disk list is already ordered, so adding them needs no format change.
