//! The generator's Grass tab: generator-only groundcover plugins.
//!
//! A framed list with a fixed command column, one grey hint above and one below.
//! Picking a plugin here removes it from the load order (`enforce_grass_wins`).
//! Rows are always live; the Plugins tab shows the losing side as an inert row.
//!
//! The list is **filtered**, not complete: it shows what [`super::grass_scan`]
//! classified as groundcover, plus whatever the user has already picked. Showing
//! every plugin behind a `Select suggested` button made the user trigger the
//! classification by hand and then re-read a list they had no reason to review;
//! the filter is that button's result, applied up front. Keeping the user's own
//! picks in the filter is what lets the heuristic be wrong without the selection
//! vanishing, and it costs no persisted state.
//!
//! Nothing is pre-checked. A saved selection is restored verbatim and an
//! unconfigured job opens with everything clear.
//!
//! Order is meaningful: the list is its own load order, and
//! [`crate::plugins::PluginUniverse::write_into`] emits
//! it by load-order key so a grass master precedes its dependents.

use eframe::egui::{Align, Button, Frame, Layout, RichText, ScrollArea, Ui, Vec2, vec2};
use rust_i18n::t;

use crate::{plugins::SortMode, style, ui::tooltip};

use super::GeneratorState;
use super::dirs::PluginDirsEditor;

/// Matches the Plugins page's column so the two tabs' lists line up.
const COLUMN_WIDTH: f32 = 156.0;

pub(super) fn page(ui: &mut Ui, generator: &mut GeneratorState) {
    let visible = visible_rows(generator);

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(t!("generator.grass.select"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Counted against the filtered list, not the whole universe: "3 of
            // 900 selected" says nothing, and the other 897 are not offered here.
            ui.label(
                RichText::new(t!(
                    "generator.grass.selected_count",
                    selected = generator.universe.grass_count(),
                    total = visible.len()
                ))
                .color(style::MUTED),
            );
        });
    });
    ui.add_space(4.0);

    // Above the list rather than beside the footer: this hint is the page's
    // explanation and runs to several lines in the wordier locales, and drawing
    // it first lets the list take whatever height is left instead of the layout
    // having to predict how tall it will be.
    style::hint(ui, t!("generator.grass.hint"));
    ui.add_space(4.0);

    // Two lines of footer: the closing hint names a button and already wraps
    // in French and Russian.
    let hint_height = ui.text_style_height(&eframe::egui::TextStyle::Body);
    let body_height = (ui.available_height() - 2.0 * hint_height - 22.0).max(160.0);

    ui.horizontal_top(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let list_width = (ui.available_width() - COLUMN_WIDTH - gap).max(160.0);
        candidate_list(ui, generator, &visible, vec2(list_width, body_height));
        command_column(ui, generator, vec2(COLUMN_WIDTH, body_height));
    });

    ui.add_space(4.0);
    style::hint(ui, t!("generator.grass.standard_usage"));

    super::dirs::dialog(ui, generator);
}

/// Indices into `universe.entries` for the rows this page offers, in view order.
///
/// The filter is "classified as groundcover **or** already picked". The second
/// term is what keeps a selection visible when the heuristic disagrees with it;
/// ordinary mods fail both and never appear. A row that also sits in the game's
/// load order survives the filter and renders inert. Since the two selections
/// are mutually exclusive, that set is exactly "groundcover accidentally left in
/// the load order", which is the one case worth surfacing here.
fn visible_rows(generator: &GeneratorState) -> Vec<usize> {
    let entries = &generator.universe.entries;
    let mut rows: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.grass || generator.grass_scan.is_grass(&entry.full_path))
        .map(|(index, _)| index)
        .collect();

    // An index projection rather than `apply_sort`, which reorders `entries` in
    // place and would silently reorder the Plugins tab's view as well.
    match generator.grass_sort {
        SortMode::Name => rows.sort_by_key(|&row| entries[row].sort_key_name()),
        SortMode::Type => rows.sort_by_key(|&row| entries[row].sort_key_type()),
        SortMode::LoadOrder => rows.sort_by_key(|&row| entries[row].sort_key_load_order()),
    }
    rows
}

fn candidate_list(ui: &mut Ui, generator: &mut GeneratorState, visible: &[usize], size: Vec2) {
    ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
        Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(ui.visuals().widgets.inactive.bg_stroke)
            .inner_margin(4)
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("generator_grass_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if visible.is_empty() {
                            empty_state(ui, generator);
                            return;
                        }
                        // Always live, and never inert for being in the load
                        // order: this tab is the authority now (`enforce_grass_wins`),
                        // so picking a plugin here takes it *out* of the load
                        // order rather than being refused because it is in one.
                        // That is also the only way to reach an active
                        // groundcover plugin once the Plugins tab is read-only.
                        let mut changed = false;
                        for &index in visible {
                            let entry = &mut generator.universe.entries[index];
                            let path = entry.full_path.display().to_string();
                            changed |= tooltip(ui.checkbox(&mut entry.grass, &entry.file_name), path).changed();
                        }
                        if changed {
                            generator.universe.refresh_enabled();
                        }
                    });
            });
    });
}

/// What an empty list means depends on whether the scan has finished, and the
/// two readings call for opposite actions, so the page says which it is rather
/// than letting rows appear unannounced a moment after the window opens.
fn empty_state(ui: &mut Ui, generator: &GeneratorState) {
    if generator.grass_scan.is_running() {
        ui.horizontal(|ui| {
            ui.spinner();
            style::hint(ui, t!("generator.grass.scanning"));
        });
    } else {
        // Names the way out: groundcover normally lives outside `Data Files`, so
        // an empty list most often means its directory was never registered.
        style::hint(ui, t!("generator.grass.none_found"));
    }
}

fn command_column(ui: &mut Ui, generator: &mut GeneratorState, size: Vec2) {
    ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
        ui.set_width(size.x);
        let button = vec2(size.x, ui.spacing().interact_size.y);

        // `add_enabled_ui` rather than `add_enabled`, so the label still centres:
        // `add_sized` centres its content where a `min_size` on the button alone
        // would leave the text left-aligned against its neighbours.
        let rescan = ui
            .add_enabled_ui(!generator.grass_scan.is_running(), |ui| {
                ui.add_sized(button, Button::new(t!("generator.grass.rescan"))).clicked()
            })
            .inner;
        if rescan {
            generator
                .grass_scan
                .rescan(generator.universe.plugin_paths(), generator.universe.data_dirs());
        }
        if ui.add_sized(button, Button::new(t!("common.actions.clear_all"))).clicked() {
            for entry in &mut generator.universe.entries {
                entry.grass = false;
            }
            // Sync mode: the plugins these picks were holding out of the load
            // order rejoin it, so the Plugins tab is right again next frame.
            generator.universe.refresh_enabled();
        }

        if ui.add_sized(button, Button::new(t!("generator.dirs.button"))).clicked() {
            generator.dirs_editor = Some(PluginDirsEditor::open(&generator.universe.extra_dirs));
        }

        ui.add_space(6.0);
        sort_group(ui, generator, size.x);
    });
}

/// The Plugins page's sort control over this page's own [`SortMode`].
///
/// `by load order` is not decoration here: grass plugins can be masters, and the
/// masters-first component of that key is what decides resolution inside the
/// list, so this view can be made to match what gets written.
fn sort_group(ui: &mut Ui, generator: &mut GeneratorState, width: f32) {
    Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(width - 2.0 * ui.spacing().item_spacing.x);
        ui.label(RichText::new(t!("generator.plugins.sort_view")).color(style::MUTED));
        let sort = &mut generator.grass_sort;
        tooltip(
            ui.radio_value(sort, SortMode::Name, t!("generator.plugins.sort_name")),
            t!("generator.grass.sort_tip"),
        );
        tooltip(
            ui.radio_value(sort, SortMode::Type, t!("generator.plugins.sort_type")),
            t!("generator.grass.sort_tip"),
        );
        tooltip(
            ui.radio_value(sort, SortMode::LoadOrder, t!("generator.plugins.sort_load_order")),
            t!("generator.grass.sort_tip"),
        );
    });
}
