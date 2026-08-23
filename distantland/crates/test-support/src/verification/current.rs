//! Decoding needed by the retained static-output equivalence assertion.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use image_dds::ddsfile::{D3DFormat, Dds};

use crate::mge_xe::distant_statics::{PackedDistantStatic, load_static_meshes};
use crate::mge_xe::usage_data::{UsageData, load_usage_data};
use crate::output_index::{OutputValidation, open_output_snapshot};
use crate::statics::atlas::{ALPHA_ATLAS_PREFIX, OPAQUE_ATLAS_PREFIX};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DecodedDdsFormat {
    Bc1,
    Bc3,
}

impl DecodedDdsFormat {
    fn d3d_format(self) -> D3DFormat {
        match self {
            Self::Bc1 => D3DFormat::DXT1,
            Self::Bc3 => D3DFormat::DXT5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DecodedDdsMip {
    pub(super) level: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rgba: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DecodedDds {
    pub(super) format: DecodedDdsFormat,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mips: Vec<DecodedDdsMip>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DecodedStaticOutput {
    pub(super) static_meshes: Vec<PackedDistantStatic>,
    pub(super) usage_data: UsageData,
    pub(super) static_atlas_pages: BTreeMap<String, DecodedDds>,
}

pub(super) fn load_static_output(output_root: &Path) -> Result<DecodedStaticOutput> {
    let snapshot = open_output_snapshot(output_root, Duration::from_secs(30), OutputValidation::Full)
        .with_context(|| format!("failed to select committed output under {}", output_root.display()))?;
    let paths = snapshot.paths();

    let mut static_meshes = Vec::new();
    for shard_path in &paths.static_mesh_shard_paths {
        let mut shard =
            load_static_meshes(shard_path).with_context(|| format!("failed to decode {}", shard_path.display()))?;
        static_meshes.append(&mut shard);
    }
    let usage_data = load_usage_data(&paths.usage_data_path)
        .with_context(|| format!("failed to decode {}", paths.usage_data_path.display()))?;
    ensure!(
        usize::try_from(usage_data.num_meshes).ok() == Some(static_meshes.len()),
        "{} declares {} meshes but the fixed shard set contains {} records",
        paths.usage_data_path.display(),
        usage_data.num_meshes,
        static_meshes.len()
    );
    let static_atlas_pages = if snapshot.atlas_pages().is_empty() {
        load_static_atlas_pages(&paths.atlas_texture_dir)?
    } else {
        load_selected_static_atlas_pages(snapshot.atlas_pages())?
    };

    Ok(DecodedStaticOutput {
        static_meshes,
        usage_data,
        static_atlas_pages,
    })
}

fn load_static_atlas_pages(directory: &Path) -> Result<BTreeMap<String, DecodedDds>> {
    let entries = fs::read_dir(directory)
        .with_context(|| format!("failed to enumerate static atlas directory {}", directory.display()))?;
    let mut pages = BTreeMap::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to enumerate static atlas directory {}", directory.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| anyhow!("static atlas filename is not UTF-8: {}", PathBuf::from(name).display()))?;
        if !name.starts_with(OPAQUE_ATLAS_PREFIX) || !name.ends_with(".dds") {
            continue;
        }
        insert_static_atlas_page(&mut pages, name, &entry.path())?;
    }
    Ok(pages)
}

fn load_selected_static_atlas_pages(paths: &[PathBuf]) -> Result<BTreeMap<String, DecodedDds>> {
    let mut pages = BTreeMap::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("static atlas filename is not UTF-8: {}", path.display()))?
            .to_owned();
        insert_static_atlas_page(&mut pages, name, path)?;
    }
    Ok(pages)
}

fn insert_static_atlas_page(pages: &mut BTreeMap<String, DecodedDds>, name: String, path: &Path) -> Result<()> {
    validate_atlas_page_name(&name)?;
    let format = if name.starts_with(ALPHA_ATLAS_PREFIX) {
        DecodedDdsFormat::Bc3
    } else {
        DecodedDdsFormat::Bc1
    };
    let page = load_dds(path, format)?;
    if pages.insert(name.clone(), page).is_some() {
        bail!("duplicate static atlas filename {name}");
    }
    Ok(())
}

fn validate_atlas_page_name(name: &str) -> Result<()> {
    let (prefix, suffix) = if let Some(suffix) = name.strip_prefix(ALPHA_ATLAS_PREFIX) {
        (ALPHA_ATLAS_PREFIX, suffix)
    } else if let Some(suffix) = name.strip_prefix(OPAQUE_ATLAS_PREFIX) {
        (OPAQUE_ATLAS_PREFIX, suffix)
    } else {
        bail!("invalid static atlas filename {name}");
    };
    let stem = suffix
        .strip_suffix(".dds")
        .ok_or_else(|| anyhow!("invalid static atlas filename {name}"))?;
    if !stem.is_empty() {
        let page = stem
            .strip_prefix('_')
            .ok_or_else(|| anyhow!("invalid static atlas filename {name}"))?;
        ensure!(
            !page.is_empty() && page.bytes().all(|byte| byte.is_ascii_digit()) && page != "0",
            "invalid static atlas filename {name}; expected {prefix}.dds or {prefix}_<positive-page>.dds"
        );
    }
    Ok(())
}

fn load_dds(path: &Path, format: DecodedDdsFormat) -> Result<DecodedDds> {
    let bytes = fs::read(path).with_context(|| format!("failed to read required output {}", path.display()))?;
    let dds = Dds::read(std::io::Cursor::new(bytes))
        .with_context(|| format!("failed to parse DDS header for {}", path.display()))?;
    ensure!(
        dds.header10.is_none(),
        "{} uses a DX10 DDS header; expected legacy D3D9 {:?}",
        path.display(),
        format.d3d_format()
    );
    ensure!(
        dds.get_d3d_format() == Some(format.d3d_format()),
        "{} DDS format mismatch: expected {:?}, found {:?}",
        path.display(),
        format.d3d_format(),
        dds.get_d3d_format()
    );

    let width = dds.get_width();
    let height = dds.get_height();
    ensure!(width != 0 && height != 0, "{} has zero DDS dimensions", path.display());
    let mip_count = dds.get_num_mipmap_levels().max(1);
    let full_mips = u32::BITS - width.max(height).leading_zeros();
    ensure!(
        mip_count == full_mips,
        "{} DDS mip count mismatch: expected {full_mips}, found {mip_count}",
        path.display()
    );

    let image = image_dds::image_from_dds(&dds, 0).with_context(|| format!("failed to decode {} mip 0", path.display()))?;
    ensure!(
        image.dimensions() == (width, height),
        "{} mip 0 decoded to {:?}, expected {width}x{height}",
        path.display(),
        image.dimensions()
    );
    Ok(DecodedDds {
        format,
        width,
        height,
        mips: vec![DecodedDdsMip {
            level: 0,
            width,
            height,
            rgba: image.into_raw(),
        }],
    })
}
