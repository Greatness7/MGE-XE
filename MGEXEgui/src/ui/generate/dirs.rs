//! The `Data directories…` dialog, shared by the Plugins and Grass tabs.
//!
//! These directories supply BSAs, meshes, and textures in addition to plugins,
//! so a groundcover folder added here contributes its meshes too.
//!
//! A `Window` rather than a page state: it edits something the pages own and
//! returns to them.

use std::path::{Path, PathBuf};

use eframe::egui::{Align, Align2, Button, Frame, Layout, ScrollArea, Ui, Window};
use rust_i18n::t;

use crate::{plugins::same_path, style};

use super::GeneratorState;
use super::widgets::selectable_label;

/// Extra data directories, edited in a dialog over the page.
///
/// A working copy: `Save` hands it to [`crate::plugins::PluginUniverse::set_extra_dirs`]
/// and `Cancel` drops it, so a half-finished edit never reaches the scan.
pub(crate) struct PluginDirsEditor {
    dirs: Vec<PathBuf>,
    selected: Option<usize>,
    /// Advice about the directory just added, shown until the next action.
    /// Empty means none.
    notice: String,
}

impl PluginDirsEditor {
    pub(super) fn open(dirs: &[PathBuf]) -> Self {
        Self {
            dirs: dirs.to_vec(),
            selected: None,
            notice: String::new(),
        }
    }
}

pub(super) fn dialog(ui: &mut Ui, generator: &mut GeneratorState) {
    let Some(mut editor) = generator.dirs_editor.take() else {
        return;
    };
    let base = generator.universe.base_data_dir();
    let mut save = false;
    let mut close = false;

    Window::new(t!("generator.dirs.title"))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(420.0)
        .show(ui.ctx(), |ui| {
            style::hint(ui, t!("generator.dirs.hint"));
            ui.add_space(4.0);

            Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .stroke(ui.visuals().widgets.inactive.bg_stroke)
                .inner_margin(4)
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .id_salt("generator_plugin_dirs")
                        .auto_shrink([false, false])
                        .min_scrolled_height(160.0)
                        .max_height(160.0)
                        .show(ui, |ui| {
                            ui.set_min_width(400.0);
                            if editor.dirs.is_empty() {
                                style::hint(ui, t!("generator.dirs.none"));
                            }
                            for (index, dir) in editor.dirs.iter().enumerate() {
                                let selected = editor.selected == Some(index);
                                if selectable_label(ui, selected, dir.display().to_string()).clicked() {
                                    editor.selected = Some(index);
                                }
                            }
                        });
                });

            if !editor.notice.is_empty() {
                ui.add_space(4.0);
                ui.colored_label(style::WARN, &editor.notice);
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(t!("common.actions.add")).clicked() {
                    editor.notice.clear();
                    if let Some(picked) = rfd::FileDialog::new()
                        .set_title(t!("generator.dirs.add_title").as_ref())
                        .set_directory(&base)
                        .pick_folder()
                    {
                        // The base layer is always searched, and a repeat entry
                        // would just shadow itself. Both are silently ignored.
                        let known = same_path(&picked, &base) || editor.dirs.iter().any(|dir| same_path(dir, &picked));
                        if !known {
                            // Advice, not a rejection: a directory that
                            // contributes only meshes and textures is legitimate,
                            // and the overlay picks those up either way.
                            if !has_plugins(&picked) {
                                editor.notice = t!("generator.dirs.no_plugins").into_owned();
                            }
                            editor.dirs.push(picked);
                        }
                    }
                }
                if ui
                    .add_enabled(editor.selected.is_some(), Button::new(t!("common.actions.remove")))
                    .clicked()
                    && let Some(index) = editor.selected.take()
                    && index < editor.dirs.len()
                {
                    editor.notice.clear();
                    editor.dirs.remove(index);
                }
                if ui.button(t!("common.actions.clear")).clicked() {
                    editor.dirs.clear();
                    editor.selected = None;
                    editor.notice.clear();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(t!("common.actions.cancel")).clicked() {
                        close = true;
                    }
                    if ui.button(t!("common.actions.save")).clicked() {
                        save = true;
                    }
                });
            });
        });

    if save {
        generator.universe.set_extra_dirs(editor.dirs);
        // The universe just changed shape, so the classification has to catch up
        // with it. Cached verdicts survive; only new files are read.
        generator.grass_scan.start(generator.universe.plugin_paths());
    } else if !close {
        generator.dirs_editor = Some(editor);
    }
}

/// Whether `dir` holds a plugin at its top level.
///
/// Scanning is deliberately non-recursive, matching [`crate::plugins`]: BAIN
/// archives ship mutually-exclusive variant subfolders, and loading all of them
/// is worse than loading none.
fn has_plugins(dir: &Path) -> bool {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return false;
    };
    read_dir.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        name.ends_with(".esm") || name.ends_with(".esp")
    })
}
