//! The Distant Land settings tab.

use eframe::egui::{self, Align, DragValue, Layout, RichText, Ui};
use mge_config::{
    DRAW_DISTANCE_RANGE, DistantLandSettings, FAR_STATIC_END_RANGE, FOG_ABOVE_END_RANGE, FOG_ABOVE_START_RANGE,
    FOG_BELOW_END_RANGE, FOG_BELOW_START_RANGE, FOG_INTERIOR_END_RANGE, FOG_INTERIOR_START_RANGE, FogSettings, GuiSettings,
    NEAR_STATIC_END_RANGE, STATIC_MIN_SIZE_RANGE, Settings, VERY_FAR_STATIC_END_RANGE, WaterSettings,
};
use rust_i18n::t;

use crate::{app::GuiApp, config, distant::DistantLandStatus, style};

use crate::ui::{
    SPIN_W, caption_row, check_row, combo_index_localized_sized, combo_value_localized_sized, labeled_row, range_header,
    range_row, right_aligned, spin_row, spinner_width, split, tooltip, value_field,
};

// Display-only labels; both settings are persisted by index/enum rather than caption.
const SHADOW_DETAIL: [&str; 2] = ["distant.lighting.medium_detail", "distant.lighting.high_detail"];

// Index is `gui.auto_distance_mode`; the two arrays must stay in step.
const AUTO_DISTANCE_MODES: [&str; 3] = [
    "distant.automatic.by_draw_distance",
    "distant.automatic.by_fog_end",
    "distant.automatic.by_no_pop",
];
const AUTO_DISTANCE_MODE_TIPS: [&str; 3] = [
    "distant.automatic.by_draw_distance_tip",
    "distant.automatic.by_fog_end_tip",
    "distant.automatic.by_no_pop_tip",
];

#[derive(Clone, Copy)]
struct DistantEnablement {
    distant_land: bool,
    auto_distances: bool,
    manual: bool,
    statics: bool,
    draw_distance: bool,
    fog_above: bool,
    fog_interior: bool,
    reflect_land: bool,
    ripples: bool,
    exponential_fog: bool,
    sun_shadows: bool,
    per_pixel: bool,
}

impl DistantEnablement {
    fn from_settings(settings: &Settings) -> Self {
        let distant = &settings.distant_land;
        let auto_distances = settings.gui.auto_distances;
        // Both fog-based modes leave the fog spinners live and derive draw distance from them.
        let by_fog_end = matches!(settings.gui.auto_distance_mode, 1 | 2);
        let manual = !auto_distances;
        let statics = distant.statics;
        Self {
            distant_land: distant.enabled,
            auto_distances,
            manual,
            statics,
            draw_distance: manual || !by_fog_end,
            fog_above: manual || by_fog_end,
            fog_interior: manual && statics,
            reflect_land: distant.water.reflect_land,
            ripples: distant.water.dynamic_ripples,
            exponential_fog: distant.fog.exponential,
            sun_shadows: distant.shadows.enabled,
            per_pixel: distant.per_pixel_lighting,
        }
    }
}

/// A cell-distance spinner. The range constants are the schema's, so a bound
/// can never drift between this control and what `mge-config` will accept.
fn distance(value: &mut f32, range: (f32, f32)) -> DragValue<'_> {
    DragValue::new(value).range(range.0..=range.1).speed(0.1).fixed_decimals(1)
}

fn draw_distance_card(
    ui: &mut Ui,
    settings: &mut DistantLandSettings,
    enablement: DistantEnablement,
    generator_open: bool,
) -> bool {
    let mut open_generator = false;
    style::card(ui, t!("distant.draw_distance.title"), |ui| {
        // On its own row the card is two rows deep like `Automatic distances`
        // opposite, so the two columns start on a common line.
        //
        // Deliberately outside every distant-land enablement gate: you need the
        // generator exactly when distant land is not generated yet, which is
        // also when the rest of the tab is disabled. Disabled while the
        // generator window is open (it is non-modal, so the tab stays readable
        // behind it); the window being open is not the same as a run being in
        // flight.
        if tooltip(
            ui.add_enabled(!generator_open, egui::Button::new(t!("distant.draw_distance.generator"))),
            t!("distant.draw_distance.generator_tip"),
        )
        .clicked()
        {
            open_generator = true;
        }
        // Gate the whole row rather than just the spinner, so the caption
        // greys out with it. The other cards get this from the card-wide
        // `add_enabled_ui`, which this one cannot use because the generator
        // button must stay enabled when distant land is off.
        ui.add_enabled_ui(enablement.distant_land && enablement.draw_distance, |ui| {
            let row = ui.horizontal(|ui| {
                spinner_width(ui, SPIN_W);
                ui.add(distance(&mut settings.draw_distance, DRAW_DISTANCE_RANGE));
                ui.label(t!("distant.draw_distance.cells"));
            });
            tooltip(row.response, t!("distant.draw_distance.distance_tip"));
        });
    });

    open_generator
}

fn water_card(ui: &mut Ui, settings: &mut WaterSettings, enablement: DistantEnablement) {
    style::card_enabled(ui, enablement.distant_land, t!("distant.water.title"), |ui| {
        caption_row(ui, t!("distant.water.reflections").as_ref());
        split(
            ui,
            |ui| {
                tooltip(
                    ui.checkbox(&mut settings.reflect_sky, t!("distant.water.sky")),
                    t!("distant.water.sky_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.reflect_land, t!("distant.water.landscape")),
                    t!("distant.water.landscape_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.blur_reflections, t!("distant.water.blur_reflections")),
                    t!("distant.water.blur_reflections_tip"),
                );
            },
            |ui| {
                tooltip(
                    ui.add_enabled(
                        enablement.reflect_land && enablement.statics,
                        egui::Checkbox::new(&mut settings.reflect_near_statics, t!("distant.water.nearby_statics")),
                    ),
                    t!("distant.water.nearby_statics_tip"),
                );
                tooltip(
                    ui.add_enabled(
                        enablement.statics,
                        egui::Checkbox::new(&mut settings.reflect_interiors, t!("distant.water.interiors")),
                    ),
                    t!("distant.water.interiors_tip"),
                );
            },
        );
        ui.separator();
        tooltip(
            ui.checkbox(&mut settings.dynamic_ripples, t!("distant.water.dynamic_ripples")),
            t!("distant.water.dynamic_ripples_tip"),
        );
        tooltip(
            spin_row(
                ui,
                t!("distant.water.wave_height").as_ref(),
                enablement.ripples,
                egui::DragValue::new(&mut settings.wave_height).range(0..=250),
            ),
            t!("distant.water.wave_height_tip"),
        );
        tooltip(
            spin_row(
                ui,
                t!("distant.water.caustics").as_ref(),
                true,
                egui::DragValue::new(&mut settings.caustics_intensity)
                    .range(0..=100)
                    .suffix(" %"),
            ),
            t!("distant.water.caustics_tip"),
        );
    });
}

fn lighting_and_shadows_card(
    ui: &mut Ui,
    settings: &mut DistantLandSettings,
    enablement: DistantEnablement,
    open_lighting: &mut bool,
) {
    style::card_enabled(ui, enablement.distant_land, t!("distant.lighting.title"), |ui| {
        // The schema stores a resolution; this control has always offered two.
        let mut detail = usize::from(settings.shadows.map_resolution >= 2048);
        let (shadow_checkbox, shadow_combo) = check_row(
            ui,
            &mut settings.shadows.enabled,
            t!("distant.lighting.solar_shadows").as_ref(),
            |ui| {
                ui.add_enabled_ui(enablement.sun_shadows, |ui| {
                    combo_index_localized_sized(ui, "shadow_map_resolution", &mut detail, SHADOW_DETAIL, Some(110.0))
                })
                .inner
            },
        );
        settings.shadows.map_resolution = if detail == 0 { 1024 } else { 2048 };
        tooltip(shadow_checkbox, t!("distant.lighting.solar_shadows_tip"));
        if let Some(response) = shadow_combo {
            tooltip(response, t!("distant.lighting.shadow_detail_tip"));
        }
        let (per_pixel_checkbox, mode_combo) = check_row(
            ui,
            &mut settings.per_pixel_lighting,
            t!("distant.lighting.per_pixel").as_ref(),
            |ui| {
                ui.add_enabled_ui(enablement.per_pixel, |ui| {
                    combo_value_localized_sized(
                        ui,
                        "ppl_mode",
                        &mut settings.per_pixel_mode,
                        &config::PPL_VALUES,
                        Some(110.0),
                    )
                })
                .inner
            },
        );
        tooltip(per_pixel_checkbox, t!("distant.lighting.per_pixel_tip"));
        if let Some(response) = mode_combo {
            tooltip(response, t!("distant.lighting.mode_tip"));
        }
        right_aligned(ui, |ui| {
            if tooltip(
                ui.button(t!("distant.lighting.settings")),
                t!("distant.lighting.settings_tip"),
            )
            .clicked()
            {
                *open_lighting = true;
            }
        });
    });
}

fn automatic_distances_card(ui: &mut Ui, settings: &mut GuiSettings, enablement: DistantEnablement) {
    style::card_enabled(ui, enablement.distant_land, t!("distant.automatic.title"), |ui| {
        tooltip(
            ui.checkbox(&mut settings.auto_distances, t!("distant.automatic.enabled")),
            t!("distant.automatic.enabled_tip"),
        );
        // The combo indexes by position; the schema stores the same index as a `u8`.
        let mut mode = usize::from(settings.auto_distance_mode);
        ui.add_enabled_ui(enablement.auto_distances, |ui| {
            let width = ui.available_width();
            if let Some(response) =
                combo_index_localized_sized(ui, "auto_distance_mode", &mut mode, AUTO_DISTANCE_MODES, Some(width))
            {
                // A combo cannot carry per-item tooltips, so describe the current mode.
                tooltip(response, t!(AUTO_DISTANCE_MODE_TIPS[mode]));
            }
        });
        settings.auto_distance_mode = mode.min(2) as u8;
    });
}

fn distant_statics_card(
    ui: &mut Ui,
    settings: &mut DistantLandSettings,
    enablement: DistantEnablement,
    status: &DistantLandStatus,
) {
    // The near-static minimum size is baked into the generated data, so the
    // original showed it in a read-only text box rather than a spinner.
    let near_size = status
        .min_static_size
        .map_or_else(|| "—".to_owned(), |size| format!("{size:.0}"));
    style::card_enabled(ui, enablement.distant_land, t!("distant.statics.title"), |ui| {
        // The column captions share the row with the group's own
        // checkbox; as their own row they cost height the tab has
        // no room for.
        ui.horizontal(|ui| {
            tooltip(
                ui.checkbox(&mut settings.statics, t!("distant.statics.enabled")),
                t!("distant.statics.enabled_tip"),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let height = ui.spacing().interact_size.y;
                for caption in [t!("distant.statics.end_distance"), t!("distant.statics.minimum_size")] {
                    ui.add_sized([SPIN_W, height], egui::Label::new(RichText::new(caption).color(style::MUTED)));
                }
            });
        });
        ui.add_enabled_ui(enablement.statics, |ui| {
            tooltip(
                labeled_row(ui, t!("distant.statics.near").as_ref(), |ui| {
                    spinner_width(ui, SPIN_W);
                    ui.add_enabled(
                        enablement.manual,
                        distance(&mut settings.near_static_end, NEAR_STATIC_END_RANGE),
                    );
                    value_field(ui, near_size.as_str(), SPIN_W);
                }),
                t!("distant.statics.near_tip"),
            );
            // Not `range_row`: auto distances leaves the minimum
            // sizes editable while disabling the end distances.
            // Not `labeled_row` either: its right-to-left layout
            // reverses egui's creation-order tab traversal.
            tooltip(
                ui.horizontal(|ui| {
                    ui.label(t!("distant.statics.far").as_ref());
                    let avail = ui.available_width();
                    let needed = SPIN_W * 2.0 + ui.spacing().item_spacing.x;
                    ui.add_space((avail - needed).max(0.0));
                    spinner_width(ui, SPIN_W);
                    ui.add(
                        egui::DragValue::new(&mut settings.far_static_min_size)
                            .range(STATIC_MIN_SIZE_RANGE.0..=STATIC_MIN_SIZE_RANGE.1)
                            .speed(1.0),
                    );
                    ui.add_enabled(
                        enablement.manual,
                        distance(&mut settings.far_static_end, FAR_STATIC_END_RANGE),
                    );
                })
                .response,
                t!("distant.statics.far_tip"),
            );
            tooltip(
                ui.horizontal(|ui| {
                    ui.label(t!("distant.statics.very_far").as_ref());
                    let avail = ui.available_width();
                    let needed = SPIN_W * 2.0 + ui.spacing().item_spacing.x;
                    ui.add_space((avail - needed).max(0.0));
                    spinner_width(ui, SPIN_W);
                    ui.add(
                        egui::DragValue::new(&mut settings.very_far_static_min_size)
                            .range(STATIC_MIN_SIZE_RANGE.0..=STATIC_MIN_SIZE_RANGE.1)
                            .speed(1.0),
                    );
                    ui.add_enabled(
                        enablement.manual,
                        distance(&mut settings.very_far_static_end, VERY_FAR_STATIC_END_RANGE),
                    );
                })
                .response,
                t!("distant.statics.very_far_tip"),
            );
        });
    });
}

fn fog_card(ui: &mut Ui, settings: &mut FogSettings, enablement: DistantEnablement, open_weather: &mut bool) {
    style::card_enabled(ui, enablement.distant_land, t!("distant.fog.title"), |ui| {
        range_header(ui, t!("distant.fog.start").as_ref(), t!("distant.fog.end").as_ref());
        tooltip(
            range_row(
                ui,
                t!("distant.fog.above_water").as_ref(),
                enablement.fog_above,
                distance(&mut settings.above_water_start, FOG_ABOVE_START_RANGE),
                distance(&mut settings.above_water_end, FOG_ABOVE_END_RANGE),
            ),
            t!("distant.fog.above_water_tip"),
        );
        tooltip(
            range_row(
                ui,
                t!("distant.fog.below_water").as_ref(),
                enablement.manual,
                distance(&mut settings.below_water_start, FOG_BELOW_START_RANGE),
                distance(&mut settings.below_water_end, FOG_BELOW_END_RANGE),
            ),
            t!("distant.fog.below_water_tip"),
        );
        tooltip(
            range_row(
                ui,
                t!("distant.fog.interiors").as_ref(),
                enablement.fog_interior,
                // Two decimals: interior fog starts at fractions of a cell.
                distance(&mut settings.interior_start, FOG_INTERIOR_START_RANGE).fixed_decimals(2),
                distance(&mut settings.interior_end, FOG_INTERIOR_END_RANGE),
            ),
            t!("distant.fog.interiors_tip"),
        );
        ui.separator();
        tooltip(
            ui.checkbox(&mut settings.exponential, t!("distant.fog.exponential").as_ref()),
            t!("distant.fog.exponential_tip"),
        );
        ui.horizontal(|ui| {
            tooltip(
                ui.add_enabled(
                    enablement.exponential_fog,
                    egui::Checkbox::new(&mut settings.atmosphere_scattering, t!("distant.fog.scattering")),
                ),
                t!("distant.fog.scattering_tip"),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if tooltip(
                    ui.button(t!("distant.fog.weather_settings")),
                    t!("distant.fog.weather_settings_tip"),
                )
                .clicked()
                {
                    *open_weather = true;
                }
            });
        });
    });
}

impl GuiApp {
    pub(crate) fn show_distant_land(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        // Why an unusable generated set cannot be turned on, carried twice: as
        // a hover tooltip, so the reason is legible before the click, and
        // through `set_error` on the bounced tick.
        let blocked_reason = if self.ui.distant.status.complete {
            String::new()
        } else {
            let missing = &self.ui.distant.status.missing;
            let count = missing.len();
            let items = missing.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
            let version = self.ui.distant.status.version;
            let key = match (self.ui.distant.status.supported_version, version, count) {
                (false, Some(_), 0) => "distant.blocked.format",
                (false, Some(_), 1) => "distant.blocked.format_missing_one",
                (false, Some(_), _) => "distant.blocked.format_missing_other",
                (false, None, 0) => "distant.blocked.not_found",
                (false, None, 1) => "distant.blocked.not_found_missing_one",
                (false, None, _) => "distant.blocked.not_found_missing_other",
                (true, _, 1) => "distant.blocked.missing_one",
                (true, _, _) => "distant.blocked.missing_other",
            };
            t!(key, version = version.unwrap_or_default(), count = count, items = items).into_owned()
        };

        // The enabled checkbox stays interactive even when the generated set is
        // unusable: unticking must never be refused, and a bounced tick explains
        // itself through `blocked_reason`.
        let mut response = ui.checkbox(&mut self.settings.mge.distant_land.enabled, t!("distant.enabled"));
        if !self.ui.distant.status.complete {
            response = response.on_hover_text(blocked_reason.as_str());
            if response.changed() && self.settings.mge.distant_land.enabled {
                self.settings.mge.distant_land.enabled = false;
                self.set_error(blocked_reason);
            }
        } else {
            tooltip(response, t!("distant.enabled_tip"));
        }
        tooltip(
            ui.add_enabled(
                self.settings.mge.distant_land.enabled,
                egui::Checkbox::new(
                    &mut self.settings.mge.distant_land.automatic_rebuild,
                    t!("distant.auto_rebuild"),
                ),
            ),
            t!("distant.auto_rebuild_tip"),
        );
        ui.add_space(6.0);

        // Enable rules, matching the legacy grey-out behaviour:
        //  - auto distances drives the fog and static end distances, but never
        //    the two static minimum sizes, and which of draw distance /
        //    above-water fog is the free variable depends on the radio mode;
        //  - statics gate the interior fog and the two reflection options;
        //  - exponential fog gates the multiplier and scattering.
        let enablement = DistantEnablement::from_settings(&self.settings.mge);

        let mut open_lighting = false;
        let mut open_weather = false;
        ui.columns(2, |columns| {
            if draw_distance_card(
                &mut columns[0],
                &mut self.settings.mge.distant_land,
                enablement,
                self.ui.distant.generator.is_some(),
            ) {
                self.open_generator();
            }
            columns[0].add_space(3.0);
            water_card(&mut columns[0], &mut self.settings.mge.distant_land.water, enablement);
            columns[0].add_space(3.0);
            lighting_and_shadows_card(
                &mut columns[0],
                &mut self.settings.mge.distant_land,
                enablement,
                &mut open_lighting,
            );

            automatic_distances_card(&mut columns[1], &mut self.settings.mge.gui, enablement);
            columns[1].add_space(3.0);
            distant_statics_card(
                &mut columns[1],
                &mut self.settings.mge.distant_land,
                enablement,
                &self.ui.distant.status,
            );
            columns[1].add_space(3.0);
            fog_card(
                &mut columns[1],
                &mut self.settings.mge.distant_land.fog,
                enablement,
                &mut open_weather,
            );
        });

        if open_lighting {
            self.open_lighting_settings();
        }
        if open_weather {
            self.open_weather_settings();
        }

        if !self.settings.mge.distant_land.fog.exponential {
            // Clear scattering along with the fog mode rather than leaving a
            // disabled tick behind.
            self.settings.mge.distant_land.fog.atmosphere_scattering = false;
        }
        config::update_auto_distances(&mut self.settings.mge, self.ui.distant.status.min_static_size);
    }
}
