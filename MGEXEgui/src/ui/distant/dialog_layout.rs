//! Layout vocabulary shared by the weather and per-pixel lighting dialog
//! windows: designer geometry constants, row tints, and the WinForms-style
//! controls egui lacks.

use eframe::egui::{Align2, Color32, DragValue, FontId, Rect, Stroke, Ui, UiBuilder, pos2};

use crate::style;

// Legacy client geometry, in 96-DPI logical pixels.
//
// The two forms are the same design at two widths: ten colour-coded weather
// rows on a 32 px pitch, one or more group-box columns of spinners over them,
// and a Reset/Save/Cancel bar. The form-specific columns live beside their
// windows; the constants here are shared because both forms used the same
// values.
pub(super) const ROW_X0: f32 = 2.0;
pub(super) const ROW_Y0: f32 = 41.0;
pub(super) const ROW_STEP: f32 = 32.0;
pub(super) const ROW_H: f32 = 30.0;
/// Where the weather-name column ends, and so where the first group box starts.
pub(super) const NAME_X1: f32 = 116.0;
pub(super) const NAME_PAD: f32 = 6.0;
pub(super) const GB_Y: f32 = 8.0;
pub(super) const GB_H: f32 = 355.0;
pub(super) const NUD_H: f32 = 23.0;
pub(super) const BTN_Y: f32 = 374.0;
pub(super) const BTN_H: f32 = 23.0;
pub(super) const BTN_W: f32 = 112.0;

/// How far a group-box caption sits in from the box's left edge.
const CAPTION_INSET: f32 = 10.0;

/// The legacy row colours, each blended 30 % over `APP_BG`. The original strips
/// are near-white Windows system colours with black text; kept at full
/// saturation they would be the brightest thing in a dark UI, so the hue
/// identity is preserved and the value is not.
pub(super) const WEATHER_TINTS: [Color32; 10] = [
    Color32::from_rgb(22, 80, 99), // DeepSkyBlue
    Color32::from_rgb(74, 87, 91), // LightBlue
    Color32::from_rgb(88, 88, 88), // Gainsboro
    Color32::from_rgb(80, 80, 80), // Silver
    Color32::from_rgb(75, 81, 89), // LightSteelBlue
    Color32::from_rgb(58, 63, 68), // LightSlateGray
    Color32::from_rgb(85, 76, 64), // Tan
    Color32::from_rgb(84, 50, 50), // IndianRed
    Color32::from_rgb(99, 97, 97), // Snow
    Color32::from_rgb(91, 91, 97), // Lavender
];

/// One `NumericUpDown` cell, at exactly the rectangle the designer gave it.
/// `interact_size` is set locally so the spinner fills the cell rather than
/// floating centred inside it (pitfall 3).
pub(super) fn spinner(ui: &mut Ui, rect: Rect, drag: DragValue<'_>) {
    spinner_enabled(ui, rect, drag, true);
}

/// [`spinner`] with the control's enabled state, for a cell that does not apply
/// to its row and is shown greyed out rather than left blank.
pub(super) fn spinner_enabled(ui: &mut Ui, rect: Rect, drag: DragValue<'_>, enabled: bool) {
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.spacing_mut().interact_size = rect.size();
        ui.add_enabled(enabled, drag);
    });
}

/// A WinForms group box: a bordered rectangle whose top edge is interrupted by
/// the caption. egui has no equivalent: `style::card` draws a filled title band
/// instead, which cannot sit behind the full-width weather strips.
/// Returns the caption's rectangle, so callers can hang the legacy group-box
/// tooltip off it without covering the controls inside.
pub(super) fn group_box(ui: &mut Ui, rect: Rect, title: &str, font: &FontId) -> Rect {
    let painter = ui.painter();
    let stroke = Stroke::new(1.0, style::BORDER);
    let galley = painter.layout_no_wrap(title.to_owned(), font.clone(), style::TEXT);
    let top = rect.top();
    // The caption sits just past a short stub of the top border, as WinForms
    // draws it, not centred.
    let text_x = rect.left() + CAPTION_INSET;
    let caption = Rect::from_min_size(pos2(text_x, top - galley.size().y / 2.0), galley.size());
    for segment in [
        [pos2(rect.left(), top), pos2(text_x - 4.0, top)],
        [pos2(caption.right() + 4.0, top), pos2(rect.right(), top)],
        [pos2(rect.left(), top), pos2(rect.left(), rect.bottom())],
        [pos2(rect.right(), top), pos2(rect.right(), rect.bottom())],
        [pos2(rect.left(), rect.bottom()), pos2(rect.right(), rect.bottom())],
    ] {
        painter.line_segment(segment, stroke);
    }
    painter.text(caption.left_center(), Align2::LEFT_CENTER, title, font.clone(), style::TEXT);
    caption
}
