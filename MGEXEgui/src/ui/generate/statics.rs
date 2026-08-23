//! The generator's Statics tab: what becomes a distant static, and how hard it
//! is simplified.
//!
//! Departures:
//! - The three mesh-simplification floats are `DragValue`-bound to `f32`, so
//!   they cannot become invalid; see [`super::widgets::float_spinner`] for why
//!   the bounds are what they are.
//! - `Add override file…` / `Remove selected` / `Edit list` sit together in one
//!   command column matching the Plugins page.
//! - **Duplicate override files are refused** at the click rather than only at
//!   Generate, matching the Plugins page's dedupe rule.

use std::path::PathBuf;

use distantland::{
    DEFAULT_STATIC_ATLAS_MAX_SIZE, GenerationSettings, SUPPORTED_STATIC_ATLAS_SIZES, SUPPORTED_STATIC_TEXTURE_LONG_SIZES,
    SUPPORTED_STATIC_TEXTURE_SHORT_SIZES,
};
use eframe::egui::{Align, Button, DragValue, Frame, Id, Layout, ScrollArea, Ui, Vec2, vec2};
use rust_i18n::t;

use crate::{
    platform, style,
    ui::{SPIN_W, spinner_width, tooltip},
};

use super::{
    GeneratorState,
    widgets::{capped_size_options, columns, columns_equal_height, field, float_spinner, selectable_label, size_combo},
};

/// Width of the size combos, shared with the Landscape page so the two read as
/// one control rather than two that happen to list numbers.
const SIZE_COMBO_W: f32 = 84.0;

/// Width of the override-list command column, matching the Plugins page's.
const COMMAND_WIDTH: f32 = 156.0;

/// Height of the override-file list, four rows at the current density.
const LIST_HEIGHT: f32 = 92.0;

pub(super) fn page(ui: &mut Ui, generator: &mut GeneratorState) {
    ScrollArea::vertical()
        .id_salt("generator_statics")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let GeneratorState {
                job,
                override_selected,
                error,
                max_texture_dimension,
                ..
            } = generator;
            let settings = &mut job.settings;

            ui.add_space(2.0);
            // One closure, not `split`'s two: both arms borrow the same
            // `settings`, which two closures could not do.
            columns_equal_height(
                ui,
                Id::new("generator_statics_cards"),
                2,
                |ui, index, min_height| match index {
                    0 => detail_card(ui, settings, min_height),
                    _ => include_card(ui, settings, min_height),
                },
            );
            ui.add_space(3.0);
            texture_card(ui, settings, *max_texture_dimension);
            ui.add_space(3.0);
            mesh_card(ui, settings);
            ui.add_space(6.0);
            override_group(ui, settings, override_selected, error);
        });
}

fn detail_card(ui: &mut Ui, settings: &mut GenerationSettings, min_height: f32) -> f32 {
    style::card_sized(ui, true, min_height, t!("generator.statics.detail"), |ui| {
        field(
            ui,
            t!("generator.statics.min_size").as_ref(),
            t!("generator.statics.min_size_hint").as_ref(),
            |ui| {
                spinner_width(ui, SPIN_W);
                // Stored as `f32` but edited in whole game units, rounded on
                // change so a fraction from a hand-edited job cannot survive a
                // touch.
                let response = ui.add(
                    DragValue::new(&mut settings.min_static_size)
                        .range(0.0..=65535.0)
                        .speed(1.0)
                        .fixed_decimals(0),
                );
                if response.changed() {
                    settings.min_static_size = settings.min_static_size.round();
                }
            },
        );
        ui.add_space(6.0);
        // The setting is a `0.0..=1.0` fraction, edited as the percentage its
        // label promises.
        field(
            ui,
            t!("generator.statics.grass_density").as_ref(),
            t!("generator.statics.grass_density_hint").as_ref(),
            |ui| {
                spinner_width(ui, SPIN_W);
                let mut percent = (settings.grass_density * 100.0).round();
                if ui
                    .add(DragValue::new(&mut percent).range(0.0..=100.0).speed(1.0).fixed_decimals(0))
                    .changed()
                {
                    settings.grass_density = percent / 100.0;
                }
            },
        );
    })
    .1
}

fn include_card(ui: &mut Ui, settings: &mut GenerationSettings, min_height: f32) -> f32 {
    style::card_sized(ui, true, min_height, t!("generator.statics.include"), |ui| {
        ui.checkbox(&mut settings.include_large_interiors, t!("generator.statics.large_interiors"));
        ui.checkbox(
            &mut settings.include_behaves_like_exterior,
            t!("generator.statics.behaves_exterior"),
        );
        ui.checkbox(
            &mut settings.include_interiors_with_water,
            t!("generator.statics.interiors_water"),
        );
        style::hint(ui, t!("generator.statics.load_time_hint"));
        tooltip(
            ui.checkbox(&mut settings.include_activators, t!("generator.statics.activators")),
            t!("generator.statics.activators_tip"),
        );
        tooltip(
            ui.checkbox(&mut settings.include_misc, t!("generator.statics.misc")),
            t!("generator.statics.misc_tip"),
        );
    })
    .1
}

/// The three atlas-sizing caps, side by side. Two texture caps rather than one
/// "max texture size", because a pre-made atlas stacks its sub-textures along one
/// axis: capping the longest side crushes every one of them, while capping the
/// short side leaves the stack intact. Short first, since it is the one that governs
/// ordinary near-square art, and the long axis only ever loosens it.
fn texture_card(ui: &mut Ui, settings: &mut GenerationSettings, max_texture_dimension: Option<u32>) {
    let atlas_options = capped_size_options(SUPPORTED_STATIC_ATLAS_SIZES, max_texture_dimension);
    style::card(ui, t!("generator.statics.textures"), |ui| {
        columns(ui, 3, |ui, index| match index {
            0 => field(
                ui,
                t!("generator.statics.max_texture_short").as_ref(),
                t!("generator.statics.max_texture_short_hint").as_ref(),
                |ui| {
                    size_combo(
                        ui,
                        "gen_static_texture_short",
                        &mut settings.max_static_texture_short_axis,
                        SUPPORTED_STATIC_TEXTURE_SHORT_SIZES,
                        SIZE_COMBO_W,
                    );
                    ui.label(t!("generator.units.texels"));
                },
            ),
            1 => field(
                ui,
                t!("generator.statics.max_texture_long").as_ref(),
                t!("generator.statics.max_texture_long_hint").as_ref(),
                |ui| {
                    size_combo(
                        ui,
                        "gen_static_texture_long",
                        &mut settings.max_static_texture_long_axis,
                        SUPPORTED_STATIC_TEXTURE_LONG_SIZES,
                        SIZE_COMBO_W,
                    );
                    ui.label(t!("generator.units.texels"));
                },
            ),
            _ => field(
                ui,
                t!("generator.statics.max_atlas").as_ref(),
                t!(
                    "generator.statics.max_atlas_hint",
                    max = DEFAULT_STATIC_ATLAS_MAX_SIZE.to_string()
                )
                .as_ref(),
                |ui| {
                    size_combo(
                        ui,
                        "gen_static_atlas_size",
                        &mut settings.max_static_atlas_size,
                        &atlas_options,
                        SIZE_COMBO_W,
                    );
                    ui.label(t!("generator.units.texels"));
                },
            ),
        });
    });
}

fn mesh_card(ui: &mut Ui, settings: &mut GenerationSettings) {
    style::card(ui, t!("generator.statics.mesh"), |ui| {
        columns(ui, 3, |ui, index| match index {
            0 => field(
                ui,
                t!("generator.statics.target_error").as_ref(),
                t!("generator.statics.target_error_hint").as_ref(),
                |ui| float_spinner(ui, &mut settings.static_mesh_target_error, 0.0, 0.01),
            ),
            1 => field(
                ui,
                t!("generator.statics.normals_weight").as_ref(),
                t!("generator.statics.normals_weight_hint").as_ref(),
                |ui| float_spinner(ui, &mut settings.static_mesh_normal_weight, 0.0, 0.01),
            ),
            _ => field(
                ui,
                t!("generator.statics.colors_weight").as_ref(),
                t!("generator.statics.colors_weight_hint").as_ref(),
                |ui| float_spinner(ui, &mut settings.static_mesh_color_weight, 0.0, 0.01),
            ),
        });
    });
}

/// The override-list group: a master checkbox captioning a list of files and the
/// three commands that act on it.
fn override_group(ui: &mut Ui, settings: &mut GenerationSettings, selected: &mut Option<usize>, error: &mut String) {
    tooltip(
        ui.checkbox(&mut settings.use_override_list, t!("generator.statics.use_overrides")),
        t!("generator.statics.use_overrides_tip"),
    );
    ui.add_space(4.0);

    style::card_enabled(ui, settings.use_override_list, t!("generator.statics.override_lists"), |ui| {
        ui.horizontal_top(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let list_width = (ui.available_width() - COMMAND_WIDTH - gap).max(160.0);
            override_list(ui, &settings.override_files, selected, vec2(list_width, LIST_HEIGHT));
            override_commands(ui, settings, selected, error, vec2(COMMAND_WIDTH, LIST_HEIGHT));
        });
    });
}

fn override_list(ui: &mut Ui, files: &[PathBuf], selected: &mut Option<usize>, size: Vec2) {
    ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
        Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(ui.visuals().widgets.inactive.bg_stroke)
            .inner_margin(4)
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("generator_override_files")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if files.is_empty() {
                            style::hint(ui, t!("generator.statics.no_overrides"));
                            return;
                        }
                        for (index, path) in files.iter().enumerate() {
                            // The row shows the file name while the setting keeps
                            // the full path; the path is the tooltip, so two
                            // files of the same name in different folders stay
                            // tellable apart.
                            let name = path
                                .file_name()
                                .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
                                .into_owned();
                            if selectable_label(ui, *selected == Some(index), name)
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                *selected = Some(index);
                            }
                        }
                    });
            });
    });
}

fn override_commands(
    ui: &mut Ui,
    settings: &mut GenerationSettings,
    selected: &mut Option<usize>,
    error: &mut String,
    size: Vec2,
) {
    ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
        ui.set_width(size.x);
        let button = vec2(size.x, ui.spacing().interact_size.y);
        // A stale index survives a `Remove` on a shorter list, so re-check the
        // bound rather than trusting the stored selection.
        let row = selected.filter(|index| *index < settings.override_files.len());

        if ui
            .add_sized(button, Button::new(t!("generator.statics.add_override")))
            .clicked()
            && let Some(picked) = rfd::FileDialog::new()
                .set_title(t!("generator.statics.add_override_title").as_ref())
                .add_filter(t!("generator.statics.override_filter").as_ref(), &["ovr", "txt", "toml"])
                .add_filter(t!("generator.statics.all_files").as_ref(), &["*"])
                .pick_file()
        {
            let duplicate = settings
                .override_files
                .iter()
                .any(|existing| existing.to_string_lossy().eq_ignore_ascii_case(&picked.to_string_lossy()));
            if duplicate {
                // Silently dropping it would leave the user looking at a list
                // that already appears correct.
                *error = t!("generator.statics.duplicate_override", path = picked.display().to_string()).into_owned();
            } else {
                error.clear();
                settings.override_files.push(picked);
                *selected = Some(settings.override_files.len() - 1);
            }
        }

        ui.add_enabled_ui(row.is_some(), |ui| {
            if ui
                .add_sized(button, Button::new(t!("generator.statics.remove_selected")))
                .clicked()
                && let Some(index) = row
            {
                settings.override_files.remove(index);
                *selected = None;
            }
            if tooltip(
                ui.add_sized(button, Button::new(t!("generator.statics.edit_list"))),
                t!("generator.statics.edit_list_hint"),
            )
            .clicked()
                && let Some(index) = row
                && let Err(failure) = platform::open_with_shell(&settings.override_files[index])
            {
                *error = format!("{failure:#}");
            }
        });
    });
}
