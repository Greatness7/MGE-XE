//! Conversion from the intermediate model into the MGE-XE `static_meshes` wire types.
//!
//! Changes here track the output format, not simplification tuning (see [`super::process`]).

use half::f16;
use tes3::nif::NiBound;

use crate::mge_xe::distant_statics::{
    ComponentRecord, PackedDistantStatic, PackedSubset, PackedVertex, StaticType, UvBoundRecord, float_to_u8,
    pack_d3dcolor_vclr,
};
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
        // Grass never indexes a palette: `XE Mod Grass.fx` transforms the full `pos` through
        // instancing, and grass never reaches the atlas path. Serializing an identity record for
        // it would be dead bytes, and `position.w` must stay 1.0.
        let is_grass = self.static_type == StaticType::StaticGrass;

        for subset32 in self.subsets {
            let emissive = (255.0 * subset32.emissive) as u8;
            let mut subset = PackedSubset::default();
            subset.bounding_sphere = pack_bounding_sphere(subset32.bounding_sphere);
            subset.bounding_box = subset32.bounding_box;
            if horizon_footprint_eligible {
                subset.horizon_footprint = horizon_footprint_from_vertices(&subset32.vertices, subset32.bounding_box);
            }
            subset.vertices.reserve(subset32.vertices.len());

            // The authoritative palette is rebuilt from the vertices themselves in
            // first-appearance order, so a stale `Subset::uv_bounds` cannot ship a wrong palette.
            // It is deterministic because vertex order is already deterministic after
            // `optimize_vertex_fetch_in_place`. The linear find is over a Vec bounded by
            // `UV_BOUND_PALETTE_CAP` — mean ~6 entries — so no hash map is warranted; the writer
            // hard-fails if the rebuilt palette exceeds the cap.
            let mut palette_keys: Vec<[u32; 4]> = Vec::new();

            for vertex in subset32.vertices {
                let mut cv = PackedVertex::default();

                let ordinal = if is_grass {
                    f16::ONE
                } else {
                    let key = vertex.uv_bound.bits();
                    let index = palette_keys.iter().position(|existing| *existing == key).unwrap_or_else(|| {
                        palette_keys.push(key);
                        subset.palette.push(UvBoundRecord {
                            bound: [
                                vertex.uv_bound.min_y,
                                vertex.uv_bound.max_x,
                                vertex.uv_bound.min_x,
                                vertex.uv_bound.max_y,
                            ],
                        });
                        palette_keys.len() - 1
                    });
                    f16::from_f32(index as f32)
                };

                cv.position = [
                    f16::from_f32(vertex.position.x),
                    f16::from_f32(vertex.position.y),
                    f16::from_f32(vertex.position.z),
                    ordinal,
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

                cv.color = pack_d3dcolor_vclr(
                    float_to_u8(vertex.color.x),
                    float_to_u8(vertex.color.y),
                    float_to_u8(vertex.color.z),
                    float_to_u8(vertex.color.w),
                );

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
