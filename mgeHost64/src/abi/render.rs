use crate::abi::math::{BoundingSphere, D3dxMatrix, D3dxVector3};
use bytemuck::{Pod, Zeroable};
use std::cmp::Ordering;

/// Dynamic-visibility toggle for a group of meshes owned by the client.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct DynVisFlag {
    /// Dynamic-visibility group index written by the client.
    pub group_index: u16,
    /// Non-zero when the referenced meshes should render.
    pub enable: u8,
    /// Reserved for ABI alignment.
    pub _padding0: u8,
}

/// Pair of GPU buffer handles for one generated landscape mesh.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct LandscapeBuffers {
    /// Vertex-buffer handle.
    pub vb: u32,
    /// Index-buffer handle.
    pub ib: u32,
}

/// Render-ready mesh record returned to the 32-bit client.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct RenderMesh {
    /// Non-zero when the mesh is currently enabled for rendering.
    pub enabled: u8,
    /// Non-zero when the subset uses alpha blending.
    pub has_alpha: u8,
    /// Non-zero when the subset animates UV coordinates.
    pub animate_uv: u8,
    /// Reserved for ABI alignment.
    pub _padding0: u8,
    /// Texture handle used for sorting and rendering.
    pub tex: u32,
    /// World transform applied by the client.
    pub transform: D3dxMatrix,
    /// Vertex count.
    pub verts: i32,
    /// Vertex-buffer handle.
    pub v_buffer: u32,
    /// Triangle count.
    pub faces: i32,
    /// Index-buffer handle.
    pub i_buffer: u32,
}

impl RenderMesh {
    /// Sort comparator resolving priority by render state (texture -> vertex buffer).
    pub fn compare_by_state(lhs: &Self, rhs: &Self) -> Ordering {
        lhs.tex.cmp(&rhs.tex).then(lhs.v_buffer.cmp(&rhs.v_buffer))
    }

    /// Sort comparator resolving priority strictly by texture identifier.
    pub fn compare_by_texture(lhs: &Self, rhs: &Self) -> Ordering {
        lhs.tex.cmp(&rhs.tex)
    }
}

/// Generated terrain-horizon culling footprint for one static subset.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct HorizonFootprint {
    /// Subset-local maximum Z.
    pub max_z: f32,
    /// Number of valid vertices in `footprint_xy`; zero means absent.
    pub vertex_count: u8,
    /// Reserved for ABI alignment.
    pub _padding0: [u8; 3],
    /// Subset-local convex XY polygon. Unused slots are zero.
    pub footprint_xy: [[f32; 2]; 6],
}

/// Shared subset metadata for one distant static mesh chunk.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct DistantSubset {
    /// Bounding sphere in model space.
    pub sphere: BoundingSphere,
    /// Axis-aligned minimum corner in model space.
    pub aabb_min: D3dxVector3,
    /// Axis-aligned maximum corner in model space.
    pub aabb_max: D3dxVector3,
    /// Texture handle for this subset.
    pub tex: u32,
    /// Non-zero when the subset uses alpha blending.
    pub has_alpha: u8,
    /// Non-zero when the subset animates UV coordinates.
    pub has_uv_controller: u8,
    /// Reserved for ABI alignment.
    pub _padding0: [u8; 2],
    /// Vertex-buffer handle.
    pub vbuffer: u32,
    /// Index-buffer handle.
    pub ibuffer: u32,
    /// Vertex count.
    pub verts: i32,
    /// Triangle count.
    pub faces: i32,
    /// Optional generated terrain-horizon culling footprint.
    pub horizon_footprint: HorizonFootprint,
    /// Cumulative face count for the far-static tier.
    pub far_faces: i32,
    /// Cumulative face count for the very-far-static tier.
    pub very_far_faces: i32,
}

/// Shared instance metadata for one distant static object.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct DistantStatic {
    /// Static classification used to choose a quadtree bucket.
    pub kind: u8,
    /// Reserved for ABI alignment.
    pub _padding0: [u8; 3],
    /// Bounding sphere of the full static in model space.
    pub sphere: BoundingSphere,
    /// Axis-aligned minimum corner in model space.
    pub aabb_min: D3dxVector3,
    /// Axis-aligned maximum corner in model space.
    pub aabb_max: D3dxVector3,
    /// First subset index in the shared subset array.
    pub first_subset_index: u32,
    /// Number of subsets belonging to this static.
    pub num_subsets: u32,
}
