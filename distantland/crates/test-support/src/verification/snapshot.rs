//! Static-output canonicalization retained for atlas incrementality tests.

mod statics;

use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value};

use crate::mge_xe::usage_data::{DataSource, DynamicVisGroup, UsageData, UsageDataReference};
use crate::verification::compare::compare_snapshots;
use crate::verification::current::{DecodedDdsMip, DecodedStaticOutput, load_static_output};
use crate::verification::logical::{byte_string_value, canonical_multiset, escaped_name_key, f32_bits, value_digest};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct StaticOutputSnapshot {
    pub(super) logical: Value,
}

impl StaticOutputSnapshot {
    pub(super) fn digest(&self) -> String {
        value_digest(&self.logical)
    }
}

/// Asserts that two output roots have equivalent decoded static meshes, atlas materials, and
/// ordinal-resolved usage data.
#[track_caller]
pub fn assert_static_outputs_equivalent(incremental: &Path, clean: &Path) {
    let left = snapshot_output_root(incremental).unwrap_or_else(|error| {
        panic!(
            "failed to decode incremental static output {}: {error:#}",
            incremental.display()
        )
    });
    let right = snapshot_output_root(clean)
        .unwrap_or_else(|error| panic!("failed to decode clean static output {}: {error:#}", clean.display()));
    let comparison = compare_snapshots(&left, &right, 16);
    assert!(
        comparison.equal,
        "static outputs differ\nleft digest: {}\nright digest: {}\nmismatches: {:#?}\nomitted mismatches: {}",
        comparison.left_digest, comparison.right_digest, comparison.mismatches, comparison.omitted_mismatches
    );
}

fn snapshot_output_root(output_root: &Path) -> Result<StaticOutputSnapshot> {
    let decoded = load_static_output(output_root)?;
    snapshot_static_output(&decoded)
}

fn snapshot_static_output(decoded: &DecodedStaticOutput) -> Result<StaticOutputSnapshot> {
    let statics = statics::build_statics(decoded)?;
    let usage = build_usage(&decoded.usage_data, &statics.logical_ids)?;

    let mut logical = Map::new();
    logical.insert("materials".to_owned(), statics.materials);
    logical.insert("records".to_owned(), statics.records);
    logical.insert("usage".to_owned(), usage);
    Ok(StaticOutputSnapshot {
        logical: Value::Object(logical),
    })
}

pub(super) fn crop_rgba_digest(mip: &DecodedDdsMip, x: u32, y: u32, width: u32, height: u32) -> Result<String> {
    Ok(crop_rgba_hash(mip, x, y, width, height)?.to_hex().to_string())
}

fn crop_rgba_hash(mip: &DecodedDdsMip, x: u32, y: u32, width: u32, height: u32) -> Result<blake3::Hash> {
    let x_end = x.checked_add(width).context("texel crop width overflows u32")?;
    let y_end = y.checked_add(height).context("texel crop height overflows u32")?;
    ensure!(
        x_end <= mip.width && y_end <= mip.height,
        "texel crop {x},{y} {width}x{height} exceeds mip level {} dimensions {}x{}",
        mip.level,
        mip.width,
        mip.height
    );
    let row_stride = mip.width as usize * 4;
    ensure!(
        mip.rgba.len() == row_stride * mip.height as usize,
        "mip level {} RGBA payload length {} does not match {}x{}",
        mip.level,
        mip.rgba.len(),
        mip.width,
        mip.height
    );
    let mut hasher = blake3::Hasher::new();
    for row in y..y_end {
        let start = row as usize * row_stride + x as usize * 4;
        let end = start + width as usize * 4;
        hasher.update(&mip.rgba[start..end]);
    }
    Ok(hasher.finalize())
}

fn data_source_name(source: DataSource) -> &'static str {
    match source {
        DataSource::Journal => "journal",
        DataSource::Global => "global",
        DataSource::UniqueObject => "unique_object",
    }
}

fn visibility_group_value(group: &DynamicVisGroup) -> Value {
    let unpadded_id = {
        let bytes: &[u8] = group.id.as_ref();
        let end = bytes.iter().rposition(|&byte| byte != 0).map_or(0, |index| index + 1);
        &bytes[..end]
    };
    let ranges = group
        .enabled_ranges
        .iter()
        .map(|&[start, end]| Value::Array(vec![Value::from(start), Value::from(end)]))
        .collect();

    let mut map = Map::new();
    map.insert(
        "data_source".to_owned(),
        Value::String(data_source_name(group.data_source).to_owned()),
    );
    map.insert("id".to_owned(), byte_string_value(unpadded_id));
    map.insert("enabled_ranges".to_owned(), canonical_multiset(ranges));
    Value::Object(map)
}

fn placement_value(
    reference: &UsageDataReference,
    static_ids: &[String],
    group_values: &[Value],
    scope: &str,
) -> Result<Value> {
    ensure!(
        reference._padding == 0,
        "usage.data {scope} reference has nonzero padding {}",
        reference._padding
    );
    let static_id = static_ids.get(reference.id as usize).with_context(|| {
        format!(
            "usage.data {scope} reference id {} is outside the static directory",
            reference.id
        )
    })?;
    let visibility = match reference.vis_index {
        0 => Value::String("always_visible".to_owned()),
        index => group_values
            .get(usize::from(index) - 1)
            .cloned()
            .with_context(|| format!("usage.data {scope} reference vis index {index} is outside the group table"))?,
    };

    let mut map = Map::new();
    map.insert("static".to_owned(), Value::String(static_id.clone()));
    map.insert("visibility".to_owned(), visibility);
    map.insert(
        "location".to_owned(),
        Value::Array(vec![
            f32_bits(reference.location.x),
            f32_bits(reference.location.y),
            f32_bits(reference.location.z),
        ]),
    );
    map.insert(
        "rotation".to_owned(),
        Value::Array(vec![
            f32_bits(reference.rotation.x),
            f32_bits(reference.rotation.y),
            f32_bits(reference.rotation.z),
        ]),
    );
    map.insert("scale".to_owned(), f32_bits(reference.scale));
    Ok(Value::Object(map))
}

fn build_usage(usage: &UsageData, static_ids: &[String]) -> Result<Value> {
    let group_values: Vec<Value> = usage.vis_groups.iter().map(visibility_group_value).collect();
    let exterior = usage
        .exterior
        .iter()
        .map(|reference| placement_value(reference, static_ids, &group_values, "exterior"))
        .collect::<Result<Vec<_>>>()?;

    let mut interiors: std::collections::BTreeMap<String, Vec<Value>> = std::collections::BTreeMap::new();
    for interior in &usage.interiors {
        let name_bytes: &[u8] = interior.name.as_ref();
        let end = name_bytes.iter().rposition(|&byte| byte != 0).map_or(0, |index| index + 1);
        let key = escaped_name_key(&name_bytes[..end]);
        let scope = format!("interior {key}");
        let placements = interior
            .references
            .iter()
            .map(|reference| placement_value(reference, static_ids, &group_values, &scope))
            .collect::<Result<Vec<_>>>()?;
        interiors.entry(key).or_default().extend(placements);
    }

    let mut interiors_object = Map::new();
    for (key, placements) in interiors {
        interiors_object.insert(key, canonical_multiset(placements));
    }

    let mut map = Map::new();
    map.insert("exterior".to_owned(), canonical_multiset(exterior));
    map.insert("interiors".to_owned(), Value::Object(interiors_object));
    map.insert("min_static_size".to_owned(), f32_bits(usage.min_static_size));
    Ok(Value::Object(map))
}
