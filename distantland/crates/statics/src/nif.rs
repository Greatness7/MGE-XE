//! NIF geometry traversal and transform helpers.

use bytemuck::must_cast_slice;
use glam::Affine3A;
use hashbrown::HashSet;
use minsphere::{BoundingSphere, BoundingSphereScratch};
use str_utils::*;

use tes3::nif::*;

use crate::mge_xe::distant_statics::WATER_LOOK_MAX_LENGTH;
use crate::model::SubsetWater;
use crate::overrides::WaterNames;
use crate::vfs::normalize::make_normalized;

/// LOD distance threshold (in game units) used when selecting which LOD child to extract.
///
/// This matches the distant-static render distance used by MGE-XE so that the extracted
/// geometry corresponds to the level-of-detail the engine would display at that range.
const LOD_DIST: f32 = 8192.0;

#[derive(Clone, Copy, Default)]
struct Properties {
    alpha: NiLink<NiAlphaProperty>,
    material: NiLink<NiMaterialProperty>,
    texturing: NiLink<NiTexturingProperty>,
}

pub struct Geometry<'a> {
    pub shape: &'a NiTriShape,
    pub data: &'a NiTriShapeData,
    pub transform: Affine3A,
    /// The shape is a dry space in water, or a part of one: it has a mask name, or it is
    /// under an object that has one.
    pub dry_space: bool,
    properties: Properties,
}

impl<'a> Geometry<'a> {
    pub(crate) fn base_texture_path(&self, stream: &'a NiStream) -> Option<&'a str> {
        let texturing_property = stream.get(self.properties.texturing)?;
        let texture_map = texturing_property.texture_maps.first()?.as_ref()?;
        let TextureMap::Map(map) = texture_map else {
            return None;
        };

        let texture = stream.get(map.texture)?;
        let TextureSource::External(path) = &texture.source else {
            return None;
        };

        Some(path.as_str())
    }

    pub(crate) fn has_alpha(&self, stream: &'a NiStream) -> bool {
        if let Some(alpha_prop) = stream.get(self.properties.alpha) {
            alpha_prop.alpha_blending() || alpha_prop.alpha_testing()
        } else {
            false
        }
    }

    /// Returns whether this geometry has both a UV controller and the
    /// `mge.distant.scroll` opt-in extra-data tag.
    ///
    /// MGE-XE only enables distant UV animation when both are present. The
    /// controller detects animated UVs, while the string tag acts as an
    /// explicit author opt-in for the runtime's fixed scrolling approximation.
    pub(crate) fn has_uv_controller(&self, stream: &'a NiStream) -> bool {
        self.shape.controllers_of_type::<NiUVController>(stream).next().is_some()
            && self
                .shape
                .extra_datas_of_type::<NiStringExtraData>(stream)
                .any(|data| data.value.eq_ignore_ascii_case("mge.distant.scroll"))
    }

    pub(crate) fn material_property(&self, stream: &'a NiStream) -> Option<&'a NiMaterialProperty> {
        stream.get(self.properties.material)
    }

    /// Returns the stable identity of the shared shape data.
    pub(crate) fn data_id(&self) -> *const NiTriShapeData {
        std::ptr::from_ref(self.data)
    }

    /// Computes a minimum object-space bounding sphere, ignoring non-finite positions.
    pub(crate) fn object_space_bounding_sphere(&self, sphere_scratch: &mut BoundingSphereScratch) -> NiBound {
        let points: &[[f32; 3]] = must_cast_slice(&self.data.vertices);
        let bound = BoundingSphere::from_points_with_scratch(points, sphere_scratch);

        NiBound {
            center: bound.center.map(|v| v as f32).into(),
            radius: bound.radius as f32,
        }
    }

    pub(crate) fn place_bound(&self, object_space: NiBound) -> NiBound {
        object_space.transformed_by(&self.transform)
    }
}

/// What the water rules say about one mesh.
#[derive(Clone, Copy, Debug)]
pub struct MeshWater<'a> {
    /// Name prefixes of the objects that are left out, with everything under them.
    pub body_names: &'a [String],
    /// Name prefixes of the objects that mark a dry space.
    pub mask_names: &'a [String],
    /// Whether the shapes under a mask name are kept, as dry spaces. Without it they are left
    /// out, with everything under them.
    pub keeps_dry_spaces: bool,
    /// How the shapes that are left are drawn. `SubsetWater::None` for a mesh that keeps its
    /// own look.
    pub surface: SubsetWater,
}

fn name_starts_with_any(name: &str, prefixes: &[String]) -> bool {
    prefixes
        .iter()
        .any(|prefix| name.len() >= prefix.len() && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Returns whether lowercased `text` holds one of `words` with no letter on either side.
fn has_word(text: &str, words: &[String]) -> bool {
    words.iter().any(|word| {
        text.match_indices(word.as_str()).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + word.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphabetic()) && !after.is_some_and(|c| c.is_ascii_alphabetic())
        })
    })
}

/// Decides whether a mesh is distant water, and how its surface is drawn.
///
/// `registered` is the mesh override: `Some(false)` is never water, `Some(true)` is water
/// without any name. Otherwise the mesh is water when an object in it has a surface name or a
/// body name. The first surface name carries the words for the look.
///
/// The objects are taken in the order of the mod that makes the water near the player: from
/// the root, depth first, the children of a node in their order, and no farther than the first
/// surface name. An object that nothing under the root refers to does not count.
///
/// A mesh that is not water has rules too when an object in it has a mask name: its dry spaces
/// are kept apart from its other shapes. `registered_dry_space` is the mesh override for them:
/// `Some(false)` leaves them out of the distant mesh.
pub fn mesh_water<'a>(
    stream: &NiStream,
    names: &'a WaterNames,
    registered: Option<bool>,
    registered_dry_space: Option<bool>,
) -> Option<MeshWater<'a>> {
    if registered == Some(false) {
        return None;
    }

    let mut tag: Option<String> = None;
    let mut has_body = false;
    let mut has_mask = false;
    let root = stream.roots.first().copied().unwrap_or_default();
    let mut stack = vec![root.key];
    // A malformed file can have a cycle. An object is looked at once.
    let mut seen = HashSet::new();
    while let Some(key) = stack.pop() {
        let Some(this) = stream.objects.get(key) else {
            continue;
        };
        let Ok(object) = <&NiAVObject>::try_from(this) else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        if name_starts_with_any(&object.name, &names.surface) {
            tag = Some(object.name.to_ascii_lowercase());
            break;
        }
        has_body |= name_starts_with_any(&object.name, &names.body);
        has_mask |= name_starts_with_any(&object.name, &names.mask);
        if let Ok(node) = <&NiNode>::try_from(this) {
            for child in node.children.iter().rev() {
                stack.push(child.key);
            }
        }
    }
    let is_water = registered == Some(true) || tag.is_some() || has_body;
    if !is_water && !has_mask {
        return None;
    }

    let tag = tag.unwrap_or_default();
    // A mesh with a dry space and no water, a boat, keeps its own look: only the shape of
    // the dry space is not drawn.
    let surface = if !is_water || has_word(&tag, &names.plain_words) {
        SubsetWater::None
    } else if has_word(&tag, &names.sky_only_words) {
        SubsetWater::SkyOnly
    } else {
        SubsetWater::ReflectsScene
    };
    Some(MeshWater {
        body_names: &names.body,
        mask_names: &names.mask,
        keeps_dry_spaces: registered_dry_space != Some(false),
        surface,
    })
}

/// Returns the text after the `wv:` prefix of a string, if the string has the prefix.
///
/// The match is the pattern `^%s*[Ww][Vv]:%s*(.*)$` of the mod that makes the water near the
/// player. White space is the white space of C.
fn text_after_look_prefix(value: &str) -> Option<&str> {
    let rest = value.trim_start_matches(is_c_space);
    if !rest.get(..3)?.eq_ignore_ascii_case("wv:") {
        return None;
    }
    Some(rest[3..].trim_matches(is_c_space))
}

/// White space as `isspace` of C knows it.
fn is_c_space(c: char) -> bool {
    matches!(c, ' ' | '\t'..='\r')
}

/// Returns a look line as a metadata file gives it: without white space at its ends, and
/// without the `wv:` prefix if the text has one.
pub fn look_line_without_prefix(value: &str) -> &str {
    text_after_look_prefix(value).unwrap_or_else(|| value.trim_matches(is_c_space))
}

/// Says why the file format does not take a look line, or `None` when it takes the line.
///
/// The format takes at most [`WATER_LOOK_MAX_LENGTH`] bytes of ASCII without NUL.
pub fn water_look_fault(look: &str) -> Option<&'static str> {
    const { assert!(WATER_LOOK_MAX_LENGTH == 255) };
    if look.len() > WATER_LOOK_MAX_LENGTH {
        Some("is longer than 255 bytes")
    } else if !look.is_ascii() || look.contains('\0') {
        Some("is not ASCII")
    } else {
        None
    }
}

/// Returns whether a look line takes the opacity of the water from the vertex colours: its
/// last `opacity` key has the value `vertex`.
///
/// The words are read as the runtime reads them: white space is between the words, the key is
/// the text before the first `=` of a word, and the case of the key does not count. The case of
/// the value does not count either, as for the mod that makes the water near the player.
pub fn look_has_vertex_opacity(look: &str) -> bool {
    look.split(is_c_space)
        .filter_map(|word| word.split_once('='))
        .rfind(|(key, _)| key.eq_ignore_ascii_case("opacity"))
        .is_some_and(|(_, value)| value.eq_ignore_ascii_case("vertex"))
}

/// Finds the look line of a water mesh: the text after the `wv:` prefix of string extra data,
/// without white space at its ends. The text is not read here: the runtime knows the keys.
///
/// The objects are taken in the order of the mod that makes the water near the player: from
/// the root, depth first, the children of a node in their order, and at each object its extra
/// data in the order of the chain. The first string with the prefix is the look line, also when
/// no text follows the prefix. An object that nothing under the root refers to does not count.
pub fn mesh_water_look(stream: &NiStream) -> Option<&str> {
    let root = stream.roots.first().copied().unwrap_or_default();
    let mut stack = vec![root.key];
    // A malformed file can have a cycle. An object is looked at once.
    let mut seen = HashSet::new();
    while let Some(key) = stack.pop() {
        let Some(this) = stream.objects.get(key) else {
            continue;
        };
        let Ok(object) = <&NiAVObject>::try_from(this) else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        // The chain of a malformed file can have a cycle too. It has no more links than the
        // file has objects.
        let look = object
            .extra_datas_of_type::<NiStringExtraData>(stream)
            .take(stream.objects.len())
            .find_map(|data| text_after_look_prefix(&data.value));
        if look.is_some() {
            return look;
        }
        if let Ok(node) = <&NiNode>::try_from(this) {
            for child in node.children.iter().rev() {
                stack.push(child.key);
            }
        }
    }
    None
}

/// Clears root-node transforms before traversal.
pub(crate) fn clear_root_node_transforms(stream: &mut NiStream) {
    for root in &stream.roots {
        if let Some(object) = stream.objects.get_mut(root.key)
            && let Ok(node) = <&mut NiNode>::try_from(object)
        {
            node.clear_transform();
        }
    }
}

/// Iterates visible triangle shapes with accumulated transforms and render properties.
///
/// DFS traversal skips dynamic effects, particles, collision nodes, culled/editor markers,
/// inactive LOD/switch children, missing geometry, and unsupported texture formats.
/// LOD selection uses the child covering the `LOD_DIST` sample.
/// Billboard nodes use their stored transforms like ordinary nodes, preserving legacy
/// fixed-geometry behavior.
///
/// Skinned shapes are included; callers are expected to have baked deformation first, via
/// [`NiStream::apply_skins`], which clears the skin instance.
///
/// In a water mesh the body of the water is left out: the game hides it when it runs. A shape
/// that is drawn as water is kept without a texture and without UVs: the water pass reads
/// neither. So is the shape of a dry space, which no pass draws.
pub(crate) fn visible_geometries<'a>(
    stream: &'a NiStream,
    water: Option<MeshWater<'a>>,
) -> impl Iterator<Item = Geometry<'a>> {
    let root = stream.roots.first().copied().unwrap_or_default();
    let drawn_as_water = water.is_some_and(|water| water.surface.is_water());

    // The engine only handles markers when the root has "mrk" string data.
    let has_markers = stream.root_has_string_data_starting_with("mrk");

    let mut stack = vec![(root.key, Affine3A::IDENTITY, Properties::default(), false)];

    std::iter::from_fn(move || {
        while let Some((key, transform, properties, mut dry_space)) = stack.pop() {
            let Some(this) = stream.objects.get(key) else {
                continue;
            };

            if this.is_instance_of::<NiDynamicEffect>()
                || this.is_instance_of::<NiBSParticleNode>()
                || this.is_instance_of::<RootCollisionNode>()
            {
                continue;
            }

            let properties = if let Ok(object) = <&NiAVObject>::try_from(this) {
                if object.app_culled() || (has_markers && is_editor_marker(object)) {
                    continue;
                }
                if let Some(water) = water {
                    if name_starts_with_any(&object.name, water.body_names) {
                        continue;
                    }
                    if name_starts_with_any(&object.name, water.mask_names) {
                        if !water.keeps_dry_spaces {
                            continue;
                        }
                        dry_space = true;
                    }
                }
                resolved_properties(stream, object, properties)
            } else {
                properties
            };

            if let Ok(node) = <&NiLODNode>::try_from(this) {
                let transform = transform * node.transform();
                for (child, &[min, max]) in node.children.iter().zip(&node.lod_levels) {
                    if LOD_DIST >= min && LOD_DIST < max {
                        stack.push((child.key, transform, properties, dry_space));
                        break;
                    }
                }
                continue;
            }

            if let Ok(node) = <&NiSwitchNode>::try_from(this) {
                let transform = transform * node.transform();
                if let Some(child) = node.children.get(node.active_index) {
                    stack.push((child.key, transform, properties, dry_space));
                }
                continue;
            }

            if let Ok(node) = <&NiNode>::try_from(this) {
                let transform = transform * node.transform();
                for child in node.children.iter().rev() {
                    stack.push((child.key, transform, properties, dry_space));
                }
                continue;
            }

            let Ok(shape) = <&NiTriShape>::try_from(this) else {
                continue;
            };

            let Some(data) = stream.get_as::<_, NiTriShapeData>(shape.geometry_data) else {
                continue;
            };

            if data.vertices.is_empty() || data.triangles.is_empty() {
                continue;
            }
            let needs_no_texture = drawn_as_water || dry_space;
            if data.uv_sets.is_empty() && !needs_no_texture {
                continue;
            }

            let transform = transform * shape.transform();
            let geometry = Geometry {
                shape,
                data,
                transform,
                dry_space,
                properties,
            };

            if !needs_no_texture && !geometry.base_texture_path(stream).is_some_and(is_valid_texture_format) {
                continue;
            }

            return Some(geometry);
        }
        None
    })
}

/// Normalizes all external texture source paths in-place for VFS key lookups.
///
/// Converts ASCII uppercase to lowercase and replaces forward slashes with backslashes,
/// matching the key format expected by `Vfs::resolve_texture`.
pub(crate) fn normalize_texture_paths(stream: &mut NiStream) {
    for texture in stream.objects_of_type_mut::<NiSourceTexture>() {
        if let TextureSource::External(path) = &mut texture.source {
            make_normalized(path);
        }
    }
}

fn resolved_properties(stream: &NiStream, object: &NiAVObject, mut properties: Properties) -> Properties {
    for property in &object.properties {
        apply_property(stream, *property, &mut properties);
    }

    properties
}

fn apply_property(stream: &NiStream, property: NiLink<NiProperty>, properties: &mut Properties) {
    match stream.objects.get(property.key) {
        Some(NiType::NiAlphaProperty(_)) => properties.alpha = property.cast(),
        Some(NiType::NiMaterialProperty(_)) => properties.material = property.cast(),
        Some(NiType::NiTexturingProperty(_)) => properties.texturing = property.cast(),
        _ => {}
    }
}

fn is_editor_marker(object: &NiAVObject) -> bool {
    object
        .name
        .starts_with_ignore_ascii_case_with_lowercase_multiple(&["editormarker", "tri editormarker"])
        .is_some()
}

fn is_valid_texture_format(path: &str) -> bool {
    path.ends_with_ignore_ascii_case_with_lowercase_multiple(&[".bmp", ".dds", ".tga"])
        .is_some()
}

#[cfg(test)]
mod tests {
    use tes3::nif::*;

    use super::{
        has_word, look_has_vertex_opacity, look_line_without_prefix, mesh_water, mesh_water_look, name_starts_with_any,
        text_after_look_prefix, water_look_fault,
    };
    use crate::model::SubsetWater;
    use crate::overrides::WaterNames;

    fn kit_names() -> WaterNames {
        WaterNames {
            surface: vec!["watervolume".to_owned()],
            body: vec!["waterbody".to_owned()],
            mask: vec!["watermask".to_owned()],
            plain_words: vec!["plain".to_owned()],
            sky_only_words: vec!["skyonly".to_owned()],
        }
    }

    fn named_shape(name: &str) -> NiTriShape {
        let mut shape = NiTriShape::default();
        shape.name = name.into();
        shape
    }

    #[test]
    fn a_dry_space_gives_a_mesh_rules_and_does_not_make_it_water() {
        let names = kit_names();

        // A boat: a hull and the dry space inside it. The hull keeps its own look.
        let boat = stream_with(&["Hull", "WaterMask carries"]);
        let water = mesh_water(&boat, &names, None, None).expect("a mesh with a dry space has rules");
        assert_eq!(water.surface, SubsetWater::None);
        assert!(water.keeps_dry_spaces);

        // The mesh override can leave the dry spaces out. The mesh still has rules: they are
        // what leaves the shapes out.
        let water = mesh_water(&boat, &names, None, Some(false)).expect("rules");
        assert_eq!(water.surface, SubsetWater::None);
        assert!(!water.keeps_dry_spaces);
        assert!(mesh_water(&boat, &names, None, Some(true)).expect("rules").keeps_dry_spaces);

        // Water with a dry space in it: the mesh is water.
        let pond = stream_with(&["WaterVolume", "WaterMask"]);
        let water = mesh_water(&pond, &names, None, None).expect("water mesh");
        assert_eq!(water.surface, SubsetWater::ReflectsScene);
        assert!(water.keeps_dry_spaces);

        // Without the name in the table, a mesh with only that shape has no rules.
        let none = WaterNames {
            mask: Vec::new(),
            ..kit_names()
        };
        assert!(mesh_water(&boat, &none, None, None).is_none());
    }

    fn stream_with(shapes: &[&str]) -> NiStream {
        let mut stream = NiStream::new();
        let mut root = NiNode::default();
        for name in shapes {
            let shape = stream.insert(named_shape(name));
            root.children.push(shape.cast());
        }
        let root = stream.insert(root);
        stream.roots.push(root.cast());
        stream
    }

    #[test]
    fn the_first_water_name_is_the_first_in_the_scene_graph_not_in_the_file() {
        // The file holds the objects in this order: the sky-only shape, the plain shape, the
        // rock, the group, the root. The scene graph is root -> [group -> [rock, plain],
        // sky-only]. The group is a switch node that shows the rock: all its children count.
        let mut stream = NiStream::new();
        let sky_only = stream.insert(named_shape("WaterVolume skyonly"));
        let plain = stream.insert(named_shape("WaterVolume plain"));
        let rock = stream.insert(named_shape("Rock"));
        let mut group = NiSwitchNode::default();
        group.children.push(rock.cast());
        group.children.push(plain.cast());
        let group = stream.insert(group);
        let mut root = NiNode::default();
        root.children.push(group.cast());
        root.children.push(sky_only.cast());
        let root = stream.insert(root);
        stream.roots.push(root.cast());

        // Depth first, the plain shape comes before the sky-only shape.
        let names = kit_names();
        let water = mesh_water(&stream, &names, None, None).expect("water mesh");
        assert_eq!(water.surface, SubsetWater::None);

        // With the children the other way round, the sky-only shape is first.
        let Some(NiType::NiNode(root)) = stream.objects.get_mut(root.key) else {
            panic!("root node");
        };
        root.children.reverse();
        let water = mesh_water(&stream, &names, None, None).expect("water mesh");
        assert_eq!(water.surface, SubsetWater::SkyOnly);
    }

    #[test]
    fn a_water_name_that_nothing_refers_to_does_not_count() {
        let names = kit_names();
        for unreachable in ["WaterVolume plain", "WaterBody"] {
            let mut stream = NiStream::new();
            stream.insert(named_shape(unreachable));
            let rock = stream.insert(named_shape("Rock"));
            let mut root = NiNode::default();
            root.children.push(rock.cast());
            let root = stream.insert(root);
            stream.roots.push(root.cast());

            // Nothing under the root is named as water.
            assert!(mesh_water(&stream, &names, None, None).is_none());

            // The surface under the root gives the look, not the object that is first in the file.
            let sky_only = stream.insert(named_shape("WaterVolume skyonly"));
            let Some(NiType::NiNode(root)) = stream.objects.get_mut(root.key) else {
                panic!("root node");
            };
            root.children.push(sky_only.cast());
            let water = mesh_water(&stream, &names, None, None).expect("water mesh");
            assert_eq!(water.surface, SubsetWater::SkyOnly);
        }
    }

    /// Adds string extra data in front of the chain that `next` starts.
    fn string_data(stream: &mut NiStream, value: &str, next: NiLink<NiExtraData>) -> NiLink<NiExtraData> {
        let mut data = NiStringExtraData::default();
        data.value = value.into();
        data.next = next;
        stream.insert(data).cast()
    }

    #[test]
    fn the_look_line_is_the_first_in_the_scene_graph_not_in_the_file() {
        // The file holds the objects in this order: the look of the far shape, the look of the
        // near shape, the far shape, the near shape, the rock, the group, the root. The scene
        // graph is root -> [group -> [rock, near], far].
        let mut stream = NiStream::new();
        let far_look = string_data(&mut stream, "wv: speed=2", NiLink::default());
        let near_look = string_data(&mut stream, "WV:flow=0,-140", NiLink::default());
        let mut far = named_shape("Far");
        far.extra_data = far_look;
        let far = stream.insert(far);
        let mut near = named_shape("Near");
        near.extra_data = near_look;
        let near = stream.insert(near);
        let rock = stream.insert(named_shape("Rock"));
        let mut group = NiNode::default();
        group.children.push(rock.cast());
        group.children.push(near.cast());
        let group = stream.insert(group);
        let mut root = NiNode::default();
        root.children.push(group.cast());
        root.children.push(far.cast());
        let root = stream.insert(root);
        stream.roots.push(root.cast());

        // Depth first, the near shape comes before the far shape.
        assert_eq!(mesh_water_look(&stream), Some("flow=0,-140"));

        // With the children the other way round, the far shape is first.
        let Some(NiType::NiNode(node)) = stream.objects.get_mut(root.key) else {
            panic!("root node");
        };
        node.children.reverse();
        assert_eq!(mesh_water_look(&stream), Some("speed=2"));

        // A look line on the root comes before every child. In a chain the first string with
        // the prefix counts, not the first string.
        let last = string_data(&mut stream, "wv: glow=1", NiLink::default());
        let first = string_data(&mut stream, "wv: scale=3", last);
        let other = string_data(&mut stream, "NCO", first);
        let Some(NiType::NiNode(node)) = stream.objects.get_mut(root.key) else {
            panic!("root node");
        };
        node.extra_data = other;
        assert_eq!(mesh_water_look(&stream), Some("scale=3"));
    }

    #[test]
    fn a_look_line_that_nothing_refers_to_does_not_count() {
        let mut stream = NiStream::new();
        // String data that no object has, and a shape with a look line that no node has.
        string_data(&mut stream, "wv: speed=2", NiLink::default());
        let lost_look = string_data(&mut stream, "wv: speed=3", NiLink::default());
        let mut lost = named_shape("Lost");
        lost.extra_data = lost_look;
        stream.insert(lost);
        let rock = stream.insert(named_shape("Rock"));
        let mut root = NiNode::default();
        root.children.push(rock.cast());
        let root = stream.insert(root);
        stream.roots.push(root.cast());

        assert_eq!(mesh_water_look(&stream), None);

        // The look line under the root is found, not the one that is first in the file.
        let look = string_data(&mut stream, "wv: speed=4", NiLink::default());
        let Some(NiType::NiTriShape(rock)) = stream.objects.get_mut(rock.key) else {
            panic!("rock shape");
        };
        rock.extra_data = look;
        assert_eq!(mesh_water_look(&stream), Some("speed=4"));
    }

    #[test]
    fn the_first_look_prefix_wins_also_without_text() {
        let mut stream = NiStream::new();
        let later = string_data(&mut stream, "wv: speed=2", NiLink::default());
        let mut shape = named_shape("Shape");
        shape.extra_data = later;
        let shape = stream.insert(shape);
        let empty = string_data(&mut stream, "wv:  ", NiLink::default());
        let mut root = NiNode::default();
        root.extra_data = empty;
        root.children.push(shape.cast());
        let root = stream.insert(root);
        stream.roots.push(root.cast());

        assert_eq!(mesh_water_look(&stream), Some(""));
    }

    #[test]
    fn the_look_prefix_matches_without_case_after_white_space() {
        assert_eq!(
            text_after_look_prefix("wv: flow=0,-140 speed=1.2"),
            Some("flow=0,-140 speed=1.2")
        );
        assert_eq!(
            text_after_look_prefix(" \t\r\nWv:\tshader=tw_foam \r\n"),
            Some("shader=tw_foam")
        );
        assert_eq!(text_after_look_prefix("wV:p0=0.4,0.5,1.5"), Some("p0=0.4,0.5,1.5"));
        assert_eq!(text_after_look_prefix("wv:"), Some(""));
        assert_eq!(text_after_look_prefix("wv :speed=2"), None);
        assert_eq!(text_after_look_prefix("x wv: speed=2"), None);
        assert_eq!(text_after_look_prefix("wv"), None);
        assert_eq!(text_after_look_prefix("w\u{e9}:"), None);
        assert_eq!(text_after_look_prefix("mge.distant.scroll"), None);
    }

    #[test]
    fn a_look_line_of_a_metadata_file_can_have_the_prefix() {
        assert_eq!(look_line_without_prefix("flow=0,-140 glow=0.3"), "flow=0,-140 glow=0.3");
        assert_eq!(look_line_without_prefix(" \tflow=0,-140 \r\n"), "flow=0,-140");
        assert_eq!(look_line_without_prefix("wv: flow=0,-140"), "flow=0,-140");
        assert_eq!(look_line_without_prefix(" WV:flow=0,-140 "), "flow=0,-140");
        assert_eq!(look_line_without_prefix("wv:"), "");
        assert_eq!(look_line_without_prefix("  "), "");
        // The prefix is taken away once.
        assert_eq!(look_line_without_prefix("wv: wv: speed=2"), "wv: speed=2");
    }

    #[test]
    fn the_file_format_takes_255_bytes_of_ascii() {
        assert_eq!(water_look_fault(""), None);
        assert_eq!(water_look_fault(&"x".repeat(255)), None);
        assert_eq!(water_look_fault(&"x".repeat(256)), Some("is longer than 255 bytes"));
        assert_eq!(water_look_fault("shader=caf\u{e9}"), Some("is not ASCII"));
        assert_eq!(water_look_fault("speed=2\0"), Some("is not ASCII"));
    }

    #[test]
    fn opacity_from_vertices_is_the_last_opacity_key_with_the_value_vertex() {
        assert!(look_has_vertex_opacity("opacity=vertex"));
        assert!(look_has_vertex_opacity("flow=0,-140 opacity=vertex shader=tw_foam"));
        assert!(look_has_vertex_opacity("flow=0,-140\topacity=vertex\r\nspeed=2"));
        // The case of the key and of the value does not count.
        assert!(look_has_vertex_opacity("Opacity=Vertex"));
        assert!(look_has_vertex_opacity("OPACITY=VERTEX"));
        // The last key decides.
        assert!(look_has_vertex_opacity("opacity=0.5 opacity=vertex"));
        assert!(!look_has_vertex_opacity("opacity=vertex opacity=0.5"));

        assert!(!look_has_vertex_opacity(""));
        assert!(!look_has_vertex_opacity("flow=0,-140"));
        assert!(!look_has_vertex_opacity("opacity=0.5"));
        assert!(!look_has_vertex_opacity("tint=vertex"));
        // A word is a key and a value with one `=` and no white space between them.
        assert!(!look_has_vertex_opacity("opacity = vertex"));
        assert!(!look_has_vertex_opacity("opacity= vertex"));
        assert!(!look_has_vertex_opacity("opacity=vertex,1"));
        assert!(!look_has_vertex_opacity("opacity=vertexes"));
        assert!(!look_has_vertex_opacity("opacity==vertex"));
        assert!(!look_has_vertex_opacity("my_opacity=vertex"));
        assert!(!look_has_vertex_opacity("opacity"));
    }

    #[test]
    fn water_words_need_a_boundary_on_both_sides() {
        let words = ["plain".to_owned()];
        assert!(has_word("watervolume plain", &words));
        assert!(has_word("watervolume depth=300 plain.001", &words));
        assert!(!has_word("watervolume explained", &words));
        assert!(!has_word("watervolume plains", &words));
        assert!(!has_word("watervolume", &words));
    }

    #[test]
    fn water_names_match_by_prefix_without_case() {
        let names = ["waterbody".to_owned()];
        assert!(name_starts_with_any("WaterBody", &names));
        assert!(name_starts_with_any("WaterBody.001", &names));
        assert!(!name_starts_with_any("Water", &names));
        assert!(!name_starts_with_any("Tri WaterBody", &names));
    }
}
