//! Parsers for `Morrowind.ini` game-file, archive, and data-directory entries.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Result, bail};
use atoi::atoi;
use bstr::{ByteSlice, io::BufReadExt};
use hashbrown::HashMap;
use str_utils::*;

/// Parse the game files section of a "Morrowind.ini" file.
///
/// # Errors
///
/// Returns any I/O error encountered while reading the INI file.
pub fn parse_morrowind_game_files(ini_path: &Path) -> io::Result<Vec<PathBuf>> {
    let data_files = ini_path.with_file_name("Data Files");
    parse_morrowind_game_files_with_data_dirs(ini_path, &[data_files])
}

/// Parse the game files section of a "Morrowind.ini" file using an explicit data-dir search path.
///
/// # Errors
///
/// Returns any I/O error encountered while reading the INI file.
pub fn parse_morrowind_game_files_with_data_dirs(ini_path: &Path, data_dirs: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let file = File::open(ini_path)?;

    let mut reader = BufReader::with_capacity(2048, file);

    reader.for_byte_line(|line| {
        if let Some(true) = is_game_files_section_header(line) {
            Ok(false)
        } else {
            Ok(true)
        }
    })?;

    let mut game_files = HashMap::<usize, PathBuf>::with_capacity(4);

    reader.for_byte_line(|line| {
        let line = line.trim_start();

        if line.starts_with(b"[") {
            return Ok(false);
        }

        if !line.starts_with_ignore_ascii_case_with_lowercase(b"gamefile") {
            return Ok(true);
        }

        let Some((key, value)) = line[8..].split_once_str(b"=") else {
            return Ok(true);
        };

        let Some(index) = atoi(key.trim_end()) else {
            return Ok(true);
        };

        let Ok(value) = value.trim().to_str() else {
            return Ok(true);
        };

        if value
            .ends_with_ignore_ascii_case_with_lowercase_multiple(&[".esp", ".esm"])
            .is_none()
        {
            return Ok(true);
        }

        // TODO: Does the game bail if not accessible, or does that later?
        // TODO: Avoid allocations using `set_filename` here?
        let Some(path) = resolve_existing_path_in_data_dirs(value, data_dirs) else {
            return Ok(true);
        };

        game_files.insert(index, path);

        Ok(true)
    })?;

    // Morrowind stops at the first gap in the numbered GameFile list.
    let mut game_files: Vec<_> = (0..) //
        .map_while(move |i| game_files.remove(&i))
        .collect();

    game_files.sort_by_cached_key(load_order_sorter);

    Ok(game_files)
}

/// Parse the archives section of a "Morrowind.ini" file.
///
/// # Errors
///
/// Returns any I/O error encountered while reading the INI file.
pub fn parse_morrowind_archive_files(ini_path: &Path) -> io::Result<Vec<PathBuf>> {
    let data_files = ini_path.with_file_name("Data Files");
    parse_morrowind_archive_files_with_data_dirs(ini_path, &[data_files])
}

/// Parse the archives section of a "Morrowind.ini" file using an explicit data-dir search path.
///
/// `Morrowind.bsa` is always prepended as the base archive regardless of what the INI
/// lists, matching the game engine's hardcoded loading behaviour. The INI's `Archive N`
/// entries follow in their contiguous index order, stopping at the first missing index.
///
/// # Errors
///
/// Returns any I/O error encountered while reading the INI file.
pub fn parse_morrowind_archive_files_with_data_dirs(ini_path: &Path, data_dirs: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let file = File::open(ini_path)?;

    let mut reader = BufReader::with_capacity(2048, file);

    reader.for_byte_line(|line| {
        if let Some(true) = is_archives_section_header(line) {
            Ok(false)
        } else {
            Ok(true)
        }
    })?;

    let mut archive_files = HashMap::<usize, Option<PathBuf>>::with_capacity(4);

    reader.for_byte_line(|line| {
        let line = line.trim_start();

        if line.starts_with(b"[") {
            return Ok(false);
        }

        if !line.starts_with_ignore_ascii_case_with_lowercase(b"archive") {
            return Ok(true);
        }

        let Some((key, value)) = line[7..].split_once_str(b"=") else {
            return Ok(true);
        };

        let Some(index) = atoi(key.trim()) else {
            return Ok(true);
        };

        let Ok(value) = value.trim().to_str() else {
            archive_files.insert(index, None);
            return Ok(true);
        };

        if !value.ends_with_ignore_ascii_case_with_lowercase(".bsa") {
            archive_files.insert(index, None);
            return Ok(true);
        }

        archive_files.insert(index, Some(resolve_archive_path(value, data_dirs)));

        Ok(true)
    })?;

    let mut archives = vec![resolve_archive_path("Morrowind.bsa", data_dirs)];
    archives.extend((0..).map_while(move |i| archive_files.remove(&i)).flatten());

    Ok(archives)
}

/// Finds a named file by joining it against each data directory, highest-priority last.
///
/// Returns the path from the highest-priority directory that contains the file,
/// or `None` if the file does not exist in any of the directories.
fn resolve_existing_path_in_data_dirs(value: &str, data_dirs: &[PathBuf]) -> Option<PathBuf> {
    data_dirs.iter().rev().map(|dir| dir.join(value)).find(|path| path.is_file())
}

/// Resolves a BSA filename to an absolute path, falling back to the last data directory.
///
/// Unlike plugin resolution, missing archives are not rejected here. A non-existent
/// path is returned so that the caller can attempt to open it and emit a proper error.
fn resolve_archive_path(value: &str, data_dirs: &[PathBuf]) -> PathBuf {
    resolve_existing_path_in_data_dirs(value, data_dirs)
        .unwrap_or_else(|| data_dirs.last().expect("data_dirs must not be empty").join(value))
}

/// Sort key for plugin load order: masters (`.esm`) before plugins (`.esp`), then by
/// file modification time within each group, matching the game engine's ordering rules.
fn load_order_sorter(path: &PathBuf) -> (bool, SystemTime) {
    let time = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or_else(|_err| SystemTime::now());

    let master = path
        .as_os_str()
        .as_encoded_bytes()
        .ends_with_ignore_ascii_case_with_lowercase(b".esm");

    (!master, time)
}

/// Returns `Some(true)` when `line` is the `[Game Files]` INI section header.
fn is_game_files_section_header(line: &[u8]) -> Option<bool> {
    line.trim_start()
        .strip_prefix(b"[")?
        .split_once_str(b"]")?
        .0
        .trim()
        .eq_ignore_ascii_case(b"game files")
        .then_some(true)
}

/// Returns `Some(true)` when `line` is the `[Archives]` INI section header.
fn is_archives_section_header(line: &[u8]) -> Option<bool> {
    line.trim_start()
        .strip_prefix(b"[")?
        .split_once_str(b"]")?
        .0
        .trim()
        .eq_ignore_ascii_case(b"archives")
        .then_some(true)
}

/// Returns the default `Data Files` directory implied by `Morrowind.ini`.
///
/// # Errors
///
/// Returns an error when the INI path does not appear to live beside a valid
/// `Data Files` directory.
#[inline]
pub fn morrowind_data_dirs(ini_path: &Path) -> Result<Vec<PathBuf>> {
    let data_files = ini_path.with_file_name("Data Files");
    if !data_files.is_dir() {
        bail!("'Morrowind.ini' is not inside 'Data Files' directory: {}", ini_path.display());
    }
    Ok(vec![data_files])
}

#[cfg(test)]
mod tests;
