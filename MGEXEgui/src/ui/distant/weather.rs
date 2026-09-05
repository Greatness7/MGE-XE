//! The **Distant Land Weather Settings** window: a per-weather matrix of fog
//! and wind multipliers, drawn at legacy designer coordinates, with an interior
//! row above the weathers for the one wind setting that has no weather.

use eframe::egui::{
    Align2, Button, CentralPanel, Color32, Context, DragValue, Frame, Rect, Sense, TextStyle, Ui, ViewportBuilder,
    ViewportCommand, ViewportId, vec2,
};
use mge_config::{GRASS_INTERIOR_WIND_RANGE, GrassSettings, WeatherSet, WeatherSettings};
use rust_i18n::t;

use crate::ui::tooltip;
use crate::{app::GuiApp, config::WEATHER_NAMES, style};

use super::dialog_layout::{
    BTN_H, BTN_W, BTN_Y, GB_H, GB_Y, NAME_PAD, NAME_X1, NUD_H, ROW_H, ROW_STEP, ROW_X0, ROW_Y0, WEATHER_TINTS, group_box,
    spinner, spinner_enabled,
};

/// Draft state for the **Distant Land Weather Settings** window, with the same
/// drop-to-cancel contract as `LightingEditorState`.
pub(crate) struct WeatherEditorState {
    draft: [WeatherSettings; 10],
    /// The interior row. Interiors have no weather, so grass there gets this
    /// constant wind and the fog columns do not apply.
    interior_wind: f32,
    viewport_ready: bool,
    focus_pending: bool,
}

impl WeatherEditorState {
    fn new(weather: &WeatherSet, grass: &GrassSettings) -> Self {
        Self {
            draft: weather.as_array().map(|value| *value),
            interior_wind: grass.interior_wind,
            viewport_ready: false,
            focus_pending: false,
        }
    }
}

/// One row taller than the legacy form: the interior row sits above the ten
/// weathers, so the group boxes and the button bar move down by one pitch.
const WEATHER_SIZE: [f32; 2] = [524.0, 409.0 + ROW_STEP];
const WEATHER_GB_H: f32 = GB_H + ROW_STEP;
const WEATHER_BTN_Y: f32 = BTN_Y + ROW_STEP;
/// Where the weather rows start, below the interior row.
const WEATHER_ROW_Y0: f32 = ROW_Y0 + ROW_STEP;
/// The interior strip. It is a setting rather than a weather and has no legacy
/// tint, so it takes the button fill, the one dark grey the window already uses.
const INTERIOR_TINT: Color32 = style::CARD;
const WEATHER_ROW_X1: f32 = 522.0;
const WEATHER_GB_X: [f32; 3] = [NAME_X1, 262.0, 408.0];
/// `gbWind` and `gbFogDay` are 140 wide; `gbFogOffsDay` holds a narrower
/// spinner and is 114.
const WEATHER_GB_W: [f32; 3] = [140.0, 140.0, 114.0];
/// Every column puts its `NumericUpDown` at `(6, …)`; only the widths differ.
const WEATHER_NUD_W: [f32; 3] = [66.0, 66.0, 40.0];
/// The per-row `Default` buttons, at the group-box-relative `x` the designer
/// gave them.
const WEATHER_DEF_DX: [f32; 3] = [78.0, 78.0, 52.0];
const WEATHER_DEF_W: f32 = 56.0;
const WEATHER_DEF_H: f32 = 22.0;
const WEATHER_BTN_X: [f32; 3] = [116.0, 258.0, 400.0];

/// `gbWind` / `gbFogDay` / `gbFogOffsDay`: caption and the legacy tooltip from
/// `[DLWeatherForm.Tooltips]`. The fog-offset text originally claimed a
/// `0% - 90%` range that its own `NumericUpDown` never enforced; the stated
/// range here is the one the control actually has.
const WEATHER_COLUMNS: [(&str, &str); 3] = [
    ("distant.weather.wind_factor", "distant.weather.wind_factor_tip"),
    ("distant.weather.fog_range_factor", "distant.weather.fog_range_factor_tip"),
    ("distant.weather.fog_offset", "distant.weather.fog_offset_tip"),
];

/// Column index to the `WeatherSettings` field it edits, so the three columns
/// can be drawn by one loop.
const WEATHER_FIELD: [fn(&mut WeatherSettings) -> &mut f32; 3] = [
    |weather| &mut weather.wind,
    |weather| &mut weather.fog_ratio,
    |weather| &mut weather.fog_offset,
];

/// A column's spinner over `value`: the legacy `NumericUpDown` DecimalPlaces /
/// Increment / Minimum / Maximum, column by column.
fn column_drag(column: usize, value: &mut f32) -> DragValue<'_> {
    let drag = DragValue::new(value);
    match column {
        0 => drag.speed(0.05).range(0.0..=1.0).fixed_decimals(3),
        1 => drag.speed(0.01).range(0.001..=2.0).fixed_decimals(3),
        _ => drag.speed(1.0).range(0.0..=200.0).fixed_decimals(0),
    }
}

impl GuiApp {
    /// Opens the weather window over the stored values.
    pub(in crate::ui) fn open_weather_settings(&mut self) {
        let distant = &self.settings.mge.distant_land;
        self.ui.distant.weather = Some(WeatherEditorState::new(&distant.weather, &distant.grass));
    }

    /// Called every frame from `show_dialogs`; renders the child window while
    /// the editor state exists.
    pub(in crate::ui) fn show_weather_dialog(&mut self, ctx: &Context) {
        let Some(state) = self.ui.distant.weather.as_ref() else {
            return;
        };

        let viewport_ready = state.viewport_ready;
        let mut builder = ViewportBuilder::default()
            .with_title(t!("distant.weather.title"))
            .with_inner_size(WEATHER_SIZE)
            .with_resizable(false)
            // Legacy `MaximizeBox = false`, `MinimizeBox = false`.
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_clamp_size_to_monitor_size(true)
            .with_visible(viewport_ready);
        if let Some(icon) = crate::load_icon() {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(ViewportId::from_hash_of("mge_weather_settings"), builder, |ui, _class| {
            self.weather_body(ui)
        });

        if let Some(state) = self.ui.distant.weather.as_mut()
            && !state.viewport_ready
        {
            state.viewport_ready = true;
            state.focus_pending = true;
            ctx.request_repaint();
        }
    }

    fn weather_body(&mut self, ui: &mut Ui) {
        let Some(mut state) = self.ui.distant.weather.take() else {
            return;
        };

        // Dropped: the window closes and the draft goes with it, which is what
        // the legacy form's `CancelButton = bCancel` did.
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            return;
        }
        if state.focus_pending {
            ui.ctx().send_viewport_cmd(ViewportCommand::Focus);
            state.focus_pending = false;
        }

        let default_set = WeatherSet::default();
        let mut defaults = default_set.as_array().map(|value| *value);
        let interior_default = GrassSettings::default().interior_wind;
        let mut act = None;

        // No inner margin: the legacy designer coordinates below are *client*
        // coordinates and already carry their own 2 px / 8 px edges.
        CentralPanel::default().frame(Frame::NONE.fill(style::APP_BG)).show(ui, |ui| {
            ui.set_min_size(vec2(WEATHER_SIZE[0], WEATHER_SIZE[1]));
            let origin = ui.min_rect().min;
            let at = |x, y, width, height| Rect::from_min_size(origin + vec2(x, y), vec2(width, height));
            // Controls are shorter than the row they sit in; the designer
            // centred them by hand and this reproduces the same offsets.
            let centred = |y: f32, height: f32| y + (ROW_H - height) / 2.0;
            let font = ui.style().text_styles[&TextStyle::Body].clone();

            // The group boxes are painted first and the weather strips over
            // them, so their side borders survive only in the 2 px gaps
            // between rows, which is exactly how the original reads.
            for (column, (title, tip)) in WEATHER_COLUMNS.iter().enumerate() {
                let title = t!(*title);
                let caption = group_box(
                    ui,
                    at(WEATHER_GB_X[column], GB_Y, WEATHER_GB_W[column], WEATHER_GB_H),
                    title.as_ref(),
                    &font,
                );
                // The legacy tooltip was on the whole group box, but a hover
                // rect that large would sit over every spinner in the column.
                // The caption is the part that reads as the column's label.
                tooltip(
                    ui.interact(caption, ui.id().with(("weather_gb", column)), Sense::hover()),
                    t!(*tip),
                );
            }

            // The interior row. Only the wind column applies: interiors have no
            // weather, so there is no fog to scale or offset, and those two
            // cells are shown disabled at their neutral values.
            {
                let y = ROW_Y0;
                let name = at(ROW_X0, y, NAME_X1 - ROW_X0, ROW_H);
                ui.painter()
                    .rect_filled(at(ROW_X0, y, WEATHER_ROW_X1 - ROW_X0, ROW_H), 0.0, INTERIOR_TINT);
                ui.painter().text(
                    origin + vec2(NAME_X1 - NAME_PAD, y + ROW_H / 2.0),
                    Align2::RIGHT_CENTER,
                    t!("weather.interior"),
                    font.clone(),
                    style::TEXT,
                );
                tooltip(
                    ui.interact(name, ui.id().with("weather_interior_name"), Sense::hover()),
                    t!("distant.weather.interior_tip"),
                );

                spinner(
                    ui,
                    at(WEATHER_GB_X[0] + 6.0, centred(y, NUD_H), WEATHER_NUD_W[0], NUD_H),
                    DragValue::new(&mut state.interior_wind)
                        .speed(0.05)
                        .range(GRASS_INTERIOR_WIND_RANGE.0..=GRASS_INTERIOR_WIND_RANGE.1)
                        .fixed_decimals(3),
                );
                if ui
                    .put(
                        at(
                            WEATHER_GB_X[0] + WEATHER_DEF_DX[0],
                            centred(y, WEATHER_DEF_H),
                            WEATHER_DEF_W,
                            WEATHER_DEF_H,
                        ),
                        Button::new(t!("common.actions.default")),
                    )
                    .clicked()
                {
                    state.interior_wind = interior_default;
                }

                let mut neutral = WeatherSettings::default();
                for column in 1..WEATHER_COLUMNS.len() {
                    spinner_enabled(
                        ui,
                        at(WEATHER_GB_X[column] + 6.0, centred(y, NUD_H), WEATHER_NUD_W[column], NUD_H),
                        column_drag(column, WEATHER_FIELD[column](&mut neutral)),
                        false,
                    );
                    ui.add_enabled_ui(false, |ui| {
                        ui.put(
                            at(
                                WEATHER_GB_X[column] + WEATHER_DEF_DX[column],
                                centred(y, WEATHER_DEF_H),
                                WEATHER_DEF_W,
                                WEATHER_DEF_H,
                            ),
                            Button::new(t!("common.actions.default")),
                        );
                    });
                }
            }

            for (index, name) in WEATHER_NAMES.iter().enumerate() {
                let y = WEATHER_ROW_Y0 + ROW_STEP * index as f32;
                ui.painter()
                    .rect_filled(at(ROW_X0, y, WEATHER_ROW_X1 - ROW_X0, ROW_H), 0.0, WEATHER_TINTS[index]);
                ui.painter().text(
                    origin + vec2(NAME_X1 - NAME_PAD, y + ROW_H / 2.0),
                    Align2::RIGHT_CENTER,
                    t!(*name),
                    font.clone(),
                    style::TEXT,
                );

                for column in 0..WEATHER_COLUMNS.len() {
                    let field = WEATHER_FIELD[column];
                    spinner(
                        ui,
                        at(WEATHER_GB_X[column] + 6.0, centred(y, NUD_H), WEATHER_NUD_W[column], NUD_H),
                        column_drag(column, field(&mut state.draft[index])),
                    );
                    if ui
                        .put(
                            at(
                                WEATHER_GB_X[column] + WEATHER_DEF_DX[column],
                                centred(y, WEATHER_DEF_H),
                                WEATHER_DEF_W,
                                WEATHER_DEF_H,
                            ),
                            Button::new(t!("common.actions.default")),
                        )
                        .clicked()
                    {
                        *field(&mut state.draft[index]) = *field(&mut defaults[index]);
                    }
                }
            }

            if tooltip(
                ui.put(
                    at(WEATHER_BTN_X[0], WEATHER_BTN_Y, BTN_W, BTN_H),
                    Button::new(t!("common.actions.reset")),
                ),
                t!("distant.weather.reset_tip"),
            )
            .clicked()
            {
                state.draft = defaults;
                state.interior_wind = interior_default;
            }
            if tooltip(
                ui.put(
                    at(WEATHER_BTN_X[1], WEATHER_BTN_Y, BTN_W, BTN_H),
                    Button::new(t!("common.actions.save")),
                ),
                t!("distant.weather.save_tip"),
            )
            .clicked()
            {
                act = Some(true);
            }
            if tooltip(
                ui.put(
                    at(WEATHER_BTN_X[2], WEATHER_BTN_Y, BTN_W, BTN_H),
                    Button::new(t!("common.actions.cancel")),
                ),
                t!("distant.weather.cancel_tip"),
            )
            .clicked()
            {
                act = Some(false);
            }
        });

        match act {
            Some(true) => {
                for (index, target) in self.settings.mge.distant_land.weather.as_mut_array().into_iter().enumerate() {
                    *target = state.draft[index];
                }
                self.settings.mge.distant_land.grass.interior_wind = state.interior_wind;
                self.set_success(t!("messages.weather_applied"));
            }
            Some(false) => {} // dropped: window closes
            None => self.ui.distant.weather = Some(state),
        }
    }
}
