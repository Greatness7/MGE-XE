//! NIF input adapter that builds intermediate distant statics from mesh files.

use std::time::Instant;

use glam::{Vec3, Vec4};
use hashbrown::HashMap;
use minsphere::BoundingSphereScratch;
use rayon::prelude::*;
use str_utils::*;
use tes3::nif::*;

use crate::mge_xe::distant_statics::StaticType;
use crate::model::{DistantStatic, Subset, UvBound, Vertex, passes_min_radius};
use crate::nif::*;
use crate::overrides::StaticOverrides;
use crate::{DistantStatics, UsageInfo, Vfs};
use distantland_foundation::identity::{ContentIdentity, MeshResolutionFact};
use tracing::*;

/// Identity facts captured while consuming one requested NIF.
pub struct MeshConsumption {
    /// Normalized mesh key requested by usage scanning.
    pub requested_key: String,
    /// Content identity when the resolved source bytes were read.
    pub identity: Option<ContentIdentity>,
    /// VFS resolution decision when the requested mesh resolved.
    pub resolution: Option<MeshResolutionFact>,
}

/// Builds distant statics and retains source identity facts for each requested mesh.
pub fn create_distant_statics_with_identities(
    usage: &UsageInfo<'_>,
    vfs: &Vfs,
    min_radius: f32,
    door_size_multiplier: f32,
    overrides: &StaticOverrides,
) -> (DistantStatics, Vec<MeshConsumption>) {
    let (mut distant_statics, identities): (DistantStatics, Vec<MeshConsumption>) = {
        let span = tracing::info_span!(
            "statics.process_nifs",
            report = true,
            requested_mesh_count = usage.mesh_scale_maximums.len() as u64,
            generated_static_count = tracing::field::Empty,
            identity_read_hash_worker_us = tracing::field::Empty,
            nif_parse_extract_worker_us = tracing::field::Empty
        );
        let _guard = span.enter();
        let extracted = usage
            .mesh_scale_maximums
            .par_iter()
            .map(|(rel_path, max_scale)| {
                let extraction = DistantStatic::from_nif_with_identity(
                    rel_path,
                    vfs,
                    *max_scale,
                    min_radius,
                    usage.door_meshes.contains(rel_path),
                    door_size_multiplier,
                    usage.forced_meshes.contains(rel_path),
                    overrides,
                );
                (rel_path.to_string(), extraction)
            })
            .collect::<Vec<_>>();

        let mut distant_statics = DistantStatics::default();
        let mut identities = Vec::with_capacity(extracted.len());
        let mut identity_read_hash_worker_us = 0_u64;
        let mut nif_parse_extract_worker_us = 0_u64;
        for (rel_path, extraction) in extracted {
            identity_read_hash_worker_us = identity_read_hash_worker_us.saturating_add(extraction.identity_read_hash_us);
            nif_parse_extract_worker_us = nif_parse_extract_worker_us.saturating_add(extraction.parse_extract_us);
            if let Some(distant_static) = extraction.distant_static {
                distant_statics.insert(rel_path.clone(), distant_static);
            }
            identities.push(MeshConsumption {
                requested_key: rel_path,
                identity: extraction.identity,
                resolution: extraction.resolution,
            });
        }

        span.record("generated_static_count", distant_statics.len() as u64);
        span.record("identity_read_hash_worker_us", identity_read_hash_worker_us);
        span.record("nif_parse_extract_worker_us", nif_parse_extract_worker_us);
        (distant_statics, identities)
    };

    distant_statics.sort_unstable_keys();
    (distant_statics, identities)
}

/// Result of one NIF extraction with consumption-boundary identity facts.
pub struct NifExtraction {
    /// Generated distant static, absent when parsing, filtering, or extraction rejects the mesh.
    pub distant_static: Option<DistantStatic>,
    /// Content identity when the resolved source bytes were read successfully.
    pub identity: Option<ContentIdentity>,
    /// VFS resolution decision, retained even when reading or extraction later fails.
    pub resolution: Option<MeshResolutionFact>,
    /// Sum-of-worker time spent resolving, reading, and hashing the mesh source.
    pub identity_read_hash_us: u64,
    /// Sum-of-worker time spent parsing and extracting the NIF after its bytes are available.
    pub parse_extract_us: u64,
}

pub fn inferred_static_type(rel_path: &str) -> StaticType {
    // Note: Separators are always normalized by the vfs.
    if rel_path.starts_with_ignore_ascii_case("grass\\") {
        StaticType::StaticGrass
    } else if rel_path.starts_with_ignore_ascii_case("trees\\") {
        StaticType::StaticTree
    } else if rel_path.starts_with_ignore_ascii_case("x\\") {
        StaticType::StaticBuilding
    } else {
        StaticType::StaticAuto
    }
}

pub fn resolve_static_type(rel_path: &str, force_generate: bool, overrides: &StaticOverrides) -> Option<StaticType> {
    let override_entry = overrides.mesh_overrides.get(rel_path);
    if let Some(ovr) = override_entry
        && ovr.ignore
        && !force_generate
    {
        return None;
    }

    Some(match override_entry {
        Some(ovr) if !matches!(ovr.static_type, StaticType::StaticAuto) => ovr.static_type,
        _ => inferred_static_type(rel_path),
    })
}

fn average_emissive(material: &NiMaterialProperty) -> f32 {
    material.emissive_color.element_sum() / 3.0
}

#[derive(Default)]
struct TriangleSanitization {
    triangles: Vec<[u16; 3]>,
    dropped_out_of_bounds: usize,
    dropped_non_finite_position: usize,
    dropped_degenerate: usize,
}

impl TriangleSanitization {
    fn dropped_total(&self) -> usize {
        self.dropped_out_of_bounds + self.dropped_non_finite_position + self.dropped_degenerate
    }
}

fn sanitize_triangles(triangles: &[[u16; 3]], vertices: &[Vertex]) -> TriangleSanitization {
    let mut result = TriangleSanitization {
        triangles: Vec::with_capacity(triangles.len()),
        ..TriangleSanitization::default()
    };

    for &triangle in triangles {
        let [i0, i1, i2] = triangle.map(usize::from);
        let Some(v0) = vertices.get(i0) else {
            result.dropped_out_of_bounds += 1;
            continue;
        };
        let Some(v1) = vertices.get(i1) else {
            result.dropped_out_of_bounds += 1;
            continue;
        };
        let Some(v2) = vertices.get(i2) else {
            result.dropped_out_of_bounds += 1;
            continue;
        };

        if !(v0.position.is_finite() && v1.position.is_finite() && v2.position.is_finite()) {
            result.dropped_non_finite_position += 1;
            continue;
        }

        let face = (v1.position - v0.position).cross(v2.position - v0.position);
        if face.length_squared() < 1e-12 {
            result.dropped_degenerate += 1;
            continue;
        }

        result.triangles.push(triangle);
    }

    result
}

impl DistantStatic {
    /// Builds a distant static while retaining mesh resolution and content identity.
    pub fn from_nif_with_identity(
        rel_path: &str,
        vfs: &Vfs,
        max_scale: f32,
        min_radius: f32,
        is_door: bool,
        door_size_multiplier: f32,
        force_generate: bool,
        overrides: &StaticOverrides,
    ) -> NifExtraction {
        let identity_started = Instant::now();
        let Some(asset) = vfs.resolve_mesh(rel_path) else {
            trace!("NIF path not found in VFS: {:?}", rel_path);
            return NifExtraction {
                distant_static: None,
                identity: None,
                resolution: None,
                identity_read_hash_us: u64::try_from(identity_started.elapsed().as_micros()).unwrap_or(u64::MAX),
                parse_extract_us: 0,
            };
        };
        let resolution = MeshResolutionFact {
            rule: asset
                .mesh_resolution_rule
                .expect("resolve_mesh always supplies a mesh resolution rule"),
            resolved_key: asset.key.to_owned(),
        };

        let Ok(bytes) = vfs.read_asset_bytes(&asset) else {
            trace!("Failed to load NIF bytes: {:?}", rel_path);
            return NifExtraction {
                distant_static: None,
                identity: None,
                resolution: Some(resolution),
                identity_read_hash_us: u64::try_from(identity_started.elapsed().as_micros()).unwrap_or(u64::MAX),
                parse_extract_us: 0,
            };
        };
        let identity = ContentIdentity::from_bytes(&bytes);
        let identity_read_hash_us = u64::try_from(identity_started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let extraction_started = Instant::now();
        let distant_static = Self::from_nif_bytes(
            rel_path,
            &bytes,
            vfs,
            max_scale,
            min_radius,
            is_door,
            door_size_multiplier,
            force_generate,
            overrides,
        );

        NifExtraction {
            distant_static,
            identity: Some(identity),
            resolution: Some(resolution),
            identity_read_hash_us,
            parse_extract_us: u64::try_from(extraction_started.elapsed().as_micros()).unwrap_or(u64::MAX),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn from_nif_bytes(
        rel_path: &str,
        bytes: &[u8],
        vfs: &Vfs,
        max_scale: f32,
        min_radius: f32,
        is_door: bool,
        door_size_multiplier: f32,
        force_generate: bool,
        overrides: &StaticOverrides,
    ) -> Option<DistantStatic> {
        let static_type = resolve_static_type(rel_path, force_generate, overrides)?;

        let Ok(mut stream) = NiStream::from_bytes(bytes) else {
            trace!("Failed to parse NIF: {:?}", rel_path);
            return None;
        };

        if stream.roots.is_empty() {
            trace!("NIF has no root nodes: {:?}", rel_path);
            return None;
        }

        stream.apply_skin_deforms();

        clear_root_node_transforms(&mut stream);
        normalize_texture_paths(&mut stream);

        let shapes: Vec<_> = visible_geometries(&stream).collect();
        if shapes.is_empty() {
            return None;
        }

        // Compute bounds only for visible geometries and cache each shared shape-data bound.
        let bounding_sphere = ({
            let mut sphere_scratch = BoundingSphereScratch::new();
            let mut object_space: HashMap<*const NiTriShapeData, NiBound> = HashMap::with_capacity(shapes.len());
            shapes
                .iter()
                .map(|geometry| {
                    let bound = *object_space
                        .entry(geometry.data_id())
                        .or_insert_with(|| geometry.object_space_bounding_sphere(&mut sphere_scratch));
                    geometry.place_bound(bound)
                })
                .reduce(|acc, bound| acc.merged_with(bound))
        })?;

        // Radius is multiplied by reference scale, so filter against the maximum observed scale.
        if !passes_min_radius(
            bounding_sphere.radius,
            static_type,
            is_door,
            max_scale,
            min_radius,
            door_size_multiplier,
        ) {
            return None;
        }

        // TODO: Skip long or thin meshes based on bounding box?
        // TODO: Skip meshes with too low alpha threshold?

        let mut subsets = Vec::with_capacity(shapes.len());

        for geometry in shapes {
            let data = geometry.data;
            let transform = geometry.transform;
            let mut subset = Subset::default();

            // TODO: Verify how the engine handles slash prefixes - is `trim_matches` more appropriate?
            let texture_path = geometry
                .base_texture_path(&stream)
                .unwrap_or_default()
                .trim_prefix("\\")
                .trim_prefix("textures")
                .trim_prefix("\\");

            if texture_path.is_empty() {
                continue;
            }

            let Some(uv_set) = data.uv_set(0) else {
                let message = format!(
                    "{rel_path}: skipped malformed static subset with {} vertices and {} UV values",
                    data.vertices.len(),
                    data.uv_sets.len()
                );
                debug!(
                    mesh_path = rel_path,
                    vertex_count = data.vertices.len(),
                    uv_value_count = data.uv_sets.len(),
                    message = %message,
                    "Skipped malformed NIF subset with incomplete UV set"
                );
                continue;
            };

            let has_normals = data.normals.len() == data.vertices.len();
            let has_colors = data.vertex_colors.len() == data.vertices.len();
            let material = geometry.material_property(&stream);
            let material_color = material
                .map(|material| material.diffuse_color.extend(material.alpha))
                .unwrap_or(Vec4::ONE);

            // `uv_set` yields exactly `data.vertices.len()` values, so every vertex is
            // written here; collecting sizes the buffer once instead of zero-filling it.
            subset.vertices = data
                .vertices
                .iter()
                .zip(uv_set)
                .enumerate()
                .map(|(i, (position, uv))| Vertex {
                    position: transform.transform_point3(*position),
                    normal: if has_normals {
                        transform.transform_vector3(data.normals[i])
                    } else {
                        Vec3::Z
                    },
                    uv: *uv,
                    color: if has_colors { data.vertex_colors[i] } else { material_color },
                    uv_bound: UvBound {
                        min_y: 0.0,
                        max_x: 1.0,
                        min_x: 0.0,
                        max_y: 1.0,
                    },
                })
                .collect();

            let sanitized = sanitize_triangles(&data.triangles, &subset.vertices);
            if sanitized.dropped_total() > 0 {
                let message = format!(
                    "{rel_path}: dropped {} malformed triangle(s) during static extraction \
                     (out_of_bounds={}, non_finite_position={}, degenerate={}, retained={})",
                    sanitized.dropped_total(),
                    sanitized.dropped_out_of_bounds,
                    sanitized.dropped_non_finite_position,
                    sanitized.dropped_degenerate,
                    sanitized.triangles.len()
                );
                debug!(
                    mesh_path = rel_path,
                    retained_triangle_count = sanitized.triangles.len(),
                    dropped_out_of_bounds = sanitized.dropped_out_of_bounds,
                    dropped_non_finite_position = sanitized.dropped_non_finite_position,
                    dropped_degenerate = sanitized.dropped_degenerate,
                    message = %message,
                    "Dropped malformed NIF triangles during static extraction"
                );
            }
            if sanitized.triangles.is_empty() {
                continue;
            }
            subset.triangles = sanitized.triangles;

            // Static texture resolution is total: unresolved or unsupported texture
            // references are remapped to the embedded visible error texture.
            subset.texture = crate::SubsetTexture::Source(vfs.resolve_static_texture_sym_or_error(texture_path));

            subset.has_alpha = geometry.has_alpha(&stream);
            subset.has_uv_controller = geometry.has_uv_controller(&stream);
            subset.emissive = material.map(average_emissive).unwrap_or(0.0);

            subsets.push(subset);
        }

        if subsets.is_empty() {
            trace!("NIF has no valid subsets above min radius: {:?}", rel_path);
            return None;
        }

        let mut this = DistantStatic::default();
        this.static_type = static_type;
        this.max_scale = max_scale;
        this.is_door = is_door;
        this.subsets = subsets;
        this.update_bounds();

        Some(this)
    }
}

#[cfg(test)]
mod tests;
