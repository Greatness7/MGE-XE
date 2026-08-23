//! Source metadata and per-`(texture, domain)` usage aggregation.

use std::io::Read;
use std::path::Path;

use anyhow::{Result, ensure};
use blake3::{Hash, Hasher};
use hashbrown::HashMap;
use itertools::Itertools;
use rayon::prelude::*;
use tracing::info_span;

use super::math::{TriangleFailure, analyze_triangle_density};
use super::{AtlasDomain, TextureAxisCaps, baseline_dims_for};
use crate::mge_xe::distant_statics::StaticType;
use crate::texture_io::{TextureFormat, texture_format_from_key};
use crate::vfs::NormalizedStr;
use crate::vfs::directory_map::AssetSource;
use crate::{AtlasTextureSet, DistantStatic, DistantStatics, IndexSet, Subset, Vfs};

/// Bytes read from the front of a loose file when probing dimensions. The image decoders may also
/// inspect palette or color-map data during construction, so preserve this existing probe budget.
const HEADER_PROBE_LEN: usize = 1 << 16;

/// Source-texture metadata gathered in a single read pass before atlas setup.
#[derive(Clone, Copy, Debug)]
pub struct SourceTextureInfo {
    /// On-disk byte size (folded into the dedupe/atlas-cache fingerprint).
    pub size: u64,
    /// BLAKE3 content hash (folded into the dedupe/atlas-cache fingerprint).
    pub hash: [u8; 32],
    /// Source width in texels, or `0` when dimensions could not be probed.
    pub width: u32,
    /// Source height in texels, or `0` when dimensions could not be probed.
    pub height: u32,
    /// DDS mip levels present, or `1` for non-DDS / unprobed sources.
    pub mip_count: u32,
    /// Whether the source is a DDS file.
    pub is_dds: bool,
}

/// Triangle identity used to break equal density measurements deterministically.
#[derive(Clone, Debug)]
pub(crate) struct UseId {
    pub(crate) static_key: String,
    pub(crate) subset_index: u32,
    pub(crate) triangle_index: u32,
}

/// Deterministic per-`(texture, domain)` accumulator: commutative extrema, boolean fallback flags,
/// and stable-id tie-breaks so the result never depends on Rayon order.
#[derive(Clone, Debug)]
pub struct TextureUsageAggregate {
    /// The use that produced `area_density_min` (stable tie-break).
    pub(crate) limiting_use: Option<UseId>,
    /// Minimum isotropic area density across valid uses. This is the sizing constraint.
    pub(crate) area_density_min: f32,
    /// Count of valid, non-degenerate measured triangles.
    pub(crate) valid_count: u64,
    /// Count of uncertain / rank-0 / invalid triangles. Weighed against `valid_count` rather than
    /// forcing the baseline outright, so one bad triangle cannot pin a whole texture.
    pub(crate) uncertain_count: u64,
    /// Whether the texture's own dimensions are unknown, making any measurement meaningless
    /// (fail-closed). Not set by per-triangle failures. See `uncertain_count`.
    pub(crate) needs_baseline: bool,
}

impl Default for TextureUsageAggregate {
    fn default() -> Self {
        Self {
            limiting_use: None,
            area_density_min: f32::INFINITY,
            valid_count: 0,
            uncertain_count: 0,
            needs_baseline: false,
        }
    }
}

impl TextureUsageAggregate {
    fn record_valid(&mut self, area_density: f32, static_key: &str, subset_index: u32, triangle_index: u32) {
        self.valid_count += 1;

        if better_limiting(
            area_density,
            (static_key, subset_index, triangle_index),
            self.area_density_min,
            self.limiting_use.as_ref(),
        ) {
            self.area_density_min = area_density;
            self.limiting_use = Some(UseId {
                static_key: static_key.to_owned(),
                subset_index,
                triangle_index,
            });
        }
    }

    fn merge_from(&mut self, other: TextureUsageAggregate) {
        self.valid_count += other.valid_count;
        self.uncertain_count += other.uncertain_count;
        self.needs_baseline |= other.needs_baseline;

        if let Some(other_use) = other.limiting_use {
            let candidate = (
                other_use.static_key.as_str(),
                other_use.subset_index,
                other_use.triangle_index,
            );
            if better_limiting(
                other.area_density_min,
                candidate,
                self.area_density_min,
                self.limiting_use.as_ref(),
            ) {
                self.area_density_min = other.area_density_min;
                self.limiting_use = Some(other_use);
            }
        }
    }
}

/// Returns whether `(density, candidate)` should replace the current limiting use: smaller density
/// wins, ties broken on the lexicographically smaller `(static key, subset, triangle)`.
fn better_limiting(density: f32, candidate: (&str, u32, u32), current_density: f32, current_use: Option<&UseId>) -> bool {
    match current_use {
        None => true,
        Some(current) => {
            density < current_density
                || (density == current_density
                    && candidate < (current.static_key.as_str(), current.subset_index, current.triangle_index))
        }
    }
}

/// Reads each atlas-eligible source once for fingerprints and lightweight dimensions metadata.
///
/// This hoisted pass combines the atlas-cache fingerprint read with dimension probing, using
/// headers or lightweight image probes rather than a full pixel decode.
pub fn collect_static_texture_source_info(
    vfs: &Vfs,
    textures: &AtlasTextureSet<IndexSet<String>>,
) -> Result<HashMap<String, SourceTextureInfo>> {
    // A texture may be referenced by both domains; probe each distinct source exactly once.
    let keys = textures
        .opaque
        .iter()
        .chain(&textures.alpha)
        .map(String::as_str)
        .sorted_unstable()
        .dedup()
        .collect_vec();

    let entries: Vec<(String, SourceTextureInfo)> = keys
        .par_iter()
        .map_init(Vec::new, |header, &key| {
            source_info_for_key(vfs, key, header).map(|info| (key.to_owned(), info))
        })
        .filter_map(|entry| entry)
        .collect();

    // Every atlas-eligible key resolves in the VFS texture map (misses already remap to the
    // embedded error texture), so the probe must cover all of them.
    ensure!(
        entries.len() == keys.len(),
        "source texture probe dropped {} of {} keys",
        keys.len() - entries.len(),
        keys.len()
    );
    Ok(entries.into_iter().collect())
}

fn source_info_for_key(vfs: &Vfs, key: &str, header: &mut Vec<u8>) -> Option<SourceTextureInfo> {
    let source = vfs.maps.textures.get(NormalizedStr::from_normalized(key))?;
    let (size, hash, (width, height, mip_count, is_dds)) = match source {
        AssetSource::Loose { path } => {
            let size = std::fs::metadata(path).ok()?.len();
            let hash = Hasher::new().update_mmap(path).ok()?.finalize();
            let dims = match read_header(path, HEADER_PROBE_LEN, header) {
                Some(header) => probe_dimensions(key, header),
                None => (0, 0, 1, false),
            };
            (size, hash, dims)
        }
        AssetSource::Bsa { .. } => {
            let asset = vfs.resolve_texture(key)?;
            let bytes = vfs.read_asset_bytes(&asset).ok()?;
            let hash = Hasher::new().update(&bytes).finalize();
            (bytes.len() as u64, hash, probe_dimensions(key, &bytes))
        }
        AssetSource::Embedded { bytes } => {
            let hash = Hasher::new().update(bytes).finalize();
            (bytes.len() as u64, hash, probe_dimensions(key, bytes))
        }
    };

    Some(SourceTextureInfo {
        size,
        hash: *hash.as_bytes(),
        width,
        height,
        mip_count,
        is_dds,
    })
}

fn read_header<'a>(path: &Path, max: usize, buf: &'a mut Vec<u8>) -> Option<&'a [u8]> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    buf.resize(len.min(max), 0);
    file.read_exact(buf).ok()?;
    Some(buf)
}

/// Returns `(width, height, mip_count, is_dds)`, falling back to zero dimensions when a source
/// cannot be probed (the planner then keeps that texture at baseline).
///
/// The decoder is chosen from `key`'s extension, matching [`decode_texture_rgba`], so that a
/// texture probed here and decoded later agree on what it is. `key` is a VFS map key and is
/// therefore already resolved.
///
/// [`decode_texture_rgba`]: crate::texture_io::decode_texture_rgba
fn probe_dimensions(key: &str, bytes: &[u8]) -> (u32, u32, u32, bool) {
    match texture_format_from_key(key) {
        Some(TextureFormat::Dds) => match probe_dds(bytes) {
            Some((width, height, mip_count)) => (width, height, mip_count, true),
            None => (0, 0, 1, true),
        },
        Some(TextureFormat::Image(format)) => {
            match image::ImageReader::with_format(std::io::Cursor::new(bytes), format).into_dimensions() {
                Ok((width, height)) => (width, height, 1, false),
                Err(_) => (0, 0, 1, false),
            }
        }
        None => (0, 0, 1, false),
    }
}

/// Parses width/height/mip-count from a DDS header without decoding pixels.
fn probe_dds(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    // DDS magic (4) + DDS_HEADER: dwFlags @8, dwHeight @12, dwWidth @16, dwMipMapCount @28.
    if bytes.len() < 32 {
        return None;
    }
    let read_u32 =
        |offset: usize| u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]);
    const DDSD_MIPMAPCOUNT: u32 = 0x0002_0000;
    let flags = read_u32(8);
    let height = read_u32(12);
    let width = read_u32(16);
    let mip_count = if flags & DDSD_MIPMAPCOUNT != 0 {
        read_u32(28).max(1)
    } else {
        1
    };
    Some((width, height, mip_count))
}

/// Rebuilds per-domain `(size, hash)` vectors from the hoisted source-info map.
pub fn fingerprints_from_source_info(
    source_info: &HashMap<String, SourceTextureInfo>,
    textures: &AtlasTextureSet<IndexSet<String>>,
) -> AtlasTextureSet<Vec<(u64, Hash)>> {
    let domain = |set: &IndexSet<String>| -> Vec<(u64, Hash)> {
        set.iter()
            .map(|key| {
                let info = source_info.get(key).expect("source info missing for atlas texture key");
                (info.size, Hash::from(info.hash))
            })
            .collect()
    };
    AtlasTextureSet::new(domain(&textures.opaque), domain(&textures.alpha))
}

/// Aggregates per-`(texture, domain)` usage from the pre-merge per-mesh statics.
///
/// Runs in parallel; per-worker accumulators merge by texture key using commutative extrema, boolean
/// OR, and stable-id tie-breaks, so the result is independent of Rayon order. `caps` selects the
/// baseline dims `(W, H)` the Jacobian is evaluated against.
pub fn analyze_static_texture_usage(
    vfs: &Vfs,
    distant_statics: &DistantStatics,
    source_info: &HashMap<String, SourceTextureInfo>,
    caps: TextureAxisCaps,
) -> AtlasTextureSet<HashMap<String, TextureUsageAggregate>> {
    let _guard = info_span!(
        "statics.sizing_geometry",
        report = true,
        static_count = distant_statics.len() as u64
    )
    .entered();
    distant_statics
        .par_iter()
        .fold(UsageSet::default, |mut acc, (key, ds)| {
            accumulate_static(&mut acc, key, ds, vfs, source_info, caps);
            acc
        })
        .reduce(UsageSet::default, merge_usage_sets)
}

/// Type alias for the two-domain usage accumulator.
type UsageSet = AtlasTextureSet<HashMap<String, TextureUsageAggregate>>;

fn merge_usage_sets(mut left: UsageSet, right: UsageSet) -> UsageSet {
    merge_usage_map(&mut left.opaque, right.opaque);
    merge_usage_map(&mut left.alpha, right.alpha);
    left
}

fn merge_usage_map(into: &mut HashMap<String, TextureUsageAggregate>, from: HashMap<String, TextureUsageAggregate>) {
    for (key, aggregate) in from {
        into.entry(key).or_default().merge_from(aggregate);
    }
}

/// Accumulates every atlas-eligible subset of one static into `acc`.
fn accumulate_static(
    acc: &mut UsageSet,
    static_key: &str,
    ds: &DistantStatic,
    vfs: &Vfs,
    source_info: &HashMap<String, SourceTextureInfo>,
    caps: TextureAxisCaps,
) {
    // Grass is not atlased, so it must not contribute to atlas downscale sizing.
    if ds.static_type == StaticType::StaticGrass {
        return;
    }
    let scale = ds.max_scale;
    for (subset_index, subset) in ds.subsets.iter().enumerate() {
        if subset.has_uv_controller {
            continue;
        }
        let Some(sym) = subset.texture.source_sym() else {
            continue;
        };
        let Some(key) = vfs.texture_key_for_sym(sym) else {
            continue;
        };
        let domain = if subset.has_alpha {
            AtlasDomain::Alpha
        } else {
            AtlasDomain::Opaque
        };
        let map = match domain {
            AtlasDomain::Opaque => &mut acc.opaque,
            AtlasDomain::Alpha => &mut acc.alpha,
        };
        let entry = map.entry_ref(key).or_default();

        // Missing or unprobeable source dimensions: fail closed and keep the baseline.
        let Some((width, height)) = source_info.get(key).and_then(|info| baseline_dims_for(info, caps)) else {
            entry.needs_baseline = true;
            entry.uncertain_count += 1;
            continue;
        };
        accumulate_subset(entry, static_key, subset_index as u32, subset, scale, width, height);
    }
}

fn accumulate_subset(
    entry: &mut TextureUsageAggregate,
    static_key: &str,
    subset_index: u32,
    subset: &Subset,
    scale: f32,
    width: u32,
    height: u32,
) {
    let vertex_count = subset.vertices.len();
    for (triangle_index, &[i0, i1, i2]) in subset.triangles.iter().enumerate() {
        if i0 as usize >= vertex_count || i1 as usize >= vertex_count || i2 as usize >= vertex_count {
            entry.uncertain_count += 1;
            continue;
        }
        let v0 = &subset.vertices[i0 as usize];
        let v1 = &subset.vertices[i1 as usize];
        let v2 = &subset.vertices[i2 as usize];
        match analyze_triangle_density(
            v0.position,
            v1.position,
            v2.position,
            v0.uv,
            v1.uv,
            v2.uv,
            width,
            height,
            scale,
        ) {
            Ok(area_density) => entry.record_valid(area_density, static_key, subset_index, triangle_index as u32),
            // A zero-area world triangle contributes no density evidence either way, so it is
            // neither a valid measurement nor grounds to distrust the ones we did take.
            Err(TriangleFailure::IgnoredDegenerateWorld) => {}
            Err(TriangleFailure::ConstantUv | TriangleFailure::UncertainWorldMapping | TriangleFailure::NonFinite) => {
                entry.uncertain_count += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests;
