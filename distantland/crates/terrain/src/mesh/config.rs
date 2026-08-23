//! Configuration and policy for terrain mesh simplification.

const SIMPLIFIER_OPTION_BIT_LOCK_BORDER: u32 = 1 << 2;
const SIMPLIFIER_OPTION_BIT_ERROR_ABSOLUTE: u32 = 1 << 3;

/// Simplifier option bits hashed into the terrain fingerprint.
///
/// Bits 0 and 1 are reserved for meshopt's `Regularize` and `Permissive`, which terrain
/// simplification does not use; the value is fixed so it stays comparable across runs.
pub(crate) const MESH_SIMPLIFIER_OPTION_BITS: u32 = SIMPLIFIER_OPTION_BIT_LOCK_BORDER | SIMPLIFIER_OPTION_BIT_ERROR_ABSOLUTE;

/// Attribute weights used by terrain mesh simplification.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeshSimplifierWeights {
    /// Weight applied to the precomputed smoothed-normal field.
    pub smoothed_normal: f32,
    /// Weight applied to vertex colors.
    pub color: f32,
}

/// Complete simplifier configuration selected for terrain mesh work.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshSimplifierConfig {
    /// Absolute geometric simplification error target.
    pub target_error: f32,
    pub(crate) weights: MeshSimplifierWeights,
}

/// Returns the selected production mesh simplifier configuration for one terrain-detail target error.
pub fn selected_mesh_simplifier_config(target_error: f32, weights: MeshSimplifierWeights) -> MeshSimplifierConfig {
    MeshSimplifierConfig { target_error, weights }
}

/// Returns the meshopt options terrain simplification always runs with.
///
/// Must stay in agreement with [`MESH_SIMPLIFIER_OPTION_BITS`], which records the same
/// choice in the terrain fingerprint.
pub(crate) fn meshopt_simplify_options() -> meshopt::SimplifyOptions {
    meshopt::SimplifyOptions::LockBorder | meshopt::SimplifyOptions::ErrorAbsolute
}
