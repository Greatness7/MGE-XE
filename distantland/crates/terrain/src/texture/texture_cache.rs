use rayon::prelude::*;

use super::*;
use crate::texture_io::{
    TextureFormat, decode_dds, plan_dds_mip_load, resize_rgba_to_max_dimension, texture_format_from_key,
};

/// Cache of terrain textures and generated mip chains keyed by normalized texture path.
pub struct TerrainTextureCache<'a> {
    pub(crate) images: IndexMap<&'a str, TerrainTexture>,
    pub(crate) default_key: &'a str,
}

/// Ordered unique terrain texture paths referenced by the current terrain cells.
#[derive(Clone, Debug)]
pub(crate) struct TerrainTextureInputs<'a> {
    pub(crate) default_key: &'a str,
    pub(crate) ordered_paths: IndexSet<&'a str>,
}

/// BC1-compatible color blocks for a single mip level, retained alongside the
/// decoded RGBA mip so the source atlas builder can pass blocks through without
/// a decode/re-encode round-trip when alignment permits.
///
/// Blocks may come from DXT1/BC1 data directly, or from the color half of
/// DXT3/BC2 and DXT5/BC3 blocks when the alpha data can be discarded safely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bc1Mip {
    /// Width of this mip level in pixels.
    pub width: u32,
    /// Height of this mip level in pixels.
    pub height: u32,
    /// BC1-compatible color blocks in row-major order, 8 bytes per 4x4 block.
    pub blocks: Vec<u8>,
}

/// A terrain texture with its mip chain for trilinear sampling.
///
/// `bytes_hash` serves as the identity fingerprint used by atlas cache keys and is always valid
/// as soon as an entry exists. In production it's the Blake3 hash of the raw on-disk asset bytes
/// (set by `load_one`); for synthetic textures built via `from_base`, it covers the base RGBA
/// bytes instead, so distinct inputs still produce distinct keys.
///
/// `levels` and `bc1_mips`, by contrast, are only populated once `TerrainTextureCache::decode`
/// has run; before that they're empty and `pending` holds the raw asset bytes instead. This
/// identity/decode split lets a clean terrain diff skip decoding. It only needs `bytes_hash`.
/// decoding entirely.
#[derive(Clone)]
pub struct TerrainTexture {
    /// Mip chain from base level down to 1x1. Empty until decoded. See the struct docs.
    pub levels: SmallVec<[RgbaImage; 9]>,
    /// BC1-compatible color blocks per mip level, indexed in parallel with `levels`.
    /// `bc1_mips[i] = Some(...)` when the source DDS supplied aligned DXT1/BC1
    /// blocks or safely reusable color blocks from DXT3/BC2 or DXT5/BC3; `None`
    /// otherwise (synthetic textures, resized sources, unsupported formats, unsafe
    /// BC2/BC3 color blocks, or generated tail mips below the source's deepest
    /// stored mip).
    pub bc1_mips: SmallVec<[Option<Bc1Mip>; 9]>,
    /// Identity fingerprint used by atlas cache keys; see the struct docs for its provenance
    /// and how it differs between loaded and synthetic textures.
    pub bytes_hash: [u8; 32],
    /// Raw on-disk asset bytes awaiting decode, with the decoder chosen from the resolved
    /// asset's extension. `Some` only between `load_one` (read + hash) and `decode`; `None`
    /// for synthetic textures, which are decoded immediately, and taken once decoding
    /// consumes it. `pub(crate)` only so tests elsewhere in this crate can construct
    /// already-decoded fixtures directly.
    pub(crate) pending: Option<PendingDecode>,
}

/// Undecoded asset bytes plus the decoder selected for them.
///
/// The format is captured at load time rather than derived at decode time because the cache is
/// keyed by the *requested* texture path, while the VFS may have resolved that to a `.dds`
/// counterpart. The key's extension can therefore name the wrong decoder.
#[derive(Clone)]
pub(crate) struct PendingDecode {
    /// Decoder selected from the resolved asset key's extension.
    pub(crate) format: TextureFormat,
    /// Raw asset bytes as read from the VFS.
    pub(crate) bytes: Vec<u8>,
}

impl TerrainTexture {
    /// Creates a `TerrainTexture` from a base RGBA image, generating the full mip chain.
    ///
    /// `bytes_hash` is derived from the base image pixels since there are no on-disk
    /// bytes available for synthetic textures.
    pub(crate) fn from_base(base: RgbaImage) -> Self {
        let bytes_hash = *blake3::hash(base.as_raw()).as_bytes();
        let levels = generate_mip_chain(base);
        let bc1_mips = std::iter::repeat_n(None, levels.len()).collect();
        Self {
            levels,
            bc1_mips,
            bytes_hash,
            pending: None,
        }
    }

    /// Decodes `pending` into `levels`/`bc1_mips` in place, clearing `pending`.
    /// A no-op returning `true` if there is nothing pending (already decoded). Returns `false`,
    /// leaving `levels`/`bc1_mips` empty, if the pending bytes cannot be decoded as an image.
    ///
    /// The decoder comes from the extension recorded at load time, never from the bytes; a file
    /// whose contents disagree with its extension fails here, as it would in the engine.
    fn decode(&mut self, max_dimension: u32) -> bool {
        let Some(PendingDecode { format, bytes }) = self.pending.take() else {
            return true;
        };
        let decoded = match format {
            TextureFormat::Dds => TerrainTextureCache::load_dds_mips(&bytes, max_dimension),
            TextureFormat::Image(format) => image::load_from_memory_with_format(&bytes, format)
                .ok()
                .map(|img| TerrainTexture::from_base(resize_rgba_to_max_dimension(img.into_rgba8(), max_dimension))),
        };
        match decoded {
            Some(texture) => {
                self.levels = texture.levels;
                self.bc1_mips = texture.bc1_mips;
                true
            }
            None => false,
        }
    }

    /// Returns a reference to the base (highest-resolution) mip level.
    pub(crate) fn base(&self) -> &RgbaImage {
        &self.levels[0]
    }

    /// Returns the BC1-compatible mip whose dimensions match `logical_tile_size`,
    /// if the source DDS supplied one. Used by the atlas builder's block-copy fast path.
    pub fn bc1_mip_at_size(&self, logical_tile_size: u32) -> Option<&Bc1Mip> {
        self.levels.iter().zip(self.bc1_mips.iter()).find_map(|(level, bc1)| {
            let bc1 = bc1.as_ref()?;
            (level.width() == logical_tile_size && level.height() == logical_tile_size).then_some(bc1)
        })
    }
}

/// Generates a complete mip chain down to 1×1 for `base`, using 2×2 sRGB-correct downsampling.
fn generate_mip_chain(base: RgbaImage) -> SmallVec<[RgbaImage; 9]> {
    let mut levels = SmallVec::<[_; 9]>::new();
    levels.push(base);

    while let Some(prev) = levels.last()
        && (prev.width() > 1 || prev.height() > 1)
    {
        let next_w = (prev.width() / 2).max(1);
        let next_h = (prev.height() / 2).max(1);
        let next = downsample_2x2_srgb_correct(prev, next_w, next_h);
        levels.push(next);
    }

    levels
}

/// Downsamples `src` to `(width, height)` by averaging 2×2 pixel blocks in linear
/// RGB space, then re-encodes to sRGB bytes. Alpha is averaged linearly.
///
/// This intentionally differs from `dds::downsample_2x2_rgba8_parallel`, which
/// averages RGBA bytes directly for fast generated DDS mip emission.
fn downsample_2x2_srgb_correct(src: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    // Indexing raw bytes rather than `get_pixel`/`put_pixel` drops per-texel bounds checks
    // from the inner loop; the table lookup replaces the `powf` in `srgb_sample_to_linear`.
    // Accumulation order is unchanged, so results stay bit-identical to the scalar version.
    let table = srgb_u8_to_linear_table();
    let src_width = src.width() as usize;
    let src_height = src.height() as usize;
    let src_pixels = src.as_raw();
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);

    for y in 0..height as usize {
        let sy = y * 2;
        for x in 0..width as usize {
            let sx = x * 2;

            let mut rgb_sum = [0.0f32; 3];
            let mut alpha_sum = 0u32;
            let mut count = 0u32;

            for oy in 0..2 {
                let py = sy + oy;
                if py >= src_height {
                    continue;
                }
                let row = &src_pixels[py * src_width * 4..(py + 1) * src_width * 4];
                for ox in 0..2 {
                    let px = sx + ox;
                    if px >= src_width {
                        continue;
                    }
                    let texel = &row[px * 4..px * 4 + 4];
                    for c in 0..3 {
                        rgb_sum[c] += table[texel[c] as usize];
                    }
                    alpha_sum += texel[3] as u32;
                    count += 1;
                }
            }

            out.extend_from_slice(&[
                linear_to_srgb_u8(rgb_sum[0] / count as f32),
                linear_to_srgb_u8(rgb_sum[1] / count as f32),
                linear_to_srgb_u8(rgb_sum[2] / count as f32),
                (alpha_sum / count) as u8,
            ]);
        }
    }

    RgbaImage::from_raw(width, height, out).expect("downsampled buffer matches its dimensions")
}

impl<'a> TerrainTextureCache<'a> {
    /// Collects every unique terrain texture path referenced by `terrain_cells` plus the
    /// default fallback entry.
    pub(crate) fn collect_inputs(vfs: &'a Vfs, terrain_cells: &TerrainCells<'a>) -> TerrainTextureInputs<'a> {
        let default_key = default_land_texture_key(vfs);
        let mut ordered_paths: IndexSet<&'a str> = Default::default();
        ordered_paths.insert(default_key);

        for cell in terrain_cells.values() {
            for row in cell.texture_indices.iter() {
                for &raw in row.iter() {
                    let path = resolve_raw_vtex(cell, raw, default_key);
                    ordered_paths.insert(path);
                }
            }
        }

        TerrainTextureInputs {
            default_key,
            ordered_paths,
        }
    }

    /// Loads the collected terrain texture inputs: reads each asset's bytes from the VFS and
    /// hashes them, but does not decode pixel data or generate mips.
    ///
    /// This is the identity phase: `bytes_hash` is all the unit fingerprint and tier-0 dedupe
    /// need, and a clean terrain diff never has to touch a decoded pixel. Callers that do need
    /// decoded textures (an actual DDS rebuild) call [`Self::decode`] on the returned cache.
    pub(crate) fn load_inputs(vfs: &'a Vfs, inputs: &TerrainTextureInputs<'a>) -> Self {
        let span = info_span!(
            "terrain.load_textures",
            report = true,
            terrain_texture_inputs = inputs.ordered_paths.len() as u64,
            terrain_texture_count = tracing::field::Empty
        );
        let _guard = span.enter();

        // Each load is an independent VFS read plus a hash, and `Vfs` is read-only (it is
        // already shared across rayon workers by the statics stage), so the loads run
        // concurrently. Results are reassembled in `ordered_paths` order afterwards, which keeps
        // both cache iteration order and the failure warnings deterministic.
        let ordered_paths = inputs.ordered_paths.iter().copied().collect::<Vec<&'a str>>();
        let loaded = ordered_paths
            .par_iter()
            .map(|&path| Self::load_one(vfs, path))
            .collect::<Vec<_>>();

        let mut images: IndexMap<&'a str, TerrainTexture> =
            IndexMap::with_capacity_and_hasher(ordered_paths.len(), Default::default());
        for (path, texture) in ordered_paths.iter().copied().zip(loaded) {
            match texture {
                Some(texture) => {
                    images.insert(path, texture);
                }
                None => {
                    warn!("Failed to load terrain texture: {}", path);
                }
            }
        }

        if !images.contains_key(inputs.default_key) {
            warn!("Default terrain texture not found, using fallback");
            images.insert(
                inputs.default_key,
                TerrainTexture::from_base(RgbaImage::from_pixel(4, 4, image::Rgba([69, 51, 33, 255]))),
            );
        }

        info!(
            "Terrain texture cache: {} textures loaded ({} failed)",
            images.len(),
            inputs.ordered_paths.len().saturating_sub(images.len())
        );
        span.record("terrain_texture_count", images.len() as u64);

        TerrainTextureCache {
            images,
            default_key: inputs.default_key,
        }
    }

    /// Reads one texture's bytes from the VFS and hashes them, without decoding. Returns `None`
    /// only if the asset cannot be resolved or read. Unlike the old combined load, an unreadable
    /// *format* is not detected here; decode failure surfaces later, in [`Self::decode`].
    fn load_one(vfs: &Vfs, path: &str) -> Option<TerrainTexture> {
        let asset = vfs.resolve_texture(path)?;
        // From the resolved key, not `path`: a requested `.tga` may have resolved to a `.dds`.
        let format = texture_format_from_key(asset.key)?;
        let bytes = vfs.read_asset_bytes(&asset).ok()?;
        let bytes_hash = *blake3::hash(&bytes).as_bytes();
        Some(TerrainTexture {
            levels: SmallVec::new(),
            bc1_mips: SmallVec::new(),
            bytes_hash,
            pending: Some(PendingDecode {
                format,
                bytes: bytes.into_owned(),
            }),
        })
    }

    /// Decodes every texture's pixel data and mip chain in parallel, in place.
    ///
    /// `load_inputs` only reads and hashes assets; a texture's `levels`/`bc1_mips` stay empty
    /// until this runs. Call it once, right before atlas or patch-albedo building, on a cache
    /// that turned out to need decoded pixels after all. This means an actual DDS rebuild; a clean
    /// terrain diff never calls this.
    ///
    /// A texture whose bytes fail to decode is dropped from the cache entirely (matching the old
    /// combined load's behavior), so `get` falls back to the default texture for that key; the
    /// default texture itself is synthetic and always already decoded, so it can't be dropped.
    pub(crate) fn decode(&mut self, max_dimension: u32) {
        let default_key = self.default_key;
        let decoded: Vec<bool> = self
            .images
            .par_values_mut()
            .map(|texture| texture.decode(max_dimension))
            .collect();

        // Both traversals preserve insertion order, so each result still identifies its texture.
        let mut decoded = decoded.into_iter();
        self.images.retain(|path, _| {
            let keep = decoded.next().expect("decode result count must match the texture cache");
            if !keep {
                warn!("Failed to decode terrain texture: {}", path);
            }
            keep
        });
        debug_assert!(decoded.next().is_none(), "decode result count must match the texture cache");
        debug_assert!(
            self.images.contains_key(default_key),
            "default terrain texture must always decode"
        );
    }

    /// Parses a DDS byte slice and extracts its mip chain, starting from the best source mip for
    /// the selected terrain tile-size limit.
    ///
    /// `bytes_hash` in the returned texture is `[0; 32]`; the caller (`TerrainTexture::decode`)
    /// leaves the real hash untouched. It was already set from the raw on-disk bytes.
    fn load_dds_mips(bytes: &[u8], max_dimension: u32) -> Option<TerrainTexture> {
        if !bytes.starts_with(b"DDS ") {
            return None;
        }

        let dds = decode_dds(bytes).ok()?;
        let mip_count = dds.get_num_mipmap_levels().max(1);
        let plan = plan_dds_mip_load(dds.get_width(), dds.get_height(), mip_count, max_dimension);
        let base = image_dds::image_from_dds(&dds, plan.start_level).ok()?;

        if plan.needs_resize() {
            return Some(TerrainTexture::from_base(resize_rgba_to_max_dimension(base, max_dimension)));
        }

        let bc1_extract = Bc1CompatibleSourceExtractor::new(&dds);
        let mut levels = SmallVec::<[RgbaImage; 9]>::new();
        let mut bc1_mips = SmallVec::<[Option<Bc1Mip>; 9]>::new();
        levels.push(base);
        bc1_mips.push(bc1_extract.as_ref().and_then(|e| e.extract(plan.start_level)));
        for mip in (plan.start_level + 1)..mip_count {
            let img = image_dds::image_from_dds(&dds, mip).ok()?;
            bc1_mips.push(bc1_extract.as_ref().and_then(|e| e.extract(mip)));
            levels.push(img);
        }

        if levels.last().is_some_and(|img| img.width() > 1 || img.height() > 1) {
            let popped = levels.pop().unwrap();
            bc1_mips.pop();
            let generated = generate_mip_chain(popped);
            for _ in 0..generated.len() {
                bc1_mips.push(None);
            }
            levels.extend(generated);
        }

        Some(TerrainTexture {
            levels,
            bc1_mips,
            bytes_hash: [0; 32],
            pending: None,
        })
    }

    /// Returns a texture by key, falling back to the default texture when it is missing.
    pub fn get(&self, key: &str) -> &TerrainTexture {
        self.images.get(key).unwrap_or_else(|| &self.images[self.default_key])
    }

    /// Returns the number of successfully loaded texture entries.
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Returns the default (fallback) terrain texture.
    pub(crate) fn default_texture(&self) -> &TerrainTexture {
        &self.images[self.default_key]
    }
}

/// Extracts BC1-compatible color blocks for individual mips from supported DDS formats.
struct Bc1CompatibleSourceExtractor<'a> {
    surface: image_dds::Surface<&'a [u8]>,
    source_block_bytes: usize,
    color_offset: usize,
    requires_opaque_bc1_check: bool,
}

impl<'a> Bc1CompatibleSourceExtractor<'a> {
    fn new(dds: &'a image_dds::ddsfile::Dds) -> Option<Self> {
        let surface = image_dds::Surface::from_dds(dds).ok()?;
        let (source_block_bytes, color_offset, requires_opaque_bc1_check) = match surface.image_format {
            image_dds::ImageFormat::BC1RgbaUnorm | image_dds::ImageFormat::BC1RgbaUnormSrgb => (8, 0, false),
            image_dds::ImageFormat::BC2RgbaUnorm
            | image_dds::ImageFormat::BC2RgbaUnormSrgb
            | image_dds::ImageFormat::BC3RgbaUnorm
            | image_dds::ImageFormat::BC3RgbaUnormSrgb => (16, 8, true),
            _ => return None,
        };
        Some(Self {
            surface,
            source_block_bytes,
            color_offset,
            requires_opaque_bc1_check,
        })
    }

    fn extract(&self, mip: u32) -> Option<Bc1Mip> {
        let width = (self.surface.width >> mip).max(1);
        let height = (self.surface.height >> mip).max(1);
        if !width.is_multiple_of(4) || !height.is_multiple_of(4) {
            return None;
        }
        let bytes = self.surface.get(0, 0, mip)?;
        let block_count = (width / 4) as usize * (height / 4) as usize;
        let expected = block_count * self.source_block_bytes;
        if bytes.len() < expected {
            return None;
        }
        let blocks = if self.source_block_bytes == 8 && self.color_offset == 0 {
            bytes[..block_count * 8].to_vec()
        } else {
            let mut blocks = Vec::with_capacity(block_count * 8);
            for block in bytes[..expected].chunks_exact(self.source_block_bytes) {
                let color = &block[self.color_offset..self.color_offset + 8];
                if self.requires_opaque_bc1_check && !bc1_block_uses_opaque_mode(color) {
                    return None;
                }
                blocks.extend_from_slice(color);
            }
            blocks
        };
        Some(Bc1Mip { width, height, blocks })
    }
}

fn bc1_block_uses_opaque_mode(block: &[u8]) -> bool {
    let color0 = u16::from_le_bytes(block[0..2].try_into().expect("BC1 color block has color0"));
    let color1 = u16::from_le_bytes(block[2..4].try_into().expect("BC1 color block has color1"));
    color0 > color1
}

#[cfg(test)]
mod tests;
