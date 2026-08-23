use super::*;
use crate::texture_io::decode_texture_rgba;

/// Atlas-local alias relation for one dedupe tier (`Off` vs `Exact`).
///
/// Keeps Exact maps dense and fail-closed; Off uses an explicit identity sentinel so no
/// key→key `HashMap` is allocated.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AtlasAliasRelation {
    /// Every key resolves to itself (`TextureDedupeMode::Off`).
    Identity,
    /// Dense original→canonical map (`TextureDedupeMode::Exact`); missing keys are errors.
    Complete(HashMap<String, String>),
}

impl AtlasAliasRelation {
    /// Resolves `key` without cloning.
    ///
    /// `Identity` returns the borrowed input. `Complete` returns a borrow into the map value.
    /// A missing Complete entry is an error (fail-closed), not identity.
    pub(super) fn resolve<'a>(&'a self, key: &'a str) -> anyhow::Result<&'a str> {
        match self {
            Self::Identity => Ok(key),
            Self::Complete(map) => map
                .get(key)
                .map(String::as_str)
                .ok_or_else(|| anyhow::anyhow!("atlas alias map missing key '{key}'")),
        }
    }
}

/// Builds the [`PackerConfig`] used for all atlas packing runs.
///
/// Key choices:
/// - `MaxRects` with `BestAreaFit` heuristic for high packing density.
/// - Rotation disabled so UV coordinates remain axis-aligned.
/// - Power-of-two page dimensions required for DXT mipmap compatibility.
/// - Border and texture-extrusion values come from the atlas constants.
/// - `gpu_max` is the configured GPU-safe cap on each page's longest dimension.
pub(crate) fn make_packer_config(gpu_max: u32) -> PackerConfig {
    PackerConfig::builder()
        .with_max_dimensions(gpu_max, gpu_max)
        .allow_rotation(false)
        .force_max_dimensions(false)
        .pow2(true)
        .square(false)
        .trim(ATLAS_TRIM)
        .border_padding(ATLAS_BORDER_PADDING)
        .texture_padding(ATLAS_TEXTURE_PADDING)
        .texture_extrusion(ATLAS_TEXTURE_EXTRUSION)
        .family(AlgorithmFamily::MaxRects)
        .mr_heuristic(MaxRectsHeuristic::BestAreaFit)
        .build()
}

/// Loads an image from a resolved VFS asset, capping its longest dimension at `dim`.
///
/// DDS files pick the closest matching mip level for `dim` before falling back to a resize pass.
/// All other formats are decoded at full resolution and only resized when needed to satisfy the cap.
fn load_image(asset: &ResolvedAsset<'_>, vfs: &Vfs, dim: u32) -> Result<image::DynamicImage, Box<dyn std::error::Error>> {
    let bytes = vfs.read_asset_bytes(asset)?;
    let img = decode_texture_rgba(asset.key, &bytes, dim)?;

    Ok(image::DynamicImage::ImageRgba8(img))
}

pub(crate) struct PreparedAtlasInputs {
    pub(crate) layout_items: Vec<LayoutItem>,
    pub(crate) images: HashMap<String, image::RgbaImage>,
    /// Visible prepared RGBA fingerprint for each decoded canonical key.
    pub(crate) content_fingerprints: HashMap<String, [u8; 32]>,
    pub(crate) prepared_source_bytes: u64,
    pub(crate) decoded_texture_count: usize,
}

/// Loads and normalizes source inputs, performs decoded dedupe, and computes placements without
/// allocating output-page canvases.
///
/// Each texture's longest dimension is capped at its planned value in `dims` (the global cap unless
/// the sizing plan reduced it). A texture that fails to load becomes a 1×1 magenta placeholder
/// rather than an error: badly authored assets are routine in a heavily modded install, and one
/// unreadable texture must not abort a whole generation run.
pub(crate) fn prepare_textures(
    textures: &IndexSet<String>,
    vfs: &Vfs,
    dims: &HashMap<String, u32>,
    dedupe_mode: TextureDedupeMode,
    domain: AtlasDomain,
) -> (PreparedAtlasInputs, AtlasAliasRelation) {
    let (alias, layout_items, images, content_fingerprints, prepared_source_bytes, decoded_texture_count) = {
        let span = info_span!(
            "atlas.prepare_textures",
            report = true,
            domain = domain.as_str(),
            input_texture_count = textures.len() as u64,
            canonical_texture_count = tracing::field::Empty,
            prepared_source_bytes = tracing::field::Empty,
        );
        let _guard = span.enter();
        let mut inputs: Vec<(String, image::RgbaImage)> = textures
            .par_iter()
            .map(|key| {
                let asset = vfs.resolve_texture(key).expect("texture key not in VFS");
                let dim = *dims.get(key).expect("planned dimension missing for texture key");
                let image = match load_image(&asset, vfs, dim) {
                    Ok(image) => image.into_rgba8(),
                    Err(error) => {
                        error!("Failed to open texture {}: {}", key, error);
                        image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 255, 255]))
                    }
                };
                (key.clone(), image)
            })
            .collect();
        // Sorted key order fixes decoded-identity canonicals (first-seen among sorted inputs).
        inputs.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let decoded_texture_count = inputs.len();

        let alias = match dedupe_mode {
            TextureDedupeMode::Off => AtlasAliasRelation::Identity,
            TextureDedupeMode::Exact => {
                // Fingerprint decoded buffers under an outer par_iter so all cores hash textures
                // concurrently instead of one at a time. par_iter over a slice is order-preserving,
                // so collecting into a Vec keeps the sorted-key order that fixes build_alias_map's
                // first-seen canonical. Per-buffer hashing stays serial: benchmarks showed inner
                // update_rayon regresses once the outer pool is saturated by many large textures.
                let keyed: Vec<_> = inputs
                    .par_iter()
                    .map(|(key, image)| {
                        let fingerprint = decoded_fingerprint(image.width(), image.height(), image.as_raw());
                        (key.clone(), fingerprint)
                    })
                    .collect();
                let (_canonicals, alias_map) = build_alias_map(keyed);
                // Dense map from the same keys: indexing panics only on programmer error.
                inputs.retain(|(key, _)| alias_map[key] == *key);
                AtlasAliasRelation::Complete(alias_map)
            }
        };

        let mut layout_items = Vec::with_capacity(inputs.len());
        let mut images = HashMap::with_capacity(inputs.len());
        let mut content_fingerprints = HashMap::with_capacity(inputs.len());
        let mut prepared_source_bytes = 0_u64;
        for (key, image) in inputs {
            let (width, height) = image.dimensions();
            let (trimmed_rect, source) = compute_trim_rect(&image, 0);
            let (width, height, trimmed, source) = match trimmed_rect {
                Some(rect) => (rect.w, rect.h, true, source),
                None => (width, height, false, Rect::new(0, 0, width, height)),
            };
            prepared_source_bytes = prepared_source_bytes.saturating_add(image.as_raw().len() as u64);
            layout_items.push(LayoutItem {
                key: key.clone(),
                w: width,
                h: height,
                source: Some(source),
                source_size: Some(image.dimensions()),
                trimmed,
            });
            content_fingerprints.insert(key.clone(), visible_content_fingerprint(&image, source));
            images.insert(key, image);
        }
        span.record("canonical_texture_count", images.len() as u64);
        span.record("prepared_source_bytes", prepared_source_bytes);
        (
            alias,
            layout_items,
            images,
            content_fingerprints,
            prepared_source_bytes,
            decoded_texture_count,
        )
    };
    (
        PreparedAtlasInputs {
            layout_items,
            images,
            content_fingerprints,
            prepared_source_bytes,
            decoded_texture_count,
        },
        alias,
    )
}

pub(crate) fn pack_layout_only(
    layout_items: Vec<LayoutItem>,
    gpu_max: u32,
) -> tex_packer_core::error::Result<tex_packer_core::Atlas> {
    pack_layout_items(layout_items, make_packer_config(gpu_max))
}

/// Fingerprints exactly the visible RGBA rectangle that will be composited into an atlas slot.
pub(crate) fn visible_content_fingerprint(image: &image::RgbaImage, source: Rect) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(&source.w.to_le_bytes());
    hasher.update(&source.h.to_le_bytes());
    let stride = image.width() as usize * 4;
    let row_bytes = source.w as usize * 4;
    for y in source.y..source.y + source.h {
        let start = y as usize * stride + source.x as usize * 4;
        hasher.update(&image.as_raw()[start..start + row_bytes]);
    }
    *hasher.finalize().as_bytes()
}

pub(crate) fn compose_planned_page(
    images: &HashMap<String, image::RgbaImage>,
    page: &tex_packer_core::Page,
) -> image::RgbaImage {
    let mut canvas = image::RgbaImage::new(page.width, page.height);
    for frame in &page.frames {
        let source = images.get(&frame.key).expect("planned atlas frame source missing");
        tex_packer_core::compositing::blit_rgba(
            source,
            &mut canvas,
            frame.frame.x,
            frame.frame.y,
            frame.source.x,
            frame.source.y,
            frame.source.w,
            frame.source.h,
            frame.rotated,
            ATLAS_TEXTURE_EXTRUSION,
            false,
        );
    }
    canvas
}

#[cfg(test)]
mod alias_relation_tests {
    use super::AtlasAliasRelation;
    use hashbrown::HashMap;

    #[test]
    fn identity_resolves_any_key_to_itself() {
        let relation = AtlasAliasRelation::Identity;
        assert_eq!(relation.resolve("any/path.dds").unwrap(), "any/path.dds");
        assert_eq!(relation.resolve("other").unwrap(), "other");
    }

    #[test]
    fn complete_resolves_canonical_and_alias_keys() {
        let mut map = HashMap::new();
        map.insert("a.dds".into(), "a.dds".into());
        map.insert("b.dds".into(), "a.dds".into());
        let relation = AtlasAliasRelation::Complete(map);
        assert_eq!(relation.resolve("a.dds").unwrap(), "a.dds");
        assert_eq!(relation.resolve("b.dds").unwrap(), "a.dds");
    }

    #[test]
    fn complete_missing_key_fails_closed() {
        let relation = AtlasAliasRelation::Complete(HashMap::new());
        let err = relation.resolve("missing.dds").unwrap_err();
        assert!(err.to_string().contains("missing.dds"));
    }

    #[test]
    fn identity_is_not_equal_to_complete_identity_map() {
        let mut map = HashMap::new();
        map.insert("a.dds".into(), "a.dds".into());
        assert_ne!(AtlasAliasRelation::Identity, AtlasAliasRelation::Complete(map));
    }

    #[test]
    fn two_tier_resolution_source_then_decoded() {
        // Production-order chain: sorted keys 0, a, b. Tier-0 first-seen source: b→a (0,a canons).
        // Tier-1 sorted first-seen decoded: a→0 (0 is the earlier equal). Final b→0.
        let mut tier0 = HashMap::new();
        tier0.insert("0.dds".into(), "0.dds".into());
        tier0.insert("a.dds".into(), "a.dds".into());
        tier0.insert("b.dds".into(), "a.dds".into());
        let mut tier1 = HashMap::new();
        // Dense over Tier-0 canonicals only.
        tier1.insert("0.dds".into(), "0.dds".into());
        tier1.insert("a.dds".into(), "0.dds".into());
        let tier0 = AtlasAliasRelation::Complete(tier0);
        let tier1 = AtlasAliasRelation::Complete(tier1);
        let t0 = tier0.resolve("b.dds").unwrap();
        assert_eq!(t0, "a.dds");
        assert_eq!(tier1.resolve(t0).unwrap(), "0.dds");
        // Identity∘Identity leaves the key unchanged.
        let id = AtlasAliasRelation::Identity;
        assert_eq!(id.resolve(id.resolve("x").unwrap()).unwrap(), "x");
    }
}
