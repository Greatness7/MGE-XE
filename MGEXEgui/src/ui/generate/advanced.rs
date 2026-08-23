//! The generator's Advanced tab: build options, control-map budgets, terrain
//! mesh weights, and static texture sizing.
//!
//! Every row shares one form: a fixed label column, a control on a common
//! alignment line, then a full-width hint ([`super::widgets::row`]).
//!
//! Two behaviours worth knowing:
//!
//! - **Clamped values survive if untouched.** An over-cap control-map dimension
//!   or non-MiB-aligned byte budget is preserved unless the user edits the field.
//!   Reading the display value into a local and writing back only on `changed()`
//!   reproduces that. Do not "simplify" these rows into direct bindings.
//! - **Sizing mode gates nothing.** With `Mode = Off` the three subordinate
//!   fields stay editable; `validate` checks them against the mode at
//!   save/generate time instead. Deliberate parity, not an oversight.

use distantland::{
    GenerationSettings, MAX_TERRAIN_CONTROL_TEXTURE_SIZE, SUPPORTED_STATIC_TEXTURE_SHORT_SIZES, StaticTextureSizingMode,
    TextureDedupeMode,
};
use eframe::egui::{ComboBox, DragValue, ScrollArea, Ui};
use rust_i18n::t;

use crate::{
    style,
    ui::{SPIN_W, spinner_width},
};

use super::{
    GeneratorState,
    widgets::{float_spinner, row, selectable_value, size_combo},
};

/// Width of the two preset combos on this page.
const COMBO_W: f32 = 150.0;

/// Width of the minimum-texture-size combo, matching the size combos elsewhere.
const SIZE_COMBO_W: f32 = 84.0;

const MIB: u64 = 1024 * 1024;

/// Absolute spinner ceiling, used only when the adapter's real max texture
/// dimension could not be detected. A loaded value above the effective cap is
/// displayed clamped but only written back if the user edits the field.
const MAX_CONTROL_TEXTURE_SIZE: u32 = 65536;

/// A merge radius of zero is rejected by `validate` (`> 0.0`), and a sub-unit
/// radius is meaningless in game units, so the spinner floor is one unit rather
/// than an unusable epsilon.
const MIN_MERGE_RADIUS: f64 = 1.0;

pub(super) fn page(ui: &mut Ui, generator: &mut GeneratorState) {
    ScrollArea::vertical()
        .id_salt("generator_advanced")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let max_texture_dimension = generator.max_texture_dimension;
            let settings = &mut generator.job.settings;
            ui.add_space(2.0);
            build_options_card(ui, settings);
            ui.add_space(3.0);
            control_map_card(ui, settings, max_texture_dimension);
            ui.add_space(3.0);
            terrain_mesh_card(ui, settings);
            ui.add_space(3.0);
            texture_sizing_card(ui, settings);
        });
}

fn build_options_card(ui: &mut Ui, settings: &mut GenerationSettings) {
    style::card(ui, t!("generator.advanced.build_options"), |ui| {
        row(
            ui,
            t!("generator.advanced.texture_dedupe").as_ref(),
            t!("generator.advanced.texture_dedupe_hint").as_ref(),
            |ui| {
                ComboBox::from_id_salt("gen_texture_dedupe")
                    .selected_text(t!(dedupe_label(settings.texture_dedupe_mode)))
                    .icon(style::combo_arrow_icon)
                    .width(COMBO_W)
                    .show_ui(ui, |ui| {
                        for mode in [TextureDedupeMode::Off, TextureDedupeMode::Exact] {
                            selectable_value(ui, &mut settings.texture_dedupe_mode, mode, t!(dedupe_label(mode)));
                        }
                    });
            },
        );
        row(
            ui,
            t!("generator.advanced.door_multiplier").as_ref(),
            t!("generator.advanced.door_multiplier_hint").as_ref(),
            |ui| float_spinner(ui, &mut settings.door_size_multiplier, 1.0, 0.01),
        );
        row(
            ui,
            t!("generator.advanced.merge_radius").as_ref(),
            t!("generator.advanced.merge_radius_hint").as_ref(),
            |ui| float_spinner(ui, &mut settings.merge_group_radius, MIN_MERGE_RADIUS, 16.0),
        );
        row(
            ui,
            t!("generator.advanced.merge_error").as_ref(),
            t!("generator.advanced.merge_error_hint").as_ref(),
            |ui| float_spinner(ui, &mut settings.static_mesh_merge_error_multiplier, 1.0, 0.01),
        );
    });
}

fn control_map_card(ui: &mut Ui, settings: &mut GenerationSettings, max_texture_dimension: Option<u32>) {
    let cap = max_texture_dimension
        .unwrap_or(MAX_CONTROL_TEXTURE_SIZE)
        .min(MAX_CONTROL_TEXTURE_SIZE);
    style::card(ui, t!("generator.advanced.control_map"), |ui| {
        row(
            ui,
            t!("generator.advanced.max_control_size").as_ref(),
            t!(
                "generator.advanced.max_control_size_hint",
                max = MAX_TERRAIN_CONTROL_TEXTURE_SIZE.to_string()
            )
            .as_ref(),
            |ui| {
                spinner_width(ui, SPIN_W);
                // Displayed clamped, written back only on an edit, so a loaded
                // value above the cap survives an untouched save.
                let mut size = settings.max_terrain_control_texture_size.min(cap);
                if ui.add(DragValue::new(&mut size).range(1..=cap)).changed() {
                    settings.max_terrain_control_texture_size = size;
                }
                ui.label(t!("generator.units.texels"));
            },
        );
        row(
            ui,
            t!("generator.advanced.control_memory").as_ref(),
            t!("generator.advanced.control_memory_hint").as_ref(),
            |ui| {
                spinner_width(ui, SPIN_W);
                // Persisted in bytes, edited in whole MiB. Rounding up for
                // display and writing back only on change is what preserves an
                // exact non-MiB-aligned budget through an untouched save.
                let mut mib = settings.max_terrain_control_texture_bytes.div_ceil(MIB).max(1);
                if ui.add(DragValue::new(&mut mib).range(1..=u64::from(u32::MAX))).changed() {
                    settings.max_terrain_control_texture_bytes = mib * MIB;
                }
                ui.label(t!("generator.units.mib"));
            },
        );
    });
}

fn terrain_mesh_card(ui: &mut Ui, settings: &mut GenerationSettings) {
    style::card(ui, t!("generator.advanced.terrain_mesh"), |ui| {
        // Stacked full-width: a two-column layout does not fit half this window
        // at its minimum width, and the page scrolls anyway, so vertical space
        // is the cheap axis here.
        row(
            ui,
            t!("generator.advanced.smoothed_normals").as_ref(),
            t!("generator.advanced.smoothed_normals_hint").as_ref(),
            |ui| float_spinner(ui, &mut settings.terrain_mesh_smoothed_normal_weight, 0.0, 1.0),
        );
        row(
            ui,
            t!("generator.advanced.vertex_colors").as_ref(),
            t!("generator.advanced.vertex_colors_hint").as_ref(),
            |ui| float_spinner(ui, &mut settings.terrain_mesh_color_weight, 0.0, 1.0),
        );
    });
}

fn texture_sizing_card(ui: &mut Ui, settings: &mut GenerationSettings) {
    style::card(ui, t!("generator.advanced.texture_sizing"), |ui| {
        let sizing = &mut settings.static_texture_sizing;
        row(
            ui,
            t!("generator.advanced.sizing_mode").as_ref(),
            t!("generator.advanced.sizing_mode_hint").as_ref(),
            |ui| {
                ComboBox::from_id_salt("gen_sizing_mode")
                    .selected_text(t!(sizing_label(sizing.mode)))
                    .icon(style::combo_arrow_icon)
                    .width(COMBO_W)
                    .show_ui(ui, |ui| {
                        for mode in [
                            StaticTextureSizingMode::Off,
                            StaticTextureSizingMode::Report,
                            StaticTextureSizingMode::DownscaleOpaque,
                            StaticTextureSizingMode::Downscale,
                        ] {
                            selectable_value(ui, &mut sizing.mode, mode, t!(sizing_label(mode)));
                        }
                    });
            },
        );
        row(
            ui,
            t!("generator.advanced.protected_density").as_ref(),
            t!("generator.advanced.protected_density_hint").as_ref(),
            |ui| float_spinner(ui, &mut sizing.protected_density, 0.0, 0.01),
        );
        row(
            ui,
            t!("generator.advanced.min_texture_size").as_ref(),
            t!("generator.advanced.min_texture_size_hint").as_ref(),
            |ui| {
                size_combo(
                    ui,
                    "gen_sizing_min_size",
                    &mut sizing.min_texture_size,
                    SUPPORTED_STATIC_TEXTURE_SHORT_SIZES,
                    SIZE_COMBO_W,
                );
                ui.label(t!("generator.units.texels"));
            },
        );
        row(
            ui,
            t!("generator.advanced.max_mip_reduction").as_ref(),
            t!("generator.advanced.max_mip_reduction_hint").as_ref(),
            |ui| {
                spinner_width(ui, SPIN_W);
                ui.add(DragValue::new(&mut sizing.max_mip_reduction).range(0..=6));
            },
        );
    });
}

fn dedupe_label(mode: TextureDedupeMode) -> &'static str {
    match mode {
        TextureDedupeMode::Off => "common.choices.off",
        TextureDedupeMode::Exact => "generator.advanced.exact",
    }
}

fn sizing_label(mode: StaticTextureSizingMode) -> &'static str {
    match mode {
        StaticTextureSizingMode::Off => "common.choices.off",
        StaticTextureSizingMode::Report => "generator.advanced.report_only",
        StaticTextureSizingMode::DownscaleOpaque => "generator.advanced.downscale_opaque",
        StaticTextureSizingMode::Downscale => "generator.advanced.downscale",
    }
}
