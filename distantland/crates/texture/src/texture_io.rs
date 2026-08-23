//! Shared texture decode and DDS mip-selection helpers.

use anyhow::{Result, bail};
use std::cell::RefCell;
use std::io::Cursor;

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType as ResizeFilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::RgbaImage;
use image_dds::ddsfile::Dds;

thread_local! {
    static RGBA_RESIZER: RefCell<Resizer> = RefCell::new(Resizer::new());
}

/// DDS decode plan for a requested output size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DdsMipLoadPlan {
    /// First mip level to decode from the source DDS.
    pub start_level: u32,
    /// Dimensions of the decoded mip level before any resize fallback.
    pub source_width: u32,
    /// Dimensions of the decoded mip level before any resize fallback.
    pub source_height: u32,
    /// Final requested width after mip selection and optional resize fallback.
    pub target_width: u32,
    /// Final requested height after mip selection and optional resize fallback.
    pub target_height: u32,
}

impl DdsMipLoadPlan {
    /// Returns `true` when the chosen DDS mip level still needs a resize pass.
    pub fn needs_resize(self) -> bool {
        self.source_width != self.target_width || self.source_height != self.target_height
    }
}

/// The texture formats the engine reads, identified by filename extension.
///
/// Morrowind picks a decoder purely from the extension. `NiDevImageConverter::ReadImageFile`
/// walks its readers asking each one's `IsValidExtension` and never sniffs content to choose
/// one. We do the same, for two reasons: TGA has no magic bytes to sniff for in the first place,
/// and a file whose contents disagree with its extension fails here exactly as it would in the
/// engine, rather than being silently decoded as something the game could not render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    /// DirectDraw Surface, decoded through `image_dds` so a mip level can be selected.
    Dds,
    /// A format `image` decodes directly: TGA or BMP.
    Image(image::ImageFormat),
}

/// Selects the decoder for `key` from its filename extension, or `None` when the extension is
/// not one the engine reads.
///
/// `key` must be the *resolved* asset key: the VFS substitutes a `.dds` counterpart for a
/// requested `.tga`/`.bmp` when one exists, so the requested name can name the wrong decoder.
pub fn texture_format_from_key(key: &str) -> Option<TextureFormat> {
    let extension = key.rsplit_once('.')?.1;
    if extension.eq_ignore_ascii_case("dds") {
        Some(TextureFormat::Dds)
    } else if extension.eq_ignore_ascii_case("tga") {
        Some(TextureFormat::Image(image::ImageFormat::Tga))
    } else if extension.eq_ignore_ascii_case("bmp") {
        Some(TextureFormat::Image(image::ImageFormat::Bmp))
    } else {
        None
    }
}

/// Decodes `bytes` into RGBA, limiting the largest dimension to `max_dimension`.
///
/// The decoder comes from `key`'s extension, never from the bytes. See [`TextureFormat`] for
/// why, and for the requirement that `key` be the resolved asset key.
///
/// DDS inputs select the closest mip level that does not undershoot the requested output size,
/// then fall back to a resize pass only when no exact mip match exists.
pub fn decode_texture_rgba(key: &str, bytes: &[u8], max_dimension: u32) -> Result<RgbaImage> {
    match texture_format_from_key(key) {
        Some(TextureFormat::Dds) => decode_dds_rgba(bytes, max_dimension),
        Some(TextureFormat::Image(format)) => {
            let image = image::load_from_memory_with_format(bytes, format)?.into_rgba8();
            Ok(resize_rgba_to_max_dimension(image, max_dimension))
        }
        None => bail!("texture {key} has no extension the engine reads (.dds, .tga, .bmp)"),
    }
}

/// Parses DDS bytes into an owned [`Dds`] object.
pub fn decode_dds(bytes: &[u8]) -> Result<Dds> {
    Ok(Dds::read(Cursor::new(bytes))?)
}

/// Decodes a DDS image to RGBA using the best available mip level for `max_dimension`.
pub fn decode_dds_rgba(bytes: &[u8], max_dimension: u32) -> Result<RgbaImage> {
    let dds = decode_dds(bytes)?;
    let plan = plan_dds_mip_load(dds.get_width(), dds.get_height(), dds.get_num_mipmap_levels(), max_dimension);
    let image = image_dds::image_from_dds(&dds, plan.start_level)?;
    Ok(resize_rgba_to_dimensions(image, plan.target_width, plan.target_height))
}

/// Chooses the best DDS mip level for `max_dimension`.
///
/// The selected mip is always the highest available level whose dimensions are still greater
/// than or equal to the requested output size, so the caller never has to upscale.
///
/// A `mip_count` of 0 is treated the same as 1, so only the base level is used. Legacy DDS headers omit the
/// mip count when there is no chain, so both values mean "nothing below the base to choose from".
pub fn plan_dds_mip_load(width: u32, height: u32, mip_count: u32, max_dimension: u32) -> DdsMipLoadPlan {
    let (target_width, target_height) = limited_dimensions(width, height, max_dimension);
    let mut start_level = 0;

    for candidate in 1..mip_count.max(1) {
        let (candidate_width, candidate_height) = dds_mip_dimensions(width, height, candidate);
        if candidate_width < target_width || candidate_height < target_height {
            break;
        }
        start_level = candidate;
    }

    let (source_width, source_height) = dds_mip_dimensions(width, height, start_level);
    DdsMipLoadPlan {
        start_level,
        source_width,
        source_height,
        target_width,
        target_height,
    }
}

/// Limits `image` to `max_dimension`, returning it unchanged when already within it.
pub fn resize_rgba_to_max_dimension(image: RgbaImage, max_dimension: u32) -> RgbaImage {
    let (target_width, target_height) = limited_dimensions(image.width(), image.height(), max_dimension);
    resize_rgba_to_dimensions(image, target_width, target_height)
}

/// Resizes an RGBA image to explicit dimensions using the project resize policy.
pub(crate) fn resize_rgba_to_dimensions(image: RgbaImage, target_width: u32, target_height: u32) -> RgbaImage {
    if image.width() == target_width && image.height() == target_height {
        return image;
    }

    let source = ImageRef::new(image.width(), image.height(), image.as_raw(), PixelType::U8x4)
        .expect("RGBA image buffer should match its dimensions");
    let mut resized = Image::new(target_width, target_height, PixelType::U8x4);
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(ResizeFilterType::Bilinear))
        .use_alpha(false);

    RGBA_RESIZER.with_borrow_mut(|resizer| {
        resizer
            .resize(&source, &mut resized, &options)
            .expect("RGBA resize should succeed for project-owned buffers and nonzero targets");
    });

    RgbaImage::from_raw(target_width, target_height, resized.into_vec())
        .expect("resized RGBA buffer should match target dimensions")
}

/// Resizes an RGBA image into reusable scratch storage using the project resize policy.
pub fn resize_rgba_to_dimensions_into(image: &RgbaImage, target_width: u32, target_height: u32, out: &mut Vec<u8>) {
    let needed = target_width as usize * target_height as usize * 4;
    out.resize(needed, 0);

    if image.width() == target_width && image.height() == target_height {
        out.copy_from_slice(image.as_raw());
        return;
    }

    let source = ImageRef::new(image.width(), image.height(), image.as_raw(), PixelType::U8x4)
        .expect("RGBA image buffer should match its dimensions");
    let mut resized = Image::from_slice_u8(target_width, target_height, out.as_mut_slice(), PixelType::U8x4)
        .expect("RGBA resize scratch should match target dimensions");
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(ResizeFilterType::Bilinear))
        .use_alpha(false);

    RGBA_RESIZER.with_borrow_mut(|resizer| {
        resizer
            .resize(&source, &mut resized, &options)
            .expect("RGBA resize should succeed for project-owned buffers and nonzero targets");
    });
}

/// Returns `(width, height)` capped so the longest side is at most `max_dimension`, preserving
/// aspect ratio. Never upscales: dimensions already within the cap are returned unchanged.
pub fn limited_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if width <= max_dimension && height <= max_dimension {
        return (width, height);
    }

    let scale = max_dimension as f32 / width.max(height) as f32;
    (
        ((width as f32 * scale).round() as u32).clamp(1, width),
        ((height as f32 * scale).round() as u32).clamp(1, height),
    )
}

fn dds_mip_dimensions(width: u32, height: u32, level: u32) -> (u32, u32) {
    ((width >> level).max(1), (height >> level).max(1))
}

#[cfg(test)]
mod tests;
