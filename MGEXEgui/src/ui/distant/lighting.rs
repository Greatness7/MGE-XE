//! The **Per-pixel Lighting Settings** window: sun and ambient multipliers per
//! weather, drawn at legacy designer coordinates.

use eframe::egui::{
    Align2, Button, CentralPanel, Context, DragValue, Frame, Rect, Sense, TextStyle, Ui, ViewportBuilder, ViewportCommand,
    ViewportId, vec2,
};
use mge_config::{LightingWeatherSet, WeatherLighting};
use rust_i18n::t;

use crate::ui::tooltip;
use crate::{app::GuiApp, config::WEATHER_NAMES, style};

use super::dialog_layout::{
    BTN_H, BTN_W, BTN_Y, GB_H, GB_Y, NAME_PAD, NAME_X1, NUD_H, ROW_H, ROW_STEP, ROW_X0, ROW_Y0, WEATHER_TINTS, group_box,
    spinner,
};

/// Draft state for the **Per-pixel Lighting Settings** window. Dropping it
/// closes the window and discards the edit, as the legacy form's Cancel did.
pub(crate) struct LightingEditorState {
    draft: [WeatherLighting; 10],
    viewport_ready: bool,
    focus_pending: bool,
}

impl LightingEditorState {
    fn new(lighting: &LightingWeatherSet) -> Self {
        Self {
            draft: lighting.as_array().map(|value| *value),
            viewport_ready: false,
            focus_pending: false,
        }
    }
}

const LIGHT_SIZE: [f32; 2] = [484.0, 409.0];
const LIGHT_ROW_X1: f32 = 482.0;
const LIGHT_GB_X: [f32; 2] = [NAME_X1, 302.0];
const LIGHT_GB_W: f32 = 180.0;
/// `gbSun` origin plus `udClearSun`'s `(30, …)`; `gbAmbient` plus `(26, …)`.
const LIGHT_NUD_X: [f32; 2] = [LIGHT_GB_X[0] + 30.0, LIGHT_GB_X[1] + 26.0];
const LIGHT_NUD_W: f32 = 72.0;
const LIGHT_BTN_X: [f32; 3] = [86.0, 223.0, 360.0];

/// A `LightingForm` brightness cell: two decimals, `0.05` steps, `0.00` to
/// `10.00`.
fn multiplier(value: &mut f32) -> DragValue<'_> {
    DragValue::new(value).speed(0.05).range(0.0..=10.0).fixed_decimals(2)
}

impl GuiApp {
    /// Opens the per-pixel lighting window over the stored values.
    pub(in crate::ui) fn open_lighting_settings(&mut self) {
        self.ui.distant.lighting = Some(LightingEditorState::new(&self.settings.mge.lighting.weather));
    }

    /// Called every frame from `show_dialogs`; renders the child window while
    /// the editor state exists.
    pub(in crate::ui) fn show_lighting_dialog(&mut self, ctx: &Context) {
        let Some(state) = self.ui.distant.lighting.as_ref() else {
            return;
        };

        let viewport_ready = state.viewport_ready;
        let mut builder = ViewportBuilder::default()
            .with_title(t!("distant.lighting.dialog_title"))
            .with_inner_size(LIGHT_SIZE)
            .with_resizable(false)
            // Legacy `MaximizeBox = false`, `MinimizeBox = false`.
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_clamp_size_to_monitor_size(true)
            .with_visible(viewport_ready);
        if let Some(icon) = crate::load_icon() {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(ViewportId::from_hash_of("mge_lighting_settings"), builder, |ui, _class| {
            self.lighting_body(ui)
        });

        if let Some(state) = self.ui.distant.lighting.as_mut()
            && !state.viewport_ready
        {
            state.viewport_ready = true;
            state.focus_pending = true;
            ctx.request_repaint();
        }
    }

    fn lighting_body(&mut self, ui: &mut Ui) {
        let Some(mut state) = self.ui.distant.lighting.take() else {
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

        let mut act = None;

        // No inner margin: the legacy designer coordinates below are *client*
        // coordinates and already carry their own 2 px / 12 px edges.
        CentralPanel::default().frame(Frame::NONE.fill(style::APP_BG)).show(ui, |ui| {
            ui.set_min_size(vec2(LIGHT_SIZE[0], LIGHT_SIZE[1]));
            let origin = ui.min_rect().min;
            let at = |x, y, width, height| Rect::from_min_size(origin + vec2(x, y), vec2(width, height));
            let font = ui.style().text_styles[&TextStyle::Body].clone();

            // The group boxes are painted first and the weather strips over
            // them, so their side borders survive only in the 2 px gaps
            // between rows, which is exactly how the original reads.
            for (column, (title, tip)) in [
                ("distant.lighting.sun_multiplier", "distant.lighting.sun_multiplier_tip"),
                (
                    "distant.lighting.ambient_multiplier",
                    "distant.lighting.ambient_multiplier_tip",
                ),
            ]
            .iter()
            .enumerate()
            {
                let title = t!(*title);
                let caption = group_box(ui, at(LIGHT_GB_X[column], GB_Y, LIGHT_GB_W, GB_H), title.as_ref(), &font);
                tooltip(
                    ui.interact(caption, ui.id().with(("lighting_gb", column)), Sense::hover()),
                    t!(*tip),
                );
            }

            for (index, name) in WEATHER_NAMES.iter().enumerate() {
                let y = ROW_Y0 + ROW_STEP * index as f32;
                // One strip across the whole width: the 6 px `l<Weather>2`
                // spacer between the two columns carries the row colour too.
                ui.painter()
                    .rect_filled(at(ROW_X0, y, LIGHT_ROW_X1 - ROW_X0, ROW_H), 0.0, WEATHER_TINTS[index]);
                ui.painter().text(
                    origin + vec2(NAME_X1 - NAME_PAD, y + ROW_H / 2.0),
                    Align2::RIGHT_CENTER,
                    t!(*name),
                    font.clone(),
                    style::TEXT,
                );

                let spin_y = y + (ROW_H - NUD_H) / 2.0;
                let values = &mut state.draft[index];
                spinner(
                    ui,
                    at(LIGHT_NUD_X[0], spin_y, LIGHT_NUD_W, NUD_H),
                    multiplier(&mut values.sun),
                );
                spinner(
                    ui,
                    at(LIGHT_NUD_X[1], spin_y, LIGHT_NUD_W, NUD_H),
                    multiplier(&mut values.ambient),
                );
            }

            if tooltip(
                ui.put(
                    at(LIGHT_BTN_X[0], BTN_Y, BTN_W, BTN_H),
                    Button::new(t!("common.actions.reset")),
                ),
                t!("distant.lighting.reset_tip"),
            )
            .clicked()
            {
                state.draft.fill(WeatherLighting::default());
            }
            if tooltip(
                ui.put(
                    at(LIGHT_BTN_X[1], BTN_Y, BTN_W, BTN_H),
                    Button::new(t!("common.actions.save")),
                ),
                t!("distant.lighting.save_tip"),
            )
            .clicked()
            {
                act = Some(true);
            }
            if tooltip(
                ui.put(
                    at(LIGHT_BTN_X[2], BTN_Y, BTN_W, BTN_H),
                    Button::new(t!("common.actions.cancel")),
                ),
                t!("distant.lighting.cancel_tip"),
            )
            .clicked()
            {
                act = Some(false);
            }
        });

        match act {
            Some(true) => {
                for (index, target) in self.settings.mge.lighting.weather.as_mut_array().into_iter().enumerate() {
                    *target = state.draft[index];
                }
                self.set_success(t!("messages.lighting_applied"));
            }
            Some(false) => {} // dropped: window closes
            None => self.ui.distant.lighting = Some(state),
        }
    }
}
