//! The plugin universe behind the generator's Plugins page.
//!
//! Builds the **superset** of every `.esm`/`.esp` across the data directories
//! (active or not), not just the active load order `distantland` answers.
//!
//! Deduplicates by lowercased filename (highest-priority directory wins),
//! matching Morrowind's data-layer semantics to satisfy
//! `validate_for_generation`'s duplicate-filename rejection.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::SystemTime,
};

use distantland::{GenerationJob, morrowind_data_dirs, parse_morrowind_game_files_with_data_dirs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PluginKind {
    Esm,
    Esp,
}

impl PluginKind {
    fn from_filename(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".esm") {
            Some(Self::Esm)
        } else if lower.ends_with(".esp") {
            Some(Self::Esp)
        } else {
            None
        }
    }

    /// Lower sorts earlier in the `by type` view: masters, then plugins.
    fn sort_rank(self) -> u8 {
        match self {
            Self::Esm => 0,
            Self::Esp => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PluginEntry {
    /// Absolute path on disk; this is what feeds `job.plugins`.
    pub(crate) full_path: PathBuf,
    /// Bare filename, e.g. `Morrowind.esm`. The key for matching the active load
    /// order and a saved selection, both of which are filename-based.
    pub(crate) file_name: String,
    pub(crate) enabled: bool,
    /// Picked on the Grass page as a generator-only groundcover plugin.
    ///
    /// Mutually exclusive with [`Self::enabled`], and **grass wins every clash**
    /// (see [`enforce_grass_wins`]). `validate_for_generation` rejects a job
    /// whose `plugins` and `grass_plugins` share a filename, so every mutation
    /// keeps at most one of the two set rather than building a selection that
    /// only fails at save time.
    pub(crate) grass: bool,
    kind: PluginKind,
    modified: SystemTime,
}

/// Display order for the list. Purely a view setting, see [`PluginUniverse::write_into`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SortMode {
    Name,
    Type,
    LoadOrder,
}

pub(crate) struct PluginUniverse {
    /// Morrowind install root: `Morrowind.ini` and `Data Files` hang off it.
    root: PathBuf,
    /// Data directories beyond the base `Data Files`, edited from the page's
    /// `Data directories…` dialog and persisted as `job.data_dirs`.
    pub(crate) extra_dirs: Vec<PathBuf>,
    /// Every discovered plugin, in the current display order.
    pub(crate) entries: Vec<PluginEntry>,
    pub(crate) sort: SortMode,
    /// `job.auto_sync_plugins`: the load order is authoritative and the user does
    /// not edit `enabled` directly, so anything that could stale the selection
    /// re-derives it from `Morrowind.ini` instead of restoring what was there.
    pub(crate) sync: bool,
}

impl PluginUniverse {
    /// Discover the universe for `root` and apply `job`'s saved selection.
    ///
    /// Checks are seeded from the active `Morrowind.ini` load order first, so a
    /// job that has never recorded a selection (`plugins: None`) opens on the
    /// order the game is actually running.
    ///
    /// With `job.auto_sync_plugins` the saved list is ignored outright: the live
    /// order is what the next run will generate from, so showing the stale saved
    /// one would make the page lie about what Generate does.
    pub(crate) fn load(root: &Path, job: &GenerationJob) -> Self {
        let extra_dirs = extra_dirs_from_job(root, job);
        let mut entries = scan_universe(root, &extra_dirs);
        seed_from_active(root, &extra_dirs, &mut entries);

        if let (false, Some(saved)) = (job.auto_sync_plugins, job.plugins.as_ref()) {
            let selected = file_names(saved);
            for entry in &mut entries {
                entry.enabled = selected.contains(&entry.file_name.to_ascii_lowercase());
            }
        }

        if let Some(saved) = job.grass_plugins.as_ref() {
            let selected = file_names(saved);
            for entry in &mut entries {
                entry.grass = selected.contains(&entry.file_name.to_ascii_lowercase());
            }
        }
        enforce_grass_wins(&mut entries);

        let sort = SortMode::LoadOrder;
        apply_sort(&mut entries, sort);
        Self {
            root: root.to_path_buf(),
            extra_dirs,
            entries,
            sort,
            sync: job.auto_sync_plugins,
        }
    }

    /// Turn load-order sync on or off, refreshing what the page shows immediately.
    ///
    /// Turning it on re-derives the selection now rather than at save time: the
    /// list is about to become read-only, so leaving it showing the old picks
    /// would misstate what Generate will use.
    pub(crate) fn set_sync(&mut self, sync: bool) {
        self.sync = sync;
        self.refresh_enabled();
    }

    /// Restore the invariants after a grass pick changes.
    ///
    /// In sync mode this also takes `enabled` back from the live load order, which
    /// is what makes un-picking a grass plugin restore it: the load order is the
    /// authority there, so there is something to restore *from*. With sync off
    /// there is not, and clearing a grass pick simply leaves the plugin unpicked.
    pub(crate) fn refresh_enabled(&mut self) {
        if self.sync {
            seed_from_active(&self.root, &self.extra_dirs, &mut self.entries);
        }
        enforce_grass_wins(&mut self.entries);
    }

    pub(crate) fn set_sort(&mut self, sort: SortMode) {
        self.sort = sort;
        apply_sort(&mut self.entries, sort);
    }

    /// Check or clear every entry, except the grass picks, which are left alone
    /// in both directions.
    pub(crate) fn set_all(&mut self, enabled: bool) {
        for entry in &mut self.entries {
            entry.enabled = enabled;
        }
        enforce_grass_wins(&mut self.entries);
    }

    /// Reset the selection to exactly the active `Morrowind.ini` load order, and
    /// show it in that order, the page's one-click "just do the normal thing".
    ///
    /// Grass picks all survive: under grass-wins an active groundcover plugin is
    /// held out of the load order rather than dropped from the grass list.
    pub(crate) fn use_current_load_order(&mut self) {
        seed_from_active(&self.root, &self.extra_dirs, &mut self.entries);
        enforce_grass_wins(&mut self.entries);
        self.set_sort(SortMode::LoadOrder);
    }

    /// Adopt a new set of extra data directories and rescan, keeping the check
    /// state (both selections) of every filename that survives the rescan.
    ///
    /// In sync mode only the grass picks are carried over. `enabled` is re-derived
    /// from the load order the fresh directory set resolves against, because that
    /// is what the next run will use; restoring the old checks would show a
    /// selection the new layering may no longer produce.
    pub(crate) fn set_extra_dirs(&mut self, dirs: Vec<PathBuf>) {
        let previous: HashMap<String, (bool, bool)> = self
            .entries
            .iter()
            .map(|entry| (entry.file_name.to_ascii_lowercase(), (entry.enabled, entry.grass)))
            .collect();

        self.extra_dirs = dirs;
        self.entries = scan_universe(&self.root, &self.extra_dirs);
        seed_from_active(&self.root, &self.extra_dirs, &mut self.entries);
        let sync = self.sync;
        for entry in &mut self.entries {
            if let Some(&(enabled, grass)) = previous.get(&entry.file_name.to_ascii_lowercase()) {
                if !sync {
                    entry.enabled = enabled;
                }
                entry.grass = grass;
            }
        }
        enforce_grass_wins(&mut self.entries);
        apply_sort(&mut self.entries, self.sort);
    }

    /// The base layer, always searched and therefore never addable as an extra.
    pub(crate) fn base_data_dir(&self) -> PathBuf {
        self.root.join("Data Files")
    }

    /// Every discovered plugin's path, for the generator's groundcover scan.
    pub(crate) fn plugin_paths(&self) -> Vec<PathBuf> {
        self.entries.iter().map(|entry| entry.full_path.clone()).collect()
    }

    /// The layered data directories, lowest priority first.
    ///
    /// The groundcover scan resolves a plugin's declared masters against these.
    pub(crate) fn data_dirs(&self) -> Vec<PathBuf> {
        all_data_dirs(&self.root, &self.extra_dirs)
    }

    pub(crate) fn enabled_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.enabled).count()
    }

    pub(crate) fn grass_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.grass).count()
    }

    /// Write the selection into the job's `plugins`, `data_dirs` and
    /// `grass_plugins`.
    ///
    /// **Always emitted in load order, never in the order the list happens to be
    /// showing.** `job.plugins` *is* the load order (it feeds `VfsLoadOptions`),
    /// so writing a name-sorted view would silently reorder the game's masters.
    /// Sorting is display-only by design.
    pub(crate) fn write_into(&self, job: &mut GenerationJob) {
        let mut picked: Vec<&PluginEntry> = self.entries.iter().filter(|entry| entry.enabled).collect();
        picked.sort_by_key(|entry| load_order_key(entry));
        // Always `Some`: an empty selection is a distinct state from "MGE XE has
        // never recorded one", and `save` refuses to write the former.
        job.plugins = Some(picked.into_iter().map(|entry| entry.full_path.clone()).collect());

        // Only emit explicit `data_dirs` when extras exist. A base-only install
        // lets generation derive `Data Files` from `morrowind_ini`, which keeps
        // the saved job free of an absolute path to one machine's install.
        job.data_dirs = if self.extra_dirs.is_empty() {
            None
        } else {
            let dirs: Vec<PathBuf> = all_data_dirs(&self.root, &self.extra_dirs)
                .into_iter()
                .filter(|dir| dir.is_dir())
                .collect();
            (!dirs.is_empty()).then_some(dirs)
        };

        // The grass list is its own load order: `load_grass_plugins` resolves it
        // in the order written, and a plugin's `MAST` target can only be matched
        // against entries it has already passed. Name order breaks that whenever
        // a master sorts after its dependent, and then the reference is attributed
        // to the wrong plugin and an override or delete silently no-ops.
        //
        // `load_order_key` puts every ESM ahead of every ESP, which is the
        // realistic dependency case; the residual esp-patches-esp case still
        // reports `grass_plugin_master_not_in_list`. It is also deterministic,
        // which `scan_universe`'s `HashMap` order is not, so the byte-idempotent
        // job writer is unaffected.
        let mut grass: Vec<&PluginEntry> = self.entries.iter().filter(|entry| entry.grass).collect();
        grass.sort_by_key(|entry| load_order_key(entry));
        // `None` rather than an empty list, as with `data_dirs`: an unused
        // feature leaves nothing behind in the saved job. Both forms validate.
        job.grass_plugins = (!grass.is_empty()).then(|| grass.into_iter().map(|entry| entry.full_path.clone()).collect());
    }
}

/// `<root>\Morrowind.ini`.
fn ini_path(root: &Path) -> PathBuf {
    root.join("Morrowind.ini")
}

/// Case-insensitive whole-path comparison; Windows paths are case-insensitive.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// A saved job's paths reduced to lowercased filenames. Both selections match on
/// that key, because a job file may record bare filenames or full paths.
fn file_names(paths: &[PathBuf]) -> HashSet<String> {
    paths
        .iter()
        .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().to_ascii_lowercase()))
        .collect()
}

/// Restore the mutual exclusion after anything that touches either selection.
///
/// **Grass wins.** With `auto_sync_plugins` on, the reverse rule (load order
/// wins) would make an active groundcover plugin impossible to mark as grass:
/// sync re-ticks it on the read-only Plugins tab, and the pick would be dropped
/// again on every refresh. The losing case here is only that a plugin the user
/// marked as grass is left out of the statics inputs, which is what marking it
/// as grass asked for.
fn enforce_grass_wins(entries: &mut [PluginEntry]) {
    for entry in entries {
        if entry.grass {
            entry.enabled = false;
        }
    }
}

/// Morrowind's own load-order rule: masters first, then by file mtime, matching
/// `distantland`'s `load_order_sorter`. The filename tiebreak is ours: the
/// scan comes out of a `HashMap`, so equal mtimes would otherwise order
/// arbitrarily from run to run.
fn load_order_key(entry: &PluginEntry) -> (bool, SystemTime, String) {
    (
        entry.kind != PluginKind::Esm,
        entry.modified,
        entry.file_name.to_ascii_lowercase(),
    )
}

/// The three [`SortMode`] orderings, exposed for a view that sorts an index
/// projection instead of `entries` itself.
///
/// The Grass tab needs its own order over a filtered subset, and [`apply_sort`]
/// reorders the shared `entries` in place, so using it there would silently
/// reorder the Plugins tab too. These keep both tabs' idea of "by name" and "by
/// type" identical without exposing `kind`.
impl PluginEntry {
    pub(crate) fn sort_key_name(&self) -> String {
        self.file_name.to_ascii_lowercase()
    }

    pub(crate) fn sort_key_type(&self) -> (u8, String) {
        (self.kind.sort_rank(), self.sort_key_name())
    }

    pub(crate) fn sort_key_load_order(&self) -> (bool, SystemTime, String) {
        load_order_key(self)
    }
}

fn apply_sort(entries: &mut [PluginEntry], sort: SortMode) {
    match sort {
        SortMode::Name => entries.sort_by_key(|entry| entry.file_name.to_ascii_lowercase()),
        SortMode::Type => entries.sort_by(|a, b| {
            a.kind
                .sort_rank()
                .cmp(&b.kind.sort_rank())
                .then_with(|| a.file_name.to_ascii_lowercase().cmp(&b.file_name.to_ascii_lowercase()))
        }),
        SortMode::LoadOrder => entries.sort_by_key(load_order_key),
    }
}

fn resolve_job_dir(root: &Path, dir: &Path) -> PathBuf {
    if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        root.join(dir)
    }
}

/// Extra plugin directories recorded in a saved job: `data_dirs` minus the base
/// `Data Files` layer, with duplicates and missing directories dropped.
fn extra_dirs_from_job(root: &Path, job: &GenerationJob) -> Vec<PathBuf> {
    let Some(data_dirs) = job.data_dirs.as_ref() else {
        return Vec::new();
    };

    let base = root.join("Data Files");
    let mut extras: Vec<PathBuf> = Vec::new();
    for dir in data_dirs {
        let resolved = resolve_job_dir(root, dir);
        if same_path(&resolved, &base) || !resolved.is_dir() {
            continue;
        }
        if extras.iter().any(|existing| same_path(existing, &resolved)) {
            continue;
        }
        extras.push(resolved);
    }
    extras
}

/// Data-directory layers in priority order, lowest first. Later directories
/// override earlier ones, which is the order the generator's VFS expects.
fn all_data_dirs(root: &Path, extras: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = morrowind_data_dirs(&ini_path(root)).unwrap_or_else(|_| vec![root.join("Data Files")]);
    dirs.extend(extras.iter().cloned());
    dirs
}

/// Every `.esm`/`.esp` across the data directories.
///
/// Deduplicated by lowercased filename, keeping the highest-priority layer's
/// copy, Morrowind's own override semantics, and what keeps the selection past
/// `validate_for_generation`'s duplicate-filename rejection.
fn scan_universe(root: &Path, extras: &[PathBuf]) -> Vec<PluginEntry> {
    let mut by_name: HashMap<String, PluginEntry> = HashMap::new();

    let base = root.join("Data Files");
    let layers = std::iter::once(base).chain(extras.iter().cloned());

    for dir in layers {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
                continue;
            };
            let Some(kind) = PluginKind::from_filename(&file_name) else {
                continue;
            };
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            by_name.insert(
                file_name.to_ascii_lowercase(),
                PluginEntry {
                    full_path: path,
                    file_name,
                    enabled: false,
                    grass: false,
                    kind,
                    modified,
                },
            );
        }
    }

    by_name.into_values().collect()
}

/// Set every check to exactly the active `Morrowind.ini` load order. A missing or
/// unreadable INI clears the selection rather than failing. The page still works,
/// the user just picks by hand.
fn seed_from_active(root: &Path, extras: &[PathBuf], entries: &mut [PluginEntry]) {
    let dirs = all_data_dirs(root, extras);
    let active: HashSet<String> = match parse_morrowind_game_files_with_data_dirs(&ini_path(root), &dirs) {
        Ok(paths) => paths
            .iter()
            .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().to_ascii_lowercase()))
            .collect(),
        Err(_) => HashSet::new(),
    };

    for entry in entries {
        entry.enabled = active.contains(&entry.file_name.to_ascii_lowercase());
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::*;

    /// A stub Morrowind install: `Data Files`, plugin files, and a `[Game Files]`
    /// load order. `parse_morrowind_game_files_with_data_dirs` only returns
    /// plugins that exist on disk, so the files have to be real.
    struct Install(PathBuf);

    impl Install {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("mge-gui-plugins-{name}-{unique}"));
            fs::create_dir_all(root.join("Data Files")).unwrap();
            Self(root)
        }

        fn plugin(&self, relative: &str) -> PathBuf {
            let path = self.0.join("Data Files").join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"plugin").unwrap();
            path
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.0.join("Data Files").join(relative);
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn load_order(&self, names: &[&str]) {
            let mut ini = String::from("[Game Files]\n");
            for (index, name) in names.iter().enumerate() {
                ini.push_str(&format!("GameFile{index}={name}\n"));
            }
            fs::write(self.0.join("Morrowind.ini"), ini).unwrap();
        }

        fn universe(&self, job: &GenerationJob) -> PluginUniverse {
            PluginUniverse::load(&self.0, job)
        }
    }

    impl Drop for Install {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(universe: &PluginUniverse) -> Vec<&str> {
        universe.entries.iter().map(|entry| entry.file_name.as_str()).collect()
    }

    fn enabled(universe: &PluginUniverse) -> Vec<&str> {
        universe
            .entries
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.file_name.as_str())
            .collect()
    }

    /// A job whose plugin list the user curated by hand, i.e. sync off.
    ///
    /// Sync is on by default, and a synced job ignores `plugins` outright, so every
    /// test about what a *saved* selection restores has to say so explicitly.
    fn manual_job() -> GenerationJob {
        GenerationJob {
            auto_sync_plugins: false,
            ..GenerationJob::default()
        }
    }

    fn grass_picks(universe: &PluginUniverse) -> Vec<&str> {
        universe
            .entries
            .iter()
            .filter(|entry| entry.grass)
            .map(|entry| entry.file_name.as_str())
            .collect()
    }

    #[test]
    fn the_universe_is_every_plugin_not_just_the_active_ones() {
        let install = Install::new("universe");
        install.plugin("Morrowind.esm");
        install.plugin("Unused.esp");
        install.plugin("readme.txt");
        install.load_order(&["Morrowind.esm"]);

        let universe = install.universe(&GenerationJob::default());

        let mut found = names(&universe);
        found.sort_unstable();
        assert_eq!(found, ["Morrowind.esm", "Unused.esp"]);
        // Only the active one is checked; the rest are the superset the page
        // exists to offer.
        assert_eq!(enabled(&universe), ["Morrowind.esm"]);
    }

    #[test]
    fn a_saved_selection_overrides_the_active_load_order() {
        let install = Install::new("saved-selection");
        install.plugin("Morrowind.esm");
        install.plugin("Unused.esp");
        install.load_order(&["Morrowind.esm"]);

        let job = GenerationJob {
            // Saved as a bare filename, as a hand-written job file may be.
            plugins: Some(vec![PathBuf::from("unused.ESP")]),
            ..manual_job()
        };
        let universe = install.universe(&job);

        assert_eq!(enabled(&universe), ["Unused.esp"]);
    }

    #[test]
    fn sync_ignores_the_saved_selection_and_opens_on_the_live_load_order() {
        let install = Install::new("sync-open");
        install.plugin("Morrowind.esm");
        install.plugin("Unused.esp");
        install.load_order(&["Morrowind.esm"]);

        let job = GenerationJob {
            plugins: Some(vec![PathBuf::from("Unused.esp")]),
            auto_sync_plugins: true,
            ..GenerationJob::default()
        };
        let universe = install.universe(&job);

        // The saved list is a record of the last run, not the next one. Showing it
        // would misstate what Generate is about to use.
        assert!(universe.sync);
        assert_eq!(enabled(&universe), ["Morrowind.esm"]);
    }

    #[test]
    fn sync_is_the_default_for_a_job_that_has_never_recorded_one() {
        let install = Install::new("sync-default");
        install.plugin("Morrowind.esm");
        install.load_order(&["Morrowind.esm"]);

        assert!(install.universe(&GenerationJob::default()).sync);
    }

    #[test]
    fn extra_directories_extend_the_universe_and_the_highest_layer_wins() {
        let install = Install::new("layers");
        let base_copy = install.plugin("Shared.esp");
        install.plugin("Morrowind.esm");
        let grass = install.dir("Grass");
        let grass_copy = install.plugin("Grass/Shared.esp");
        install.plugin("Grass/Rem_GL.esp");
        install.load_order(&["Morrowind.esm"]);

        let job = GenerationJob {
            data_dirs: Some(vec![install.0.join("Data Files"), grass.clone()]),
            ..GenerationJob::default()
        };
        let universe = install.universe(&job);

        assert_eq!(universe.extra_dirs, vec![grass]);
        let mut found = names(&universe);
        found.sort_unstable();
        assert_eq!(found, ["Morrowind.esm", "Rem_GL.esp", "Shared.esp"]);

        // One `Shared.esp`, and it is the extra directory's copy, otherwise the
        // selection would carry two entries with the same filename and
        // `validate_for_generation` would reject the job.
        let shared = universe.entries.iter().find(|entry| entry.file_name == "Shared.esp").unwrap();
        assert_eq!(shared.full_path, grass_copy);
        assert_ne!(shared.full_path, base_copy);
    }

    #[test]
    fn changing_the_extra_directories_keeps_the_surviving_check_states() {
        let install = Install::new("rescan");
        install.plugin("Morrowind.esm");
        install.plugin("Unused.esp");
        let grass = install.dir("Grass");
        install.plugin("Grass/Rem_GL.esp");
        install.load_order(&["Morrowind.esm"]);

        let mut universe = install.universe(&manual_job());
        // A hand-made selection that the active load order would not produce.
        universe
            .entries
            .iter_mut()
            .find(|entry| entry.file_name == "Unused.esp")
            .unwrap()
            .enabled = true;

        universe.set_extra_dirs(vec![grass]);

        assert!(names(&universe).contains(&"Rem_GL.esp"));
        let mut still_checked = enabled(&universe);
        still_checked.sort_unstable();
        assert_eq!(still_checked, ["Morrowind.esm", "Unused.esp"]);
    }

    #[test]
    fn changing_the_extra_directories_re_derives_the_selection_when_syncing() {
        let install = Install::new("rescan-sync");
        install.plugin("Morrowind.esm");
        install.plugin("Unused.esp");
        let extra = install.dir("Groundcover");
        install.plugin("Groundcover/Rem_GL.esp");
        install.load_order(&["Morrowind.esm"]);

        let mut universe = install.universe(&GenerationJob::default());
        universe
            .entries
            .iter_mut()
            .find(|entry| entry.file_name == "Unused.esp")
            .unwrap()
            .enabled = true;
        universe
            .entries
            .iter_mut()
            .find(|entry| entry.file_name == "Morrowind.esm")
            .unwrap()
            .grass = true;

        universe.set_extra_dirs(vec![extra]);

        // The new layering resolves its own load order, and that is what the run
        // will use, so `enabled` comes back from the INI rather than from the stale
        // hand-edit. The grass picks are the user's and are carried across.
        assert!(names(&universe).contains(&"Rem_GL.esp"));
        assert_eq!(grass_picks(&universe), ["Morrowind.esm"]);
        assert_eq!(enabled(&universe), Vec::<&str>::new());
    }

    #[test]
    fn the_saved_order_is_the_load_order_whatever_the_view_shows() {
        let install = Install::new("write-order");
        install.plugin("Morrowind.esm");
        install.plugin("Tribunal.esm");
        install.plugin("aaa_first_alphabetically.esp");
        install.load_order(&["Morrowind.esm", "Tribunal.esm", "aaa_first_alphabetically.esp"]);

        let mut universe = install.universe(&GenerationJob::default());
        let load_order: Vec<PathBuf> = universe.entries.iter().map(|entry| entry.full_path.clone()).collect();

        // Sorting by name puts the .esp first in the view. The written order must
        // not follow it: `job.plugins` is what the VFS loads, in order.
        universe.set_sort(SortMode::Name);
        assert_eq!(names(&universe)[0], "aaa_first_alphabetically.esp");

        let mut job = GenerationJob::default();
        universe.write_into(&mut job);
        assert_eq!(job.plugins, Some(load_order));
    }

    #[test]
    fn only_checked_plugins_are_written_and_a_base_install_emits_no_data_dirs() {
        let install = Install::new("write-selection");
        install.plugin("Morrowind.esm");
        install.plugin("Unused.esp");
        install.load_order(&["Morrowind.esm"]);

        let universe = install.universe(&GenerationJob::default());
        let mut job = GenerationJob::default();
        universe.write_into(&mut job);

        assert_eq!(job.plugins, Some(vec![install.0.join("Data Files").join("Morrowind.esm")]));
        assert_eq!(job.data_dirs, None);
    }

    #[test]
    fn clearing_and_selecting_everything_covers_the_whole_universe() {
        let install = Install::new("select-all");
        install.plugin("Morrowind.esm");
        install.plugin("Unused.esp");
        install.load_order(&["Morrowind.esm"]);

        let mut universe = install.universe(&GenerationJob::default());
        universe.set_all(false);
        assert_eq!(universe.enabled_count(), 0);
        universe.set_all(true);
        assert_eq!(universe.enabled_count(), 2);

        universe.use_current_load_order();
        assert_eq!(enabled(&universe), ["Morrowind.esm"]);
        assert_eq!(universe.sort, SortMode::LoadOrder);
    }

    #[test]
    fn a_saved_grass_selection_is_restored_and_grass_wins_a_clash() {
        let install = Install::new("saved-grass");
        install.plugin("Morrowind.esm");
        install.plugin("Rem_GL.esp");
        install.load_order(&["Morrowind.esm"]);

        let job = GenerationJob {
            plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
            // A bare, differently-cased filename as a hand-written job may carry,
            // plus one name claimed by both lists, which the generator rejects.
            // The model has to break the tie rather than reproduce it.
            grass_plugins: Some(vec![PathBuf::from("rem_gl.ESP"), PathBuf::from("Morrowind.esm")]),
            ..manual_job()
        };
        let universe = install.universe(&job);

        // Grass takes the contested name, and takes it from both the saved plugin
        // list and the active load order.
        assert_eq!(grass_picks(&universe), ["Morrowind.esm", "Rem_GL.esp"]);
        assert_eq!(enabled(&universe), Vec::<&str>::new());
    }

    #[test]
    fn neither_selecting_nor_clearing_every_plugin_disturbs_the_grass_list() {
        let install = Install::new("grass-exclusive");
        install.plugin("Morrowind.esm");
        install.plugin("Rem_GL.esp");
        install.load_order(&["Morrowind.esm"]);

        let job = GenerationJob {
            plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
            grass_plugins: Some(vec![PathBuf::from("Rem_GL.esp")]),
            ..manual_job()
        };
        let mut universe = install.universe(&job);

        // Both are Plugins-page buttons; neither says anything about the grass tab.
        universe.set_all(false);
        assert_eq!(grass_picks(&universe), ["Rem_GL.esp"]);

        universe.set_all(true);
        assert_eq!(grass_picks(&universe), ["Rem_GL.esp"]);
        // `Select all` skips the grass pick rather than claiming it.
        assert_eq!(enabled(&universe), ["Morrowind.esm"]);
    }

    #[test]
    fn the_active_load_order_never_takes_back_a_grass_pick() {
        let install = Install::new("grass-reseed");
        install.plugin("Morrowind.esm");
        install.plugin("Rem_GL.esp");
        install.load_order(&["Morrowind.esm"]);

        let job = GenerationJob {
            plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
            grass_plugins: Some(vec![PathBuf::from("Rem_GL.esp")]),
            ..manual_job()
        };
        let mut universe = install.universe(&job);

        universe.use_current_load_order();
        assert_eq!(grass_picks(&universe), ["Rem_GL.esp"]);
        assert_eq!(enabled(&universe), ["Morrowind.esm"]);

        // Groundcover left active in the game's load order. It stays grass and is
        // held out of the statics inputs, the whole point of grass-wins and the
        // only reachable state once a synced Plugins tab stops being editable.
        install.load_order(&["Morrowind.esm", "Rem_GL.esp"]);
        universe.use_current_load_order();
        assert_eq!(grass_picks(&universe), ["Rem_GL.esp"]);
        assert_eq!(enabled(&universe), ["Morrowind.esm"]);
    }

    #[test]
    fn unpicking_a_grass_plugin_restores_it_to_a_synced_load_order() {
        let install = Install::new("grass-unpick");
        install.plugin("Morrowind.esm");
        install.plugin("Rem_GL.esp");
        install.load_order(&["Morrowind.esm", "Rem_GL.esp"]);

        let job = GenerationJob {
            grass_plugins: Some(vec![PathBuf::from("Rem_GL.esp")]),
            auto_sync_plugins: true,
            ..GenerationJob::default()
        };
        let mut universe = install.universe(&job);
        assert_eq!(enabled(&universe), ["Morrowind.esm"]);

        for entry in &mut universe.entries {
            entry.grass = false;
        }
        universe.refresh_enabled();

        // Sync mode has an authority to restore from, so the plugin rejoins the
        // load order instead of being left unselected on both tabs.
        assert_eq!(enabled(&universe), ["Morrowind.esm", "Rem_GL.esp"]);
    }

    #[test]
    fn changing_the_extra_directories_keeps_the_grass_picks() {
        let install = Install::new("rescan-grass");
        install.plugin("Morrowind.esm");
        install.plugin("Rem_GL.esp");
        let extra = install.dir("Groundcover");
        install.plugin("Groundcover/Rem_AC.esp");
        install.load_order(&["Morrowind.esm"]);

        let job = GenerationJob {
            plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
            grass_plugins: Some(vec![PathBuf::from("Rem_GL.esp")]),
            ..manual_job()
        };
        let mut universe = install.universe(&job);

        universe.set_extra_dirs(vec![extra]);

        assert!(names(&universe).contains(&"Rem_AC.esp"));
        assert_eq!(grass_picks(&universe), ["Rem_GL.esp"]);
    }

    #[test]
    fn grass_plugins_are_written_in_load_order_and_omitted_when_none_are_picked() {
        let install = Install::new("write-grass");
        install.plugin("Morrowind.esm");
        // Named so that alphabetical order and load order disagree: the master
        // sorts last by name, and it is the master that has to be written first.
        let zzz = install.plugin("zzz_grass.esm");
        let rem = install.plugin("Rem_GL.esp");
        install.load_order(&["Morrowind.esm"]);

        let mut universe = install.universe(&GenerationJob::default());
        let mut job = GenerationJob::default();
        universe.write_into(&mut job);
        assert_eq!(job.grass_plugins, None);

        for entry in &mut universe.entries {
            if entry.file_name != "Morrowind.esm" {
                entry.grass = true;
            }
        }
        universe.write_into(&mut job);

        // The grass list is resolved in the order written, so a master must
        // precede anything that lists it in `MAST`. `load_order_key` puts every
        // ESM first. Deterministic too, so the scan's `HashMap` order never
        // reaches the byte-idempotent job writer.
        assert_eq!(job.grass_plugins, Some(vec![zzz, rem]));
    }
}
