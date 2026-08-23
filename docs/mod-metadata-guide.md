# Guide: Distant Land Mod Metadata (`-metadata.toml`)

MGE XE supports plugin-specific configuration files using the `-metadata.toml` format. This allows mod authors to ship custom distant land generation settings packaged directly with their mods, eliminating the need for users to manually edit global or legacy `.ovr` files.

This guide explains how to structure, configure, and ship `-metadata.toml` files for your Morrowind mods.

---

## 1. File Naming and Location

To be discovered by MGE XE, the metadata file must reside in the **same directory** as your plugin and match the plugin's file stem:

* **Plugin**: `<Data Files>\MyMod.esp`  
  **Metadata**: `<Data Files>\MyMod-metadata.toml`
* **Plugin**: `<Data Files>\My.Complex.Mod.esm`  
  **Metadata**: `<Data Files>\My.Complex.Mod-metadata.toml`

Only `.esp` and `.esm` plugins participate: the load order is read from
`Morrowind.ini`'s `GameFile` list and filtered to those two extensions, so an
OpenMW `.omwaddon` package is never a plugin input.

### Discovery Rules
1. **Load Order**: Only metadata files associated with **currently active plugins** in the user's load order are read.
2. **Coexistence**: The file is shared with other tools (such as MWSE). MGE XE only reads the `[tools.mge-xe.distantland]` table; all other sections are ignored.
3. **Fail-Soft**: If the file contains syntax errors, MGE XE logs a warning and skips the file but continues generating distant land.

### Global Override TOML

The generator's ordered override-file list also accepts TOML documents using the same
`[tools.mge-xe.distantland]` schema. These explicitly selected files are global rather than tied
to an active plugin, and malformed configured files stop generation with an error instead of being
skipped. MGE XE ships `MGE3\MGE XE Default Statics Classifiers.toml` as an enabled-by-default,
commented example.

---

## 2. TOML Structure

All MGE XE distant land directives must be placed under the `[tools.mge-xe.distantland]` namespace. Below is a complete example showing all supported configuration keys:

```toml
[tools.mge-xe.distantland]
# Force-include references by Object ID (case-insensitive)
include_objects = ["special_static_ref"]

# Exclude references by Object ID (case-insensitive)
exclude_objects = ["foo_chargen_boat", "some_marker"]

# Force-include interiors (treated as exteriors for generation; case-insensitive)
include_interiors = ["Foo Big Cavern"]

# Exclude interiors from generation (case-insensitive)
exclude_interiors = []

# Per-mesh static classification overrides. TOML 1.1 permits a multiline
# inline table, keeping the map compact without repeating its full header.
statics = {
    'meshes\foo\bigrock.nif'   = { type = "very_far", reduction = 50 },
    'meshes\foo\fern.nif'      = { type = "grass", grass_density = 40 },
    'meshes\foo\fx_marker.nif' = { ignore = true },
    'meshes\foo\shack.nif'     = { type = "building", ignore_script = true },
}

# Dynamic visibility groups
[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "journal"
script = "foo_bridge_script"      # Script attached to the moving static/activator
journal = "foo_mq_bridge"         # Journal ID to check
ranges = [[50, 100]]              # Enabled when journal index is [50, 100] (inclusive)

[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "global"
script = "foo_gate_script"        # Script attached to the gate static/activator
global = "fooGateState"           # Global variable to check
ranges = [[1, 1]]                 # Enabled when variable equals 1

[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "unique_object"
object = "foo_lighthouse"         # Controlling unique object ID
linked_objects = ["foo_lighthouse_lamp", "foo_lighthouse_door"] # Objects sharing visibility
```

---

## 3. Configuration Fields

### Object and Interior Filters

* **`include_objects`** (array of strings, optional)  
  A list of object IDs (case-insensitive). References to these objects are forced into distant land generation regardless of their automatic classification.
* **`exclude_objects`** (array of strings, optional)  
  A list of object IDs (case-insensitive). References to these objects are excluded from distant land generation.
  > [!NOTE]  
  > If an object ID appears in both `include_objects` and `exclude_objects`, **exclusion wins** and a warning is logged.
* **`include_interiors`** (array of strings, optional)  
  A list of interior cell names (case-insensitive). References/meshes in these cells will be generated as part of distant land (useful for open-sky interiors).
* **`exclude_interiors`** (array of strings, optional)  
  A list of interior cell names (case-insensitive) to explicitly ignore during generation.
  > [!NOTE]  
  > If an interior cell name appears in both `include_interiors` and `exclude_interiors`, **exclusion wins** and a warning is logged.

---

### Per-Mesh Overrides (`statics = { ... }`)

`statics` is a table whose keys are VFS-relative mesh paths (case-insensitive). MGE XE's TOML 1.1
parser accepts the compact multiline inline-table form shown above.

> [!TIP]  
> Use **single-quoted literal strings** (e.g. `'meshes\foo\bar.nif'`) for keys. This prevents TOML from treating backslashes (`\`) as escape characters. Forward and backward slashes are normalized, and prefixes like `Data Files\` or `Meshes\` are automatically stripped.

Each mesh path maps to a table containing one or more of the following properties:

| Key | Type | Description |
| :--- | :--- | :--- |
| `type` | string | Explicitly sets the static type. Must be one of: `"auto"`, `"near"`, `"far"`, `"very_far"`, `"grass"`, `"tree"`, `"building"`. |
| `ignore` | boolean | If `true`, the mesh is entirely excluded from distant land generation. |
| `grass_density` | integer | Grass density percentage (`0` to `100`). Only valid when `type = "grass"`. |
| `reduction` | integer | Mesh simplification percentage (`0` to `100`). `0` disables simplification (same as legacy `.ovr` `use_old_reduction`). |
| `ignore_script` | boolean | If `true`, MGE XE ignores any scripts attached to references of this mesh (disabling dynamic script visibility checks for it, same as legacy `.ovr` `no_script`). |

---

### Dynamic Visibility (`[[tools.mge-xe.distantland.dynamic_visibility]]`)

Dynamic visibility allows distant land objects to show or hide dynamically at runtime based on game state (e.g., quests progressing or global variables toggling). This is an array of tables where each entry must specify a `kind` tag.

#### 1. Journal-Gated Visibility (`kind = "journal"`)
Shows the static objects when a journal index is within specific ranges.
* `script` (string): The script name (case-insensitive) attached to the references that join this visibility group.
* `journal` (string): The journal quest ID (case-insensitive) to monitor.
* `ranges` (array of `[low, high]` integer pairs): Up to **8** inclusive index ranges that enable visibility.
  * *Example*: `ranges = [[10, 20], [50, 100]]` makes objects visible if the quest index is between 10 and 20 (inclusive) OR between 50 and 100 (inclusive).

#### 2. Global-Gated Visibility (`kind = "global"`)
Shows the static objects when a global variable is within specific ranges.
* `script` (string): The script name (case-insensitive) attached to the references that join this visibility group.
* `global` (string): The global variable name (case-insensitive) to monitor.
* `ranges` (array of `[low, high]` integer pairs): Up to **8** inclusive value ranges that enable visibility.

#### 3. Unique-Object-Linked Visibility (`kind = "unique_object"`)
Links the visibility of multiple objects to a primary "controlling" object.
The group follows the controlling reference's runtime disabled/deleted state;
script- and quest-value conditions use the `journal` or `global` kinds instead.
* `object` (string): The primary controlling unique object ID (case-insensitive).
* `linked_objects` (array of strings, optional): Additional object IDs (case-insensitive) that share the primary object's visibility state.

---

## 4. Precedence and Merging Rules

When distant land is generated, overrides from different sources are merged in a strict order:

1. **Configured Override Files**: `.ovr`, `.txt`, and TOML sources are applied first, in the order configured in the GUI.
2. **Metadata Files**: Applied next, processed in the load order of the active plugins.

### Merging Behavior
* **Scalar Overrides**: For mesh types, ignore flags, and cell/object filters, the **last writer wins**. If a plugin metadata file overrides a setting that was previously set by a legacy `.ovr` file or a prior plugin, the new value takes precedence.
* **Conflict Logs**: If a plugin metadata file overwrites a directive established by a different source file for the same key, a diagnostic warning is logged in the generation log.
* **Dynamic Visibility**: Visibility groups are merged and deduplicated across all sources rather than overwritten. Multiple plugins or `.ovr` files can register scripts or ranges to the same group.

---

## 5. Cache Invalidation and Staleness

MGE XE tracks the normalized path and content hash of all discovered
`-metadata.toml` files.

* If you add, edit, or delete a `-metadata.toml` file, the distant land generator detects that the load-order fingerprint has changed.
* A subsequent output-status check flags the distant land as **stale**. Regenerate the output to apply the metadata changes.

---

## 6. Mapping from Legacy `.ovr` Format

If you are migrating existing `.ovr` override rules to TOML, use this conversion table:

| Legacy `.ovr` Rule | TOML Equivalent |
| :--- | :--- |
| `meshes\path\rock.nif = near` | `[tools.mge-xe.distantland]`<br>`statics = { 'meshes\path\rock.nif' = { type = "near" } }` |
| `meshes\path\rock.nif = reduction_50` | `[tools.mge-xe.distantland]`<br>`statics = { 'meshes\path\rock.nif' = { reduction = 50 } }` |
| `meshes\path\rock.nif = grass_40` | `[tools.mge-xe.distantland]`<br>`statics = { 'meshes\path\rock.nif' = { type = "grass", grass_density = 40 } }` |
| `meshes\path\rock.nif = ignore` | `[tools.mge-xe.distantland]`<br>`statics = { 'meshes\path\rock.nif' = { ignore = true } }` |
| `meshes\path\rock.nif = no_script` | `[tools.mge-xe.distantland]`<br>`statics = { 'meshes\path\rock.nif' = { ignore_script = true } }` |
| `meshes\path\rock.nif = use_old_reduction` | `[tools.mge-xe.distantland]`<br>`statics = { 'meshes\path\rock.nif' = { reduction = 0 } }` |
| `[names]`<br>`my_object = enable`<br>`bad_object = disable` | `[tools.mge-xe.distantland]`<br>`include_objects = ["my_object"]`<br>`exclude_objects = ["bad_object"]` |
| `[interiors]`<br>`My Cell = enable`<br>`Bad Cell = disable` | `[tools.mge-xe.distantland]`<br>`include_interiors = ["My Cell"]`<br>`exclude_interiors = ["Bad Cell"]` |
| `[dynamic_vis]`<br>`my_script = journal my_quest 50-100` | `[[tools.mge-xe.distantland.dynamic_visibility]]`<br>`kind = "journal"`<br>`script = "my_script"`<br>`journal = "my_quest"`<br>`ranges = [[50, 100]]` |

---

## 7. Implementation Reference

The parser logic is implemented in Rust, in the distant-land generator's `statics` crate:

* `distantland/crates/statics/src/metadata.rs` — parsing and the TOML struct definitions.
* `distantland/crates/statics/src/overrides.rs` — the merge and override-builder logic.
