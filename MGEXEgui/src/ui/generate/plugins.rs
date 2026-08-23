//! The generator's Plugins tab: the game's load order.
//!
//! Two deliberate departures:
//! - No `Continue` button, because the window is not a wizard.
//! - `Clear selected` is called **`Clear all`**, because the original cleared the
//!   whole list, not the highlighted row.
//!
//! Rows carry groundcover annotations from [`super::grass_scan`]. Both are
//! advisory: leaving a groundcover plugin in the load order loses most of its
//! placements.

use eframe::egui::{Align, Button, Frame, Layout, RichText, ScrollArea, Ui, Vec2, vec2};
use rust_i18n::t;

use crate::{
    plugins::{PluginUniverse, SortMode},
    style,
    ui::tooltip,
};

use super::GeneratorState;
use super::dirs::PluginDirsEditor;

/// Width of the fixed command column. The list takes everything else, as in the
/// original. The column's job is to keep its labels and ordering from moving.
const COLUMN_WIDTH: f32 = 156.0;

pub(super) fn page(ui: &mut Ui, generator: &mut GeneratorState) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(t!("generator.plugins.select"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let universe = &generator.universe;
            ui.label(
                RichText::new(t!(
                    "generator.plugins.selected_count",
                    selected = universe.enabled_count(),
                    total = universe.entries.len()
                ))
                .color(style::MUTED),
            );
        });
    });
    ui.add_space(4.0);

    // Fill the tab body while preserving its bottom spacing.
    let body_height = (ui.available_height() - 18.0).max(160.0);

    ui.horizontal_top(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let list_width = (ui.available_width() - COLUMN_WIDTH - gap).max(160.0);
        plugin_list(ui, generator, vec2(list_width, body_height));
        command_column(ui, generator, vec2(COLUMN_WIDTH, body_height));
    });

    super::dirs::dialog(ui, generator);
}

fn plugin_list(ui: &mut Ui, generator: &mut GeneratorState, size: Vec2) {
    // `allocate_ui` alone would inherit the caller's horizontal layout and lay the
    // rows out side by side.
    ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
        Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(ui.visuals().widgets.inactive.bg_stroke)
            .inner_margin(4)
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("generator_plugin_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if generator.universe.entries.is_empty() {
                            style::hint(ui, t!("generator.plugins.none_found"));
                            return;
                        }
                        let sync = generator.universe.sync;
                        for entry in &mut generator.universe.entries {
                            if entry.grass {
                                // Inert rather than hidden. The Grass tab is the
                                // authority over a clash, so this row is where
                                // the losing side of that rule is visible; it
                                // says where the plugin went rather than the
                                // plugin silently vanishing from the load order.
                                ui.horizontal(|ui| {
                                    let path = entry.full_path.display().to_string();
                                    tooltip(
                                        ui.add_enabled(false, eframe::egui::Checkbox::new(&mut false, &entry.file_name)),
                                        path,
                                    );
                                    ui.label(RichText::new(t!("generator.plugins.in_grass_list")).color(style::MUTED));
                                });
                                continue;
                            }

                            ui.horizontal(|ui| {
                                // Under sync the list is a read-out of the live
                                // load order, not an editor: a tick here would be
                                // discarded at commit, when the same order is
                                // re-derived. Shown disabled rather than as plain
                                // labels so the checked state still reads.
                                let path = entry.full_path.display().to_string();
                                tooltip(
                                    ui.add_enabled(!sync, eframe::egui::Checkbox::new(&mut entry.enabled, &entry.file_name)),
                                    path,
                                );
                                // Never a reason to hide the row: the heuristic is
                                // conservative, not authoritative. `WARN` rather
                                // than `MUTED` because leaving groundcover here
                                // loses most of its placements. Advisory, but a
                                // real consequence, and this label carries more
                                // weight under sync, when the fix is on the Grass
                                // tab and this row can no longer be unticked.
                                if generator.grass_scan.is_grass(&entry.full_path) {
                                    tooltip(
                                        ui.label(RichText::new(t!("generator.plugins.looks_like_grass")).color(style::WARN)),
                                        t!("generator.plugins.looks_like_grass_tip"),
                                    );
                                }
                            });
                        }
                    });
            });
    });
}

fn command_column(ui: &mut Ui, generator: &mut GeneratorState, size: Vec2) {
    ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
        ui.set_width(size.x);
        let button = vec2(size.x, ui.spacing().interact_size.y);
        let sync = generator.universe.sync;

        // Both edit the selection sync owns, so both go with the list.
        ui.add_enabled_ui(!sync, |ui| {
            if ui.add_sized(button, Button::new(t!("common.actions.select_all"))).clicked() {
                generator.universe.set_all(true);
            }
            if ui.add_sized(button, Button::new(t!("common.actions.clear_all"))).clicked() {
                generator.universe.set_all(false);
            }
        });
        if ui.add_sized(button, Button::new(t!("generator.dirs.button"))).clicked() {
            generator.dirs_editor = Some(PluginDirsEditor::open(&generator.universe.extra_dirs));
        }

        ui.add_space(6.0);
        sort_group(ui, &mut generator.universe, size.x);

        // The bottom of the column: sync, then the manual action it replaces.
        let rows = if sync { 1.0 } else { 2.0 };
        let gaps = (rows - 1.0) * ui.spacing().item_spacing.y;
        let remaining = ui.available_height() - rows * button.y - gaps;
        if remaining > 0.0 {
            ui.add_space(remaining);
        }
        if !sync
            && ui
                .add_sized(button, Button::new(t!("generator.plugins.use_load_order")))
                .clicked()
        {
            generator.universe.use_current_load_order();
        }
        let mut on = sync;
        if tooltip(
            ui.checkbox(&mut on, t!("generator.plugins.auto_sync")),
            t!("generator.plugins.auto_sync_tip"),
        )
        .changed()
        {
            generator.job.auto_sync_plugins = on;
            generator.universe.set_sync(on);
        }
    });
}

fn sort_group(ui: &mut Ui, universe: &mut PluginUniverse, width: f32) {
    Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(width - 2.0 * ui.spacing().item_spacing.x);
        ui.label(RichText::new(t!("generator.plugins.sort_view")).color(style::MUTED));
        // Display only: the saved plugin order is always the load order, never
        // whatever this view happens to show (`PluginUniverse::write_into`).
        let mut sort = universe.sort;
        tooltip(
            ui.radio_value(&mut sort, SortMode::Name, t!("generator.plugins.sort_name")),
            t!("generator.plugins.sort_tip"),
        );
        tooltip(
            ui.radio_value(&mut sort, SortMode::Type, t!("generator.plugins.sort_type")),
            t!("generator.plugins.sort_tip"),
        );
        tooltip(
            ui.radio_value(&mut sort, SortMode::LoadOrder, t!("generator.plugins.sort_load_order")),
            t!("generator.plugins.sort_tip"),
        );
        if sort != universe.sort {
            universe.set_sort(sort);
        }
    });
}
