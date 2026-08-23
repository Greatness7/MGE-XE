//! Conversion from the intermediate model into the MGE-XE `static_meshes` wire types.
//!
//! Changes here track the output format, not simplification tuning (see [`super::process`]).

use half::f16;
use tes3::nif::NiBound;

use crate::mge_xe::distant_statics::{ComponentRecord, PackedDistantStatic, PackedSubset, PackedVertex};
use crate::vfs::TextureSym;

use super::{DistantStatic, SubsetTexture, horizon::horizon_footprint_from_vertices};

impl DistantStatic {
    /// Converts the intermediate representation into the final TES3 format.
    ///
    /// For door statics, the written top-level bounding-sphere radius is inflated by
    /// `door_size_multiplier` so MGE-XE buckets the door into a farther draw distance.
    /// Subset spheres, the AABB, and rendered geometry are left at true size.
    pub fn into_distant_static(self, vfs: &crate::Vfs, door_size_multiplier: f32) -> PackedDistantStatic {
        let mut ds = PackedDistantStatic::default();
        ds.static_type = self.static_type;
        ds.bounding_sphere = pack_bounding_sphere(self.bounding_sphere);
        if self.is_door {
            ds.bounding_sphere.radius *= door_size_multiplier;
        }
        ds.bounding_box = self.bounding_box;
        ds.subsets.reserve(self.subsets.len());
        let horizon_footprint_eligible = self.horizon_footprint_eligible;

        for subset32 in self.subsets {
            let emissive = (255.0 * subset32.emissive) as u8;
            let mut subset = PackedSubset::default();
            subset.bounding_sphere = pack_bounding_sphere(subset32.bounding_sphere);
            subset.bounding_box = subset32.bounding_box;
            if horizon_footprint_eligible {
                subset.horizon_footprint = horizon_footprint_from_vertices(&subset32.vertices, subset32.bounding_box);
            }
            subset.vertices.reserve(subset32.vertices.len());

            for vertex in subset32.vertices {
                let mut cv = PackedVertex::default();

                cv.position = [
                    f16::from_f32(vertex.position.x),
                    f16::from_f32(vertex.position.y),
                    f16::from_f32(vertex.position.z),
                    f16::ONE,
                ];

                cv.uv = [
                    f16::from_f32(vertex.uv.x), //
                    f16::from_f32(vertex.uv.y),
                ];

                cv.normal = [
                    (255.0 * (vertex.normal.x * 0.5 + 0.5)) as u8,
                    (255.0 * (vertex.normal.y * 0.5 + 0.5)) as u8,
                    (255.0 * (vertex.normal.z * 0.5 + 0.5)) as u8,
                    emissive,
                ];

                cv.color = [
                    (vertex.color.x * 255.0 + 0.5) as u8,
                    (vertex.color.y * 255.0 + 0.5) as u8,
                    (vertex.color.z * 255.0 + 0.5) as u8,
                    (vertex.color.w * 255.0 + 0.5) as u8,
                ];

                cv.uv_bound = [
                    f16::from_f32(vertex.uv_bound.min_y),
                    f16::from_f32(vertex.uv_bound.max_x),
                    f16::from_f32(vertex.uv_bound.min_x),
                    f16::from_f32(vertex.uv_bound.max_y),
                ];

                subset.vertices.push(cv);
            }

            subset.triangles = subset32.triangles;
            subset.components = subset32
                .components
                .into_iter()
                .map(|component| ComponentRecord {
                    first_triangle: component.first_triangle,
                    triangle_count: component.triangle_count,
                    radius: component.radius,
                    classification: component.classification as u8,
                    reserved: [0; 3],
                })
                .collect();
            subset.has_alpha = subset32.has_alpha as u8;
            subset.has_uv_controller = subset32.has_uv_controller as u8;
            subset.texture = subset32.texture.to_packed_path(vfs, subset32.has_alpha);

            ds.subsets.push(subset);
        }

        ds
    }
}

impl SubsetTexture {
    /// Returns the source texture symbol when this points at a VFS texture.
    pub fn source_sym(self) -> Option<TextureSym> {
        match self {
            Self::Source(sym) if !sym.is_empty() => Some(sym),
            Self::Source(_) | Self::AtlasPage(_) => None,
        }
    }

    fn to_packed_path(self, vfs: &crate::Vfs, has_alpha: bool) -> Box<str> {
        match self {
            Self::Source(sym) => vfs.texture_key_for_sym(sym).unwrap_or("").into(),
            Self::AtlasPage(page_id) => {
                let prefix = if has_alpha {
                    crate::atlas::ALPHA_ATLAS_PREFIX
                } else {
                    crate::atlas::OPAQUE_ATLAS_PREFIX
                };
                crate::atlas::atlas_page_string(prefix, page_id as usize).into_boxed_str()
            }
        }
    }
}

fn pack_bounding_sphere(bound: NiBound) -> crate::mge_xe::distant_statics::BoundingSphere {
    crate::mge_xe::distant_statics::BoundingSphere {
        center: bound.center,
        radius: bound.radius,
    }
}
