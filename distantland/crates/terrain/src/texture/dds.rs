use super::*;

use anyhow::{Context as _, bail};

#[cfg(test)]
use crate::dds::mip_capacity_hint;
use crate::dds::{
    bcn_level_bytes, downsample_2x2_rgba8_parallel, encode_bc1_into, legacy_d3d_header, mipmap_count, next_mip_dimensions,
    write_legacy_dds_header,
};
use crate::package::Bc1MipChain;

// D3DX9 / MGE-XE require legacy DDS headers (no DX10 extended header):
// build the header directly with Header::new_d3d.

/// Writes a pre-encoded BC1 mip chain using a legacy D3D9 DDS header.
///
/// The chain is supplied in image orientation and written without any vertical flip,
/// matching the top-left orientation the terrain shader expects.
///
/// # Errors
///
/// Returns an error if the mip chain is incomplete, has undersized mip payloads,
/// or if file output fails.
#[tracing::instrument(skip_all)]
#[cfg(test)]
pub fn save_bc1_dds_from_chain_unflipped(chain: &Bc1MipChain, path: &Path) -> anyhow::Result<()> {
    use image_dds::ddsfile::D3DFormat;

    if chain.mips.len() != chain.max_lod as usize + 1 {
        bail!("BC1 mip chain length does not match max_lod");
    }

    let span = info_span!(
        "terrain.save_bc1_dds_from_chain",
        report = true,
        path = tracing::field::display(path.display()),
        bytes = tracing::field::Empty
    );
    let _guard = span.enter();
    let mip_count = chain.mips.len() as u32;
    let header =
        legacy_d3d_header(chain.width, chain.height, D3DFormat::DXT1, mip_count).context("failed to create DDS header")?;

    let mut bytes_written = 0_u64;
    {
        let _write_guard = info_span!(
            "terrain.write_bc1_dds_from_chain",
            report = true,
            path = tracing::field::display(path.display()),
            bytes = tracing::field::Empty
        )
        .entered();
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        bytes_written += write_legacy_dds_header(&mut writer, &header)?;
        for (mip, blocks) in chain.mips.iter().enumerate() {
            let mip = mip as u32;
            let width = (chain.width >> mip).max(1);
            let height = (chain.height >> mip).max(1);
            let expected = bcn_level_bytes(width, height, 8);
            if blocks.len() < expected {
                bail!("BC1 mip payload is smaller than its dimensions require");
            }
            writer.write_all(&blocks[..expected])?;
            bytes_written += expected as u64;
        }
        writer.flush()?;
        tracing::Span::current().record("bytes", bytes_written);
    }
    span.record("bytes", bytes_written);
    Ok(())
}

/// Encodes and writes a DXT1 DDS file in top-left image orientation.
///
/// `include_mips = false` writes only the base level; `true` writes the full mip chain down
/// to 1×1.
///
/// # Errors
///
/// Returns an error if DDS encoding or file output fails.
#[tracing::instrument(skip_all)]
#[cfg(test)]
pub fn save_bc1_dds_unflipped(img: RgbaImage, path: &Path, include_mips: bool) -> anyhow::Result<()> {
    use image_dds::ddsfile::D3DFormat;

    let max_lod = include_mips.then_some(u32::MAX);
    let span = info_span!(
        "terrain.save_bc1_dds",
        report = true,
        path = tracing::field::display(path.display()),
        bytes = tracing::field::Empty
    );
    let _guard = span.enter();
    let (header, img) = {
        let encode_span = info_span!("terrain.encode_bc1_dds", report = true, bytes = tracing::field::Empty);
        let _guard = encode_span.enter();
        let mip_count = bc1_mip_count(img.width(), img.height(), max_lod);
        let header = legacy_d3d_header(img.width(), img.height(), D3DFormat::DXT1, mip_count)
            .context("failed to create DDS header")?;

        encode_span.record(
            "bytes",
            bc1_dds_capacity_hint_with_max_lod(img.width(), img.height(), max_lod) as u64,
        );
        (header, img)
    };
    let mut bytes_written = 0_u64;
    {
        let _guard = info_span!(
            "terrain.write_bc1_dds",
            report = true,
            path = tracing::field::display(path.display()),
            bytes = tracing::field::Empty
        )
        .entered();
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        bytes_written += write_legacy_dds_header(&mut writer, &header)?;
        bytes_written += write_bc1_mip_chain(&mut writer, img, max_lod)?;
        writer.flush()?;
        tracing::Span::current().record("bytes", bytes_written);
    }
    span.record("bytes", bytes_written);
    Ok(())
}

/// Encodes and writes an uncompressed `A8B8G8R8` DDS file in top-left image orientation.
///
/// `include_mips = false` writes only the base level; `true` writes the full mip chain down
/// to 1×1.
///
/// # Errors
///
/// Returns an error if DDS encoding or file output fails.
#[tracing::instrument(skip_all)]
#[cfg(test)]
pub fn save_rgba8_dds_unflipped(img: RgbaImage, path: &Path, include_mips: bool) -> anyhow::Result<()> {
    use image_dds::ddsfile::D3DFormat;

    let span = info_span!(
        "terrain.save_rgba8_dds",
        report = true,
        path = tracing::field::display(path.display()),
        bytes = tracing::field::Empty
    );
    let _guard = span.enter();
    let (header, img) = {
        let encode_span = info_span!("terrain.encode_rgba8_dds", report = true, bytes = tracing::field::Empty);
        let _guard = encode_span.enter();
        // Rgba8Unorm stores bytes as [R, G, B, A]; D3DFormat::A8B8G8R8 has R in bits 7:0.
        // The layouts are the same, so no swizzle is needed.
        let mip_count = if include_mips {
            mipmap_count(img.width(), img.height())
        } else {
            1
        };
        let header = legacy_d3d_header(img.width(), img.height(), D3DFormat::A8B8G8R8, mip_count)
            .context("failed to create DDS header")?;

        encode_span.record(
            "bytes",
            rgba8_dds_capacity_hint(img.width(), img.height(), include_mips) as u64,
        );
        (header, img)
    };
    let mut bytes_written = 0_u64;
    {
        let _guard = info_span!(
            "terrain.write_rgba8_dds",
            report = true,
            path = tracing::field::display(path.display()),
            bytes = tracing::field::Empty
        )
        .entered();
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        bytes_written += write_legacy_dds_header(&mut writer, &header)?;
        bytes_written += write_rgba8_mip_chain(&mut writer, img, include_mips)?;
        writer.flush()?;
        tracing::Span::current().record("bytes", bytes_written);
    }
    span.record("bytes", bytes_written);
    Ok(())
}

/// Streams a pre-encoded BC1 mip chain into an already-authorized writer.
pub fn write_bc1_dds_from_chain_unflipped(chain: &Bc1MipChain, writer: &mut impl Write) -> anyhow::Result<u64> {
    use image_dds::ddsfile::D3DFormat;

    if chain.mips.len() != chain.max_lod as usize + 1 {
        bail!("BC1 mip chain length does not match max_lod");
    }
    let header = legacy_d3d_header(chain.width, chain.height, D3DFormat::DXT1, chain.mips.len() as u32)
        .context("failed to create DDS header")?;
    let mut bytes_written = write_legacy_dds_header(writer, &header)?;
    for (mip, blocks) in chain.mips.iter().enumerate() {
        let mip = mip as u32;
        let width = (chain.width >> mip).max(1);
        let height = (chain.height >> mip).max(1);
        let expected = bcn_level_bytes(width, height, 8);
        if blocks.len() < expected {
            bail!("BC1 mip payload is smaller than its dimensions require");
        }
        writer.write_all(&blocks[..expected])?;
        bytes_written += expected as u64;
    }
    Ok(bytes_written)
}

/// Streams a top-left DXT1 DDS image into an already-authorized writer.
pub fn write_bc1_dds_unflipped(img: RgbaImage, writer: &mut impl Write, include_mips: bool) -> anyhow::Result<u64> {
    use image_dds::ddsfile::D3DFormat;

    let max_lod = include_mips.then_some(u32::MAX);
    let header = legacy_d3d_header(
        img.width(),
        img.height(),
        D3DFormat::DXT1,
        bc1_mip_count(img.width(), img.height(), max_lod),
    )
    .context("failed to create DDS header")?;
    Ok(write_legacy_dds_header(writer, &header)? + write_bc1_mip_chain(writer, img, max_lod)?)
}

/// Streams a top-left `A8B8G8R8` DDS image into an already-authorized writer.
pub fn write_rgba8_dds_unflipped(img: RgbaImage, writer: &mut impl Write, include_mips: bool) -> anyhow::Result<u64> {
    use image_dds::ddsfile::D3DFormat;

    let mip_count = if include_mips {
        mipmap_count(img.width(), img.height())
    } else {
        1
    };
    let header = legacy_d3d_header(img.width(), img.height(), D3DFormat::A8B8G8R8, mip_count)
        .context("failed to create DDS header")?;
    Ok(write_legacy_dds_header(writer, &header)? + write_rgba8_mip_chain(writer, img, include_mips)?)
}

/// Encodes and writes a BC1 mip chain to `writer`.
///
/// This deliberately does not reuse `dds::write_bcn_mip_chain`: that one appends into a `Vec`
/// it can encode into directly, while this streams each level out as it is produced, and the two
/// report under separate `report = true` span names so atlas and terrain compress wall stay
/// distinguishable in `generation_report.toml`. The substantive work is shared through `crate::dds`:
/// block-grid padding, parallel BCn encode, and the 2×2 box filter.
fn write_bc1_mip_chain(writer: &mut impl Write, mut img: RgbaImage, max_lod: Option<u32>) -> std::io::Result<u64> {
    // Drive the loop from the same count that sized the header, so the payload cannot
    // disagree with it.
    let mip_count = bc1_mip_count(img.width(), img.height(), max_lod);
    let chain_span = info_span!(
        "terrain.write_diffuse_mip_chain",
        report = true,
        mip_count = mip_count as u64,
        bytes = tracing::field::Empty
    );
    let _chain_guard = chain_span.enter();
    let mut bytes_written = 0_u64;
    let mut block = Vec::new();
    let mut pad = Vec::new();
    for mip_level in 0..mip_count {
        let width = img.width();
        let height = img.height();
        let size = bcn_level_bytes(width, height, 8);
        {
            let compress_span = info_span!(
                "terrain.compress_diffuse_mip_bc1",
                report = true,
                mip_level = mip_level as u64,
                width = width as u64,
                height = height as u64,
                bytes = tracing::field::Empty
            );
            let _compress_guard = compress_span.enter();
            if block.len() < size {
                block.resize(size, 0);
            }
            encode_bc1_into(&img, &mut pad, &mut block[..size]);
            compress_span.record("bytes", size as u64);
        }
        {
            let _write_guard = info_span!(
                "terrain.write_diffuse_mip_data",
                report = true,
                mip_level = mip_level as u64,
                bytes = size as u64
            )
            .entered();
            writer.write_all(&block[..size])?;
        }
        bytes_written += size as u64;

        if mip_level + 1 == mip_count {
            break;
        }

        let (next_width, next_height) = next_mip_dimensions(width, height);
        {
            let _downsample_guard = info_span!(
                "terrain.downsample_diffuse_mip",
                report = true,
                mip_level = mip_level as u64,
                width = width as u64,
                height = height as u64,
                next_width = next_width as u64,
                next_height = next_height as u64,
                filter = "box_2x2"
            )
            .entered();
            img = downsample_2x2_rgba8_parallel(&img);
        }
    }
    chain_span.record("bytes", bytes_written);
    Ok(bytes_written)
}

/// Writes an uncompressed RGBA8 mip chain (for `A8B8G8R8` normal-map DDS files).
fn write_rgba8_mip_chain(writer: &mut impl Write, mut img: RgbaImage, include_mips: bool) -> std::io::Result<u64> {
    let mut bytes_written = 0_u64;
    loop {
        let raw = img.as_raw();
        writer.write_all(raw)?;
        bytes_written += raw.len() as u64;

        if !include_mips || (img.width() == 1 && img.height() == 1) {
            break;
        }

        img = downsample_2x2_rgba8_parallel(&img);
    }
    Ok(bytes_written)
}

#[cfg(test)]
fn bc1_dds_capacity_hint_with_max_lod(width: u32, height: u32, max_lod: Option<u32>) -> usize {
    128 + bc1_payload_size(width, height, max_lod) as usize
}

/// Yields `(width, height)` for every level a BC1 chain from `width`×`height` will contain.
///
/// The chain runs down to 1×1 unless `max_lod` caps it; `None` means base level only.
///
/// This is the single source of truth for how far a chain extends. The header's mip count and
/// the payload written after it are derived from this one walk, because a header that disagrees
/// with the payload produces a malformed DDS file that D3DX9 reads past the end of.
fn bc1_mip_levels(width: u32, height: u32, max_lod: Option<u32>) -> impl Iterator<Item = (u32, u32)> {
    let mut next = Some((0_u32, width.max(1), height.max(1)));
    std::iter::from_fn(move || {
        let (level, width, height) = next?;
        next = if max_lod.is_none_or(|max_lod| level >= max_lod) || (width == 1 && height == 1) {
            None
        } else {
            let (next_width, next_height) = next_mip_dimensions(width, height);
            Some((level + 1, next_width, next_height))
        };
        Some((width, height))
    })
}

/// Returns the number of mip levels a BC1 chain will contain, for the DDS header.
pub(crate) fn bc1_mip_count(width: u32, height: u32, max_lod: Option<u32>) -> u32 {
    bc1_mip_levels(width, height, max_lod).count() as u32
}

/// Returns the total BC1 payload bytes (excluding the 128-byte header) for a chain.
///
/// Used both to size capacity hints and to budget the terrain source atlas against the
/// control-texture limits.
pub(crate) fn bc1_payload_size(width: u32, height: u32, max_lod: Option<u32>) -> u64 {
    bc1_mip_levels(width, height, max_lod)
        .map(|(width, height)| bcn_level_bytes(width, height, 8) as u64)
        .sum()
}

/// Estimates the total byte capacity for an uncompressed RGBA8 DDS file (header + full mip chain).
#[cfg(test)]
pub(crate) fn rgba8_dds_capacity_hint(width: u32, height: u32, include_mips: bool) -> usize {
    if !include_mips {
        return 128 + width as usize * height as usize * 4;
    }
    mip_capacity_hint(width, height, |width, height| width as usize * height as usize * 4)
}
