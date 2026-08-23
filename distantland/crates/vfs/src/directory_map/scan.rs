use std::time::SystemTime;

use super::*;

/// Scans all provided data directories to build global meshes and textures mappings.
///
/// The directories are scanned in order. To support layered overriding (like Morrowind),
/// later folders will override earlier ones. Discards files that do not fall into expected
/// asset directories.
///
/// # Errors
///
/// Returns an error when a data-directory tree cannot be scanned.
pub fn build_directory_map(data_dirs: &[PathBuf]) -> Result<DirectoryMaps> {
    let mut maps = DirectoryMaps::default();
    overlay_loose_files(&mut maps, data_dirs, &[])?;
    maps.insert_embedded_error_texture();

    Ok(maps)
}

/// Overlays loose files from `data_dirs` onto the current asset maps using Morrowind precedence.
///
/// `archive_mtimes` maps each archive index to its modification time so loose-vs-BSA precedence
/// can be resolved against already-indexed BSA entries.
pub(crate) fn overlay_loose_files(
    maps: &mut DirectoryMaps,
    data_dirs: &[PathBuf],
    archive_mtimes: &[SystemTime],
) -> Result<()> {
    scan_loose_candidates(data_dirs, |candidate| {
        candidate.asset_dir.map_mut(maps).insert_loose_normalized(
            candidate.normalized_key,
            candidate.absolute_path,
            candidate.last_write_time,
            archive_mtimes,
        );
    })
}

/// Maximum directory recursion depth for loose asset scanning.
///
/// Guards against infinite loops caused by circular directory symlinks.
const MAX_LOOSE_SCAN_RECURSION_DEPTH: usize = 40;

/// Collects all loose asset candidates from `data_dirs` and delivers them in a deterministic
/// order to `visit`.
///
/// Each data directory is scanned in parallel; results are sorted by
/// `(data_dir_index, asset_dir, key, path)` before delivery so that override precedence
/// is applied consistently regardless of filesystem enumeration order.
fn scan_loose_candidates(data_dirs: &[PathBuf], mut visit: impl FnMut(LooseCandidate)) -> Result<()> {
    let tasks = collect_loose_scan_tasks(data_dirs)?;
    let results: Vec<_> = tasks.into_par_iter().map(scan_asset_root_task).collect::<Result<_>>()?;
    let mut candidates = Vec::new();

    for result in results {
        candidates.extend(result);
    }

    candidates.sort_by(|left, right| {
        left.data_dir_index
            .cmp(&right.data_dir_index)
            .then_with(|| left.asset_dir.cmp(&right.asset_dir))
            .then_with(|| left.normalized_key.cmp(&right.normalized_key))
            .then_with(|| left.absolute_path.cmp(&right.absolute_path))
    });

    for candidate in candidates {
        visit(candidate);
    }

    Ok(())
}

/// Enumerates `Meshes` and `Textures` subdirectories in each data directory and
/// creates one scan task per discovered asset-type root.
fn collect_loose_scan_tasks(data_dirs: &[PathBuf]) -> Result<Vec<LooseScanTask>> {
    let mut tasks = Vec::new();

    for (data_dir_index, dir) in data_dirs.iter().enumerate() {
        let root_entries = fs::read_dir(dir).with_context(|| format!("Failed to read data directory {}", dir.display()))?;
        for root_entry in root_entries {
            let root_entry = root_entry.with_context(|| format!("Failed to enumerate {}", dir.display()))?;
            let Some(asset_dir) = asset_dir_name(root_entry.file_name().as_encoded_bytes()) else {
                continue;
            };
            collect_scan_task(&root_entry, data_dir_index, asset_dir, &mut tasks)?;
        }
    }

    Ok(tasks)
}

/// Registers a scan task for an asset-type root entry, handling both real directories
/// and directory symlinks (symlinks to plain files are ignored at this level).
fn collect_scan_task(
    root_entry: &DirEntry,
    data_dir_index: usize,
    asset_dir: AssetDir,
    tasks: &mut Vec<LooseScanTask>,
) -> Result<()> {
    let file_type = root_entry
        .file_type()
        .with_context(|| format!("Failed to read file type for {}", root_entry.path().display()))?;
    if file_type.is_dir() {
        tasks.push(LooseScanTask {
            data_dir_index,
            asset_dir,
            root_path: root_entry.path(),
        });
        return Ok(());
    }

    if file_type.is_symlink() {
        let path = root_entry.path();
        if let Some(metadata) = symlink_target_metadata(&path)
            && metadata.is_dir()
        {
            tasks.push(LooseScanTask {
                data_dir_index,
                asset_dir,
                root_path: path,
            });
        }
    }

    Ok(())
}

fn scan_asset_root_task(task: LooseScanTask) -> Result<Vec<LooseCandidate>> {
    let mut candidates = Vec::new();
    scan_asset_tree(
        &task.root_path,
        Path::new(""),
        task.data_dir_index,
        task.asset_dir,
        0,
        &mut candidates,
    )?;
    Ok(candidates)
}

/// Recursively walks `current_path`, producing a [`LooseCandidate`] for every file.
///
/// `logical_path` tracks the relative path from the asset-type root and becomes the
/// normalized map key. Directory symlinks are followed; file symlinks are treated as
/// regular files. The recursion depth is bounded by [`MAX_LOOSE_SCAN_RECURSION_DEPTH`]
/// to prevent infinite loops from circular symlinks.
///
/// # Errors
///
/// Returns an error if the recursion depth limit is exceeded or if a directory entry
/// cannot be read.
fn scan_asset_tree(
    current_path: &Path,
    logical_path: &Path,
    data_dir_index: usize,
    asset_dir: AssetDir,
    depth: usize,
    out: &mut Vec<LooseCandidate>,
) -> Result<()> {
    ensure!(
        depth <= MAX_LOOSE_SCAN_RECURSION_DEPTH,
        "Loose asset scan recursion depth exceeded while scanning {}",
        current_path.display()
    );
    let entries =
        fs::read_dir(current_path).with_context(|| format!("Failed to read asset directory {}", current_path.display()))?;

    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to enumerate {}", current_path.display()))?;
        let entry_path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to read file type for {}", entry_path.display()))?;
        let entry_logical_path = logical_path.join(entry.file_name());

        if file_type.is_dir() {
            scan_asset_tree(&entry_path, &entry_logical_path, data_dir_index, asset_dir, depth + 1, out)?;
            continue;
        }

        if file_type.is_file() {
            let metadata = entry_metadata(&entry)?;
            out.push(build_loose_candidate(
                data_dir_index,
                asset_dir,
                entry_logical_path,
                entry_path,
                &metadata,
            )?);
            continue;
        }

        if file_type.is_symlink()
            && let Some(metadata) = symlink_target_metadata(&entry_path)
        {
            if metadata.is_dir() {
                scan_asset_tree(&entry_path, &entry_logical_path, data_dir_index, asset_dir, depth + 1, out)?;
            } else if metadata.is_file() {
                out.push(build_loose_candidate(
                    data_dir_index,
                    asset_dir,
                    entry_logical_path,
                    entry_path,
                    &metadata,
                )?);
            }
        }
    }

    Ok(())
}

/// Reads metadata for a directory entry, providing a path-contextual error on failure.
fn entry_metadata(entry: &DirEntry) -> Result<Metadata> {
    entry
        .metadata()
        .with_context(|| format!("Failed to read metadata for {}", entry.path().display()))
}

/// Resolves a symlinked entry's target metadata, following the link.
///
/// Returns `None` for a broken symlink after logging a warning, so one dangling link
/// cannot abort the whole asset scan. Only the rare symlink branch calls this, so the
/// extra follow-`stat` is acceptable; the common real-file/dir path stays on the
/// syscall-free [`entry_metadata`].
fn symlink_target_metadata(path: &Path) -> Option<Metadata> {
    match fs::metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(err) => {
            warn!("Skipping broken symlink {}: {err}", path.display());
            None
        }
    }
}

/// Constructs a [`LooseCandidate`] from a file's logical and physical paths.
///
/// `logical_path` is normalized to produce the asset map key. Non-UTF-8 path components
/// are lossy-converted and a warning is emitted, but the file is still indexed.
///
/// # Errors
///
/// Returns an error if the file's modification time cannot be read.
fn build_loose_candidate(
    data_dir_index: usize,
    asset_dir: AssetDir,
    logical_path: PathBuf,
    absolute_path: PathBuf,
    metadata: &Metadata,
) -> Result<LooseCandidate> {
    let path_str = logical_path.to_string_lossy();
    if let std::borrow::Cow::Owned(_) = path_str {
        warn!("Non-unicode file path: '{logical_path:?}' -> '{path_str}'");
    }

    let last_write_time = metadata
        .modified()
        .with_context(|| format!("Failed to read last-write time for {}", absolute_path.display()))?;

    Ok(LooseCandidate {
        data_dir_index,
        asset_dir,
        normalized_key: normalize(&path_str).into_owned(),
        absolute_path,
        last_write_time,
    })
}

/// Maps a directory entry name to the corresponding [`AssetDir`] variant (case-insensitive).
///
/// Returns `None` for any name that is neither `meshes` nor `textures`.
fn asset_dir_name(name: &[u8]) -> Option<AssetDir> {
    if name.eq_ignore_ascii_case(b"meshes") {
        Some(AssetDir::Meshes)
    } else if name.eq_ignore_ascii_case(b"textures") {
        Some(AssetDir::Textures)
    } else {
        None
    }
}
