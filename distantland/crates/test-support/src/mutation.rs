//! Typed mutations for hermetic generation fixtures.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use glam::Vec3;
use tes3::esp::{Landscape, Plugin};
use tes3::nif::{NiStream, NiTriShapeData};

use super::fixture::{gradient_image, write_bc1_dds};

/// Rewrites one fixture texture with deterministic BC1 texels.
///
/// # Errors
///
/// Returns an error when `path` escapes the fixture data directory, the texture is missing, or
/// the replacement DDS cannot be encoded or installed.
pub fn rewrite_fixture_texture(data_dir: &Path, path: &str, size: u32, salt: u8) -> Result<()> {
    let path = fixture_path(data_dir, path)?;
    ensure!(path.is_file(), "fixture texture {} does not exist", path.display());
    let temporary = temporary_path(&path);
    write_bc1_dds(&temporary, &gradient_image(size, salt))?;
    replace_from_temporary(&path, &temporary)
}

/// Offsets one vertex in the first triangle-shape data block of a fixture NIF.
///
/// # Errors
///
/// Returns an error when `path` escapes the fixture data directory, the NIF cannot be loaded or
/// serialized, or the requested vertex does not exist.
pub fn edit_fixture_nif_geometry(data_dir: &Path, path: &str, vertex: usize, delta: [f32; 3]) -> Result<()> {
    let path = fixture_path(data_dir, path)?;
    let mut stream = NiStream::from_path(&path).with_context(|| format!("loading {}", path.display()))?;
    let data = stream
        .objects_of_type_mut::<NiTriShapeData>()
        .next()
        .context("fixture NIF contains no NiTriShapeData")?;
    let vertex = data.vertices.get_mut(vertex).context("fixture NIF vertex is out of range")?;
    *vertex += Vec3::from_array(delta);
    data.update_center_radius();
    let bytes = stream.save_bytes().context("serializing mutated fixture NIF")?;
    replace_bytes(&path, &bytes)
}

/// Adds `delta` to one stored LAND height in a fixture plugin.
///
/// # Errors
///
/// Returns an error when `plugin` escapes the fixture data directory, the plugin or LAND record
/// cannot be loaded, the requested vertex is absent, the stored `i8` would overflow, or the
/// updated plugin cannot be installed.
pub fn edit_fixture_land_height(data_dir: &Path, plugin: &str, cell: [i32; 2], vertex: [usize; 2], delta: i8) -> Result<()> {
    let path = fixture_path(data_dir, plugin)?;
    let mut plugin = Plugin::from_path(&path).with_context(|| format!("loading {}", path.display()))?;
    let grid = (cell[0], cell[1]);
    let land = plugin
        .objects_of_type_mut::<Landscape>()
        .find(|land| land.grid == grid)
        .with_context(|| format!("fixture LAND cell {grid:?} was not found"))?;
    let [x, y] = vertex;
    let value = land
        .vertex_heights
        .data
        .get_mut(y)
        .and_then(|row| row.get_mut(x))
        .context("LAND height vertex is out of range")?;
    *value = value
        .checked_add(delta)
        .context("LAND height mutation would overflow its stored i8")?;
    let bytes = plugin
        .save_bytes()
        .with_context(|| format!("serializing {}", path.display()))?;
    replace_bytes(&path, &bytes)
}

fn replace_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = temporary_path(path);
    fs::write(&temporary, bytes).with_context(|| format!("writing temporary {}", temporary.display()))?;
    replace_from_temporary(path, &temporary)
}

fn replace_from_temporary(path: &Path, temporary: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("removing old {}", path.display()))?;
    }
    fs::rename(temporary, path).with_context(|| format!("replacing {}", path.display()))
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".fixture-new");
    PathBuf::from(name)
}

fn fixture_path(data_dir: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    ensure!(!relative.is_absolute(), "fixture path must be relative: {relative:?}");
    ensure!(
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "fixture path must not escape the fixture data directory: {relative:?}"
    );
    Ok(data_dir.join(relative))
}
