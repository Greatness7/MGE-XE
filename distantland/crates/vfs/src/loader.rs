//! One-time construction of a `Vfs`: INI discovery, data-directory resolution,
//! plugin/archive resolution, BSA loading, and asset-map building.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use bstr::ByteSlice;
use hashbrown::HashSet;
use tes3::bsa::Archive as Tes3Archive;
use tracing::warn;

use super::config_parsers::*;
use super::directory_map::*;
use super::normalize::{normalize, trim_normalized_prefix};
use super::{LoadedBsa, Vfs, VfsLoadOptions};

impl Vfs {
    /// Load a new VFS instance from disk.
    ///
    /// If no config path was provided it will try to use the default install path.
    ///
    /// # Errors
    ///
    /// Returns an error if the install metadata, load order, or archive metadata cannot
    /// be resolved.
    pub fn load(options: &VfsLoadOptions) -> Result<Self> {
        Self::load_with_asset_maps(options, true)
    }

    /// Load VFS metadata without scanning mesh/texture asset directories.
    ///
    /// This is only suitable for status/fingerprint code that needs resolved
    /// data directories and active plugins.
    ///
    /// # Errors
    ///
    /// Returns an error if the install metadata or load order cannot be resolved.
    pub fn load_metadata_only(options: &VfsLoadOptions) -> Result<Self> {
        Self::load_with_asset_maps(options, false)
    }

    fn load_with_asset_maps(options: &VfsLoadOptions, build_asset_maps: bool) -> Result<Self> {
        let ini_path = match &options.morrowind_ini {
            Some(path) => path.clone(),
            None => find_morrowind_ini()?,
        };

        let explicit_data_dirs = options.data_dirs.as_deref();
        let data_dirs = resolve_data_dirs(&ini_path, explicit_data_dirs)?;

        let active_plugins = match options.plugins.as_deref() {
            Some(plugins) => resolve_selected_plugins(plugins, &data_dirs)?,
            None => match explicit_data_dirs {
                Some(_) => parse_morrowind_game_files_with_data_dirs(&ini_path, &data_dirs)?,
                None => parse_morrowind_game_files(&ini_path)?,
            },
        };

        let archive_paths = match explicit_data_dirs {
            Some(_) => parse_morrowind_archive_files_with_data_dirs(&ini_path, &data_dirs)?,
            None => parse_morrowind_archive_files(&ini_path)?,
        };

        let archives = load_bsa_archives(&archive_paths);

        let maps = if build_asset_maps {
            build_vfs_maps(&data_dirs, &archives)?
        } else {
            DirectoryMaps::default()
        };

        Ok(Vfs {
            ini_path,
            data_dirs,
            active_plugins,
            archives,
            maps,
        })
    }
}

/// Loads BSA archives from a list of paths, skipping any that cannot be opened.
///
/// Failures emit a warning rather than aborting, so a single corrupt or missing
/// archive does not prevent the rest of the archive list from being used.
pub(super) fn load_bsa_archives(paths: &[PathBuf]) -> Vec<LoadedBsa> {
    let mut archives = Vec::with_capacity(paths.len());

    for path in paths {
        let modified = match fs::metadata(path).and_then(|metadata| metadata.modified()) {
            Ok(modified) => modified,
            Err(err) => {
                warn!("Skipping BSA archive {}: {}", path.display(), err);
                continue;
            }
        };

        let archive = match Tes3Archive::from_path(path.as_path()) {
            Ok(archive) => archive,
            Err(err) => {
                warn!("Skipping BSA archive {}: {}", path.display(), err);
                continue;
            }
        };

        archives.push(LoadedBsa { modified, archive });
    }

    archives
}

/// Validates and returns the effective data-directory list.
///
/// When explicit directories are provided they are validated to exist on disk and
/// canonicalized. When `None` is given, the default `Data Files` sibling directory
/// of `ini_path` is derived instead.
///
/// # Errors
///
/// Returns an error if any configured directory does not exist or if the default
/// `Data Files` directory cannot be found beside `ini_path`.
fn resolve_data_dirs(ini_path: &Path, configured: Option<&[PathBuf]>) -> Result<Vec<PathBuf>> {
    match configured {
        Some(data_dirs) => {
            ensure!(!data_dirs.is_empty(), "Configured data_dirs must not be empty");
            data_dirs
                .iter()
                .map(|path| {
                    ensure!(
                        path.is_dir(),
                        "Configured data directory is not a directory: {}",
                        path.display()
                    );
                    Ok(path.canonicalize().unwrap_or_else(|_| path.clone()))
                })
                .collect()
        }
        None => morrowind_data_dirs(ini_path),
    }
}

/// Resolves an explicit plugin list to absolute paths, deduplicating by filename.
///
/// Bare filenames are resolved against `data_dirs` (highest-priority directory wins).
/// Paths that are absolute or contain a parent component are used as-is.
///
/// # Errors
///
/// Returns an error if any plugin has an invalid extension, a duplicate filename,
/// or cannot be found on disk.
pub fn resolve_selected_plugins(plugins: &[PathBuf], data_dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::with_capacity(plugins.len());
    let mut seen_names = HashSet::with_capacity(plugins.len());

    for plugin in plugins {
        ensure!(
            plugin
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("esm") || ext.eq_ignore_ascii_case("esp")),
            "Selected plugin must have .esm or .esp extension: {}",
            plugin.display()
        );

        let file_name = plugin
            .file_name()
            .with_context(|| format!("Selected plugin has no filename: {}", plugin.display()))?;
        let file_name_key = file_name.to_string_lossy().to_ascii_lowercase();
        if let Some(duplicate) = seen_names.replace(file_name_key) {
            bail!("Selected plugins contain duplicate filename: {duplicate}");
        }

        let has_parent = plugin.parent().is_some_and(|parent| !parent.as_os_str().is_empty());
        let resolved_path = if plugin.is_absolute() || has_parent {
            ensure!(plugin.is_file(), "Selected plugin was not found: {}", plugin.display());
            plugin.clone()
        } else {
            resolve_plugin_name(file_name, data_dirs)
                .with_context(|| format!("Selected plugin was not found in data_dirs: {}", plugin.display()))?
        };

        resolved.push(resolved_path);
    }

    Ok(resolved)
}

/// Searches `data_dirs` in reverse order (highest priority first) for a file with `file_name`.
///
/// Reverse iteration mirrors Morrowind's data-layer override semantics: the last directory
/// in the list has the highest priority and shadows earlier entries with the same name.
fn resolve_plugin_name(file_name: &OsStr, data_dirs: &[PathBuf]) -> Option<PathBuf> {
    data_dirs
        .iter()
        .rev()
        .map(|dir| dir.join(file_name))
        .find(|path| path.is_file())
}

/// Builds mesh and texture asset maps by indexing BSA archives then overlaying loose files.
///
/// BSAs are indexed first; loose files with a newer modification time will displace them
/// during `overlay_loose_files`, matching Morrowind's runtime asset-resolution precedence.
///
/// # Errors
///
/// Returns an error if any data directory cannot be enumerated.
pub(super) fn build_vfs_maps(data_dirs: &[PathBuf], archives: &[LoadedBsa]) -> Result<DirectoryMaps> {
    let mut maps = DirectoryMaps::default();

    index_bsa_archives(&mut maps, archives);
    let archive_mtimes: Vec<_> = archives.iter().map(|archive| archive.modified).collect();
    overlay_loose_files(&mut maps, data_dirs, &archive_mtimes)?;
    maps.insert_embedded_error_texture();

    Ok(maps)
}

/// Inserts every `Meshes\` and `Textures\` entry from each BSA into the asset maps.
///
/// The optional `data files\` prefix is stripped from entry names before insertion so
/// that all map keys are rooted directly at the asset-type directory (e.g. `foo\bar.nif`).
fn index_bsa_archives(maps: &mut DirectoryMaps, archives: &[LoadedBsa]) {
    for (archive_index, loaded) in archives.iter().enumerate() {
        // Nameless archives are loadable by the engine, which can look entries up by hash,
        // but this VFS resolves everything by name and so has nothing to index from one.
        if !loaded.archive.has_names() {
            warn!("BSA archive {archive_index} stores no file names; skipping asset indexing");
            continue;
        }

        for entry in loaded.archive.entries() {
            let Some(entry_name) = entry.name() else { continue };
            let entry_name = entry_name.as_bytes();
            let entry_name_lossy = String::from_utf8_lossy(entry_name);
            let normalized = normalize(&entry_name_lossy);
            let normalized = trim_normalized_prefix(&normalized, "data files\\");

            let (asset_dir, asset_key) = if let Some(asset_key) = normalized.strip_prefix("meshes\\") {
                ("meshes", asset_key)
            } else if let Some(asset_key) = normalized.strip_prefix("textures\\") {
                ("textures", asset_key)
            } else {
                continue;
            };

            let source = AssetSource::Bsa {
                archive_index,
                entry_name: entry_name.to_vec(),
            };

            match asset_dir {
                "meshes" => {
                    maps.meshes.insert_normalized(asset_key.to_owned(), source);
                }
                "textures" => {
                    maps.textures.insert_normalized(asset_key.to_owned(), source);
                }
                _ => unreachable!(),
            }
        }
    }
}

/// Attempts to locate `Morrowind.ini` from the Windows registry.
///
/// # Errors
///
/// Returns an error if the Morrowind install path cannot be read from the registry.
pub fn find_morrowind_ini() -> io::Result<PathBuf> {
    use tracing::debug;
    use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};
    const SUBKEY: &str = "SOFTWARE\\WOW6432Node\\Bethesda Softworks\\Morrowind";

    let installed_path: String = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(SUBKEY)?
        .get_value("Installed Path")?;

    debug!("got installed_path: {installed_path}");

    Ok(PathBuf::from_iter([installed_path.as_str(), "Morrowind.ini"]))
}
