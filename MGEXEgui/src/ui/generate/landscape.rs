//! The generator's Landscape tab: the terrain half of the output.
//!
//! Two deliberate departures:
//! - **No `Auto` terrain-detail item.** It was a UI-only sentinel that mapped
//!   straight to `TerrainDetail::High` and never persisted, so in egui, where
//!   the combo binds to `settings.terrain_detail` itself, keeping it would
//!   cost a session-only flag. `High` is `TerrainDetail::default()`, so a
//!   fresh job lands where `Auto` did.
//! - The master checkbox is drawn **above** the group instead of over its top
//!   border. That Win32 convention has no egui equivalent, and `card_enabled`
//!   already greys a group's caption along with its body.

use distantland::{
    DEFAULT_TERRAIN_ATLAS_MAX_SIZE, GenerationSettings, SUPPORTED_TERRAIN_ATLAS_SIZES, SUPPORTED_TERRAIN_TEXTURE_SIZES,
    TerrainDetail,
};
use eframe::egui::{ComboBox, Ui};
use rust_i18n::t;

use crate::{
    style,
    ui::{split, tooltip},
};

use super::{
    GeneratorState,
    widgets::{capped_size_options, field, selectable_value, size_combo},
};

/// Width of the two power-of-two size combos. Sized for `16384` plus the arrow,
/// and shared so the left column's two rows land on one edge.
const SIZE_COMBO_W: f32 = 84.0;

/// The detail combo width. Pinned rather than stretched: a combo left to fill a
/// `split` column re-flows every time the window is resized while its content
/// never needs the room.
const DETAIL_COMBO_W: f32 = 150.0;

/// Coarse to fine; deliberately not the enum's declaration order reversed.
const DETAIL_ORDER: [TerrainDetail; 5] = [
    TerrainDetail::UltraHigh,
    TerrainDetail::VeryHigh,
    TerrainDetail::High,
    TerrainDetail::Medium,
    TerrainDetail::Low,
];

pub(super) fn page(ui: &mut Ui, generator: &mut GeneratorState) {
    let atlas_options = capped_size_options(SUPPORTED_TERRAIN_ATLAS_SIZES, generator.max_texture_dimension);
    // The four fields the page owns, taken apart so the two column closures hold
    // disjoint borrows of the settings.
    let GenerationSettings {
        generate_terrain,
        max_terrain_texture_size,
        max_terrain_atlas_size,
        terrain_detail,
        ..
    } = &mut generator.job.settings;

    ui.add_space(2.0);
    // The page's master switch: it only gates the group, and never clears or
    // resets the values inside it.
    tooltip(
        ui.checkbox(generate_terrain, t!("generator.landscape.generate")),
        t!("generator.landscape.generate_tip"),
    );
    ui.add_space(4.0);

    style::card_enabled(ui, *generate_terrain, t!("generator.landscape.terrain"), |ui| {
        split(
            ui,
            |ui| {
                field(
                    ui,
                    t!("generator.landscape.max_texture").as_ref(),
                    t!("generator.landscape.max_texture_hint").as_ref(),
                    |ui| {
                        size_combo(
                            ui,
                            "gen_terrain_texture_size",
                            max_terrain_texture_size,
                            SUPPORTED_TERRAIN_TEXTURE_SIZES,
                            SIZE_COMBO_W,
                        );
                        ui.label(t!("generator.units.texels"));
                    },
                );
                ui.add_space(6.0);
                field(
                    ui,
                    t!("generator.landscape.max_atlas").as_ref(),
                    t!(
                        "generator.landscape.max_atlas_hint",
                        max = DEFAULT_TERRAIN_ATLAS_MAX_SIZE.to_string()
                    )
                    .as_ref(),
                    |ui| {
                        size_combo(
                            ui,
                            "gen_terrain_atlas_size",
                            max_terrain_atlas_size,
                            &atlas_options,
                            SIZE_COMBO_W,
                        );
                        ui.label(t!("generator.units.texels"));
                    },
                );
            },
            |ui| {
                field(
                    ui,
                    t!("generator.landscape.detail").as_ref(),
                    t!("generator.landscape.detail_hint").as_ref(),
                    |ui| detail_combo(ui, terrain_detail),
                );
            },
        );
    });
}

fn detail_combo(ui: &mut Ui, value: &mut TerrainDetail) {
    ComboBox::from_id_salt("gen_terrain_detail")
        .selected_text(t!(detail_label(*value)))
        .icon(style::combo_arrow_icon)
        .width(DETAIL_COMBO_W)
        .show_ui(ui, |ui| {
            for preset in DETAIL_ORDER {
                selectable_value(ui, value, preset, t!(detail_label(preset)));
            }
        });
}

/// Localization keys for each preset. `TerrainDetail`'s own `Display` is the
/// snake_case serialization tag the fingerprint is built from, not a label. A
/// match keeps the two apart and makes a new preset a compile error here.
fn detail_label(detail: TerrainDetail) -> &'static str {
    match detail {
        TerrainDetail::UltraHigh => "generator.quality.ultra_high",
        TerrainDetail::VeryHigh => "generator.quality.very_high",
        TerrainDetail::High => "generator.quality.high",
        TerrainDetail::Medium => "generator.quality.medium",
        TerrainDetail::Low => "generator.quality.low",
    }
}
