mod merge;
mod tolerant;

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;
use toml_edit::visit_mut::VisitMut;
use toml_edit::{DocumentMut, Formatted, Item, RawString, Table, value};

use crate::schema::{SCHEMA_VERSION, Settings};
use crate::validation::{Warning, validate, validate_bounds};
use merge::{current_value_at_path, force_value_into_table, merge_changed_tables};
use tolerant::{PathSegment, display_path, dotted_segments, error_segments, remove_bad_value, remove_path};

pub const FILE_NAME: &str = "mgeXE.toml";
pub const DEFAULT_DOCUMENT: &str = include_str!("default.toml");

/// Root table owned by the distant-land generator rather than by this crate's schema.
///
/// It is preserved verbatim across Restore Defaults and omitted from exported copies. This
/// crate sits below `distantland` in the dependency graph, so the name is duplicated
/// here and must stay equal to `distantland::GENERATION_JOB_NAMESPACE`.
const GENERATOR_TABLE: &str = "generation";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Returns the highest `doc_position` used anywhere in `table`'s subtree.
///
/// Implicit tables carry no position of their own, so they contribute nothing here.
fn max_table_position(table: &Table) -> isize {
    let mut max = table.position().unwrap_or(0);
    for (_, item) in table.iter() {
        match item {
            Item::Table(child) => max = max.max(max_table_position(child)),
            Item::ArrayOfTables(array) => {
                for child in array.iter() {
                    max = max.max(max_table_position(child));
                }
            }
            _ => {}
        }
    }
    max
}

/// Ensures `table`'s header is preceded by a blank line.
///
/// A table parsed as the first one in its own document carries an empty prefix, which would
/// otherwise butt its header directly against the last key of the table it now follows. Any
/// leading comment the producer attached is kept, with the blank line placed above it.
fn separate_from_preceding_table(table: &mut Table) {
    let decor = table.decor_mut();
    let existing = decor.prefix().and_then(RawString::as_str).unwrap_or("");
    if !existing.starts_with('\n') {
        decor.set_prefix(format!("\n{existing}"));
    }
}

/// Renumbers `table` and its nested tables consecutively from `next`, depth first.
///
/// `Table::insert` preserves an item's existing `doc_position`, and a table parsed from a
/// standalone document carries low positions from that parse. The encoder sorts tables by
/// position, so splicing one in without renumbering interleaves it with the tables already
/// present instead of appending it.
fn renumber_tables(table: &mut Table, next: &mut isize) {
    table.set_position(Some(*next));
    *next += 1;
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(child) => renumber_tables(child, next),
            Item::ArrayOfTables(array) => {
                for child in array.iter_mut() {
                    renumber_tables(child, next);
                }
            }
            _ => {}
        }
    }
}

/// Rewrites every float it visits as the shortest decimal that reads back as the same `f32`.
///
/// The schema stores floats as `f32`, but serde widens them to `f64` on the way into the
/// document, so `5.36f32` is written out as `5.360000133514404`. Formatting the narrowed value
/// yields the shortest spelling that round-trips through `f32`, leaving the loaded value
/// bit-identical while writing `5.36`.
struct NarrowFloats;

impl VisitMut for NarrowFloats {
    fn visit_float_mut(&mut self, node: &mut Formatted<f64>) {
        let Ok(narrowed) = (*node.value() as f32).to_string().parse::<f64>() else {
            return;
        };
        let decor = node.decor().clone();
        *node = Formatted::new(narrowed);
        *node.decor_mut() = decor;
    }
}

/// Serializes `settings` into a document free of `f64` widening artifacts.
fn settings_document(settings: &Settings) -> Result<DocumentMut, toml_edit::ser::Error> {
    let mut document = toml_edit::ser::to_document(settings)?;
    NarrowFloats.visit_document_mut(&mut document);
    Ok(document)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenState {
    Valid,
    MissingDefaults,
    InvalidDefaults,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Revision {
    Missing,
    Present(u64),
}

#[derive(Clone, Debug)]
pub struct ConfigDocument {
    path: PathBuf,
    document: DocumentMut,
    settings: Settings,
    baseline: Settings,
    revision: Revision,
    state: OpenState,
    writes_disabled: bool,
    warnings: Vec<Warning>,
    diagnostic: Option<String>,
    forced_paths: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid configuration {path}: {message}")]
    Invalid { path: PathBuf, message: String },
    #[error("configuration {0} was removed; reload was not applied")]
    MissingOnReload(PathBuf),
    #[error("configuration changed on disk; reload it before saving")]
    RevisionConflict,
    #[error("configuration was invalid at startup and saving remains disabled until a successful reload")]
    WritesDisabled,
    #[error("first-run creation collided with a new configuration; the new file was left untouched")]
    CreationCollision,
    #[error("write {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("replace {path}: {source}")]
    Replace { path: PathBuf, source: io::Error },
    #[error("configuration schema error: {0}")]
    Schema(String),
    #[error("copy target is the live configuration: {0}")]
    CopyTargetsSource(PathBuf),
}

impl ConfigDocument {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        match fs::read(&path) {
            Ok(bytes) => match parse_bytes(&path, &bytes) {
                Ok((document, settings, warnings)) => Self {
                    path,
                    document,
                    baseline: settings.clone(),
                    settings,
                    revision: Revision::Present(hash_bytes(&bytes)),
                    state: OpenState::Valid,
                    writes_disabled: false,
                    warnings,
                    diagnostic: None,
                    forced_paths: Vec::new(),
                },
                Err(error) => Self::invalid_fallback(path, error.to_string()),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (document, settings, warnings) =
                    parse_bytes(&path, DEFAULT_DOCUMENT.as_bytes()).expect("embedded defaults are valid");
                Self {
                    path,
                    document,
                    baseline: settings.clone(),
                    settings,
                    revision: Revision::Missing,
                    state: OpenState::MissingDefaults,
                    writes_disabled: false,
                    warnings,
                    diagnostic: None,
                    forced_paths: Vec::new(),
                }
            }
            Err(error) => Self::invalid_fallback(path.clone(), format!("read {}: {error}", path.display())),
        }
    }

    fn invalid_fallback(path: PathBuf, diagnostic: String) -> Self {
        let (document, settings, warnings) =
            parse_bytes(&path, DEFAULT_DOCUMENT.as_bytes()).expect("embedded defaults are valid");
        let revision = current_revision(&path).unwrap_or(Revision::Missing);
        Self {
            path,
            document,
            baseline: settings.clone(),
            settings,
            revision,
            state: OpenState::InvalidDefaults,
            writes_disabled: true,
            warnings,
            diagnostic: Some(diagnostic),
            forced_paths: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state(&self) -> OpenState {
        self.state
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    pub fn replace_settings(&mut self, mut settings: Settings) -> Result<(), ConfigError> {
        self.warnings = validate(&mut settings).map_err(|error| ConfigError::Schema(error.to_string()))?;
        self.settings = settings;
        self.forced_paths.clear();
        Ok(())
    }

    pub fn reset_to_defaults(&mut self, clear_unknown: bool) {
        let defaults = Settings::default();
        if clear_unknown {
            let generator_table = self.document.remove(GENERATOR_TABLE);
            self.document = DEFAULT_DOCUMENT.parse().expect("embedded defaults are valid TOML");
            if let Some(generator_table) = generator_table {
                // The preserved table still carries positions from the previous parse, which
                // do not correspond to the freshly parsed defaults; renumber it onto the end.
                self.insert_root_table_last(GENERATOR_TABLE, generator_table);
            }
            self.baseline = defaults.clone();
        }
        self.settings = defaults;
        self.warnings.clear();
        self.forced_paths.clear();
    }

    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }

    pub fn writes_disabled(&self) -> bool {
        self.writes_disabled
    }

    pub fn needs_creation(&self) -> bool {
        self.state == OpenState::MissingDefaults && !self.writes_disabled
    }

    pub fn reload(&mut self) -> Result<(), ConfigError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ConfigError::MissingOnReload(self.path.clone()));
            }
            Err(source) => {
                return Err(ConfigError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let (document, settings, warnings) = parse_bytes(&self.path, &bytes)?;
        self.document = document;
        self.baseline = settings.clone();
        self.settings = settings;
        self.revision = Revision::Present(hash_bytes(&bytes));
        self.state = OpenState::Valid;
        self.writes_disabled = false;
        self.warnings = warnings;
        self.diagnostic = None;
        self.forced_paths.clear();
        Ok(())
    }

    pub fn save(&mut self) -> Result<(), ConfigError> {
        if self.writes_disabled {
            return Err(ConfigError::WritesDisabled);
        }

        let current = current_revision(&self.path).map_err(|source| ConfigError::Read {
            path: self.path.clone(),
            source,
        })?;
        if current != self.revision {
            if self.revision == Revision::Missing {
                self.writes_disabled = true;
                return Err(ConfigError::CreationCollision);
            }
            return Err(ConfigError::RevisionConflict);
        }

        let baseline = settings_document(&self.baseline).map_err(|error| ConfigError::Schema(error.to_string()))?;
        let current = settings_document(&self.settings).map_err(|error| ConfigError::Schema(error.to_string()))?;
        merge_changed_tables(self.document.as_table_mut(), baseline.as_table(), current.as_table());
        for path in &self.forced_paths {
            let segments = path.split('.').collect::<Vec<_>>();
            if let Some(value) = current_value_at_path(current.as_table(), &segments) {
                force_value_into_table(self.document.as_table_mut(), &segments, value);
            }
        }
        // Values written before narrowing existed, or edited by hand, are only rewritten when
        // they change. Narrowing the schema's own tables in place clears those out too; every
        // float under them loads as `f32`, so the spelling changes and the value does not.
        for (key, _) in current.as_table().iter() {
            if let Some(item) = self.document.get_mut(key) {
                NarrowFloats.visit_item_mut(item);
            }
        }
        let bytes = self.document.to_string().into_bytes();
        atomic_replace(&self.path, &bytes, self.revision != Revision::Missing)?;
        self.revision = Revision::Present(hash_bytes(&bytes));
        self.state = OpenState::Valid;
        self.baseline = self.settings.clone();
        self.warnings.clear();
        self.forced_paths.clear();
        Ok(())
    }

    pub fn write_copy(&self, target: &Path, mut settings: Settings) -> Result<(), ConfigError> {
        if target == self.path {
            return Err(ConfigError::CopyTargetsSource(target.to_owned()));
        }
        let revision = current_revision(target).map_err(|source| ConfigError::Read {
            path: target.to_owned(),
            source,
        })?;
        let mut copy = self.clone();
        copy.path = target.to_owned();
        copy.revision = revision;
        copy.document.remove(GENERATOR_TABLE);
        copy.warnings = validate(&mut settings).map_err(|error| ConfigError::Schema(error.to_string()))?;
        copy.settings = settings;
        copy.save()
    }

    /// Replaces one root table with the same table parsed from `source`.
    ///
    /// This keeps the live document's comments, unknown keys, revision checks, and atomic save
    /// path while allowing a schema-owning crate to supply a complete serialized table. The
    /// replaced table is placed after every other table in the document; its own internal key
    /// order and formatting come from `source`.
    ///
    /// # Errors
    ///
    /// Returns an error when `source` is not valid TOML, does not contain `name`, or contains a
    /// non-table item at that root key.
    pub fn replace_root_table_from_document(&mut self, name: &str, source: &str) -> Result<(), ConfigError> {
        let mut replacement: DocumentMut = source
            .parse()
            .map_err(|error| ConfigError::Schema(format!("invalid replacement TOML: {error}")))?;
        let mut item = replacement
            .remove(name)
            .ok_or_else(|| ConfigError::Schema(format!("replacement TOML is missing [{name}]")))?;
        if !item.is_table() {
            return Err(ConfigError::Schema(format!(
                "replacement TOML root key {name} is not a table"
            )));
        }
        NarrowFloats.visit_item_mut(&mut item);
        self.insert_root_table_last(name, item);
        Ok(())
    }

    /// Inserts `item` as a root table ordered after every table already in the document.
    ///
    /// Any existing table at `name` is removed first, so repeated saves keep the assigned
    /// positions stable instead of drifting upward.
    fn insert_root_table_last(&mut self, name: &str, mut item: Item) {
        self.document.remove(name);
        let mut next = max_table_position(self.document.as_table()) + 1;
        if let Some(table) = item.as_table_mut() {
            separate_from_preceding_table(table);
            renumber_tables(table, &mut next);
        }
        self.document.insert(name, item);
    }

    pub fn get_number(&self, path: &str) -> Option<f64> {
        self.settings.get_number(path)
    }

    pub fn set_number(&mut self, path: &str, value: f64) -> Result<(), ConfigError> {
        self.settings.set_number(path, value).map_err(ConfigError::Schema)?;
        self.warnings = validate_bounds(&mut self.settings).map_err(|error| ConfigError::Schema(error.to_string()))?;
        self.forced_paths.push(path.into());
        Ok(())
    }

    pub fn get_string(&self, path: &str) -> Option<&str> {
        self.settings.get_string(path)
    }

    pub fn set_string(&mut self, path: &str, value: &str) -> Result<(), ConfigError> {
        self.settings.set_string(path, value).map_err(ConfigError::Schema)?;
        self.warnings = validate_bounds(&mut self.settings).map_err(|error| ConfigError::Schema(error.to_string()))?;
        self.forced_paths.push(path.into());
        Ok(())
    }

    pub fn get_lines(&self, path: &str) -> Option<Vec<String>> {
        match path {
            "shaders.chain" => Some(self.settings.shaders.chain.clone()),
            "input.macros" => Some(self.settings.input.render_macros()),
            "input.triggers" => Some(self.settings.input.render_triggers()),
            "input.remap" => Some(self.settings.input.render_remap()),
            _ => None,
        }
    }
}

fn parse_bytes(path: &Path, bytes: &[u8]) -> Result<(DocumentMut, Settings, Vec<Warning>), ConfigError> {
    let text = std::str::from_utf8(bytes).map_err(|error| ConfigError::Invalid {
        path: path.to_owned(),
        message: format!("file is not valid UTF-8: {error}"),
    })?;
    let mut document = text.parse::<DocumentMut>().map_err(|error| ConfigError::Invalid {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    let declared_version = document
        .get("schema_version")
        .and_then(Item::as_integer)
        .and_then(|version| u32::try_from(version).ok());
    let mut warnings = Vec::new();
    let mut reset_owned_tables = false;

    let settings = loop {
        match tolerant::deserialize_settings(&document) {
            Ok(mut settings) => {
                settings.schema_version = SCHEMA_VERSION;
                match validate(&mut settings) {
                    Ok(validation_warnings) if validation_warnings.is_empty() => break settings,
                    Ok(validation_warnings) => {
                        let warning = validation_warnings
                            .into_iter()
                            .next()
                            .expect("non-empty warnings checked above");
                        let segments = dotted_segments(&warning.path);
                        if remove_bad_value(&mut document, &segments) {
                            let value = warning
                                .message
                                .split_once(" was clamped")
                                .map_or(warning.message.as_str(), |(value, _)| value);
                            warnings.push(Warning {
                                path: warning.path,
                                message: format!("{value} is not valid here; using the default"),
                            });
                            continue;
                        }
                        let normalized = settings_document(&settings).map_err(|error| ConfigError::Invalid {
                            path: path.to_owned(),
                            message: error.to_string(),
                        })?;
                        let keys = warning.path.split('.').collect::<Vec<_>>();
                        if let Some(value) = current_value_at_path(normalized.as_table(), &keys) {
                            force_value_into_table(document.as_table_mut(), &keys, value);
                            warnings.push(Warning {
                                path: warning.path,
                                message: format!("{}; using the normalized value", warning.message),
                            });
                            continue;
                        }
                    }
                    Err(error) => {
                        let segments = dotted_segments(&error.path);
                        if remove_bad_value(&mut document, &segments) {
                            warnings.push(Warning {
                                path: error.path,
                                message: format!("{}; using the default", error.message),
                            });
                            continue;
                        }
                    }
                }
            }
            Err(error) => {
                let segments = error_segments(error.path());
                if !segments.is_empty() && remove_bad_value(&mut document, &segments) {
                    warnings.push(Warning {
                        path: display_path(&segments),
                        message: format!("{}; using the default", error.inner()),
                    });
                    continue;
                }
            }
        }

        if reset_owned_tables {
            break Settings::default();
        }
        reset_owned_tables = true;
        warnings.push(Warning {
            path: "settings".into(),
            message: "owned settings could not be decoded; using defaults".into(),
        });
        for key in owned_root_keys() {
            document.remove(key);
        }
    };

    let ignored = tolerant::ignored_paths(&document).map_err(|error| ConfigError::Invalid {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    for segments in ignored {
        let Some(PathSegment::Key(root)) = segments.first() else {
            continue;
        };
        if root == GENERATOR_TABLE {
            continue;
        }
        let rendered = display_path(&segments);
        let preserve_root_table = segments.len() == 1
            && document
                .get(root)
                .is_some_and(|item| item.is_table() || item.is_array_of_tables());
        warnings.push(Warning {
            path: rendered,
            message: if preserve_root_table {
                "unknown root table was ignored and preserved".into()
            } else {
                "unknown key was ignored and will be removed when saved".into()
            },
        });
        if !preserve_root_table {
            remove_path(&mut document, &segments);
        }
    }

    if declared_version.is_some_and(|version| version != SCHEMA_VERSION) {
        warnings.push(Warning {
            path: "schema_version".into(),
            message: format!(
                "version {} differs from this build's version {SCHEMA_VERSION}; saving will write {SCHEMA_VERSION}",
                declared_version.expect("checked above")
            ),
        });
    }
    document["schema_version"] = value(i64::from(SCHEMA_VERSION));
    Ok((document, settings, warnings))
}

fn owned_root_keys() -> &'static [&'static str] {
    &[
        "schema_version",
        "graphics",
        "render",
        "runtime",
        "distant_land",
        "lighting",
        "shaders",
        "input",
        "gui",
    ]
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn current_revision(path: &Path) -> io::Result<Revision> {
    match fs::read(path) {
        Ok(bytes) => Ok(Revision::Present(hash_bytes(&bytes))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Revision::Missing),
        Err(error) => Err(error),
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().and_then(|value| value.to_str()).unwrap_or(FILE_NAME);
    path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), counter))
}

fn atomic_replace(path: &Path, bytes: &[u8], replace: bool) -> Result<(), ConfigError> {
    let temporary = temp_path(path);
    let write_result = (|| -> io::Result<()> {
        let mut file = File::create_new(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(source) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(ConfigError::Write { path: temporary, source });
    }

    if let Err(source) = move_file(&temporary, path, replace) {
        let _ = fs::remove_file(&temporary);
        return Err(ConfigError::Replace {
            path: path.to_owned(),
            source,
        });
    }
    Ok(())
}

#[cfg(windows)]
fn move_file(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    let flags = if replace { MOVEFILE_REPLACE_EXISTING } else { 0 };
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn move_file(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    if !replace && destination.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination already exists"));
    }
    fs::rename(source, destination)
}
