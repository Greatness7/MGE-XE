use std::time::Duration;

use eframe::egui::{
    self, Align, Button, CentralPanel, Context, Frame, Id, Layout, Margin, Modal, Rect, RichText, Ui, UiBuilder,
    ViewportBuilder, ViewportCommand, ViewportId, vec2,
};
use rust_i18n::t;

use crate::{app::GuiApp, input::input_label, platform::KeyCapture, style};

use super::MACRO_KEYS;

/// State of the key remapper window.
///
/// There is no draft: edits go directly into `settings.input.remap` and closing
/// the window is simply the end of the session. The per-edit commit lives in
/// the capture overlay instead.
#[derive(Default)]
pub struct RemapEditorState {
    /// Source key whose replacement is being captured; `Some` is also what puts
    /// the modal overlay on screen.
    pub capture_source: Option<usize>,
    pub capture: Option<KeyCapture>,
    viewport_ready: bool,
    focus_pending: bool,
}

/// The remapper's keyboard: `MACRO_KEYS` minus the two mouse rows.
/// `RemappedKeys` in `d3d8/cpp/mge/mgedinput.cpp` is a 256-entry
/// DirectInput *keyboard* table, so codes 256+ have nowhere to go.
fn remap_keyboard(ui: &mut Ui, remap: &[u8; 256], capture_source: Option<usize>, origin: egui::Pos2) -> Option<usize> {
    let mut clicked = None;
    for key in MACRO_KEYS.iter().filter(|key| key.code < remap.len()) {
        let rect = Rect::from_min_size(origin + vec2(key.x, key.y), vec2(key.width, key.height));
        let target = remap[key.code];
        // While the overlay is up the keyboard is inert and only the key being
        // rebound stays lit.
        let (enabled, selected) = match capture_source {
            Some(source) => (false, source == key.code),
            None => (true, target != 0),
        };
        let font_size = if key.label.len() > 3 { 9.0 } else { 11.0 };
        let response = ui
            .scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                ui.add_enabled(
                    enabled,
                    Button::new(RichText::new(key.label).size(font_size))
                        .selected(selected)
                        .min_size(rect.size()),
                )
            })
            .inner;
        // A 32 px key face cannot hold "A → B", so the mapping goes on hover.
        let response = if target == 0 {
            response
        } else {
            response.on_hover_text(format!("{} → {}", key.label, input_label(target as usize)))
        };
        if response.clicked() {
            clicked = Some(key.code);
        }
    }
    clicked
}

/// Width of the capture overlay; its two 96 px buttons and margins set it.
const REMAP_MODAL_W: f32 = 224.0;
const REMAP_BTN_W: f32 = 96.0;

/// Outcome of one frame of the capture overlay.
///
/// There is deliberately no `Accepted`: the replacement key arrives through
/// `KeyCapture::poll` on the parent context, not from an OK button.
enum RemapOutcome {
    Waiting,
    Cleared,
    Cancelled,
}

/// The centred capture overlay, on the same `egui::Modal` idiom as the shader
/// editor's flags dialog. Rendered inside the remapper viewport, so it belongs
/// to that window rather than the main one.
fn remap_modal(ctx: &Context, source: usize) -> RemapOutcome {
    let mut outcome = RemapOutcome::Waiting;

    let response = Modal::new(Id::new("key_remap_modal")).show(ctx, |ui| {
        ui.set_width(REMAP_MODAL_W);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(t!("input.remap.capture_title")).strong());
            ui.label(RichText::new(t!("input.remap.replacement_for", key = input_label(source))).color(style::MUTED));
        });
        ui.add_space(10.0);
        let height = ui.spacing().interact_size.y;
        ui.horizontal(|ui| {
            // Right-to-left, so the first added button sits rightmost.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_sized([REMAP_BTN_W, height], Button::new(t!("common.actions.clear")))
                    .clicked()
                {
                    outcome = RemapOutcome::Cleared;
                }
                if ui
                    .add_sized([REMAP_BTN_W, height], Button::new(t!("common.actions.cancel")))
                    .clicked()
                {
                    outcome = RemapOutcome::Cancelled;
                }
            });
        });
    });

    if matches!(outcome, RemapOutcome::Waiting) && response.should_close() {
        RemapOutcome::Cancelled
    } else {
        outcome
    }
}

impl GuiApp {
    pub(in crate::ui) fn show_remap_dialog(&mut self, ctx: &Context) {
        let Some(state) = self.ui.input.remap_editor.as_mut() else {
            return;
        };

        // Polled here rather than in the viewport body so the repaint request
        // lands on the parent viewport, which is what re-runs the immediate
        // child. `KeyCapture` reads `GetAsyncKeyState`, so it does not care
        // which window is drawing.
        if let Some(source) = state.capture_source
            && let Some(capture) = state.capture.as_mut()
        {
            match capture.poll() {
                Some(target) => {
                    self.settings.input.remap[source] = target;
                    state.capture_source = None;
                    state.capture = None;
                }
                None => ctx.request_repaint_after(Duration::from_millis(16)),
            }
        }

        let viewport_ready = state.viewport_ready;
        let mut builder = ViewportBuilder::default()
            .with_title(t!("input.remap.title"))
            .with_inner_size([764.0, 239.0])
            .with_resizable(false)
            .with_clamp_size_to_monitor_size(true)
            .with_visible(viewport_ready);
        if let Some(icon) = crate::load_icon() {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(ViewportId::from_hash_of("mge_key_remapper"), builder, |ui, _class| {
            self.remap_editor_body(ui)
        });

        if let Some(state) = self.ui.input.remap_editor.as_mut()
            && !state.viewport_ready
        {
            state.viewport_ready = true;
            state.focus_pending = true;
            ctx.request_repaint();
        }
    }

    fn remap_editor_body(&mut self, ui: &mut Ui) {
        let Some(mut state) = self.ui.input.remap_editor.take() else {
            return;
        };

        // Closing the native window ends the session; there is nothing to
        // discard, every edit is already in `self.settings.input.remap`.
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            return;
        }
        if state.focus_pending {
            ui.ctx().send_viewport_cmd(ViewportCommand::Focus);
            state.focus_pending = false;
        }

        let capture_source = state.capture_source;
        let remap = &mut self.settings.input.remap;

        CentralPanel::default()
            .frame(Frame::NONE.fill(style::APP_BG).inner_margin(Margin::same(12)))
            .show(ui, |ui| {
                ui.set_min_size(vec2(740.0, 215.0));
                let origin = ui.min_rect().min;

                if let Some(code) = remap_keyboard(ui, remap, capture_source, origin) {
                    state.capture_source = Some(code);
                    state.capture = Some(KeyCapture::begin());
                }

                // Legacy `bClear` at (620, 12) size 128×32, the same slot the
                // macro editor gives `Clear console command`.
                let clear_all = ui
                    .scope_builder(
                        UiBuilder::new().max_rect(Rect::from_min_size(origin + vec2(608.0, 0.0), vec2(128.0, 32.0))),
                        |ui| {
                            ui.add_enabled(
                                capture_source.is_none(),
                                Button::new(t!("common.actions.clear_all")).min_size(vec2(128.0, 32.0)),
                            )
                        },
                    )
                    .inner
                    .clicked();
                if clear_all {
                    remap.fill(0);
                }
            });

        if let Some(source) = capture_source {
            match remap_modal(ui.ctx(), source) {
                RemapOutcome::Waiting => {}
                RemapOutcome::Cleared => {
                    remap[source] = 0;
                    state.capture_source = None;
                    state.capture = None;
                }
                RemapOutcome::Cancelled => {
                    state.capture_source = None;
                    state.capture = None;
                }
            }
        }

        self.ui.input.remap_editor = Some(state);
    }
}
