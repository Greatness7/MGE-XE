#![allow(clippy::too_many_arguments)]

use la_arena::{Arena, Idx};

use crate::abi::{BoundingBox, BoundingSphere, Containment, D3dxVector2, D3dxVector3, D3dxVector4, RenderMesh, ViewFrustum};
use crate::error::HostError;
use crate::state::horizon::{
    HorizonCullStats, HorizonMeshBounds, HorizonTable, horizon_culled_bounds, horizon_culled_capped_xy, horizon_culled_rect,
    horizon_visible_capped_xy,
};

const QUADTREE_MAX_DEPTH: i32 = 10;
const QUADTREE_TARGET_LEAF_SIZE: usize = 15;
const QUADTREE_MIN_DIST: f32 = 20.0;

/// Stable arena index for one quadtree node.
pub type NodeId = Idx<QuadTreeNode>;
/// Stable arena index for one stored mesh.
pub type MeshId = Idx<QuadTreeMesh>;

/// Consumer of visible meshes produced by quadtree traversal.
pub trait MeshSink {
    /// Receives one renderable mesh.
    fn push_mesh(&mut self, mesh: RenderMesh) -> Result<(), HostError>;
}

impl MeshSink for Vec<RenderMesh> {
    fn push_mesh(&mut self, mesh: RenderMesh) -> Result<(), HostError> {
        self.push(mesh);
        Ok(())
    }
}

/// Raw near/far band endpoints used to choose cumulative static-LOD face counts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TierBands {
    /// Near-static band end in world units.
    pub near_end: f32,
    /// Far-static band end in world units.
    pub far_end: f32,
}

/// One renderable mesh plus the bounds needed for culling.
#[derive(Clone, Copy, Debug)]
pub struct QuadTreeMesh {
    /// Mesh payload returned to the client when visible.
    pub render_mesh: RenderMesh,
    /// Independent GPU-residency gate; dynamic visibility remains in `render_mesh.enabled`.
    pub resident: bool,
    /// Bounding sphere used for coarse culling.
    pub sphere: BoundingSphere,
    /// Bounding box used for intersection refinement.
    pub box_value: BoundingBox,
    /// Precomputed XY footprint used by the horizon-culling OBB fallback.
    pub horizon_bounds: HorizonMeshBounds,
    /// Cumulative face count selected in the far-static band.
    pub far_faces: i32,
    /// Cumulative face count selected in the very-far-static band.
    pub very_far_faces: i32,
    /// Retained near-tier face count used to restore a streamed resource after admission.
    pub near_faces: i32,
    /// Retained far-tier face count while the drawable value is zeroed for non-residency.
    pub retained_far_faces: i32,
    /// Retained very-far-tier face count while the drawable value is zeroed for non-residency.
    pub retained_very_far_faces: i32,
}

impl QuadTreeMesh {
    /// Builds a mesh whose face count is unchanged across distance bands.
    pub fn new(
        render_mesh: RenderMesh,
        sphere: BoundingSphere,
        box_value: BoundingBox,
        horizon_bounds: Option<HorizonMeshBounds>,
    ) -> Self {
        let faces = render_mesh.faces;
        Self::with_lod(render_mesh, sphere, box_value, horizon_bounds, faces, faces)
    }

    /// Builds a mesh with cumulative face counts for the far distance bands.
    pub fn with_lod(
        render_mesh: RenderMesh,
        sphere: BoundingSphere,
        box_value: BoundingBox,
        horizon_bounds: Option<HorizonMeshBounds>,
        far_faces: i32,
        very_far_faces: i32,
    ) -> Self {
        let near_faces = render_mesh.faces;
        Self {
            render_mesh,
            resident: true,
            sphere,
            box_value,
            horizon_bounds: horizon_bounds.unwrap_or_else(|| HorizonMeshBounds::from_box(box_value)),
            far_faces,
            very_far_faces,
            near_faces,
            retained_far_faces: far_faces,
            retained_very_far_faces: very_far_faces,
        }
    }
}

fn mesh_horizon_disc(mesh: &QuadTreeMesh) -> ((f32, f32), f32) {
    mesh.horizon_bounds
        .footprint_circle()
        .unwrap_or(((mesh.sphere.center.x, mesh.sphere.center.y), mesh.sphere.radius))
}

/// One quadtree node containing child quadrants and directly attached meshes.
pub struct QuadTreeNode {
    /// Child nodes in quadrant order.
    pub children: [Option<NodeId>; 4],
    /// Extent of the node's 2D square in world-space XY coordinates.
    pub box_size: f32,
    /// XY center of the node's square bounds.
    pub box_center: D3dxVector2,
    /// Bounding sphere enclosing this node's descendants.
    pub sphere: BoundingSphere,
    /// Maximum mesh top-Z over this node's subtree, for the capped horizon prune.
    pub max_z: f32,
    /// Min XY corner of the subtree's footprint box (bbox-union of descendant mesh disc boxes).
    pub xy_min: D3dxVector2,
    /// Max XY corner of the subtree's footprint box.
    pub xy_max: D3dxVector2,
    /// Meshes stored directly in this node.
    pub meshes: Vec<MeshId>,
}

impl Default for QuadTreeNode {
    fn default() -> Self {
        Self {
            children: [None, None, None, None],
            box_size: 0.0,
            box_center: D3dxVector2::default(),
            sphere: BoundingSphere::default(),
            max_z: f32::NEG_INFINITY,
            xy_min: D3dxVector2 {
                x: f32::INFINITY,
                y: f32::INFINITY,
            },
            xy_max: D3dxVector2 {
                x: f32::NEG_INFINITY,
                y: f32::NEG_INFINITY,
            },
            meshes: Vec::new(),
        }
    }
}

/// Spatial index for distant statics and landscape meshes.
pub struct QuadTree {
    root: NodeId,
    nodes: Arena<QuadTreeNode>,
    meshes: Arena<QuadTreeMesh>,
}

impl Default for QuadTree {
    fn default() -> Self {
        let mut nodes = Arena::new();
        let root = nodes.alloc(QuadTreeNode::default());
        Self {
            root,
            nodes,
            meshes: Arena::new(),
        }
    }
}

impl QuadTree {
    fn node(&self, id: NodeId) -> &QuadTreeNode {
        &self.nodes[id]
    }

    fn node_mut(&mut self, id: NodeId) -> &mut QuadTreeNode {
        &mut self.nodes[id]
    }

    /// Returns the root node.
    /// Returns the mesh referenced by `id`.
    pub fn mesh(&self, id: MeshId) -> &QuadTreeMesh {
        &self.meshes[id]
    }

    /// Returns the mesh referenced by `id` mutably.
    pub fn mesh_mut(&mut self, id: MeshId) -> &mut QuadTreeMesh {
        &mut self.meshes[id]
    }

    /// Returns the enabled flag for a mesh, for assertions in tests.
    #[cfg(test)]
    pub(crate) fn mesh_enabled(&self, id: MeshId) -> u8 {
        self.mesh(id).render_mesh.enabled
    }

    /// Inserts one completed mesh into the tree and returns its stable `MeshId`.
    pub fn insert_mesh(&mut self, mesh: QuadTreeMesh) -> MeshId {
        let mesh = self.meshes.alloc(mesh);
        self.add_mesh_to_node(self.root, mesh, QUADTREE_MAX_DEPTH);
        mesh
    }

    /// Sets the root square used to partition new inserts.
    pub fn set_box(&mut self, size: f32, center: D3dxVector2) {
        let root = self.node_mut(self.root);
        root.box_size = size;
        root.box_center = center;
    }

    /// Collapses chains of single-child nodes left behind by insertion.
    pub fn optimize(&mut self) {
        let _ = self.optimize_node(self.root);
    }

    /// Recomputes bounding spheres and max_z bottom-up.
    pub fn calc_volume(&mut self) {
        let _ = self.calc_volume_node(self.root);
    }

    /// Traverses the tree with frustum, distance culling, and optional static-LOD tier selection.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by `output`.
    pub fn get_visible_meshes_with_bands<S: MeshSink>(
        &self,
        frustum: &ViewFrustum,
        view_sphere: D3dxVector4,
        horizon: Option<&HorizonTable>,
        bands: Option<TierBands>,
        output: &mut S,
        stats: &mut HorizonCullStats,
    ) -> Result<(), HostError> {
        self.collect_visible_meshes_node(self.root, frustum, view_sphere, horizon, bands, output, stats, false)
    }

    /// Traverses the tree using only frustum culling.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by `output`.
    pub fn get_visible_meshes_coarse<S: MeshSink>(&self, frustum: &ViewFrustum, output: &mut S) -> Result<(), HostError> {
        self.collect_visible_meshes_coarse_node(self.root, frustum, output, false)
    }

    /// Inserts a mesh into `node`, splitting downward when density thresholds are exceeded.
    fn add_mesh_to_node(&mut self, node: NodeId, mesh: MeshId, depth: i32) {
        let meshes_size = self.node(node).meshes.len();
        let children = self.node(node).children;
        if depth == 0 {
            self.node_mut(node).meshes.push(mesh);
            return;
        }
        if child_count(&children) == 0 && meshes_size < QUADTREE_TARGET_LEAF_SIZE {
            self.node_mut(node).meshes.push(mesh);
            return;
        }
        if let Some(&first_id) = self.node(node).meshes.first() {
            let first = self.mesh(first_id);
            let next = self.mesh(mesh);
            let diff = next.sphere.center - first.sphere.center;
            // Nearby meshes stay grouped even past the target leaf size so dense clusters do not
            // explode into very deep trees that offer little culling benefit.
            if diff.length() <= QUADTREE_MIN_DIST {
                self.node_mut(node).meshes.push(mesh);
                return;
            }
        }

        self.push_down(node, mesh, depth);
        let existing_meshes = std::mem::take(&mut self.node_mut(node).meshes);
        for existing_mesh in existing_meshes {
            self.push_down(node, existing_mesh, depth);
        }
    }

    /// Pushes a mesh into the appropriate child quadrant, creating the child on demand.
    fn push_down(&mut self, node: NodeId, mesh: MeshId, depth: i32) {
        let center = self.mesh(mesh).sphere.center;
        let (box_size, box_center, quadrant, existing_child) = {
            let node_ref = self.node(node);
            let quadrant = quadrant_for_center(node_ref.box_center, center);
            (node_ref.box_size, node_ref.box_center, quadrant, node_ref.children[quadrant])
        };

        let child = if let Some(child) = existing_child {
            child
        } else {
            let child_box_size = box_size / 2.0;
            let child_center = child_center_for_quadrant(box_center, box_size, quadrant);
            let child = self.nodes.alloc(QuadTreeNode {
                box_size: child_box_size,
                box_center: child_center,
                ..QuadTreeNode::default()
            });
            self.node_mut(node).children[quadrant] = Some(child);
            child
        };

        self.add_mesh_to_node(child, mesh, depth - 1);
    }

    /// Removes redundant single-child nodes after bulk insertion.
    fn optimize_node(&mut self, node: NodeId) -> bool {
        let children = self.node(node).children;
        for (quadrant, child) in children.into_iter().enumerate() {
            if let Some(child_id) = child
                && self.optimize_node(child_id)
            {
                let replacement = self.node(child_id).children.into_iter().flatten().next();
                self.node_mut(node).children[quadrant] = replacement;
            }
        }
        child_count(&self.node(node).children) == 1
    }

    /// Recomputes the bounding sphere, max_z, and XY bounds for `node` and returns them.
    fn calc_volume_node(&mut self, node: NodeId) -> (BoundingSphere, f32, D3dxVector2, D3dxVector2) {
        let children = self.node(node).children;
        debug_assert!(
            self.node(node).meshes.is_empty() || children.iter().all(|c| c.is_none()),
            "Node has both children and direct meshes!"
        );

        let mut sphere = BoundingSphere::default();
        let mut max_z = f32::NEG_INFINITY;
        let mut xy_min = D3dxVector2 {
            x: f32::INFINITY,
            y: f32::INFINITY,
        };
        let mut xy_max = D3dxVector2 {
            x: f32::NEG_INFINITY,
            y: f32::NEG_INFINITY,
        };
        let mut has_children = false;
        for child in children.into_iter().flatten() {
            has_children = true;
            let (child_sphere, child_max_z, child_xy_min, child_xy_max) = self.calc_volume_node(child);
            sphere = sphere.union_with(child_sphere);
            max_z = max_z.max(child_max_z);
            xy_min.x = xy_min.x.min(child_xy_min.x);
            xy_min.y = xy_min.y.min(child_xy_min.y);
            xy_max.x = xy_max.x.max(child_xy_max.x);
            xy_max.y = xy_max.y.max(child_xy_max.y);
        }
        if !has_children {
            let node_ref = self.node(node);
            for &mesh in &node_ref.meshes {
                let mesh_ref = self.mesh(mesh);
                sphere = sphere.union_with(mesh_ref.sphere);
                max_z = max_z.max(mesh_ref.horizon_bounds.max_z);
                let cx = mesh_ref.sphere.center.x;
                let cy = mesh_ref.sphere.center.y;
                let r = mesh_ref.sphere.radius;
                xy_min.x = xy_min.x.min(cx - r);
                xy_min.y = xy_min.y.min(cy - r);
                xy_max.x = xy_max.x.max(cx + r);
                xy_max.y = xy_max.y.max(cy + r);
            }
        }
        let node_mut = self.node_mut(node);
        node_mut.sphere = sphere;
        node_mut.max_z = max_z;
        node_mut.xy_min = xy_min;
        node_mut.xy_max = xy_max;
        (sphere, max_z, xy_min, xy_max)
    }

    /// Recursively emits visible meshes with frustum and radius culling.
    fn collect_visible_meshes_node<S: MeshSink>(
        &self,
        node: NodeId,
        frustum: &ViewFrustum,
        view_sphere: D3dxVector4,
        horizon: Option<&HorizonTable>,
        bands: Option<TierBands>,
        output: &mut S,
        stats: &mut HorizonCullStats,
        inside: bool,
    ) -> Result<(), HostError> {
        let node_ref = self.node(node);
        let mut branch_inside = inside;
        if !branch_inside {
            match frustum.contains_sphere(&node_ref.sphere) {
                Containment::Outside => return Ok(()),
                Containment::Inside => branch_inside = true,
                Containment::Intersects => {}
            }
        }
        let eye = D3dxVector3 {
            x: view_sphere.x,
            y: view_sphere.y,
            z: view_sphere.z,
        };
        if view_sphere.w.is_finite() && !node_ref.sphere.empty() {
            let diff = node_ref.sphere.center - eye;
            let range_squared = diff.x * diff.x + diff.y * diff.y + diff.z * diff.z;
            let view_limit = view_sphere.w + node_ref.sphere.radius;
            if range_squared > view_limit * view_limit {
                return Ok(());
            }
        }
        if let Some(table) = horizon
            && !node_ref.sphere.empty()
            && node_ref.max_z.is_finite()
            && horizon_culled_rect(node_ref.xy_min, node_ref.xy_max, node_ref.max_z, table)
        {
            stats.nodes_pruned += 1;
            return Ok(());
        }

        for child in node_ref.children.into_iter().flatten() {
            self.collect_visible_meshes_node(child, frustum, view_sphere, horizon, bands, output, stats, branch_inside)?;
        }
        if node_ref.meshes.is_empty() {
            return Ok(());
        }

        for &mesh_id in &node_ref.meshes {
            let mesh = self.mesh(mesh_id);
            if mesh.render_mesh.enabled == 0 || !mesh.resident {
                continue;
            }
            if !branch_inside {
                match frustum.contains_sphere(&mesh.sphere) {
                    Containment::Outside => continue,
                    Containment::Intersects => {
                        // Sphere tests are cheap but conservative. Refine intersecting meshes with
                        // the oriented box before doing the more expensive distance check.
                        if frustum.contains_box(&mesh.box_value) == Containment::Outside {
                            continue;
                        }
                    }
                    Containment::Inside => {}
                }
            }
            let diff = mesh.sphere.center - eye;
            let range_squared = diff.x * diff.x + diff.y * diff.y + diff.z * diff.z;
            let view_limit = view_sphere.w + mesh.sphere.radius;
            if range_squared <= view_limit * view_limit {
                if horizon.is_some() {
                    stats.mesh_candidates += 1;
                }
                if let Some(table) = horizon {
                    let (disc_center, disc_radius) = mesh_horizon_disc(mesh);
                    if horizon_culled_capped_xy(disc_center, disc_radius, mesh.horizon_bounds.max_z, table) {
                        stats.meshes_culled += 1;
                        continue;
                    }

                    if horizon_visible_capped_xy(disc_center, disc_radius, mesh.horizon_bounds.max_z, table) {
                        // Provably above the horizon: the OBB fallback would also keep it, so skip it.
                        stats.early_accepts += 1;
                    } else {
                        stats.obb_fallback_tests += 1;
                        if horizon_culled_bounds(&mesh.horizon_bounds, table) {
                            stats.meshes_culled += 1;
                            stats.obb_fallback_culled += 1;
                            continue;
                        }
                    }
                }
                let mut render_mesh = mesh.render_mesh;
                if let Some(bands) = bands {
                    let radius = mesh.sphere.radius;
                    let near_limit = bands.near_end + radius;
                    let far_limit = bands.far_end + radius;
                    render_mesh.faces = if range_squared <= near_limit * near_limit {
                        render_mesh.faces
                    } else if range_squared <= far_limit * far_limit {
                        mesh.far_faces
                    } else {
                        mesh.very_far_faces
                    };
                    if render_mesh.faces == 0 {
                        continue;
                    }
                }
                output.push_mesh(render_mesh)?;
            }
        }
        Ok(())
    }

    /// Recursively emits visible meshes using only coarse frustum tests.
    fn collect_visible_meshes_coarse_node<S: MeshSink>(
        &self,
        node: NodeId,
        frustum: &ViewFrustum,
        output: &mut S,
        inside: bool,
    ) -> Result<(), HostError> {
        let node_ref = self.node(node);
        let mut branch_inside = inside;
        if !branch_inside {
            match frustum.contains_sphere(&node_ref.sphere) {
                Containment::Outside => return Ok(()),
                Containment::Inside => branch_inside = true,
                Containment::Intersects => {}
            }
        }

        for child in node_ref.children.into_iter().flatten() {
            self.collect_visible_meshes_coarse_node(child, frustum, output, branch_inside)?;
        }
        for &mesh_id in &node_ref.meshes {
            let mesh = self.mesh(mesh_id);
            if mesh.render_mesh.enabled == 0 || !mesh.resident {
                continue;
            }
            if !branch_inside && frustum.contains_sphere(&mesh.sphere) == Containment::Outside {
                continue;
            }
            output.push_mesh(mesh.render_mesh)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn node_count(&self) -> usize {
        self.nodes.len()
    }

    #[cfg(test)]
    fn mesh_count(&self) -> usize {
        self.meshes.len()
    }
}

/// Counts how many child slots are populated.
fn child_count(children: &[Option<NodeId>; 4]) -> i32 {
    children.iter().flatten().count() as i32
}

/// Maps a mesh center to one of the four child quadrants.
fn quadrant_for_center(box_center: D3dxVector2, center: D3dxVector3) -> usize {
    if center.y > box_center.y {
        usize::from(center.x <= box_center.x)
    } else if center.x < box_center.x {
        2
    } else {
        3
    }
}

/// Returns the child-box center for a given quadrant.
fn child_center_for_quadrant(box_center: D3dxVector2, box_size: f32, quadrant: usize) -> D3dxVector2 {
    let quarter = box_size / 4.0;
    match quadrant {
        0 => D3dxVector2 {
            x: box_center.x + quarter,
            y: box_center.y + quarter,
        },
        1 => D3dxVector2 {
            x: box_center.x - quarter,
            y: box_center.y + quarter,
        },
        2 => D3dxVector2 {
            x: box_center.x - quarter,
            y: box_center.y - quarter,
        },
        _ => D3dxVector2 {
            x: box_center.x + quarter,
            y: box_center.y - quarter,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::D3dxMatrix;
    use crate::state::horizon::horizon_culled_capped;
    use crate::state::test_support::{test_frustum, test_frustum_with_extent};

    struct FailingSink {
        pushed: usize,
        fail_after: usize,
    }

    impl MeshSink for FailingSink {
        fn push_mesh(&mut self, _mesh: RenderMesh) -> Result<(), HostError> {
            self.pushed += 1;
            if self.pushed > self.fail_after {
                return Err(HostError::listen("sink failure"));
            }
            Ok(())
        }
    }

    fn insert_test_mesh(
        tree: &mut QuadTree,
        sphere: BoundingSphere,
        box_value: BoundingBox,
        horizon_bounds: Option<HorizonMeshBounds>,
        transform: D3dxMatrix,
        has_alpha: bool,
        animate_uv: bool,
        tex: u32,
        verts: i32,
        v_buffer: u32,
        faces: i32,
        i_buffer: u32,
    ) -> MeshId {
        tree.insert_mesh(QuadTreeMesh::new(
            RenderMesh {
                enabled: 1,
                has_alpha: has_alpha as u8,
                animate_uv: animate_uv as u8,
                _padding0: 0,
                tex,
                transform,
                verts,
                v_buffer,
                faces,
                i_buffer,
            },
            sphere,
            box_value,
            horizon_bounds,
        ))
    }

    fn add_test_mesh(tree: &mut QuadTree, tex: u32, x: f32, y: f32) -> MeshId {
        insert_test_mesh(
            tree,
            BoundingSphere {
                center: D3dxVector3 { x, y, z: 0.0 },
                radius: 5.0,
            },
            BoundingBox::default(),
            None,
            D3dxMatrix::identity(),
            false,
            false,
            tex,
            3,
            tex + 100,
            1,
            tex + 200,
        )
    }

    #[test]
    fn coarse_traversal_emits_meshes_through_vec_sink() {
        let mut tree = QuadTree::default();
        tree.set_box(400.0, D3dxVector2::default());
        add_test_mesh(&mut tree, 10, 10.0, 10.0);
        add_test_mesh(&mut tree, 20, -10.0, 10.0);
        add_test_mesh(&mut tree, 30, -10.0, -10.0);

        let mut output = Vec::new();
        tree.get_visible_meshes_coarse(&test_frustum(), &mut output).unwrap();

        let textures: Vec<u32> = output.into_iter().map(|mesh| mesh.tex).collect();
        assert_eq!(textures, vec![10, 20, 30]);
    }

    #[test]
    fn precise_traversal_emits_meshes_through_vec_sink() {
        let mut tree = QuadTree::default();
        tree.set_box(400.0, D3dxVector2::default());
        add_test_mesh(&mut tree, 10, 10.0, 10.0);
        add_test_mesh(&mut tree, 20, -10.0, 10.0);
        add_test_mesh(&mut tree, 30, -10.0, -10.0);
        tree.calc_volume();

        let mut output = Vec::new();
        let mut stats = HorizonCullStats::default();
        tree.get_visible_meshes_with_bands(
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 2000.0,
            },
            None,
            None,
            &mut output,
            &mut stats,
        )
        .unwrap();

        let textures: Vec<u32> = output.into_iter().map(|mesh| mesh.tex).collect();
        assert_eq!(textures, vec![10, 20, 30]);
    }

    #[test]
    fn traversal_stops_when_sink_fails() {
        let mut tree = QuadTree::default();
        tree.set_box(400.0, D3dxVector2::default());
        add_test_mesh(&mut tree, 10, 10.0, 10.0);
        add_test_mesh(&mut tree, 20, -10.0, 10.0);
        add_test_mesh(&mut tree, 30, -10.0, -10.0);

        let mut sink = FailingSink {
            pushed: 0,
            fail_after: 1,
        };
        let error = tree.get_visible_meshes_coarse(&test_frustum(), &mut sink).unwrap_err();
        assert_eq!(sink.pushed, 2);
        assert!(matches!(error, HostError::Listen(_)));
    }

    #[test]
    fn mesh_ids_remain_stable_across_arena_growth() {
        let mut tree = QuadTree::default();
        tree.set_box(4000.0, D3dxVector2::default());
        let first = add_test_mesh(&mut tree, 1, 0.0, 0.0);
        for index in 0..600 {
            add_test_mesh(&mut tree, index as u32 + 2, index as f32 + 50.0, index as f32 + 75.0);
        }

        let mesh = tree.mesh(first);
        assert_eq!(mesh.render_mesh.tex, 1);
        assert_eq!(mesh.render_mesh.v_buffer, 101);
    }

    #[test]
    fn distributed_meshes_create_subnodes_and_remain_queryable() {
        let mut tree = QuadTree::default();
        tree.set_box(8192.0, D3dxVector2::default());
        for index in 0..2000 {
            let x = ((index % 50) as f32 * 120.0) - 3000.0;
            let y = ((index / 50) as f32 * 120.0) - 2400.0;
            add_test_mesh(&mut tree, index as u32 + 1, x, y);
        }

        tree.calc_volume();

        let mut output = Vec::new();
        tree.get_visible_meshes_coarse(&test_frustum_with_extent(10_000.0), &mut output)
            .unwrap();

        assert!(tree.node_count() > 1);
        assert_eq!(tree.mesh_count(), 2000);
        assert_eq!(output.len(), 2000);
    }

    fn make_test_bounds(max_z: f32) -> HorizonMeshBounds {
        HorizonMeshBounds {
            max_z,
            vertex_count: 0,
            footprint_xy: [(0.0, 0.0); 6],
            footprint_center: (0.0, 0.0),
            footprint_radius: 0.0,
        }
    }

    #[test]
    fn degenerate_horizon_footprint_disc_falls_back_to_sphere() {
        let sphere = BoundingSphere {
            center: D3dxVector3 {
                x: 100.0,
                y: 200.0,
                z: 300.0,
            },
            radius: 40.0,
        };
        let mesh = QuadTreeMesh::new(
            RenderMesh {
                enabled: 1,
                has_alpha: 0,
                animate_uv: 0,
                _padding0: 0,
                tex: 1,
                transform: D3dxMatrix::identity(),
                verts: 3,
                v_buffer: 101,
                faces: 1,
                i_buffer: 201,
            },
            sphere,
            BoundingBox::default(),
            Some(make_test_bounds(500.0)),
        );

        assert_eq!(mesh_horizon_disc(&mesh), ((100.0, 200.0), 40.0));
    }

    #[test]
    fn node_prune_fires_output_empty() {
        let mut tree = QuadTree::default();
        tree.set_box(400.0, D3dxVector2::default());
        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 {
                    x: 100.0,
                    y: 100.0,
                    z: 0.0,
                },
                radius: 5.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(10.0)),
            D3dxMatrix::identity(),
            false,
            false,
            1,
            3,
            101,
            1,
            201,
        );
        tree.calc_volume();

        let table = HorizonTable {
            eye: D3dxVector3 { x: 0.0, y: 0.0, z: 50.0 },
            bin_count: 8,
            ring_count: 20,
            ring_step: 10.0,
            r_near: 0.0,
            bias_obj_z: 0.0,
            bias_z: 0.0,
            max_slope: vec![1.0; 8 * 20],
        };

        let mut output = Vec::new();
        let mut stats = HorizonCullStats::default();
        tree.get_visible_meshes_with_bands(
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 50.0,
                w: 2000.0,
            },
            Some(&table),
            None,
            &mut output,
            &mut stats,
        )
        .unwrap();

        assert_eq!(output.len(), 0);
        assert_eq!(stats.nodes_pruned, 1);
        assert_eq!(stats.mesh_candidates, 0);
    }

    #[test]
    fn node_prune_declines_on_tall_member() {
        let mut tree = QuadTree::default();
        tree.set_box(800.0, D3dxVector2::default());

        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 {
                    x: 200.0,
                    y: 200.0,
                    z: 0.0,
                },
                radius: 5.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(10.0)),
            D3dxMatrix::identity(),
            false,
            false,
            1,
            3,
            101,
            1,
            201,
        );

        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 {
                    x: 205.0,
                    y: 205.0,
                    z: 0.0,
                },
                radius: 5.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(500.0)),
            D3dxMatrix::identity(),
            false,
            false,
            2,
            3,
            102,
            1,
            202,
        );

        tree.calc_volume();

        let table = HorizonTable {
            eye: D3dxVector3 { x: 0.0, y: 0.0, z: 50.0 },
            bin_count: 8,
            ring_count: 20,
            ring_step: 10.0,
            r_near: 0.0,
            bias_obj_z: 0.0,
            bias_z: 0.0,
            max_slope: vec![1.0; 8 * 20],
        };

        let mut output = Vec::new();
        let mut stats = HorizonCullStats::default();
        tree.get_visible_meshes_with_bands(
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 50.0,
                w: 2000.0,
            },
            Some(&table),
            None,
            &mut output,
            &mut stats,
        )
        .unwrap();

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].tex, 2);
        assert_eq!(stats.nodes_pruned, 0);
        assert_eq!(stats.mesh_candidates, 2);
        assert_eq!(stats.meshes_culled, 1);
    }

    fn collect_meshes_no_node_prune<S: MeshSink>(
        tree: &QuadTree,
        node: NodeId,
        frustum: &ViewFrustum,
        view_sphere: D3dxVector4,
        horizon: Option<&HorizonTable>,
        output: &mut S,
        inside: bool,
    ) -> Result<(), HostError> {
        let node_ref = tree.node(node);
        let mut branch_inside = inside;
        if !branch_inside {
            match frustum.contains_sphere(&node_ref.sphere) {
                Containment::Outside => return Ok(()),
                Containment::Inside => branch_inside = true,
                Containment::Intersects => {}
            }
        }

        for child in node_ref.children.into_iter().flatten() {
            collect_meshes_no_node_prune(tree, child, frustum, view_sphere, horizon, output, branch_inside)?;
        }
        if node_ref.meshes.is_empty() {
            return Ok(());
        }

        let eye = D3dxVector3 {
            x: view_sphere.x,
            y: view_sphere.y,
            z: view_sphere.z,
        };
        for &mesh_id in &node_ref.meshes {
            let mesh = tree.mesh(mesh_id);
            if mesh.render_mesh.enabled == 0 {
                continue;
            }
            if !branch_inside {
                match frustum.contains_sphere(&mesh.sphere) {
                    Containment::Outside => continue,
                    Containment::Intersects => {
                        if frustum.contains_box(&mesh.box_value) == Containment::Outside {
                            continue;
                        }
                    }
                    Containment::Inside => {}
                }
            }
            let diff = mesh.sphere.center - eye;
            let range_squared = diff.x * diff.x + diff.y * diff.y + diff.z * diff.z;
            let view_limit = view_sphere.w + mesh.sphere.radius;
            if range_squared <= view_limit * view_limit {
                if let Some(table) = horizon {
                    let (disc_center, disc_radius) = mesh_horizon_disc(mesh);
                    if horizon_culled_capped_xy(disc_center, disc_radius, mesh.horizon_bounds.max_z, table) {
                        continue;
                    }
                    if horizon_visible_capped_xy(disc_center, disc_radius, mesh.horizon_bounds.max_z, table) {
                        // The conservative accept skips the OBB fallback.
                    } else {
                        if horizon_culled_bounds(&mesh.horizon_bounds, table) {
                            continue;
                        }
                    }
                }
                output.push_mesh(mesh.render_mesh)?;
            }
        }
        Ok(())
    }

    #[test]
    fn node_prune_equivalence_invariant() {
        let mut seed: u32 = 42;
        let mut rng = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            seed
        };
        let mut rand_f32 = |min: f32, max: f32| {
            let r = rng() as f32 / u32::MAX as f32;
            min + r * (max - min)
        };

        for trial in 0..20 {
            let mut tree = QuadTree::default();
            tree.set_box(8192.0, D3dxVector2::default());

            let elong_x = trial % 4 == 0;
            let elong_y = trial % 4 == 1;
            let wide_nodes = trial % 4 == 2;

            for i in 0..100 {
                let x = if elong_x {
                    rand_f32(-3000.0, 3000.0)
                } else if elong_y {
                    rand_f32(-10.0, 10.0)
                } else if wide_nodes {
                    rand_f32(-4000.0, 4000.0)
                } else {
                    rand_f32(-2000.0, 2000.0)
                };

                let y = if elong_x {
                    rand_f32(-10.0, 10.0)
                } else if elong_y {
                    rand_f32(-3000.0, 3000.0)
                } else if wide_nodes {
                    rand_f32(-4000.0, 4000.0)
                } else {
                    rand_f32(-2000.0, 2000.0)
                };

                let z = if trial % 3 == 0 {
                    rand_f32(-200.0, -50.0) // negative delta bias
                } else if trial % 3 == 1 {
                    0.0 // zero top delta bias
                } else {
                    rand_f32(50.0, 200.0) // positive top delta bias
                };

                let radius = rand_f32(5.0, 150.0);
                let max_z = z + rand_f32(-20.0, 100.0);
                insert_test_mesh(
                    &mut tree,
                    BoundingSphere {
                        center: D3dxVector3 { x, y, z },
                        radius,
                    },
                    BoundingBox::default(),
                    Some(make_test_bounds(max_z)),
                    D3dxMatrix::identity(),
                    false,
                    false,
                    i,
                    3,
                    100 + i,
                    1,
                    200 + i,
                );
            }
            tree.calc_volume();

            let eye = if trial % 5 == 0 {
                D3dxVector3 {
                    x: rand_f32(-50.0, 50.0),
                    y: rand_f32(-50.0, 50.0),
                    z: rand_f32(10.0, 100.0),
                }
            } else if trial % 5 == 1 {
                D3dxVector3 {
                    x: 0.0,
                    y: rand_f32(-1000.0, -500.0),
                    z: rand_f32(0.0, 100.0),
                }
            } else {
                D3dxVector3 {
                    x: rand_f32(-200.0, 200.0),
                    y: rand_f32(-200.0, 200.0),
                    z: rand_f32(0.0, 200.0),
                }
            };
            let mut max_slope = vec![0.0; 8 * 10];
            for bin in 0..8 {
                let mut running = f32::NEG_INFINITY;
                for ring in 0..10 {
                    let idx = bin * 10 + ring;
                    let val = rand_f32(-0.5, 1.5);
                    running = running.max(val);
                    max_slope[idx] = running;
                }
            }
            let table = HorizonTable {
                eye,
                bin_count: 8,
                ring_count: 10,
                ring_step: if trial % 2 == 0 { 256.0 } else { 128.0 },
                r_near: 0.0,
                bias_obj_z: 0.0,
                bias_z: 0.0,
                max_slope,
            };

            let view_sphere = D3dxVector4 {
                x: eye.x,
                y: eye.y,
                z: eye.z,
                w: rand_f32(500.0, 3000.0),
            };

            let mut output_normal = Vec::new();
            let mut stats_normal = HorizonCullStats::default();
            tree.get_visible_meshes_with_bands(
                &test_frustum_with_extent(4000.0),
                view_sphere,
                Some(&table),
                None,
                &mut output_normal,
                &mut stats_normal,
            )
            .unwrap();

            let mut output_no_prune = Vec::new();
            collect_meshes_no_node_prune(
                &tree,
                tree.root,
                &test_frustum_with_extent(4000.0),
                view_sphere,
                Some(&table),
                &mut output_no_prune,
                false,
            )
            .unwrap();

            let normal_texs: Vec<u32> = output_normal.iter().map(|m| m.tex).collect();
            let no_prune_texs: Vec<u32> = output_no_prune.iter().map(|m| m.tex).collect();
            assert_eq!(normal_texs, no_prune_texs);
        }
    }

    #[test]
    fn empty_degenerate_nodes() {
        let mut tree = QuadTree::default();
        tree.set_box(400.0, D3dxVector2::default());
        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 {
                    x: 10.0,
                    y: 10.0,
                    z: 0.0,
                },
                radius: 5.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(f32::INFINITY)),
            D3dxMatrix::identity(),
            false,
            false,
            1,
            3,
            101,
            1,
            201,
        );
        tree.calc_volume();

        let table = HorizonTable {
            eye: D3dxVector3 { x: 0.0, y: 0.0, z: 50.0 },
            bin_count: 8,
            ring_count: 20,
            ring_step: 10.0,
            r_near: 0.0,
            bias_obj_z: 0.0,
            bias_z: 0.0,
            max_slope: vec![1.0; 8 * 20],
        };

        let mut output = Vec::new();
        let mut stats = HorizonCullStats::default();
        tree.get_visible_meshes_with_bands(
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 50.0,
                w: 2000.0,
            },
            Some(&table),
            None,
            &mut output,
            &mut stats,
        )
        .unwrap();

        assert_eq!(output.len(), 1);
        assert_eq!(stats.nodes_pruned, 0);
    }

    #[test]
    fn below_eye_node_cull_soundness() {
        let mut tree = QuadTree::default();
        tree.set_box(1000.0, D3dxVector2::default());

        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 {
                    x: 200.0,
                    y: 0.0,
                    z: -10.0,
                },
                radius: 10.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(-10.0)),
            D3dxMatrix::identity(),
            false,
            false,
            1,
            3,
            101,
            1,
            201,
        );
        tree.calc_volume();

        let table = HorizonTable {
            eye: D3dxVector3 { x: 0.0, y: 0.0, z: 50.0 },
            bin_count: 8,
            ring_count: 10,
            ring_step: 100.0,
            r_near: 0.0,
            bias_obj_z: 0.0,
            bias_z: 0.0,
            max_slope: vec![-0.4; 8 * 10],
        };

        let mut output = Vec::new();
        let mut stats = HorizonCullStats::default();
        tree.get_visible_meshes_with_bands(
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 50.0,
                w: 2000.0,
            },
            Some(&table),
            None,
            &mut output,
            &mut stats,
        )
        .unwrap();

        let mut output_no_prune = Vec::new();
        collect_meshes_no_node_prune(
            &tree,
            tree.root,
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 50.0,
                w: 2000.0,
            },
            Some(&table),
            &mut output_no_prune,
            false,
        )
        .unwrap();

        assert_eq!(output.len(), output_no_prune.len());
    }

    #[test]
    fn ring_eye_near_boundaries() {
        let mut tree = QuadTree::default();
        tree.set_box(400.0, D3dxVector2::default());
        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 { x: 2.0, y: 0.0, z: 0.0 },
                radius: 5.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(10.0)),
            D3dxMatrix::identity(),
            false,
            false,
            1,
            3,
            101,
            1,
            201,
        );
        tree.calc_volume();

        let table = HorizonTable {
            eye: D3dxVector3 { x: 0.0, y: 0.0, z: 0.0 },
            bin_count: 8,
            ring_count: 5,
            ring_step: 10.0,
            r_near: 0.0,
            bias_obj_z: 0.0,
            bias_z: 0.0,
            max_slope: vec![1.0; 8 * 5],
        };

        let mut output = Vec::new();
        let mut stats = HorizonCullStats::default();
        tree.get_visible_meshes_with_bands(
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 2000.0,
            },
            Some(&table),
            None,
            &mut output,
            &mut stats,
        )
        .unwrap();

        assert_eq!(output.len(), 1);
        assert_eq!(stats.nodes_pruned, 0);
    }

    #[test]
    fn post_optimize_monotonicity() {
        let mut tree = QuadTree::default();
        tree.set_box(1000.0, D3dxVector2::default());
        for i in 0..50 {
            insert_test_mesh(
                &mut tree,
                BoundingSphere {
                    center: D3dxVector3 {
                        x: i as f32 * 10.0 - 250.0,
                        y: i as f32 * 5.0 - 125.0,
                        z: 0.0,
                    },
                    radius: 5.0,
                },
                BoundingBox::default(),
                Some(make_test_bounds(i as f32 * 2.0)),
                D3dxMatrix::identity(),
                false,
                false,
                i,
                3,
                100 + i,
                1,
                200 + i,
            );
        }
        tree.optimize();
        tree.calc_volume();

        fn verify_monotonicity(tree: &QuadTree, node_id: NodeId) -> (f32, bool) {
            let node = tree.node(node_id);
            let mut max_z = f32::NEG_INFINITY;
            let mut has_children = false;
            for child in node.children.into_iter().flatten() {
                has_children = true;
                let (child_max, _) = verify_monotonicity(tree, child);
                max_z = max_z.max(child_max);
            }
            if !has_children {
                for &mesh_id in &node.meshes {
                    let mesh = tree.mesh(mesh_id);
                    max_z = max_z.max(mesh.horizon_bounds.max_z);
                }
            } else {
                assert!(node.meshes.is_empty(), "Node has children but also direct meshes!");
            }
            assert!(
                node.max_z >= max_z,
                "Node max_z ({}) < child/mesh max_z ({})",
                node.max_z,
                max_z
            );
            (node.max_z, has_children)
        }

        verify_monotonicity(&tree, tree.root);
    }

    #[test]
    fn node_xy_box_containment_invariant() {
        let mut tree = QuadTree::default();
        tree.set_box(2000.0, D3dxVector2::default());
        let mut seed: u32 = 12345;
        let mut rng = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            seed
        };
        let mut rand_f32 = |min: f32, max: f32| {
            let r = rng() as f32 / u32::MAX as f32;
            min + r * (max - min)
        };

        for i in 0..100 {
            let x = rand_f32(-1000.0, 1000.0);
            let y = rand_f32(-1000.0, 1000.0);
            let z = rand_f32(-100.0, 100.0);
            let radius = rand_f32(5.0, 50.0);
            insert_test_mesh(
                &mut tree,
                BoundingSphere {
                    center: D3dxVector3 { x, y, z },
                    radius,
                },
                BoundingBox::default(),
                Some(make_test_bounds(z + 10.0)),
                D3dxMatrix::identity(),
                false,
                false,
                i,
                3,
                100 + i,
                1,
                200 + i,
            );
        }
        tree.optimize();
        tree.calc_volume();

        fn verify_containment(tree: &QuadTree, node_id: NodeId) -> (D3dxVector2, D3dxVector2) {
            let node = tree.node(node_id);
            let mut expected_min = D3dxVector2 {
                x: f32::INFINITY,
                y: f32::INFINITY,
            };
            let mut expected_max = D3dxVector2 {
                x: f32::NEG_INFINITY,
                y: f32::NEG_INFINITY,
            };

            let mut has_children = false;
            for child in node.children.into_iter().flatten() {
                has_children = true;
                let (child_min, child_max) = verify_containment(tree, child);
                expected_min.x = expected_min.x.min(child_min.x);
                expected_min.y = expected_min.y.min(child_min.y);
                expected_max.x = expected_max.x.max(child_max.x);
                expected_max.y = expected_max.y.max(child_max.y);
            }
            if !has_children {
                for &mesh_id in &node.meshes {
                    let mesh = tree.mesh(mesh_id);
                    let cx = mesh.sphere.center.x;
                    let cy = mesh.sphere.center.y;
                    let r = mesh.sphere.radius;
                    expected_min.x = expected_min.x.min(cx - r);
                    expected_min.y = expected_min.y.min(cy - r);
                    expected_max.x = expected_max.x.max(cx + r);
                    expected_max.y = expected_max.y.max(cy + r);
                }
            } else {
                assert!(node.meshes.is_empty(), "Node has children but also direct meshes!");
            }

            if !node.sphere.empty() {
                assert!(node.xy_min.x <= expected_min.x + 1e-4);
                assert!(node.xy_min.y <= expected_min.y + 1e-4);
                assert!(node.xy_max.x >= expected_max.x - 1e-4);
                assert!(node.xy_max.y >= expected_max.y - 1e-4);
            }
            (node.xy_min, node.xy_max)
        }

        verify_containment(&tree, tree.root);
    }

    #[test]
    fn node_prune_tighter_than_sphere() {
        let mut tree = QuadTree::default();
        tree.set_box(1000.0, D3dxVector2::default());
        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 {
                    x: 200.0,
                    y: -100.0,
                    z: 0.0,
                },
                radius: 10.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(0.0)),
            D3dxMatrix::identity(),
            false,
            false,
            1,
            3,
            101,
            1,
            201,
        );
        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 {
                    x: 200.0,
                    y: 100.0,
                    z: 0.0,
                },
                radius: 10.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(0.0)),
            D3dxMatrix::identity(),
            false,
            false,
            2,
            3,
            102,
            1,
            202,
        );
        tree.calc_volume();

        // The node sphere sees -0.25, while the tighter rectangle sees -0.20.
        let mut max_slope = vec![-0.25; 8 * 10];
        for bin in 0..8 {
            max_slope[bin * 10 + 1] = -0.25;
            for ring in 2..10 {
                max_slope[bin * 10 + ring] = -0.20;
            }
        }

        let table = HorizonTable {
            eye: D3dxVector3 { x: 0.0, y: 0.0, z: 50.0 },
            bin_count: 8,
            ring_count: 10,
            ring_step: 50.0,
            r_near: 0.0,
            bias_obj_z: 0.0,
            bias_z: 0.0,
            max_slope,
        };

        let mut output = Vec::new();
        let mut stats = HorizonCullStats::default();
        tree.get_visible_meshes_with_bands(
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 50.0,
                w: 2000.0,
            },
            Some(&table),
            None,
            &mut output,
            &mut stats,
        )
        .unwrap();

        assert_eq!(output.len(), 0);
        assert_eq!(stats.nodes_pruned, 1);

        let node = tree.node(tree.root);
        let sphere_culled = horizon_culled_capped(node.sphere, node.max_z, &table);
        assert!(!sphere_culled, "Old sphere culling would have culled, but it shouldn't!");
    }

    #[test]
    fn node_prune_non_finite_max_z_fails_open() {
        let mut tree = QuadTree::default();
        tree.set_box(400.0, D3dxVector2::default());
        insert_test_mesh(
            &mut tree,
            BoundingSphere {
                center: D3dxVector3 {
                    x: 100.0,
                    y: 0.0,
                    z: 0.0,
                },
                radius: 10.0,
            },
            BoundingBox::default(),
            Some(make_test_bounds(f32::NEG_INFINITY)),
            D3dxMatrix::identity(),
            false,
            false,
            1,
            3,
            101,
            1,
            201,
        );
        tree.calc_volume();

        let table = HorizonTable {
            eye: D3dxVector3 { x: 0.0, y: 0.0, z: 50.0 },
            bin_count: 8,
            ring_count: 10,
            ring_step: 50.0,
            r_near: 0.0,
            bias_obj_z: 0.0,
            bias_z: 0.0,
            max_slope: vec![-10.0; 8 * 10], // very low slope, would cull anything if max_z was negative infinity
        };

        let mut output = Vec::new();
        let mut stats = HorizonCullStats::default();
        tree.get_visible_meshes_with_bands(
            &test_frustum(),
            D3dxVector4 {
                x: 0.0,
                y: 0.0,
                z: 50.0,
                w: 2000.0,
            },
            Some(&table),
            None,
            &mut output,
            &mut stats,
        )
        .unwrap();

        assert_eq!(stats.nodes_pruned, 0);
    }
}
