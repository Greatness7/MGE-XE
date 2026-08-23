//! Virtual filesystem resolution for Morrowind data directories, archives, and assets.

use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tes3::bsa::Archive as Tes3Archive;

use distantland_foundation::identity::MeshResolutionRule;

mod config_parsers;
pub use config_parsers::{morrowind_data_dirs, parse_morrowind_game_files, parse_morrowind_game_files_with_data_dirs};

mod loader;
pub use loader::find_morrowind_ini;
pub use loader::resolve_selected_plugins;

#[cfg(test)]
use loader::{build_vfs_maps, load_bsa_archives};

pub mod directory_map;
use directory_map::*;

/// Path normalization helpers for mesh and texture keys.
pub mod normalize;
pub use normalize::*;

mod texture;
pub use texture::{STATIC_ERROR_TEXTURE_DDS, STATIC_ERROR_TEXTURE_KEY, TextureSym};

/// A "virtual file system" that abstracts over the game engine's file system behaviors.
///
/// Load an instance with [`Vfs::load`] and retain it for as long as its resolved
/// assets are in use.
pub struct Vfs {
    /// Absolute path to the `Morrowind.ini` that was used to initialize this instance.
    pub ini_path: PathBuf,
    /// Resolved, canonicalized data directories in priority order (index 0 = lowest priority).
    pub data_dirs: Vec<PathBuf>,
    /// Absolute paths to active plugins in load order.
    pub active_plugins: Vec<PathBuf>,
    /// BSA archives in the same order as the INI `[Archives]` list.
    pub archives: Vec<LoadedBsa>,
    /// Pre-built mesh and texture asset maps used for all path lookups.
    pub maps: DirectoryMaps,
}

/// Optional overrides used while loading a `Vfs`.
#[derive(Clone, Debug, Default)]
pub struct VfsLoadOptions {
    /// Explicit `Morrowind.ini` path. When omitted, the default install path is discovered.
    pub morrowind_ini: Option<PathBuf>,
    /// Explicit data-directory layers. Later directories override earlier ones.
    pub data_dirs: Option<Vec<PathBuf>>,
    /// Explicit plugin load order. Bare filenames resolve against `data_dirs`.
    pub plugins: Option<Vec<PathBuf>>,
}

/// Loaded BSA archive plus the modification time used for loose-file precedence checks.
pub struct LoadedBsa {
    /// Archive file modification time; a loose file must be strictly newer to override this entry.
    modified: SystemTime,
    /// Memory-mapped BSA data. The `'static` lifetime is upheld by keeping the backing
    /// file handle alive inside the archive value itself.
    archive: Tes3Archive<'static>,
}

/// Resolved asset lookup result pairing the normalized VFS key with its backing source.
pub struct ResolvedAsset<'a> {
    /// Normalized key stored in the asset map.
    pub key: &'a str,
    /// Loose-file or archive location that currently supplies the asset.
    pub source: &'a AssetSource,
    /// MGE-XE override rule for mesh resolutions; absent for other asset kinds.
    pub mesh_resolution_rule: Option<MeshResolutionRule>,
}

impl Vfs {
    #[inline]
    pub fn ini_path(&self) -> &Path {
        &self.ini_path
    }

    /// Returns the canonical data directories in priority order.
    #[inline]
    pub fn data_dirs(&self) -> &[PathBuf] {
        &self.data_dirs
    }

    /// Returns the highest-priority data directory.
    #[inline]
    pub fn data_dir(&self) -> &Path {
        self.data_dirs.first().expect("no data directories")
    }

    /// Returns the active plugins as absolute paths in load order.
    #[inline]
    pub fn active_plugins(&self) -> &[PathBuf] {
        &self.active_plugins
    }

    /// Resolve a mesh path using the VFS, applying override conventions in priority order:
    ///
    /// 1. `{stem}_dist.nif`            -> always wins if present
    /// 2. `x{name}.nif` + `x{name}.kf` -> requires both files to exist
    /// 3. Original path                -> fallback if no overrides apply
    ///
    pub fn resolve_mesh_path<'a>(&'a self, path: &str) -> Option<&'a Path> {
        self.resolve_mesh(path)?.source.path()
    }

    /// Resolves the normalized base mesh key for a model path without applying override selection.
    pub fn resolve_model_mesh_key<'a>(&'a self, path: &str) -> Option<&'a str> {
        self.resolve_model_mesh_key_value(path).map(|(key, _)| key)
    }

    /// Looks up the base mesh entry for a model path, returning the interned key alongside its
    /// source so callers that need both spend only one hash probe.
    fn resolve_model_mesh_key_value<'a>(&'a self, path: &str) -> Option<(&'a str, &'a AssetSource)> {
        let path = normalize_mesh_override_key(path);
        self.maps.meshes.get_key_value(NormalizedStr::from_normalized(&path))
    }

    /// Resolves a mesh path after applying MGE-XE override-selection rules.
    pub fn resolve_mesh<'a>(&'a self, path: &str) -> Option<ResolvedAsset<'a>> {
        let (key, source, resolution_rule) = self.resolve_mesh_key_value(path)?;
        Some(ResolvedAsset {
            key,
            source,
            mesh_resolution_rule: Some(resolution_rule),
        })
    }

    fn resolve_mesh_key_value<'a>(&'a self, path: &str) -> Option<(&'a str, &'a AssetSource, MeshResolutionRule)> {
        // The base mesh must exist; its source doubles as the fallback when no override applies.
        let (base_key, base_source) = self.resolve_model_mesh_key_value(path)?;

        // Extract stem and parent directory for override checks.
        let (parent, separator, file_name) = match base_key.rsplit_once('\\') {
            Some((parent, file_name)) => (parent, "\\", file_name),
            None => ("", "", base_key),
        };

        // Override conventions are defined only for `.nif` names, so a base mesh with any other
        // extension has no applicable override and resolves as itself. Returning `None` here would
        // report absence for an asset the map does hold, which callers log as "not found in VFS".
        let Some(stem) = file_name.strip_suffix(".nif") else {
            return Some((base_key, base_source, MeshResolutionRule::Original));
        };

        // Check `{stem}_dist.nif`, it always wins when present.
        if let Some((key, source)) = self
            .maps
            .meshes
            .get_key_value_parts_normalized(&[parent, separator, stem, "_dist.nif"])
        {
            return Some((key, source, MeshResolutionRule::Dist));
        }

        // Check `x{stem}.nif` and `x{stem}.kf`, both must exist.
        if let Some((key, source)) = self
            .maps
            .meshes
            .get_key_value_parts_normalized(&[parent, separator, "x", stem, ".nif"])
            && self
                .maps
                .meshes
                .contains_key_parts_normalized(&[parent, separator, "x", stem, ".kf"])
        {
            return Some((key, source, MeshResolutionRule::XWithKf));
        }

        // Fallback: original path.
        Some((base_key, base_source, MeshResolutionRule::Original))
    }

    /// Resolve a texture path using the VFS.
    ///
    pub fn resolve_texture<'a>(&'a self, path: &str) -> Option<ResolvedAsset<'a>> {
        let path = normalize_texture_key(path)?;
        let path_str = path.as_ref();

        let (key, source) = if path_str.ends_with(".dds") {
            None
        } else {
            // For non-DDS paths, first try to find a same-name `.dds` counterpart.
            // DDS takes priority to match the game engine's runtime texture loading behaviour.
            let replace_start = path_str.len().checked_sub(3)?;
            let (stem_with_dot, _) = path_str.split_at(replace_start);
            self.maps.textures.get_key_value_parts_normalized(&[stem_with_dot, "dds"])
        }
        .or_else(|| self.maps.textures.get_key_value(NormalizedStr::from_normalized(path_str)))?;

        Some(ResolvedAsset {
            key,
            source,
            mesh_resolution_rule: None,
        })
    }

    /// Resolves the normalized texture key used by the VFS asset maps.
    pub fn resolve_texture_key<'a>(&'a self, path: &str) -> Option<&'a str> {
        Some(self.resolve_texture(path)?.key)
    }

    /// Resolves a static-pipeline texture key, remapping misses to the embedded error texture.
    pub fn resolve_static_texture_key_or_error<'a>(&'a self, path: &str) -> &'a str {
        let key = self.resolve_texture_key(path).unwrap_or(STATIC_ERROR_TEXTURE_KEY);
        debug_assert!(
            self.maps.textures.get(NormalizedStr::from_normalized(key)).is_some(),
            "static error texture must be present in the VFS texture map"
        );
        key
    }

    /// Resolves a static-pipeline texture symbol, remapping misses to the embedded error texture.
    pub fn resolve_static_texture_sym_or_error(&self, path: &str) -> TextureSym {
        let key = self.resolve_static_texture_key_or_error(path);
        let index = self
            .maps
            .textures
            .get_index_of(NormalizedStr::from_normalized(key))
            .expect("static texture key must be present in the VFS texture map");
        TextureSym::from_index_for_vfs(index, self).expect("texture map index exceeds TextureSym range")
    }

    /// Returns the normalized texture key for a symbol produced by this VFS.
    pub fn texture_key_for_sym(&self, sym: TextureSym) -> Option<&str> {
        let index = sym.index_for_vfs(self)?;
        self.maps.textures.get_index(index).map(|(key, _)| key)
    }

    /// Resolves a texture to a loose-file path when one exists.
    ///
    /// BSA-backed textures have no filesystem path and therefore return `None`.
    pub fn resolve_texture_path<'a>(&'a self, path: &str) -> Option<&'a Path> {
        self.resolve_texture(path)?.source.path()
    }

    /// Reads bytes from a resolved loose file or BSA entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying loose file or archive entry cannot be read.
    pub fn read_asset_bytes<'a>(&'a self, asset: &ResolvedAsset<'a>) -> io::Result<Cow<'a, [u8]>> {
        match asset.source {
            AssetSource::Loose { path } => fs::read(path).map(Cow::Owned),
            AssetSource::Embedded { bytes } => Ok(Cow::Borrowed(bytes)),
            AssetSource::Bsa {
                archive_index,
                entry_name,
            } => {
                let archive = self
                    .archives
                    .get(*archive_index)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "BSA archive index not found"))?;
                let entry = archive
                    .archive
                    .get(entry_name)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "BSA entry not found"))?;
                Ok(Cow::Borrowed(entry.as_bytes()))
            }
        }
    }
}

#[cfg(test)]
mod tests;
