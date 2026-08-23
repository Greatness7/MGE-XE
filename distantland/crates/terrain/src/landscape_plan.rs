//! Landscape patch texture planning, mirroring Morrowind's runtime behavior.
//!
//! Resolves which base and decal textures each of a cell's 16x16 patches uses,
//! and the alpha masks blending them, following the engine's patch ordering and
//! edge-inheritance rules.
//!
//! Originally extracted verbatim from the tes3 repository's `landscape` branch,
//! where it was verified as a 1:1 match to the engine; since adapted in this repo.
#![allow(clippy::identity_op, clippy::field_reassign_with_default)]

use std::collections::HashMap;

use tes3::esp::{Landscape, LandscapeTexture, Plugin};

/// Number of patches along one side of a cell.
const PATCHES_PER_SIDE: usize = 16;
/// Number of vertices along one side of a single patch.
const PATCH_VERTICES_PER_SIDE: usize = 5;
/// Number of parent group nodes along one side of a cell (each covers a 4×4 block of patches).
const PARENTS_PER_SIDE: usize = 4;
/// Number of NiTriShape children along one side of a parent group node.
const SHAPES_PER_PARENT_SIDE: usize = 4;
/// Total patches (NiTriShapes) owned by a single parent group node.
const PATCHES_PER_PARENT: usize = SHAPES_PER_PARENT_SIDE * SHAPES_PER_PARENT_SIDE;

/// Maps cell grid coordinates `(x, y)` to borrowed [`Landscape`] records.
type LandscapeLookup<'a> = HashMap<(i32, i32), &'a Landscape>;
/// Maps LTEX 0-based indices to texture file path strings.
type TextureLookup<'a> = HashMap<u32, &'a str>;

/// Per-vertex alpha values for one 5×5 patch, indexed as `[row][col]`.
type AlphaGrid = [[u8; PATCH_VERTICES_PER_SIDE]; PATCH_VERTICES_PER_SIDE];

/// Normalized name for the engine's fallback landscape texture.
pub const DEFAULT_LAND_TEXTURE_DDS: &str = "_land_default.dds";

/// Runtime texturing decision for a single 5x5 landscape patch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LandscapePatchTexturing<'a> {
    /// Base texture assigned to the patch, if any.
    pub base_texture: Option<&'a str>,
    /// Decal texture assigned to the patch, if any.
    pub decal_texture: Option<&'a str>,
    /// Vertex alpha values used for two-texture blending.
    pub alpha_grid: AlphaGrid,
    /// Whether the runtime would materialize an explicit texturing property.
    pub created_property: bool,
}

/// Runtime plan for one patch within a cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandscapePatchPlan<'a> {
    /// Patch X coordinate in the 16x16 cell grid.
    pub patch_x: usize,
    /// Patch Y coordinate in the 16x16 cell grid.
    pub patch_y: usize,
    /// Parent node index in the runtime NIF layout.
    pub parent_index: usize,
    /// Shape index inside the parent node.
    pub shape_index: usize,
    /// Texturing result for this patch.
    pub texturing: LandscapePatchTexturing<'a>,
}

/// Runtime plan for all 256 patches in a cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandscapeCellPlan<'a> {
    /// Cell grid coordinates.
    pub grid: (i32, i32),
    /// Patch plans in runtime storage order.
    pub patches: Vec<LandscapePatchPlan<'a>>,
}

/// Texture information read from a single patch position during neighbor sampling.
///
/// `id` is the opaque texture identity assigned by the sampler; two patches are
/// treated as "the same texture" for blending iff their `id`s are equal `Some`
/// values. `None` represents "no VTEX entry at this patch" and is never equal
/// to `Some(_)`.
///
/// The sampler is responsible for assigning identity in a way that is stable
/// and comparable across all cells visited during one planning run, including
/// across cell boundaries reached via neighbor sampling. Two LTEX records in
/// different per-cell tables that resolve to the same canonical texture should
/// share an `id`; two LTEX records that resolve to different canonical textures
/// must not share an `id`, even when they originally lived at the same raw VTEX
/// index in their respective cells.
///
/// `name` is the file path used by [`LandscapePatchTexturing`] (`base_texture`,
/// `decal_texture`). The planner never uses `name` for identity comparisons,
/// only `id`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SampledTextureIdentity<'a> {
    /// Opaque identity for this patch position, or `None` if no VTEX entry exists.
    ///
    /// Two patches with `Some(x) == Some(y)` are treated as the same texture
    /// for blending; `None` is never equal to `Some(_)`.
    pub id: Option<u32>,
    /// Resolved texture file path from the sampler, or `None` if the record is absent.
    pub name: Option<&'a str>,
}

impl<'a> SampledTextureIdentity<'a> {
    /// Returns `true` when this patch position has no VTEX entry (`id` is `None`).
    fn is_missing(&self) -> bool {
        self.id.is_none()
    }

    /// Returns `true` when the two textures have different identity tokens.
    fn differs_from(&self, other: &Self) -> bool {
        self.id != other.id
    }

    /// Returns `true` when both textures share the same identity, including both being `None`.
    const fn same_identity(&self, other: &Self) -> bool {
        match (self.id, other.id) {
            (None, None) => true,
            (None, Some(_)) => false,
            (Some(_), None) => false,
            (Some(a), Some(b)) => a == b,
        }
    }

    /// Returns the resolved name only if it is non-empty, otherwise `None`.
    fn non_empty_name(&self) -> Option<&'a str> {
        non_empty_texture(self.name)
    }

    /// Returns the resolved texture name, falling back to [`DEFAULT_LAND_TEXTURE_DDS`] if absent.
    fn resolved_name(&self) -> &'a str {
        self.name.unwrap_or(DEFAULT_LAND_TEXTURE_DDS)
    }
}

/// Textures sampled at a patch and its three cardinal/diagonal predecessors, used to
/// determine what blending the runtime applies at each patch boundary.
///
/// "North" and "West" are the adjacent patches that share an edge with the current one;
/// "Northwest" is the diagonal patch that shares only a corner. The grid is oriented
/// so that increasing Y is north and decreasing X is west.
#[derive(Clone, Copy, Debug, Default)]
struct NeighborTextures<'a> {
    /// Texture of the patch being planned.
    current: SampledTextureIdentity<'a>,
    /// Texture of the patch immediately north (higher Y, same X).
    north: SampledTextureIdentity<'a>,
    /// Texture of the patch immediately west (same Y, lower X).
    west: SampledTextureIdentity<'a>,
    /// Texture of the patch diagonally northwest (higher Y, lower X).
    northwest: SampledTextureIdentity<'a>,
}

impl<'a> NeighborTextures<'a> {
    /// Returns `true` when all three neighbor positions carry no texture entry.
    /// An isolated patch only blends against the engine's default fallback texture.
    fn is_isolated(&self) -> bool {
        self.north.is_missing() && self.west.is_missing() && self.northwest.is_missing()
    }

    /// Returns `true` when the north neighbor has a different texture than the current patch.
    fn north_differs(&self) -> bool {
        self.north.differs_from(&self.current)
    }

    /// Returns `true` when the west neighbor has a different texture than the current patch.
    fn west_differs(&self) -> bool {
        self.west.differs_from(&self.current)
    }

    /// Returns `true` when the northwest neighbor has a different texture than the current patch.
    fn northwest_differs(&self) -> bool {
        self.northwest.differs_from(&self.current)
    }
}

/// Fully resolved position of one patch expressed in both coordinate systems:
/// the 2-D cell grid (`patch_x`/`patch_y`) and the runtime NIF hierarchy
/// (`parent_index`/`shape_index`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PatchLocation {
    /// Patch column in the 16×16 cell grid (0 = west edge).
    patch_x: usize,
    /// Patch row in the 16×16 cell grid (0 = south edge).
    patch_y: usize,
    /// Index of the parent [`NiNode`] that owns this patch (0–15).
    parent_index: usize,
    /// Index of this patch's [`NiTriShape`] within its parent node (0–15).
    shape_index: usize,
}

impl PatchLocation {
    /// Derives grid coordinates from a runtime storage position.
    ///
    /// The runtime groups patches into 4×4 parent blocks; within each block the
    /// shapes are ordered column-major. This reverses that mapping to recover the
    /// absolute `patch_x`/`patch_y` within the cell.
    fn from_storage(parent_index: usize, shape_index: usize) -> Self {
        let patch_x = (parent_index % PARENTS_PER_SIDE) * SHAPES_PER_PARENT_SIDE + (shape_index % SHAPES_PER_PARENT_SIDE);
        let patch_y = (parent_index / PARENTS_PER_SIDE) * SHAPES_PER_PARENT_SIDE + (shape_index / SHAPES_PER_PARENT_SIDE);

        Self {
            patch_x,
            patch_y,
            parent_index,
            shape_index,
        }
    }
}

/// Builds a lookup from LTEX index to the original texture path stored in the plugin.
pub fn build_land_texture_lookup<'a>(plugin: &'a Plugin) -> TextureLookup<'a> {
    plugin
        .objects_of_type::<LandscapeTexture>()
        .map(|texture| (texture.index, texture.file_name.as_str()))
        .collect()
}

/// Builds a lookup from cell grid coordinates to LAND records.
pub fn build_landscape_lookup<'a>(plugin: &'a Plugin) -> LandscapeLookup<'a> {
    plugin
        .objects_of_type::<Landscape>()
        .map(|landscape| (landscape.grid, landscape))
        .collect()
}

/// Computes the runtime-style texturing plan for all 256 patches in a cell.
///
/// This wrapper uses a default sampler whose identity scheme is "raw VTEX index
/// of the cell currently being read." That reproduces the legacy single-plugin
/// behavior bit-for-bit, including the cross-cell quirk where the same raw
/// VTEX index in two cells with different LTEX tables is treated as a single
/// texture. Callers that span multiple plugins (and therefore multiple per-cell
/// LTEX tables) should prefer [`plan_landscape_cell_with_sampler`] and pass a
/// sampler that interns texture identity globally.
pub fn plan_landscape_cell<'a>(
    landscapes: &LandscapeLookup,
    texture_lookup: &'a TextureLookup<'a>,
    grid: (i32, i32),
) -> LandscapeCellPlan<'a> {
    plan_landscape_cell_with_sampler(grid, |cell, patch_x, patch_y| {
        let Some(landscape) = landscapes.get(&cell) else {
            return SampledTextureIdentity::default();
        };
        let raw_index = raw_vtex_index_at_world_patch(landscape, patch_x, patch_y);
        SampledTextureIdentity {
            // VTEX index 0 means "no texture assigned"; treat it as absent by checking sub(1).
            id: raw_index.checked_sub(1).map(|_| u32::from(raw_index)),
            name: lookup_vtex_texture_name(texture_lookup, raw_index),
        }
    })
}

/// Computes the runtime-style texturing plan for all 256 patches in a cell using
/// a caller-provided sampler to resolve per-patch texture identity.
///
/// `sampler(cell, patch_x, patch_y)` is invoked for every patch the planner
/// needs, including neighbors reached across cell boundaries. `patch_x` and
/// `patch_y` are always in `[0, PATCHES_PER_SIDE)` after the planner has wrapped
/// them; the planner is responsible for the cross-cell coordinate math.
///
/// Return [`SampledTextureIdentity::default`] (`id = None`, `name = None`) when
/// the requested cell has no LAND data or the patch has no VTEX entry. The
/// sampler is required to assign `id` so that two patches the runtime would
/// blend identically share an `id` even across cell boundaries. See
/// [`SampledTextureIdentity`] for the full guarantee.
pub fn plan_landscape_cell_with_sampler<'a, F>(grid: (i32, i32), sampler: F) -> LandscapeCellPlan<'a>
where
    F: Fn((i32, i32), usize, usize) -> SampledTextureIdentity<'a>,
{
    let mut patches = Vec::with_capacity(PATCHES_PER_SIDE * PATCHES_PER_SIDE);

    for location in patch_locations() {
        let is_top_left_patch = location.patch_x == 0 && location.patch_y == PATCHES_PER_SIDE - 1;
        let neighbors = sample_patch_neighbors(&sampler, grid, location.patch_x, location.patch_y);
        let texturing = plan_patch_texturing(neighbors, is_top_left_patch);

        patches.push(LandscapePatchPlan {
            patch_x: location.patch_x,
            patch_y: location.patch_y,
            parent_index: location.parent_index,
            shape_index: location.shape_index,
            texturing,
        });
    }

    LandscapeCellPlan { grid, patches }
}

/// Iterates all 256 [`PatchLocation`]s in runtime storage order (parent-major, then
/// shape-minor), matching the child ordering the engine writes to its NIF files.
fn patch_locations() -> impl Iterator<Item = PatchLocation> {
    (0..PARENTS_PER_SIDE * PARENTS_PER_SIDE) //
        .flat_map(|parent_index| {
            (0..PATCHES_PER_PARENT).map(move |shape_index| PatchLocation::from_storage(parent_index, shape_index))
        })
}

/// Samples the texture at `(patch_x, patch_y)` and its three spatial predecessors
/// (north, west, northwest) via `sampler`, returning the results bundled as
/// [`NeighborTextures`]. Coordinates passed to `sampler` are always wrapped into
/// `[0, PATCHES_PER_SIDE)`; cross-cell sampling adjusts the cell coordinate instead.
fn sample_patch_neighbors<'a, F>(sampler: &F, grid: (i32, i32), patch_x: usize, patch_y: usize) -> NeighborTextures<'a>
where
    F: Fn((i32, i32), usize, usize) -> SampledTextureIdentity<'a>,
{
    NeighborTextures {
        current: sample_patch_texture(sampler, grid, patch_x, patch_y, 0, 0),
        north: sample_patch_texture(sampler, grid, patch_x, patch_y, 0, 1),
        west: sample_patch_texture(sampler, grid, patch_x, patch_y, -1, 0),
        northwest: sample_patch_texture(sampler, grid, patch_x, patch_y, -1, 1),
    }
}

/// Resolves the patch at `(patch_x + delta_x, patch_y + delta_y)` through `sampler`,
/// crossing into an adjacent cell if the offset wraps beyond the cell boundary.
///
/// The wrapped coordinates handed to `sampler` are always in `[0, PATCHES_PER_SIDE)`.
fn sample_patch_texture<'a, F>(
    sampler: &F,
    grid: (i32, i32),
    patch_x: usize,
    patch_y: usize,
    delta_x: i32,
    delta_y: i32,
) -> SampledTextureIdentity<'a>
where
    F: Fn((i32, i32), usize, usize) -> SampledTextureIdentity<'a>,
{
    let (grid_x, patch_x) = wrap_patch_axis(grid.0, patch_x as i32 + delta_x);
    let (grid_y, patch_y) = wrap_patch_axis(grid.1, patch_y as i32 + delta_y);
    sampler((grid_x, grid_y), patch_x, patch_y)
}

/// Normalizes a patch coordinate that may lie outside `[0, PATCHES_PER_SIDE)` by
/// adjusting the cell grid index accordingly, mirroring how the runtime crosses
/// cell boundaries when sampling neighbor textures.
fn wrap_patch_axis(mut grid: i32, mut patch: i32) -> (i32, usize) {
    while patch < 0 {
        patch += PATCHES_PER_SIDE as i32;
        grid -= 1;
    }
    while patch >= PATCHES_PER_SIDE as i32 {
        patch -= PATCHES_PER_SIDE as i32;
        grid += 1;
    }

    (grid, patch as usize)
}

/// Returns the alpha blend value for a vertex at `distance` steps from a texture boundary.
///
/// The runtime uses a discrete two-step ramp: 255 at the boundary vertex, 127 one step
/// inward, and 0 everywhere else. There is no smooth gradient, only these three levels.
const fn edge_blend(distance: usize) -> u8 {
    match distance {
        0 => 255,
        1 => 127,
        _ => 0,
    }
}

/// Determines the base texture, decal texture, and alpha grid for a single patch given
/// its sampled neighbor textures.
///
/// The function replicates the runtime's four blending cases in priority order:
/// isolated (no neighbors), northwest-only diagonal, no blending needed, and
/// full north/west edge gradient. `is_top_left_patch` mirrors the runtime
/// border-update quirk for the (0, 15) patch where the engine can leave an
/// otherwise-present west neighbor out when north and northwest are both missing.
fn plan_patch_texturing<'a>(mut neighbors: NeighborTextures<'a>, is_top_left_patch: bool) -> LandscapePatchTexturing<'a> {
    if is_top_left_patch && neighbors.north.is_missing() && neighbors.northwest.is_missing() {
        neighbors.west = SampledTextureIdentity::default();
    }

    let mut texturing = LandscapePatchTexturing::default();

    // Isolated: this patch has a texture but no sampled neighbor does.
    // Blend current → fallback along both edges. Where both edges reach the same
    // vertex (the NW corner region), saturate to 255 so the corner fills cleanly.
    if neighbors.is_isolated() {
        if neighbors.current.is_missing() {
            return texturing;
        }

        texturing.alpha_grid = ISOLATED_ALPHA;
        texturing.base_texture = Some(neighbors.current.resolved_name());
        texturing.decal_texture = Some(DEFAULT_LAND_TEXTURE_DDS);
        texturing.created_property = true;
        return texturing;
    }

    // Choose overlay texture: north has priority over west.
    // None textures (raw index 0) resolve to _land_default: they represent real
    // terrain that participates in blending, not "no texture".
    let overlay = if neighbors.north_differs() {
        Some(neighbors.north)
    } else if neighbors.west_differs() {
        Some(neighbors.west)
    } else {
        None
    };

    // NW-only: neither direct edge differs, but the diagonal does.
    // Only the four corner vertices are touched, tapering away from the NW corner.
    if overlay.is_none() && neighbors.northwest_differs() {
        texturing.alpha_grid = NORTHWEST_ONLY_ALPHA;
        texturing.base_texture = Some(neighbors.current.resolved_name());
        texturing.decal_texture = Some(neighbors.northwest.resolved_name());
        texturing.created_property = true;
        return texturing;
    }

    // No blending needed at all.
    if overlay.is_none() {
        texturing.base_texture = neighbors.current.non_empty_name();
        texturing.created_property = texturing.base_texture.is_some();
        return texturing;
    }

    // Apply north and/or west edge gradients. When north and west carry different
    // textures, north wins the body and the conflicting west influence is handled
    // exclusively in the corner vertices by build_edge_alpha.
    let blend_north = neighbors.north_differs();
    let overlay = overlay.unwrap();
    let blend_west = neighbors.west_differs() && overlay.same_identity(&neighbors.west);

    texturing.alpha_grid = build_edge_alpha(blend_north, blend_west, &overlay, &neighbors);
    texturing.base_texture = Some(neighbors.current.resolved_name());
    texturing.decal_texture = Some(overlay.resolved_name());
    texturing.created_property = true;
    texturing
}

/// Builds the alpha grid used when a patch is isolated (no textured neighbors).
///
/// Applies full north and west edge gradients simultaneously. Vertices where both
/// gradients are non-zero are saturated to 255 instead of taking the max, which
/// prevents a visually concave notch at the northwest corner.
const fn build_isolated_alpha() -> AlphaGrid {
    let mut grid = [[0u8; PATCH_VERTICES_PER_SIDE]; PATCH_VERTICES_PER_SIDE];
    let mut row = 0;
    while row < PATCH_VERTICES_PER_SIDE {
        let mut col = 0;
        while col < PATCH_VERTICES_PER_SIDE {
            let n = edge_blend(4 - row);
            let w = edge_blend(col);
            grid[row][col] = if n > 0 && w > 0 { 255 } else { if w < n { n } else { w } };
            col += 1;
        }
        row += 1;
    }
    grid
}
/// Alpha grid used when a decal texture is isolated to the current patch.
const ISOLATED_ALPHA: AlphaGrid = build_isolated_alpha();

/// Builds the alpha grid for the northwest-only case, where neither direct edge differs
/// but the diagonal neighbor carries a different texture.
///
/// Only the four corner vertices (rows 3–4, cols 0–1) are non-zero, tapering away from
/// the NW corner vertex at `(4, 0)`.
const fn build_northwest_only_alpha() -> AlphaGrid {
    let mut grid = [[0u8; PATCH_VERTICES_PER_SIDE]; PATCH_VERTICES_PER_SIDE];
    grid[4][0] = 255; // NW corner itself: fully blended
    grid[4][1] = 127; // one step east along the north edge
    grid[3][0] = 127; // one step south along the west edge
    grid[3][1] = 0; // interior corner: too far from the diagonal boundary
    grid
}
/// Alpha grid used when only the northwest diagonal neighbor shares the decal.
const NORTHWEST_ONLY_ALPHA: AlphaGrid = build_northwest_only_alpha();

/// Builds a partial alpha grid covering the north/west gradient for all vertices
/// **except** the 2×2 corner block at rows 3–4, cols 0–1.
///
/// The corner block is deliberately left zeroed here because its values depend on
/// runtime neighbor texture agreement and must be computed separately by [`build_edge_alpha`].
const fn edge_body_alpha(blend_north: bool, blend_west: bool) -> AlphaGrid {
    let mut grid = [[0u8; PATCH_VERTICES_PER_SIDE]; PATCH_VERTICES_PER_SIDE];
    let mut row = 0;
    while row < PATCH_VERTICES_PER_SIDE {
        let mut col = 0;
        while col < PATCH_VERTICES_PER_SIDE {
            if !(row >= 3 && col <= 1) {
                let n = if blend_north { edge_blend(4 - row) } else { 0 };
                let w = if blend_west { edge_blend(col) } else { 0 };
                grid[row][col] = if w < n { n } else { w };
            }
            col += 1;
        }
        row += 1;
    }
    grid
}
/// Precomputed edge-only alpha grid for north blending without west blending.
const EDGE_BODY_NORTH: AlphaGrid = edge_body_alpha(true, false);
/// Precomputed edge-only alpha grid for west blending without north blending.
const EDGE_BODY_WEST: AlphaGrid = edge_body_alpha(false, true);
/// Precomputed edge-only alpha grid when both north and west edges blend.
const EDGE_BODY_BOTH: AlphaGrid = edge_body_alpha(true, true);
/// Precomputed edge-only alpha grid when neither north nor west edge blends.
const EDGE_BODY_NONE: AlphaGrid = edge_body_alpha(false, false);

/// Returns the alpha value for one of the four NW corner vertices, driven by which
/// neighbors share the overlay texture.
///
/// The grid is oriented with row 4 as the north edge and col 0 as the west edge, so:
/// - `(4, 0)` is the NW corner vertex itself
/// - `(4, 1)` is one step east along the north edge
/// - `(3, 0)` is one step south along the west edge
/// - `(3, 1)` is the interior vertex of the 2×2 corner block
///
/// `matches_nw/n/w` indicate whether the corresponding neighbor carries the same
/// overlay texture; `nw_matches_current` flags when NW matches the *current* patch instead.
const fn corner_alpha(
    row: usize,
    col: usize,
    matches_nw: bool,
    matches_n: bool,
    matches_w: bool,
    nw_matches_current: bool,
) -> u8 {
    let n = matches_n as usize;
    let w = matches_w as usize;
    let nw = matches_nw as usize;
    match (row, col) {
        (4, 0) if matches_nw => 255,
        (4, 0) if matches_n && matches_w && !nw_matches_current => 127,
        (4, 0) => 0,
        (4, 1) => [0, 127, 255][n + nw],
        (3, 0) => [0, 127, 255][w + nw],
        (3, 1) if n + w + nw >= 2 => 127,
        (3, 1) => 0,
        _ => unreachable!(),
    }
}

/// Builds the complete edge alpha grid by selecting the appropriate precomputed body
/// grid and then stamping the four NW corner vertices with neighbor-aware values.
///
/// The body grid covers all non-corner vertices; the corner values are computed by
/// [`corner_alpha`] because they depend on whether adjacent patches share the overlay.
const fn build_edge_alpha(
    blend_north: bool,
    blend_west: bool,
    overlay: &SampledTextureIdentity<'_>,
    neighbors: &NeighborTextures<'_>,
) -> AlphaGrid {
    let mut grid = match (blend_north, blend_west) {
        (true, true) => EDGE_BODY_BOTH,
        (true, false) => EDGE_BODY_NORTH,
        (false, true) => EDGE_BODY_WEST,
        (false, false) => EDGE_BODY_NONE,
    };

    let matches_nw = overlay.same_identity(&neighbors.northwest);
    let matches_n = overlay.same_identity(&neighbors.north);
    let matches_w = overlay.same_identity(&neighbors.west);
    let nw_matches_current = neighbors.northwest.same_identity(&neighbors.current);

    grid[4][0] = corner_alpha(4, 0, matches_nw, matches_n, matches_w, nw_matches_current);
    grid[4][1] = corner_alpha(4, 1, matches_nw, matches_n, matches_w, nw_matches_current);
    grid[3][0] = corner_alpha(3, 0, matches_nw, matches_n, matches_w, nw_matches_current);
    grid[3][1] = corner_alpha(3, 1, matches_nw, matches_n, matches_w, nw_matches_current);

    grid
}

/// Returns `Some(texture)` only when `texture` is `Some` and the string is non-empty.
fn non_empty_texture(texture: Option<&str>) -> Option<&str> {
    texture.filter(|texture| !texture.is_empty())
}

/// Resolves a raw 1-based VTEX index to the texture file path stored in the plugin.
///
/// Returns `None` when `raw_vtex_index` is 0 (no texture) or the LTEX record is absent.
fn lookup_vtex_texture_name<'a>(texture_lookup: &'a TextureLookup<'a>, raw_vtex_index: u16) -> Option<&'a str> {
    let i = raw_vtex_index.checked_sub(1)?;
    Some(texture_lookup.get(&i.into())?)
}

/// Reads the raw VTEX index stored in `landscape` for the patch at `(patch_x, patch_y)`.
///
/// The LAND record stores texture indices in a 4×4 parent/shape hierarchy that mirrors
/// the NIF layout, so the coordinate is remapped through `parent_index`/`shape_index`
/// before indexing into `landscape.texture_indices`.
fn raw_vtex_index_at_world_patch(landscape: &Landscape, patch_x: usize, patch_y: usize) -> u16 {
    let parent_index = (patch_x / SHAPES_PER_PARENT_SIDE) + PARENTS_PER_SIDE * (patch_y / SHAPES_PER_PARENT_SIDE);
    let shape_index = (patch_x % SHAPES_PER_PARENT_SIDE) + SHAPES_PER_PARENT_SIDE * (patch_y % SHAPES_PER_PARENT_SIDE);
    landscape.texture_indices.data[parent_index][shape_index]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sampled_texture(id: u32, name: &'static str) -> SampledTextureIdentity<'static> {
        SampledTextureIdentity {
            id: Some(id),
            name: Some(name),
        }
    }

    fn neighbor_textures(
        current: SampledTextureIdentity<'static>,
        north: SampledTextureIdentity<'static>,
        west: SampledTextureIdentity<'static>,
        northwest: SampledTextureIdentity<'static>,
    ) -> NeighborTextures<'static> {
        NeighborTextures {
            current,
            north,
            west,
            northwest,
        }
    }

    #[test]
    fn top_left_textured_patch_ignores_west_when_north_and_northwest_are_missing() {
        let texturing = plan_patch_texturing(
            neighbor_textures(
                sampled_texture(1, "current.dds"),
                SampledTextureIdentity::default(),
                sampled_texture(2, "west.dds"),
                SampledTextureIdentity::default(),
            ),
            true,
        );

        assert_eq!(texturing.base_texture, Some("current.dds"));
        assert_eq!(texturing.decal_texture, Some(DEFAULT_LAND_TEXTURE_DDS));
        assert!(texturing.created_property);
        assert_eq!(texturing.alpha_grid, ISOLATED_ALPHA);
    }

    #[test]
    fn top_left_missing_current_inherits_default_when_north_and_northwest_are_missing() {
        let texturing = plan_patch_texturing(
            neighbor_textures(
                SampledTextureIdentity::default(),
                SampledTextureIdentity::default(),
                sampled_texture(2, "west.dds"),
                SampledTextureIdentity::default(),
            ),
            true,
        );

        assert_eq!(texturing.base_texture, None);
        assert_eq!(texturing.decal_texture, None);
        assert!(!texturing.created_property);
        assert_eq!(texturing.alpha_grid, [[0; PATCH_VERTICES_PER_SIDE]; PATCH_VERTICES_PER_SIDE]);
    }

    #[test]
    fn isolated_textured_patch_still_blends_to_default() {
        let texturing = plan_patch_texturing(
            neighbor_textures(
                sampled_texture(1, "current.dds"),
                SampledTextureIdentity::default(),
                SampledTextureIdentity::default(),
                SampledTextureIdentity::default(),
            ),
            false,
        );

        assert_eq!(texturing.base_texture, Some("current.dds"));
        assert_eq!(texturing.decal_texture, Some(DEFAULT_LAND_TEXTURE_DDS));
        assert!(texturing.created_property);
        assert_eq!(texturing.alpha_grid, ISOLATED_ALPHA);
    }

    /// Builds a synthetic [`Landscape`] whose VTEX table is filled by `raw_index_at`.
    /// All other LAND fields take their default values, which is enough for the
    /// planner's purposes (it only reads `texture_indices`).
    fn synthetic_landscape(grid: (i32, i32), raw_index_at: impl Fn(usize, usize) -> u16) -> Landscape {
        let mut landscape = Landscape::default();
        landscape.grid = grid;
        let mut data = Box::new([[0u16; 16]; 16]);
        for patch_y in 0..PATCHES_PER_SIDE {
            for patch_x in 0..PATCHES_PER_SIDE {
                let parent_index =
                    (patch_x / SHAPES_PER_PARENT_SIDE) + PARENTS_PER_SIDE * (patch_y / SHAPES_PER_PARENT_SIDE);
                let shape_index =
                    (patch_x % SHAPES_PER_PARENT_SIDE) + SHAPES_PER_PARENT_SIDE * (patch_y % SHAPES_PER_PARENT_SIDE);
                data[parent_index][shape_index] = raw_index_at(patch_x, patch_y);
            }
        }
        landscape.texture_indices.data = data;
        landscape
    }

    fn find_patch<'a, 'p>(plan: &'p LandscapeCellPlan<'a>, patch_x: usize, patch_y: usize) -> &'p LandscapePatchPlan<'a> {
        plan.patches
            .iter()
            .find(|p| p.patch_x == patch_x && p.patch_y == patch_y)
            .expect("patch not found in plan")
    }

    #[test]
    fn default_sampler_matches_wrapper_for_isolated_patch() {
        // Only patch (5, 5) carries a VTEX entry; everything else is bare ground,
        // so the textured patch must be planned via the isolated blending case.
        let landscape = synthetic_landscape((0, 0), |px, py| if px == 5 && py == 5 { 1 } else { 0 });
        let mut landscapes = LandscapeLookup::new();
        landscapes.insert((0, 0), &landscape);
        let mut texture_lookup = TextureLookup::new();
        texture_lookup.insert(0, "tx_a.dds");

        let direct = plan_landscape_cell(&landscapes, &texture_lookup, (0, 0));
        let via_sampler = plan_landscape_cell_with_sampler((0, 0), |cell, px, py| {
            let Some(landscape) = landscapes.get(&cell) else {
                return SampledTextureIdentity::default();
            };
            let raw = raw_vtex_index_at_world_patch(landscape, px, py);
            SampledTextureIdentity {
                id: raw.checked_sub(1).map(|_| u32::from(raw)),
                name: lookup_vtex_texture_name(&texture_lookup, raw),
            }
        });

        assert_eq!(direct, via_sampler);

        let patch = find_patch(&direct, 5, 5);
        assert_eq!(patch.texturing.base_texture, Some("tx_a.dds"));
        assert_eq!(patch.texturing.decal_texture, Some(DEFAULT_LAND_TEXTURE_DDS));
        assert_eq!(patch.texturing.alpha_grid, ISOLATED_ALPHA);
        assert!(patch.texturing.created_property);
    }

    #[test]
    fn default_sampler_matches_wrapper_for_edge_gradient() {
        // West half of the cell uses texture "a" (raw=1), east half uses "b" (raw=2).
        // The patches along the boundary line must produce an N/W edge gradient.
        let landscape = synthetic_landscape((0, 0), |px, _py| if px < 8 { 1 } else { 2 });
        let mut landscapes = LandscapeLookup::new();
        landscapes.insert((0, 0), &landscape);
        let mut texture_lookup = TextureLookup::new();
        texture_lookup.insert(0, "tx_a.dds");
        texture_lookup.insert(1, "tx_b.dds");

        let direct = plan_landscape_cell(&landscapes, &texture_lookup, (0, 0));
        let via_sampler = plan_landscape_cell_with_sampler((0, 0), |cell, px, py| {
            let Some(landscape) = landscapes.get(&cell) else {
                return SampledTextureIdentity::default();
            };
            let raw = raw_vtex_index_at_world_patch(landscape, px, py);
            SampledTextureIdentity {
                id: raw.checked_sub(1).map(|_| u32::from(raw)),
                name: lookup_vtex_texture_name(&texture_lookup, raw),
            }
        });

        assert_eq!(direct, via_sampler);

        // patch (8, 5) is the first column of the east half: current "b", west "a" -> blend.
        let boundary = find_patch(&direct, 8, 5);
        assert_eq!(boundary.texturing.base_texture, Some("tx_b.dds"));
        assert_eq!(boundary.texturing.decal_texture, Some("tx_a.dds"));
        assert!(boundary.texturing.created_property);
    }

    #[test]
    fn sampler_distinguishes_same_raw_index_across_per_cell_tables() {
        // Two adjacent cells reach the planner with the *same* raw VTEX index but
        // distinct canonical textures. The sampler API uses opaque ids so the
        // planner must produce a blend at the border. A planner that compared raw
        // VTEX indices through a single global table would incorrectly elide it.
        let plan = plan_landscape_cell_with_sampler((0, 0), |cell, _px, _py| match cell {
            (0, 0) => SampledTextureIdentity {
                id: Some(101),
                name: Some("a.dds"),
            },
            (-1, 0) => SampledTextureIdentity {
                id: Some(102),
                name: Some("b.dds"),
            },
            _ => SampledTextureIdentity::default(),
        });

        // patch (0, 5) reaches into cell (-1, 0) for west / northwest.
        let border = find_patch(&plan, 0, 5);
        assert_eq!(border.texturing.base_texture, Some("a.dds"));
        assert_eq!(border.texturing.decal_texture, Some("b.dds"));
        assert!(border.texturing.created_property);

        // patch (1, 5) is interior; all neighbors share id 101 -> no blend.
        let interior = find_patch(&plan, 1, 5);
        assert_eq!(interior.texturing.base_texture, Some("a.dds"));
        assert_eq!(interior.texturing.decal_texture, None);
    }

    #[test]
    fn sampler_shared_identity_across_cells_avoids_blend() {
        // Both cells map every patch to the same opaque identity. The planner must
        // treat the border as a single uniform texture even though the patches come
        // from distinct cells (the inverse of the cross-cell divergence case).
        let plan = plan_landscape_cell_with_sampler((0, 0), |cell, _px, _py| match cell {
            (0, 0) | (-1, 0) | (0, 1) | (-1, 1) => SampledTextureIdentity {
                id: Some(7),
                name: Some("same.dds"),
            },
            _ => SampledTextureIdentity::default(),
        });

        let border = find_patch(&plan, 0, 5);
        assert_eq!(border.texturing.base_texture, Some("same.dds"));
        assert_eq!(border.texturing.decal_texture, None);
        assert!(border.texturing.created_property);
    }

    #[test]
    fn sampler_none_neighbors_preserve_isolated_semantics() {
        // Patch (5, 5) is textured but every neighbor reports None: must hit Case 1
        // (isolated) with the ISOLATED_ALPHA grid and a fallback decal.
        let plan = plan_landscape_cell_with_sampler((0, 0), |cell, px, py| {
            if cell == (0, 0) && px == 5 && py == 5 {
                SampledTextureIdentity {
                    id: Some(1),
                    name: Some("a.dds"),
                }
            } else {
                SampledTextureIdentity::default()
            }
        });

        let patch = find_patch(&plan, 5, 5);
        assert_eq!(patch.texturing.base_texture, Some("a.dds"));
        assert_eq!(patch.texturing.decal_texture, Some(DEFAULT_LAND_TEXTURE_DDS));
        assert_eq!(patch.texturing.alpha_grid, ISOLATED_ALPHA);
        assert!(patch.texturing.created_property);
    }

    #[test]
    fn sampler_top_left_corner_ignores_west_when_north_and_northwest_are_missing() {
        // The (0, 15) corner reproduces the runtime border-update quirk via the
        // sampler API: current and west are textured but north / northwest report
        // None, so vanilla treats the patch as isolated against the west.
        let plan = plan_landscape_cell_with_sampler((0, 0), |cell, px, py| {
            if cell == (0, 0) && px == 0 && py == 15 {
                SampledTextureIdentity {
                    id: Some(1),
                    name: Some("current.dds"),
                }
            } else if cell == (-1, 0) && px == 15 && py == 15 {
                SampledTextureIdentity {
                    id: Some(2),
                    name: Some("west.dds"),
                }
            } else {
                SampledTextureIdentity::default()
            }
        });

        let patch = find_patch(&plan, 0, 15);
        assert_eq!(patch.texturing.base_texture, Some("current.dds"));
        assert_eq!(patch.texturing.decal_texture, Some(DEFAULT_LAND_TEXTURE_DDS));
        assert_eq!(patch.texturing.alpha_grid, ISOLATED_ALPHA);
        assert!(patch.texturing.created_property);
    }

    #[test]
    fn sampler_receives_in_range_patch_coords_across_cell_wraps() {
        use std::cell::RefCell;
        use std::collections::HashSet;

        type Call = ((i32, i32), usize, usize);
        let calls: RefCell<Vec<Call>> = RefCell::new(Vec::new());

        let _plan = plan_landscape_cell_with_sampler((0, 0), |cell, px, py| {
            calls.borrow_mut().push((cell, px, py));
            SampledTextureIdentity::default()
        });

        let recorded = calls.borrow();
        assert!(!recorded.is_empty(), "sampler must be called at least once");
        for &(cell, px, py) in recorded.iter() {
            assert!(px < PATCHES_PER_SIDE, "patch_x {} out of range for cell {:?}", px, cell,);
            assert!(py < PATCHES_PER_SIDE, "patch_y {} out of range for cell {:?}", py, cell,);
        }

        // The planner samples four neighbor offsets at every patch, so a wrap must
        // be observed at each of the four corners of the cell. We assert every
        // out-of-cell neighbor cell shows up at least once.
        let cells: HashSet<_> = recorded.iter().map(|(c, _, _)| *c).collect();
        for expected in [(0, 0), (-1, 0), (0, 1), (-1, 1)] {
            assert!(cells.contains(&expected), "expected sampler to visit cell {:?}", expected);
        }
    }
}
