//! Statics canonicalization: packed static records into logical, order-free values.
//!
//! Vertex and index tables are removed by decoding every triangle into corner records and
//! rotating each triangle to its canonical cyclic form (winding preserved). Atlas page
//! placements are resolved to material content: the stored half-float `uv_bound` recovers
//! integral mip-0 frame edges. On large pages, adjacent edges can quantize to the same
//! half-float value; that packed collapse is represented as a one-texel sampling range anchored
//! at the recovered edge. The frame's decoded texels become the placement-independent material
//! identity. Deeper page mips are deterministic derivations of the composited page and are
//! therefore excluded from logical identity.

use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use blake3::{Hash, Hasher};
use half::f16;
use itertools::Itertools;
use serde_json::{Map, Value};

use crate::mge_xe::distant_statics::{
    HORIZON_FOOTPRINT_MAX_VERTS, PackedDistantStatic, PackedSubset, PackedVertex, StaticType,
};
use crate::statics::atlas::{ALPHA_ATLAS_PREFIX, OPAQUE_ATLAS_PREFIX};
use crate::verification::current::{DecodedDdsFormat, DecodedStaticOutput};
use crate::verification::logical::{compact_json, f16_bits};
use crate::verification::snapshot::crop_rgba_digest;

type Digest = [u8; 32];
type CornerBytes = [u8; 20];

/// Canonical statics domain output: the sorted record multiset plus per-record logical ids
/// in original directory order for `usage.data` ordinal resolution.
pub(super) struct StaticsSnapshot {
    /// Sorted multiset of canonical static values.
    pub(super) records: Value,
    /// Atlas/source materials interned by their decoded-content digest.
    pub(super) materials: Value,
    /// BLAKE3 logical id of each static, indexed by its decoded directory position.
    pub(super) logical_ids: Vec<String>,
}

/// Builds canonical values and logical ids for every decoded static record.
pub(super) fn build_statics(decoded: &DecodedStaticOutput) -> Result<StaticsSnapshot> {
    let mut records = Vec::with_capacity(decoded.static_meshes.len());
    let mut logical_ids = Vec::with_capacity(decoded.static_meshes.len());
    let mut materials = MaterialResolver::new(decoded);
    for (index, record) in decoded.static_meshes.iter().enumerate() {
        let digest = static_digest(&mut materials, index, record)?;
        let digest = Hash::from_bytes(digest).to_hex().to_string();
        logical_ids.push(digest.clone());
        records.push(digest);
    }
    records.sort_unstable();
    Ok(StaticsSnapshot {
        records: Value::Array(records.into_iter().map(Value::String).collect()),
        materials: materials.into_value(),
        logical_ids,
    })
}

/// Name for a static classification.
fn static_type_name(static_type: StaticType) -> &'static str {
    match static_type {
        StaticType::StaticAuto => "auto",
        StaticType::StaticNear => "near",
        StaticType::StaticFar => "far",
        StaticType::StaticVeryFar => "very_far",
        StaticType::StaticGrass => "grass",
        StaticType::StaticTree => "tree",
        StaticType::StaticBuilding => "building",
    }
}

/// Maps a stored component classification byte to its name.
fn classification_name(value: u8) -> Option<&'static str> {
    let static_type = match value {
        0 => StaticType::StaticAuto,
        1 => StaticType::StaticNear,
        2 => StaticType::StaticFar,
        3 => StaticType::StaticVeryFar,
        4 => StaticType::StaticGrass,
        5 => StaticType::StaticTree,
        6 => StaticType::StaticBuilding,
        _ => return None,
    };
    Some(static_type_name(static_type))
}

fn static_digest<'a>(materials: &mut MaterialResolver<'a>, index: usize, record: &'a PackedDistantStatic) -> Result<Digest> {
    let mut subsets = Vec::with_capacity(record.subsets.len());
    for (subset_index, subset) in record.subsets.iter().enumerate() {
        let context = format!("static {index} subset {subset_index} (texture {})", subset.texture);
        subsets.push(subset_digest(materials, record.static_type, subset, &context)?);
    }

    let mut hasher = domain_hasher(b"static-record-v2");
    hash_bounding_box(&mut hasher, &record.bounding_box);
    hash_bounding_sphere(&mut hasher, &record.bounding_sphere);
    hash_str(&mut hasher, static_type_name(record.static_type));
    hash_digest_multiset(&mut hasher, &mut subsets);
    Ok(*hasher.finalize().as_bytes())
}

fn subset_digest<'a>(
    materials: &mut MaterialResolver<'a>,
    static_type: StaticType,
    subset: &'a PackedSubset,
    context: &str,
) -> Result<Digest> {
    let mut triangles = triangle_digests(materials, static_type, subset, context)?;
    let mut components = component_digests(subset, &triangles, context)?;
    let horizon = horizon_footprint_digest(subset, context)?;

    let mut hasher = domain_hasher(b"static-subset-v2");
    hash_bounding_box(&mut hasher, &subset.bounding_box);
    hash_bounding_sphere(&mut hasher, &subset.bounding_sphere);
    hasher.update(&[u8::from(subset.has_alpha != 0)]);
    hasher.update(&[u8::from(subset.has_uv_controller != 0)]);
    hasher.update(&horizon);
    hash_digest_multiset(&mut hasher, &mut triangles);
    hash_digest_multiset(&mut hasher, &mut components);
    Ok(*hasher.finalize().as_bytes())
}

/// Hashes every triangle in serialized order.
///
/// Serialized order is preserved here so component ranges can be resolved; multiset
/// ordering is applied by the callers.
fn triangle_digests<'a>(
    materials: &mut MaterialResolver<'a>,
    static_type: StaticType,
    subset: &'a PackedSubset,
    context: &str,
) -> Result<Vec<Digest>> {
    subset
        .triangles
        .iter()
        .enumerate()
        .map(|(triangle_index, indices)| {
            let vertex = |index: u16| {
                subset
                    .vertices
                    .get(usize::from(index))
                    .with_context(|| format!("{context} triangle {triangle_index} index {index} is out of range"))
            };
            let vertices = [vertex(indices[0])?, vertex(indices[1])?, vertex(indices[2])?];
            let bound = triangle_uv_bound(static_type, &vertices, context, triangle_index)?;
            let material = materials.resolve(static_type, subset, bound, context, triangle_index)?;
            let corners = canonical_triangle_corners(vertices.map(corner_bytes));

            let mut hasher = domain_hasher(b"static-triangle-v2");
            hasher.update(&material);
            for corner in corners {
                hasher.update(&corner);
            }
            Ok(*hasher.finalize().as_bytes())
        })
        .collect()
}

/// Encodes one corner's post-quantization attributes into fixed canonical bytes.
fn corner_bytes(vertex: &PackedVertex) -> CornerBytes {
    let mut bytes = [0; 20];
    let mut offset = 0;
    for value in vertex.position {
        bytes[offset..offset + 2].copy_from_slice(&value.to_bits().to_le_bytes());
        offset += 2;
    }
    bytes[offset..offset + 4].copy_from_slice(&vertex.normal);
    offset += 4;
    bytes[offset..offset + 4].copy_from_slice(&vertex.color);
    offset += 4;
    for value in vertex.uv {
        bytes[offset..offset + 2].copy_from_slice(&value.to_bits().to_le_bytes());
        offset += 2;
    }
    bytes
}

/// Selects the lexicographically smallest cyclic corner rotation without changing winding.
fn canonical_triangle_corners(corners: [CornerBytes; 3]) -> [CornerBytes; 3] {
    let rotations = [
        corners,
        [corners[1], corners[2], corners[0]],
        [corners[2], corners[0], corners[1]],
    ];
    *rotations.iter().min().expect("three rotations are present")
}

/// Returns one regular triangle's uniform stored atlas bound.
///
/// Bounds are linearly interpolated by MGE-XE's static shader, so accepting different
/// corner values would hide runtime-visible sampling. Grass uses a different wire vertex
/// declaration and therefore has no bound to retain.
fn triangle_uv_bound(
    static_type: StaticType,
    vertices: &[&PackedVertex; 3],
    context: &str,
    triangle_index: usize,
) -> Result<Option<[f16; 4]>> {
    if static_type == StaticType::StaticGrass {
        return Ok(None);
    }
    let first = vertices.first().context("a triangle must contain a corner")?.uv_bound;
    let expected = first.map(f16::to_bits);
    for (corner, vertex) in vertices.iter().enumerate().skip(1) {
        ensure!(
            vertex.uv_bound.map(f16::to_bits) == expected,
            "{context} triangle {triangle_index} corner {corner} uv_bound differs from corner 0; a triangle must store one uniform bound"
        );
    }
    Ok(Some(first))
}

/// Resolves component ranges through the serialized triangles they cover.
///
/// Components must tile the subset's serialized triangle list exactly: contiguous from
/// triangle zero through the final triangle. The serializer enforces this; it is re-checked
/// here because the decoder only bounds ranges without requiring full coverage.
fn component_digests(subset: &PackedSubset, triangles: &[Digest], context: &str) -> Result<Vec<Digest>> {
    if subset.components.is_empty() {
        return Ok(Vec::new());
    }
    let mut expected_start = 0usize;
    let mut digests = Vec::with_capacity(subset.components.len());
    for (component_index, component) in subset.components.iter().enumerate() {
        let start = component.first_triangle as usize;
        let count = component.triangle_count as usize;
        ensure!(
            start == expected_start,
            "{context} component {component_index} begins at triangle {start}, expected {expected_start}"
        );
        let end = start
            .checked_add(count)
            .with_context(|| format!("{context} component {component_index} triangle range overflows"))?;
        ensure!(
            end <= triangles.len(),
            "{context} component {component_index} exceeds the {} serialized triangles",
            triangles.len()
        );
        expected_start = end;

        let classification = classification_name(component.classification)
            .with_context(|| format!("{context} component {component_index} has invalid classification"))?;
        ensure!(
            component.reserved == [0; 3],
            "{context} component {component_index} reserved bytes must be zero"
        );

        let mut component_triangles = triangles[start..end].to_vec();
        let mut hasher = domain_hasher(b"static-component-v2");
        hash_str(&mut hasher, classification);
        hash_f32(&mut hasher, component.radius);
        hash_digest_multiset(&mut hasher, &mut component_triangles);
        digests.push(*hasher.finalize().as_bytes());
    }
    ensure!(
        expected_start == triangles.len(),
        "{context} components cover {expected_start} of {} serialized triangles",
        triangles.len()
    );
    Ok(digests)
}

/// Canonicalizes the horizon footprint: `null` when empty, otherwise `max_z` bits plus the
/// closed polygon rotated to its canonical cyclic form (winding preserved).
fn horizon_footprint_digest(subset: &PackedSubset, context: &str) -> Result<Digest> {
    let footprint = &subset.horizon_footprint;
    let count = usize::from(footprint.vertex_count);
    ensure!(
        count <= HORIZON_FOOTPRINT_MAX_VERTS,
        "{context} horizon footprint has {count} vertices, maximum is {HORIZON_FOOTPRINT_MAX_VERTS}"
    );
    if count == 0 {
        return Ok(*domain_hasher(b"empty-horizon-footprint-v2").finalize().as_bytes());
    }
    let mut polygon = footprint.footprint_xy[..count]
        .iter()
        .map(|&[x, y]| [x.to_bits(), y.to_bits()])
        .collect_vec();
    let rotation = (0..count)
        .min_by(|&left, &right| {
            (0..count)
                .map(|offset| polygon[(left + offset) % count])
                .cmp((0..count).map(|offset| polygon[(right + offset) % count]))
        })
        .expect("a non-empty polygon has a rotation");
    polygon.rotate_left(rotation);

    let mut hasher = domain_hasher(b"horizon-footprint-v2");
    hash_f32(&mut hasher, footprint.max_z);
    hasher.update(&(count as u64).to_le_bytes());
    for [x, y] in polygon {
        hasher.update(&x.to_le_bytes());
        hasher.update(&y.to_le_bytes());
    }
    Ok(*hasher.finalize().as_bytes())
}

fn domain_hasher(domain: &[u8]) -> Hasher {
    let mut hasher = Hasher::new();
    hash_bytes(&mut hasher, domain);
    hasher
}

fn hash_bytes(hasher: &mut Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_str(hasher: &mut Hasher, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

fn hash_f32(hasher: &mut Hasher, value: f32) {
    hasher.update(&value.to_bits().to_le_bytes());
}

fn hash_bounding_box(hasher: &mut Hasher, bounds: &crate::mge_xe::distant_statics::BoundingBox) {
    for value in bounds.min.to_array().into_iter().chain(bounds.max.to_array()) {
        hash_f32(hasher, value);
    }
}

fn hash_bounding_sphere(hasher: &mut Hasher, sphere: &crate::mge_xe::distant_statics::BoundingSphere) {
    for value in sphere.center.to_array() {
        hash_f32(hasher, value);
    }
    hash_f32(hasher, sphere.radius);
}

fn hash_digest_multiset(hasher: &mut Hasher, digests: &mut [Digest]) {
    digests.sort_unstable();
    hasher.update(&(digests.len() as u64).to_le_bytes());
    for digest in digests {
        hasher.update(digest);
    }
}

/// Static atlas page family, derived from the page filename prefix.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum AtlasFamily {
    /// BC1 page for opaque subsets.
    Opaque,
    /// BC3 page for alpha-tested subsets.
    Alpha,
}

/// Runtime material identity before decoded content removes atlas placement.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MaterialKey<'a> {
    texture: &'a str,
    family: Option<AtlasFamily>,
    has_alpha: bool,
    bound: Option<[u16; 4]>,
}

/// Resolves and interns static materials so large decoded frames are stored once.
struct MaterialResolver<'a> {
    decoded: &'a DecodedStaticOutput,
    cache: crate::IndexMap<MaterialKey<'a>, Digest>,
    table: BTreeSet<Digest>,
}

impl<'a> MaterialResolver<'a> {
    fn new(decoded: &'a DecodedStaticOutput) -> Self {
        Self {
            decoded,
            cache: crate::IndexMap::default(),
            table: BTreeSet::new(),
        }
    }

    fn resolve(
        &mut self,
        static_type: StaticType,
        subset: &'a PackedSubset,
        bound: Option<[f16; 4]>,
        context: &str,
        triangle_index: usize,
    ) -> Result<Digest> {
        let family = (static_type != StaticType::StaticGrass)
            .then(|| parse_atlas_page_family(&subset.texture))
            .flatten();
        let key = MaterialKey {
            texture: &subset.texture,
            family,
            has_alpha: subset.has_alpha != 0,
            bound: bound.map(|value| value.map(f16::to_bits)),
        };
        if let Some(digest) = self.cache.get(&key) {
            return Ok(*digest);
        }

        let triangle_context = format!("{context} triangle {triangle_index}");
        let value = resolve_material(self.decoded, static_type, subset, family, bound, &triangle_context)?;
        let digest = *blake3::hash(compact_json(&value).as_bytes()).as_bytes();
        self.table.insert(digest);
        self.cache.insert(key, digest);
        Ok(digest)
    }

    fn into_value(self) -> Value {
        Value::Array(
            self.table
                .into_iter()
                .map(|digest| Value::String(Hash::from_bytes(digest).to_hex().to_string()))
                .collect(),
        )
    }
}

impl AtlasFamily {
    fn name(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Alpha => "alpha",
        }
    }

    fn expected_format(self) -> DecodedDdsFormat {
        match self {
            Self::Opaque => DecodedDdsFormat::Bc1,
            Self::Alpha => DecodedDdsFormat::Bc3,
        }
    }
}

/// Parses a generated atlas page filename into its family.
///
/// Accepts exactly the names the generator emits: `<prefix>.dds` for page zero and
/// `<prefix>_<positive-page>.dds` beyond it. Any other name is a source texture path.
fn parse_atlas_page_family(name: &str) -> Option<AtlasFamily> {
    let (family, suffix) = if let Some(suffix) = name.strip_prefix(ALPHA_ATLAS_PREFIX) {
        (AtlasFamily::Alpha, suffix)
    } else {
        (AtlasFamily::Opaque, name.strip_prefix(OPAQUE_ATLAS_PREFIX)?)
    };
    let stem = suffix.strip_suffix(".dds")?;
    if stem.is_empty() {
        return Some(family);
    }
    let page = stem.strip_prefix('_')?;
    (!page.is_empty() && page != "0" && page.bytes().all(|byte| byte.is_ascii_digit())).then_some(family)
}

/// Resolves one triangle's material: atlas placements become placement-independent decoded
/// content, while source textures keep their normalized runtime path identity.
fn resolve_material(
    decoded: &DecodedStaticOutput,
    static_type: StaticType,
    subset: &PackedSubset,
    family: Option<AtlasFamily>,
    bound: Option<[f16; 4]>,
    context: &str,
) -> Result<Value> {
    if let Some(family) = family {
        let bound = bound.with_context(|| format!("{context} atlas triangle has no uv_bound"))?;
        return resolve_atlas_material(decoded, subset, family, bound, context);
    }

    // Grass and passthrough subsets keep their VFS-resolved source texture identity; the
    // output tree cannot prove the resolved bytes, so the normalized runtime path is the
    // output-level identity. Snapshot paths display with forward slashes.
    let path = subset.texture.replace('\\', "/").to_ascii_lowercase();
    let mut map = Map::new();
    map.insert("kind".to_owned(), Value::String("source".to_owned()));
    map.insert("path".to_owned(), Value::String(path));
    if static_type != StaticType::StaticGrass {
        // Regular passthrough triangles still consume a stored bound. Grass vertices use a
        // different wire declaration, so no bound is present or recorded.
        let bound = bound.with_context(|| format!("{context} regular triangle has no uv_bound"))?;
        map.insert(
            "uv_bound".to_owned(),
            Value::Array(bound.iter().copied().map(f16_bits).collect()),
        );
    }
    Ok(Value::Object(map))
}

/// Resolves one stored half-float bound pair to an exact integral mip-0 pixel range.
///
/// The current packer places integer rectangles on power-of-two pages no larger than 16384,
/// so every generator-valid stored bound times the page dimension is exactly an integer. A
/// non-integral product means the tree was not produced by the current pipeline and decoding
/// fails rather than guessing. Equal edges are valid after half-float quantization on large
/// pages and are widened separately by [`sampling_frame_range`].
fn integral_frame_edges(min: f16, max: f16, extent: u32, context: &str, axis: &str) -> Result<(u32, u32)> {
    ensure!(extent > 0, "{context} {axis} extent must be nonzero");
    let min_px = min.to_f64_const() * f64::from(extent);
    let max_px = max.to_f64_const() * f64::from(extent);
    ensure!(
        min_px.is_finite() && max_px.is_finite() && min_px.fract() == 0.0 && max_px.fract() == 0.0,
        "{context} {axis} bounds [{min}, {max}] do not recover integral mip-0 frame edges on extent {extent}"
    );
    ensure!(
        0.0 <= min_px && min_px <= max_px && max_px <= f64::from(extent),
        "{context} {axis} bounds [{min}, {max}] are not ordered within extent {extent}"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok((min_px as u32, max_px as u32))
}

/// Converts an integral packed frame range into a nonempty texel crop.
///
/// A collapsed range represents the actual half-float wire value rather than an invalid
/// generator output. Sampling is anchored at its recovered edge, or at the final texel for an
/// edge equal to the page extent.
fn sampling_frame_range(start: u32, end: u32, extent: u32) -> (u32, u32) {
    if start < end {
        return (start, end);
    }

    let texel = start.min(extent - 1);
    (texel, texel + 1)
}

/// Resolves an atlas-page triangle to a placement-independent material: the family plus the exact
/// mip-0 frame content, keyed relative to the frame origin so page and rectangle placement
/// never enter the identity.
fn resolve_atlas_material(
    decoded: &DecodedStaticOutput,
    subset: &PackedSubset,
    family: AtlasFamily,
    bound: [f16; 4],
    context: &str,
) -> Result<Value> {
    let page = decoded
        .static_atlas_pages
        .get(subset.texture.as_ref())
        .with_context(|| format!("{context} references a missing atlas page"))?;
    ensure!(
        page.format == family.expected_format(),
        "{context} atlas page format {:?} does not match family {}",
        page.format,
        family.name()
    );
    ensure!(
        (subset.has_alpha != 0) == (family == AtlasFamily::Alpha),
        "{context} alpha flag does not match its {} atlas family",
        family.name()
    );
    let mip = page
        .mips
        .first()
        .with_context(|| format!("{context} atlas page stores no mips"))?;

    // Stored lane order is [min_v, max_u, min_u, max_v].
    let [min_v, max_u, min_u, max_v] = bound;
    let (x_start, x_end) = integral_frame_edges(min_u, max_u, mip.width, context, "u")?;
    let (y_start, y_end) = integral_frame_edges(min_v, max_v, mip.height, context, "v")?;
    let (x_start, x_end) = sampling_frame_range(x_start, x_end, mip.width);
    let (y_start, y_end) = sampling_frame_range(y_start, y_end, mip.height);
    let width = x_end - x_start;
    let height = y_end - y_start;
    let texels_digest = crop_rgba_digest(mip, x_start, y_start, width, height)
        .with_context(|| format!("{context} atlas frame crop failed"))?;

    let mut map = Map::new();
    map.insert("kind".to_owned(), Value::String("atlas".to_owned()));
    map.insert("family".to_owned(), Value::String(family.name().to_owned()));
    map.insert("width".to_owned(), Value::from(u64::from(width)));
    map.insert("height".to_owned(), Value::from(u64::from(height)));
    map.insert("texels_digest".to_owned(), Value::String(texels_digest));
    Ok(Value::Object(map))
}
