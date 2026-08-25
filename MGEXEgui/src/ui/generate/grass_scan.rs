//! Background classification of the plugin universe as groundcover or not.
//!
//! Runs on a worker thread (too slow for a frame) and caches by file mtime +
//! length so re-scanning re-classifies only what actually changed. Both the
//! Grass and Plugins tabs consume the result.
//!
//! A verdict also depends on the plugin's masters and on the data-directory
//! list that resolves them, neither of which the stamp covers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::SystemTime;

use distantland::classify_grass_plugins;

/// Cache stamp: mtime + length, enough to detect rebuilds or swaps.
type FileStamp = (SystemTime, u64);

/// One classified plugin, as it comes back from the worker.
type Classified = (PathBuf, FileStamp, bool);

#[derive(Default)]
pub(crate) struct GrassScan {
    known: HashMap<PathBuf, (FileStamp, bool)>,
    /// `Some` only while a worker is in flight.
    rx: Option<Receiver<Vec<Classified>>>,
}

impl GrassScan {
    /// Classifies every path not already covered by an up-to-date cache entry.
    ///
    /// `data_dirs` is the layered data-directory list, lowest priority first.
    ///
    /// A scan already in flight is abandoned rather than joined: its results
    /// would describe the previous directory set, and the paths that survive the
    /// change are re-submitted here anyway. Dropping the receiver lets the old
    /// worker finish into a closed channel and exit.
    pub(crate) fn start(&mut self, paths: Vec<PathBuf>, data_dirs: Vec<PathBuf>) {
        let stale: Vec<(PathBuf, FileStamp)> = paths
            .into_iter()
            .filter_map(|path| {
                let stamp = file_stamp(&path)?;
                match self.known.get(&path) {
                    Some((cached, _)) if *cached == stamp => None,
                    _ => Some((path, stamp)),
                }
            })
            .collect();

        if stale.is_empty() {
            self.rx = None;
            return;
        }

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let paths: Vec<PathBuf> = stale.iter().map(|(path, _)| path.clone()).collect();
            let verdicts = classify_grass_plugins(&paths, &data_dirs);
            let classified = stale
                .into_iter()
                .zip(verdicts)
                .map(|((path, stamp), grass)| (path, stamp, grass))
                .collect();
            // A closed channel means the window went away or the scan was
            // superseded; either way there is nothing to report.
            let _ = sender.send(classified);
        });
        self.rx = Some(receiver);
    }

    /// Folds in a finished scan. Returns whether a worker is still running, which
    /// is what the caller needs to keep requesting repaints.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(rx) = self.rx.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(classified) => {
                for (path, stamp, grass) in classified {
                    self.known.insert(path, (stamp, grass));
                }
                self.rx = None;
                false
            }
            Err(TryRecvError::Empty) => true,
            // The worker panicked. Nothing is classified, which is the same
            // state the window opened in, so there is nothing to report.
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                false
            }
        }
    }

    /// Re-reads every path, ignoring the cache.
    ///
    /// The cache stamp catches a plugin that was rebuilt, but not one whose mod
    /// manager restored an identical mtime and length, and not a mistake in the
    /// heuristic the user wants to retest. It also cannot catch a master that
    /// changed, or a precedence change that moved which file supplies one, so a
    /// changed `data_dirs` must come through here rather than [`Self::start`].
    pub(crate) fn rescan(&mut self, paths: Vec<PathBuf>, data_dirs: Vec<PathBuf>) {
        self.known.clear();
        self.start(paths, data_dirs);
    }

    pub(crate) fn is_running(&self) -> bool {
        self.rx.is_some()
    }

    /// Whether `path` classified as groundcover. Unscanned paths answer `false`,
    /// so a page that renders before the scan lands simply shows no annotations.
    pub(crate) fn is_grass(&self, path: &Path) -> bool {
        self.known.get(path).is_some_and(|(_, grass)| *grass)
    }
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}
