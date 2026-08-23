use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use hashbrown::HashMap;
use thiserror::Error;
use tracing::{error, info, warn};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

use crate::abi::{
    BoundingBox, BoundingSphere, D3dxMatrix, D3dxVector2, D3dxVector3, DistantStatic, DistantSubset, MGE_DL_VERSION,
    OcclusionFormatError, RenderMesh, STATIC_AUTO, STATIC_BUILDING, STATIC_FAR, STATIC_GRASS, STATIC_NEAR, STATIC_TREE,
    STATIC_VERY_FAR, TERRAIN_FILE_NAME, TERRAIN_OCCLUSION_FILE_NAME, TerrainFileHeader, TerrainFileLayout, USE_DISTANT_LAND,
    parse_terrain_file_layout, parse_terrain_occlusion,
};
use crate::error::HostError;
use crate::ipc::shared_vec::SharedVec;
use crate::state::distant_land::{DistantLandState, DynamicMeshRef, StaticTreeKind, WorldSpace};
use crate::state::horizon::{HorizonMeshBounds, TerrainHeightField};
use crate::state::quadtree::QuadTreeMesh;

#[derive(Clone, Copy, Debug, Default)]
pub struct UsedDistantStatic {
    pub static_ref: u32,
    pub vis_index: u16,
    pub pos: D3dxVector3,
    pub scale: f32,
    pub transform: D3dxMatrix,
    pub sphere: BoundingSphere,
    pub box_value: BoundingBox,
}

impl UsedDistantStatic {
    pub fn bounding_sphere(&self, base: BoundingSphere) -> BoundingSphere {
        BoundingSphere {
            center: self.transform.transform_coord(base.center),
            radius: base.radius * self.scale,
        }
    }

    pub fn bounding_box(&self, min: D3dxVector3, max: D3dxVector3) -> BoundingBox {
        let mut box_value = BoundingBox::default();
        box_value.set(min, max);
        box_value.transform(self.transform);
        box_value
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct LandMesh {
    sphere: BoundingSphere,
    box_value: BoundingBox,
    verts: u32,
    faces: u32,
    vbuffer: u32,
    ibuffer: u32,
}

/// On-disk `usage.data` instance record. Field order must stay byte-for-byte
/// compatible with the generator in `distantland`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
struct UsageRecord {
    static_ref: u32,
    vis_index: u16,
    _padding: u16,
    pos: D3dxVector3,
    rot: [f32; 3],
    scale: f32,
}

const _: () = {
    assert!(core::mem::size_of::<UsageRecord>() == 36);
    assert!(core::mem::align_of::<UsageRecord>() == 4);
    assert!(core::mem::offset_of!(UsageRecord, static_ref) == 0);
    assert!(core::mem::offset_of!(UsageRecord, vis_index) == 4);
    assert!(core::mem::offset_of!(UsageRecord, pos) == 8);
    assert!(core::mem::offset_of!(UsageRecord, rot) == 20);
    assert!(core::mem::offset_of!(UsageRecord, scale) == 32);
};

/// Match the C++ host's sharing mode.
fn open_readonly_file_io(path: impl AsRef<Path>) -> io::Result<File> {
    OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path)
}

fn open_readonly_file(path: impl AsRef<Path>) -> Result<File, HostError> {
    open_readonly_file_io(path).map_err(HostError::io)
}

fn read_pod<T: bytemuck::Pod, R: Read + ?Sized>(reader: &mut R) -> Result<T, HostError> {
    let mut value: T = bytemuck::Zeroable::zeroed();
    reader.read_exact(bytemuck::bytes_of_mut(&mut value))?;
    Ok(value)
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn log_timing(label: &str, milliseconds: f64) {
    info!("Distant load timing: {} {:.2} ms", label, milliseconds);
}

/// Missing optional assets log at `info`; unusable assets log at `warn`.
#[derive(Debug, Error)]
enum OcclusionLoadError {
    #[error("missing")]
    Missing,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Format(#[from] OcclusionFormatError),
}

fn load_occlusion_height_field(terrain_header: &TerrainFileHeader) -> Result<TerrainHeightField, OcclusionLoadError> {
    let path = format!("Data Files\\distantland\\{TERRAIN_OCCLUSION_FILE_NAME}");
    let mut file = match open_readonly_file_io(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(OcclusionLoadError::Missing),
        Err(error) => return Err(OcclusionLoadError::Io(error)),
    };
    let file_size = file.metadata()?.len();
    let mut bytes = Vec::with_capacity(file_size as usize);
    file.read_to_end(&mut bytes)?;
    let data = parse_terrain_occlusion(&bytes)?;
    TerrainHeightField::from_occlusion(data, terrain_header).map_err(OcclusionLoadError::Format)
}

fn log_height_field_summary(source: &str, height_field: &TerrainHeightField) {
    info!(
        "Terrain horizon height field: source={} nx={} ny={} spacing={:.1} covered_cells={} global_max_z={:?} \
         mip_levels={} mip_kib={:.1}",
        source,
        height_field.nx,
        height_field.ny,
        height_field.spacing,
        height_field.covered_cell_count(),
        height_field.global_max_z(),
        height_field.mip_level_count(),
        height_field.mip_byte_size() as f64 / 1024.0
    );
}

#[derive(Default)]
struct StaticsQtSummary {
    memory_use: usize,
    near_count: usize,
    far_count: usize,
    very_far_count: usize,
    grass_count: usize,
    dynamic_vis_links: usize,
    generated_horizon_footprints_used: usize,
    generated_horizon_footprints_rejected_invalid: usize,
    generated_horizon_footprints_rejected_transform: usize,
}

fn get_or_insert_world_space(
    world_spaces: &mut Vec<WorldSpace>,
    world_space_indices: &mut HashMap<String, usize>,
    name: String,
) -> usize {
    if let Some(&index) = world_space_indices.get(&name) {
        return index;
    }

    let index = world_spaces.len();
    world_spaces.push(WorldSpace::default());
    world_space_indices.insert(name, index);
    index
}

fn nearly_equal(lhs: f32, rhs: f32) -> bool {
    (lhs - rhs).abs() <= 1.0e-4
}

fn is_translation_only_unit_scale(used: &UsedDistantStatic) -> bool {
    let matrix = used.transform;
    used.pos.x.is_finite()
        && used.pos.y.is_finite()
        && used.pos.z.is_finite()
        && nearly_equal(used.scale, 1.0)
        && nearly_equal(matrix._11, 1.0)
        && nearly_equal(matrix._22, 1.0)
        && nearly_equal(matrix._33, 1.0)
        && nearly_equal(matrix._44, 1.0)
        && nearly_equal(matrix._12, 0.0)
        && nearly_equal(matrix._13, 0.0)
        && nearly_equal(matrix._14, 0.0)
        && nearly_equal(matrix._21, 0.0)
        && nearly_equal(matrix._23, 0.0)
        && nearly_equal(matrix._24, 0.0)
        && nearly_equal(matrix._31, 0.0)
        && nearly_equal(matrix._32, 0.0)
        && nearly_equal(matrix._34, 0.0)
        && nearly_equal(matrix._41, used.pos.x)
        && nearly_equal(matrix._42, used.pos.y)
        && nearly_equal(matrix._43, used.pos.z)
}

impl DistantLandState {
    /// Loads generated distant statics and builds per-world-space quadtrees.
    ///
    /// # Errors
    ///
    /// Returns an error when required generated files are missing while distant land is enabled,
    /// or when the on-disk version is incompatible.
    ///
    /// # Panics
    ///
    /// Panics if the shared input vectors contain out-of-bounds indices or are initialized with the wrong element type.
    pub fn init_distant_statics(
        &mut self,
        distant_statics: &SharedVec,
        distant_subsets: &SharedVec,
        far_static_min_size: f32,
        very_far_static_min_size: f32,
    ) -> Result<(), HostError> {
        let total_start = Instant::now();
        if !Path::new("Data Files\\distantland\\statics").exists() {
            warn!("Distant statics have not been generated");
            if (self.configuration.mge_flags & USE_DISTANT_LAND) == 0 {
                return Ok(());
            }
            return Err(HostError::init("Distant statics have not been generated"));
        }

        let mut file = self.begin_read_statics()?;
        let dynamic_vis_start = Instant::now();
        self.load_vis_groups_server(&mut file)?;
        log_timing("dynamic_vis.host64", elapsed_ms(dynamic_vis_start));

        let usage_start = Instant::now();
        self.read_distant_statics(
            &mut file,
            distant_statics,
            distant_subsets,
            far_static_min_size,
            very_far_static_min_size,
        )?;
        log_timing("usage.host64_read_and_build_qt", elapsed_ms(usage_start));
        log_timing("statics.host64_total", elapsed_ms(total_start));
        Ok(())
    }

    /// Opens `usage.data` after confirming the generated assets match this host version.
    fn begin_read_statics(&self) -> Result<File, HostError> {
        let mut version_file = open_readonly_file("Data Files\\distantland\\version")?;
        let version = read_pod::<u8, _>(&mut version_file)?;
        if version != MGE_DL_VERSION {
            error!("Distant statics data is from an old version and needs to be regenerated");
            return Err(HostError::init("Distant statics version mismatch"));
        }
        open_readonly_file("Data Files\\distantland\\statics\\usage.data")
    }

    /// Reads the dynamic-visibility header and sizes `self.dynamic_vis_groups` accordingly.
    fn load_vis_groups_server<R: Read + Seek>(&mut self, reader: &mut R) -> Result<(), HostError> {
        let _: u32 = read_pod(reader)?;
        let dynamic_vis_group_count: u32 = read_pod(reader)?;
        self.dynamic_vis_groups.clear();
        if dynamic_vis_group_count > 0 {
            const VIS_GROUP_RECORD_SIZE: i32 = 130;
            reader.seek(SeekFrom::Current(
                (VIS_GROUP_RECORD_SIZE * dynamic_vis_group_count as i32) as i64,
            ))?;
            self.dynamic_vis_groups
                .resize(dynamic_vis_group_count as usize + 1, Vec::new());
        }
        info!(
            "Distant load summary: dynamic_vis.host64_group_count={}",
            dynamic_vis_group_count
        );
        Ok(())
    }

    /// Reads all used static instances from disk and expands them into world-space records.
    fn read_distant_statics<R: Read>(
        &mut self,
        reader: &mut R,
        distant_statics: &SharedVec,
        distant_subsets: &SharedVec,
        far_static_min_size: f32,
        very_far_static_min_size: f32,
    ) -> Result<(), HostError> {
        const USED_DISTANT_STATIC_RECORD_SIZE: usize = core::mem::size_of::<UsageRecord>();
        const USED_DISTANT_STATIC_CHUNK_COUNT: usize = 16_384;
        let mut chunk: Vec<UsageRecord> = bytemuck::zeroed_vec(USED_DISTANT_STATIC_CHUNK_COUNT);
        let mut worldvis_memory_use = 0_usize;
        let mut read_instances_ms = 0.0;
        let mut expand_instances_ms = 0.0;
        let mut quadtree_ms = 0.0;
        let mut worldspace_count = 0_usize;
        let mut interior_worldspace_count = 0_usize;
        let mut instance_count = 0_usize;
        let mut usage_bytes_read = 0_usize;
        let mut qt_summary = StaticsQtSummary::default();

        self.current_world_space = None;
        let dynamic_vis_groups = &mut self.dynamic_vis_groups;
        let world_spaces = &mut self.world_spaces;
        let world_space_indices = &mut self.world_space_indices;
        world_spaces.clear();
        world_space_indices.clear();
        let mut world_index = 0_u32;
        loop {
            let count_start = Instant::now();
            let mut used_count: u32 = read_pod(reader)?;
            read_instances_ms += elapsed_ms(count_start);
            usage_bytes_read += std::mem::size_of::<u32>();
            if world_index != 0 && used_count == 0 {
                break;
            }

            let world_name = if world_index == 0 {
                let world_name = String::new();
                let _ = get_or_insert_world_space(world_spaces, world_space_indices, world_name.clone());
                worldspace_count += 1;
                if used_count == 0 {
                    world_index += 1;
                    continue;
                }
                world_name
            } else {
                let mut cellname = [0_u8; 64];
                let name_start = Instant::now();
                reader.read_exact(&mut cellname)?;
                read_instances_ms += elapsed_ms(name_start);
                usage_bytes_read += cellname.len();
                worldspace_count += 1;
                interior_worldspace_count += 1;
                crate::abi::c_string_from_fixed(&cellname).into_owned()
            };

            let mut world_statics = Vec::with_capacity(used_count as usize);
            while used_count > 0 {
                let to_read = USED_DISTANT_STATIC_CHUNK_COUNT.min(used_count as usize);
                used_count -= to_read as u32;
                let bytes_to_read = to_read * USED_DISTANT_STATIC_RECORD_SIZE;
                let read_start = Instant::now();
                reader.read_exact(bytemuck::cast_slice_mut(&mut chunk[..to_read]))?;
                read_instances_ms += elapsed_ms(read_start);
                usage_bytes_read += bytes_to_read;
                instance_count += to_read;
                let expand_start = Instant::now();
                for record in &chunk[..to_read] {
                    let static_ref = record.static_ref;
                    let vis_index = record.vis_index;
                    let pos = record.pos;
                    let [yaw, pitch, roll] = record.rot;
                    let scale = record.scale;
                    let stat = distant_statics.get::<DistantStatic>(static_ref);
                    // The serialized rotation order mirrors the legacy generator: scale, then roll,
                    // pitch, yaw, then translation, all in row-vector D3DX form.
                    let transform = D3dxMatrix::scaling(scale, scale, scale)
                        .multiply(D3dxMatrix::rotation_z(-roll))
                        .multiply(D3dxMatrix::rotation_y(-pitch))
                        .multiply(D3dxMatrix::rotation_x(-yaw))
                        .multiply(D3dxMatrix::translation(pos.x, pos.y, pos.z));
                    let mut used = UsedDistantStatic {
                        static_ref,
                        vis_index,
                        pos,
                        scale,
                        transform,
                        sphere: BoundingSphere::default(),
                        box_value: BoundingBox::default(),
                    };
                    used.sphere = used.bounding_sphere(stat.sphere);
                    used.box_value = used.bounding_box(stat.aabb_min, stat.aabb_max);
                    world_statics.push(used);
                }
                expand_instances_ms += elapsed_ms(expand_start);
            }

            let world_space_index = get_or_insert_world_space(world_spaces, world_space_indices, world_name);
            let world_space = &mut world_spaces[world_space_index];
            let quadtree_start = Instant::now();
            let world_summary = init_distant_statics_qt(
                dynamic_vis_groups,
                world_space_index,
                world_space,
                distant_statics,
                distant_subsets,
                &world_statics,
                far_static_min_size,
                very_far_static_min_size,
            );
            quadtree_ms += elapsed_ms(quadtree_start);
            worldvis_memory_use += world_summary.memory_use;
            qt_summary.near_count += world_summary.near_count;
            qt_summary.far_count += world_summary.far_count;
            qt_summary.very_far_count += world_summary.very_far_count;
            qt_summary.grass_count += world_summary.grass_count;
            qt_summary.dynamic_vis_links += world_summary.dynamic_vis_links;
            qt_summary.generated_horizon_footprints_used += world_summary.generated_horizon_footprints_used;
            qt_summary.generated_horizon_footprints_rejected_invalid +=
                world_summary.generated_horizon_footprints_rejected_invalid;
            qt_summary.generated_horizon_footprints_rejected_transform +=
                world_summary.generated_horizon_footprints_rejected_transform;
            world_index += 1;
        }

        log_timing("usage.read_instances_total", read_instances_ms);
        log_timing("usage.expand_instances_total", expand_instances_ms);
        log_timing("quadtree.statics_total", quadtree_ms);
        info!(
            "Distant load summary: usage.worldspace_count={} usage.instance_count={} usage.interior_worldspace_count={} usage.bytes_read={}",
            worldspace_count, instance_count, interior_worldspace_count, usage_bytes_read
        );
        info!(
            "Distant load summary: quadtree.near_count={} quadtree.far_count={} quadtree.very_far_count={} quadtree.grass_count={} quadtree.dynamic_vis_links={} horizon.generated_used={} horizon.generated_rejected_invalid={} horizon.generated_rejected_transform={}",
            qt_summary.near_count,
            qt_summary.far_count,
            qt_summary.very_far_count,
            qt_summary.grass_count,
            qt_summary.dynamic_vis_links,
            qt_summary.generated_horizon_footprints_used,
            qt_summary.generated_horizon_footprints_rejected_invalid,
            qt_summary.generated_horizon_footprints_rejected_transform
        );
        info!("Distant worldspaces memory use: {} MB", worldvis_memory_use / (1 << 20));
        Ok(())
    }

    /// Loads generated terrain metadata and builds the terrain quadtree.
    ///
    /// # Errors
    ///
    /// Returns an error when generated files cannot be read or when the client supplies fewer
    /// terrain buffers than `terrain.bin` expects.
    pub fn init_landscape(
        &mut self,
        landscape_buffers: &mut SharedVec,
        terrain_sort_token: u32,
        terrain_path: &Path,
    ) -> Result<(), HostError> {
        let total_start = Instant::now();
        let mut file = open_readonly_file(terrain_path)?;
        let file_size = file.metadata()?.len();
        let mut terrain_bytes = Vec::with_capacity(file_size as usize);
        file.read_to_end(&mut terrain_bytes)?;
        let mut read_headers_ms = 0.0;
        let mut skip_vertex_index_ms = 0.0;
        let mut total_vertices = 0_usize;
        let mut total_triangles = 0_usize;
        let mut skipped_bytes = 0_usize;
        let parse_start = Instant::now();
        let terrain = parse_terrain_file_layout(&terrain_bytes)
            .map_err(|err| HostError::init(format!("Failed to parse {TERRAIN_FILE_NAME}: {err}")))?;
        read_headers_ms += elapsed_ms(parse_start);
        let mut meshes = vec![LandMesh::default(); terrain.meshes.len()];
        landscape_buffers.start_read()?;

        let mut qtmin = D3dxVector2 {
            x: f32::MAX,
            y: f32::MAX,
        };
        let mut qtmax = D3dxVector2 {
            x: -f32::MAX,
            y: -f32::MAX,
        };
        for (index, (mesh, layout)) in meshes.iter_mut().zip(terrain.meshes.iter()).enumerate() {
            let mesh_header = layout.header;
            mesh.sphere = mesh_header.bounding_sphere();
            mesh.box_value.set(mesh_header.bounding_box_min, mesh_header.bounding_box_max);
            mesh.verts = mesh_header.vertex_count;
            mesh.faces = mesh_header.triangle_count;
            let buffer_size = layout.vertex_data_size + layout.index_data_size;
            total_vertices += mesh.verts as usize;
            total_triangles += mesh.faces as usize;
            let skip_start = Instant::now();
            skip_vertex_index_ms += elapsed_ms(skip_start);
            skipped_bytes += buffer_size;

            if index as u32 >= landscape_buffers.size() {
                landscape_buffers.wait_read(crate::abi::MAX_WAIT)?;
            }
            if index as u32 >= landscape_buffers.size() {
                error!(
                    "Client landscape buffers ended while the server still has more meshes ({} buffers found, expected {})",
                    landscape_buffers.size(),
                    terrain.header.mesh_count
                );
                landscape_buffers.end_read();
                return Err(HostError::init(
                    "Client landscape buffers ended while the server still has more meshes",
                ));
            }

            // Buffer handles arrive asynchronously from the 32-bit client, so the host waits
            // just long enough for the matching record before treating it as a hard mismatch.
            let buffers = landscape_buffers.get::<crate::abi::LandscapeBuffers>(index as u32);
            mesh.vbuffer = buffers.vb;
            mesh.ibuffer = buffers.ib;

            qtmin.x = qtmin.x.min(mesh_header.bounding_box_min.x);
            qtmin.y = qtmin.y.min(mesh_header.bounding_box_min.y);
            qtmax.x = qtmax.x.max(mesh_header.bounding_box_max.x);
            qtmax.y = qtmax.y.max(mesh_header.bounding_box_max.y);
        }
        landscape_buffers.end_read();

        let quadtree_start = Instant::now();
        if !meshes.is_empty() {
            self.land_quadtree
                .set_box((qtmax.x - qtmin.x).max(qtmax.y - qtmin.y), (qtmax + qtmin) * 0.5);
            let world = D3dxMatrix::identity();
            for mesh in meshes {
                self.land_quadtree.insert_mesh(QuadTreeMesh::new(
                    RenderMesh {
                        enabled: 1,
                        has_alpha: 0,
                        animate_uv: 0,
                        _padding0: 0,
                        tex: terrain_sort_token,
                        transform: world,
                        verts: mesh.verts as i32,
                        v_buffer: mesh.vbuffer,
                        faces: mesh.faces as i32,
                        i_buffer: mesh.ibuffer,
                    },
                    mesh.sphere,
                    mesh.box_value,
                    None,
                ));
            }
        }
        self.land_quadtree.calc_volume();
        let quadtree_ms = elapsed_ms(quadtree_start);
        if self.configuration.horizon_culling {
            // Reuse the terrain we already read and parsed above; no second disk read at load.
            // build_height_field_from invalidates the horizon epoch as it replaces the field.
            self.build_height_field_from(&terrain)?;
        } else {
            self.replace_height_field(None);
        }
        log_timing("terrain.host64_read_headers", read_headers_ms);
        log_timing("terrain.host64_skip_vertex_index_data", skip_vertex_index_ms);
        log_timing("quadtree.landscape_host64", quadtree_ms);
        info!(
            "Terrain load summary: mesh_count={} total_vertices={} total_triangles={} skipped_bytes={}",
            terrain.header.mesh_count, total_vertices, total_triangles, skipped_bytes
        );
        info!(
            "Terrain memory use: file_bytes={} skipped_bytes={} approx_total_mb={:.2}",
            file_size,
            skipped_bytes,
            (file_size as f64 + skipped_bytes as f64) / (1 << 20) as f64
        );
        // Terrain visibility queries still require a selected world space. Establish the
        // exterior here so the client may deliberately omit InitDistantStatics while keeping
        // landscape rendering available; a later statics init rebuilds the world-space map.
        let _ = get_or_insert_world_space(&mut self.world_spaces, &mut self.world_space_indices, String::new());
        log_timing("landscape.host64_total", elapsed_ms(total_start));
        Ok(())
    }

    /// (Re)builds the terrain height field used for horizon culling by reading `terrain.bin`.
    ///
    /// Reads and parses `Data Files\distantland\terrain.bin` for the paired terrain header, then
    /// loads the occluder grid from `terrain_occlusion.bin`. This is the on-demand path used when
    /// horizon culling is enabled without a resident field, where the terrain bytes are not
    /// otherwise resident. `init_landscape` does **not** call this. It already has the parsed
    /// terrain in memory and calls [`build_height_field_from`](Self::build_height_field_from)
    /// directly to avoid a second disk read.
    ///
    /// # Errors
    ///
    /// Returns an error when `terrain.bin` cannot be read or parsed. An absent or unusable occlusion
    /// asset does not fail the rebuild; the field is left cleared so horizon culling self-no-ops.
    pub fn build_height_field(&mut self) -> Result<(), HostError> {
        // Invalidate + clear before the disk read so a read/parse failure leaves the field cleared
        // and the epoch bumped (culling self-no-ops), matching build_height_field_from's contract.
        self.replace_height_field(None);
        let mut file = open_readonly_file(format!("Data Files\\distantland\\{TERRAIN_FILE_NAME}"))?;
        let file_size = file.metadata()?.len();
        let mut terrain_bytes = Vec::with_capacity(file_size as usize);
        file.read_to_end(&mut terrain_bytes)?;
        let terrain = parse_terrain_file_layout(&terrain_bytes)
            .map_err(|err| HostError::init(format!("Failed to parse {TERRAIN_FILE_NAME}: {err}")))?;
        self.build_height_field_from(&terrain)
    }

    /// Loads the terrain height field from `terrain_occlusion.bin`.
    ///
    /// The generated asset is the only production source of the occluder height field; deriving it
    /// from `terrain.bin` survives only as a test oracle. When the asset is absent or unusable, the
    /// field stays cleared so horizon culling self-no-ops and distant-land loading continues;
    /// regenerating distant land restores the asset.
    ///
    /// # Errors
    ///
    /// Always returns `Ok`. Asset failures degrade to an inactive height field instead of failing
    /// the distant-land load.
    pub(super) fn build_height_field_from(&mut self, terrain: &TerrainFileLayout) -> Result<(), HostError> {
        // Bump the epoch and clear the field before building, so a result built from the old field is
        // stale even if this rebuild fails and leaves the field cleared.
        self.replace_height_field(None);
        let occlusion_start = Instant::now();
        match load_occlusion_height_field(&terrain.header) {
            Ok(height_field) => {
                log_timing("terrain.host64_occlusion_load", elapsed_ms(occlusion_start));
                log_height_field_summary("asset", &height_field);
                self.install_rebuilt_height_field(Arc::new(height_field));
            }
            Err(OcclusionLoadError::Missing) => {
                warn!(
                    "terrain_occlusion.bin is absent; terrain horizon culling is inactive. Regenerate distant land to restore it"
                );
            }
            Err(error) => {
                warn!(
                    "terrain_occlusion.bin rejected ({error}); terrain horizon culling is inactive. Regenerate distant land to restore it"
                );
            }
        }
        Ok(())
    }
}

/// Classifies expanded statics into quadtrees and records dynamic-visibility links.
fn init_distant_statics_qt(
    dynamic_vis_groups: &mut [Vec<DynamicMeshRef>],
    world_space_index: usize,
    world_space: &mut WorldSpace,
    distant_statics: &SharedVec,
    distant_subsets: &SharedVec,
    used_statics: &[UsedDistantStatic],
    far_static_min_size: f32,
    very_far_static_min_size: f32,
) -> StaticsQtSummary {
    let mut aabb_max = D3dxVector2 {
        x: -f32::MAX,
        y: -f32::MAX,
    };
    let mut aabb_min = D3dxVector2 {
        x: f32::MAX,
        y: f32::MAX,
    };
    for used in used_statics {
        let x = used.pos.x;
        let y = used.pos.y;
        let radius = used.sphere.radius;
        aabb_max.x = aabb_max.x.max(x + radius);
        aabb_max.y = aabb_max.y.max(y + radius);
        aabb_min.x = aabb_min.x.min(x - radius);
        aabb_min.y = aabb_min.y.min(y - radius);
    }

    let box_size = (aabb_max.x - aabb_min.x).max(aabb_max.y - aabb_min.y);
    let box_center = (aabb_max + aabb_min) * 0.5;
    world_space.near_statics.set_box(box_size, box_center);
    world_space.far_statics.set_box(box_size, box_center);
    world_space.very_far_statics.set_box(box_size, box_center);
    world_space.grass_statics.set_box(box_size, box_center);

    let mut summary = StaticsQtSummary::default();
    for used in used_statics {
        let stat = distant_statics.get::<DistantStatic>(used.static_ref);
        let mut radius = used.sphere.radius;
        let (target_kind, target) = match stat.kind {
            STATIC_AUTO | STATIC_TREE | STATIC_BUILDING => {
                if stat.kind == STATIC_BUILDING {
                    // Buildings use the whole-instance bounds for all subsets to match the legacy
                    // behavior that keeps large architecture from popping between range buckets.
                    radius *= 2.0;
                }
                if radius <= far_static_min_size {
                    (StaticTreeKind::Near, &mut world_space.near_statics)
                } else if radius <= very_far_static_min_size {
                    (StaticTreeKind::Far, &mut world_space.far_statics)
                } else {
                    (StaticTreeKind::VeryFar, &mut world_space.very_far_statics)
                }
            }
            STATIC_GRASS => (StaticTreeKind::Grass, &mut world_space.grass_statics),
            STATIC_NEAR => (StaticTreeKind::Near, &mut world_space.near_statics),
            STATIC_FAR => (StaticTreeKind::Far, &mut world_space.far_statics),
            STATIC_VERY_FAR => (StaticTreeKind::VeryFar, &mut world_space.very_far_statics),
            _ => continue,
        };

        let end_index = stat.first_subset_index + stat.num_subsets;
        for subset_index in stat.first_subset_index..end_index {
            let subset = distant_subsets.get::<DistantSubset>(subset_index);
            let (bound_sphere, bound_box) = if stat.kind == STATIC_BUILDING {
                (used.sphere, used.box_value)
            } else {
                (
                    used.bounding_sphere(subset.sphere),
                    used.bounding_box(subset.aabb_min, subset.aabb_max),
                )
            };
            let horizon_bounds = if stat.kind == STATIC_BUILDING || subset.horizon_footprint.vertex_count == 0 {
                None
            } else if !is_translation_only_unit_scale(used) {
                summary.generated_horizon_footprints_rejected_transform += 1;
                None
            } else {
                match HorizonMeshBounds::from_generated_footprint(&subset.horizon_footprint, used.pos) {
                    Some(bounds) => {
                        summary.generated_horizon_footprints_used += 1;
                        Some(bounds)
                    }
                    None => {
                        summary.generated_horizon_footprints_rejected_invalid += 1;
                        None
                    }
                }
            };
            let mesh = target.insert_mesh(QuadTreeMesh::with_lod(
                RenderMesh {
                    enabled: 1,
                    has_alpha: (subset.has_alpha != 0) as u8,
                    animate_uv: (subset.has_uv_controller != 0) as u8,
                    _padding0: 0,
                    tex: subset.tex,
                    transform: used.transform,
                    verts: subset.verts,
                    v_buffer: subset.vbuffer,
                    faces: subset.faces,
                    i_buffer: subset.ibuffer,
                },
                bound_sphere,
                bound_box,
                horizon_bounds,
                subset.far_faces,
                subset.very_far_faces,
            ));
            match target_kind {
                StaticTreeKind::Near => summary.near_count += 1,
                StaticTreeKind::Far => summary.far_count += 1,
                StaticTreeKind::VeryFar => summary.very_far_count += 1,
                StaticTreeKind::Grass => summary.grass_count += 1,
            }
            if used.vis_index > 0
                && let Some(group) = dynamic_vis_groups.get_mut(used.vis_index as usize)
            {
                group.push(DynamicMeshRef {
                    world: world_space_index,
                    tree: target_kind,
                    mesh,
                });
                summary.dynamic_vis_links += 1;
            }
        }
        summary.memory_use += stat.num_subsets as usize * std::mem::size_of::<QuadTreeMesh>();
    }

    world_space.near_statics.optimize();
    world_space.near_statics.calc_volume();
    world_space.far_statics.optimize();
    world_space.far_statics.calc_volume();
    world_space.very_far_statics.optimize();
    world_space.very_far_statics.calc_volume();
    world_space.grass_statics.optimize();
    world_space.grass_statics.calc_volume();
    summary
}
