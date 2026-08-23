use eframe::egui::{self, Align, Button, ComboBox, IntoAtoms, Layout, Response, RichText, Stroke, Ui};
use rust_i18n::t;

use crate::style;

/// `Ui::selectable_label` that does not resize when hovered.
///
/// egui sizes a button as `content + button_padding`, and it reaches that total
/// two different ways: `Style::button_style` subtracts the frame's stroke width
/// from `inner_margin`, and `Frame::total_margin` adds it back, because the
/// stroke is laid out as well as painted. That cancels only while the frame is
/// actually drawn. `Button::selectable` sets `frame_when_inactive(selected)`, so
/// an unselected, unhovered label is painted with a *strokeless* frame that
/// still carries the shrunken margin. It sits 1px per side tighter than the
/// same label hovered. Stock egui hides this because its dark theme leaves
/// `widgets.inactive.bg_stroke` at width 0; our theme (see `style::install`)
/// gives inactive widgets a real border, which un-cancels the arithmetic and
/// makes tab strips and combo rows visibly jump under the pointer.
///
/// Forcing the stroke off pins the margin to one value in every state. The
/// outline is no loss: `widgets.hovered.bg_stroke` is `ACCENT`, the same color
/// as the `CARD_HOVER` fill it would be drawn over.
///
/// Use this instead of `Ui::selectable_label` / `Ui::selectable_value`
/// everywhere; the raw egui calls jitter under this theme.
pub(super) fn selectable_label<'a>(ui: &mut Ui, selected: bool, text: impl IntoAtoms<'a>) -> Response {
    ui.add(Button::selectable(selected, text).stroke(Stroke::NONE))
}

/// `Ui::selectable_value` counterpart of [`selectable_label`].
pub(super) fn selectable_value<'a, Value: PartialEq>(
    ui: &mut Ui,
    current_value: &mut Value,
    selected_value: Value,
    text: impl IntoAtoms<'a>,
) -> Response {
    let mut response = selectable_label(ui, *current_value == selected_value, text);
    if response.clicked() && *current_value != selected_value {
        *current_value = selected_value;
        response.mark_changed();
    }
    response
}

pub(super) fn tooltip(response: egui::Response, text: impl Into<egui::WidgetText>) -> egui::Response {
    let text = text.into();
    response.on_hover_text(text.clone()).on_disabled_hover_text(text)
}

pub(super) fn combo_index_localized_sized(
    ui: &mut Ui,
    id: &'static str,
    value: &mut usize,
    keys: impl AsRef<[&'static str]>,
    width: Option<f32>,
) -> Option<Response> {
    let keys = keys.as_ref();
    if keys.is_empty() {
        return None;
    }

    *value = (*value).min(keys.len() - 1);
    let mut combo = ComboBox::from_id_salt(id)
        .selected_text(t!(keys[*value]))
        .icon(style::combo_arrow_icon);
    if let Some(width) = width {
        combo = combo.width(width);
    }
    Some(
        combo
            .show_ui(ui, |ui| {
                for (index, key) in keys.iter().enumerate() {
                    selectable_value(ui, value, index, t!(*key));
                }
            })
            .response,
    )
}

pub(super) fn combo_value_sized<Value: Copy + PartialEq>(
    ui: &mut Ui,
    id: &'static str,
    value: &mut Value,
    values: &[(Value, &'static str)],
    width: Option<f32>,
) -> Option<Response> {
    let (_, selected_label) = values.iter().find(|(candidate, _)| *candidate == *value)?;

    let mut combo = ComboBox::from_id_salt(id)
        .selected_text(*selected_label)
        .icon(style::combo_arrow_icon);
    if let Some(width) = width {
        combo = combo.width(width);
    }
    Some(
        combo
            .show_ui(ui, |ui| {
                for &(candidate, label) in values {
                    selectable_value(ui, value, candidate, label);
                }
            })
            .response,
    )
}

pub(super) fn combo_value_localized_sized<Value: Copy + PartialEq>(
    ui: &mut Ui,
    id: &'static str,
    value: &mut Value,
    values: &[(Value, &'static str)],
    width: Option<f32>,
) -> Option<Response> {
    let (_, selected_key) = values.iter().find(|(candidate, _)| *candidate == *value)?;

    let mut combo = ComboBox::from_id_salt(id)
        .selected_text(t!(*selected_key))
        .icon(style::combo_arrow_icon);
    if let Some(width) = width {
        combo = combo.width(width);
    }
    Some(
        combo
            .show_ui(ui, |ui| {
                for &(candidate, key) in values {
                    selectable_value(ui, value, candidate, t!(key));
                }
            })
            .response,
    )
}

/// One row of a two-column combo: the option name, then a muted example of what
/// it produces. The example is positioned by `LayoutJob` leading space rather
/// than padding, so every row's second column lands on one edge.
///
/// The name is left as `Color32::PLACEHOLDER` so it still picks up the selected
/// and hovered foreground colors. A `Button` only supplies its text color as a
/// *fallback*, so the explicitly muted preview keeps its own color throughout.
pub(super) fn preview_job(ui: &Ui, name: &str, preview: &str, name_col_w: f32) -> egui::text::LayoutJob {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let name_w = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font_id.clone(), egui::Color32::PLACEHOLDER)
        .size()
        .x;

    let mut job = egui::text::LayoutJob::default();
    job.append(
        name,
        0.0,
        egui::TextFormat {
            font_id: font_id.clone(),
            color: egui::Color32::PLACEHOLDER,
            ..Default::default()
        },
    );
    job.append(
        preview,
        (name_col_w - name_w).max(ui.spacing().item_spacing.x),
        egui::TextFormat {
            font_id,
            color: style::MUTED,
            ..Default::default()
        },
    );
    job
}

pub(super) fn combo_value_preview_localized<Value: Copy + PartialEq>(
    ui: &mut Ui,
    id: &'static str,
    value: &mut Value,
    values: &[(Value, &'static str)],
    previews: &[&'static str],
    width: f32,
) -> Option<Response> {
    if values.len() != previews.len() {
        return None;
    }
    let selected_index = values.iter().position(|(candidate, _)| *candidate == *value)?;

    let names = values.iter().map(|(_, key)| t!(*key).into_owned()).collect::<Vec<_>>();
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let painter = ui.painter();
    let name_col_w = names
        .iter()
        .map(|name| {
            painter
                .layout_no_wrap(name.clone(), font_id.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
        + ui.spacing().item_spacing.x * 2.0;

    let selected = preview_job(ui, &names[selected_index], previews[selected_index], name_col_w);
    Some(
        ComboBox::from_id_salt(id)
            .selected_text(selected)
            .icon(style::combo_arrow_icon)
            .width(width)
            .show_ui(ui, |ui| {
                for (index, ((candidate, _), name)) in values.iter().zip(&names).enumerate() {
                    let job = preview_job(ui, name, previews[index], name_col_w);
                    selectable_value(ui, value, *candidate, job);
                }
            })
            .response,
    )
}

/// A fixed-width cell whose contents are laid out from the right edge inward, so
/// controls of differing widths still share one alignment edge and wide controls
/// grow leftward into free space instead of displacing their label.
pub(super) fn control_cell(ui: &mut Ui, width: f32, contents: impl FnOnce(&mut Ui)) {
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(egui::vec2(width, height), Layout::right_to_left(Align::Center), contents);
}

/// `DragValue` takes its minimum size from `interact_size`, so widening that is the
/// only way to make one fill its slot instead of shrinking to fit its digits.
pub(super) fn spinner_width(ui: &mut Ui, width: f32) {
    ui.spacing_mut().interact_size.x = width;
}

pub(super) fn vertical_rule(ui: &mut Ui, height: f32) {
    ui.add_sized([10.0, height], egui::Separator::default().vertical().spacing(10.0));
}

pub(super) fn labeled_row(ui: &mut Ui, label: &str, contents: impl FnOnce(&mut Ui)) -> Response {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), contents);
    })
    .response
}

/// Label on the left, a right-aligned spinner of a fixed width. This is the dense
/// WinForms idiom; a `Slider::text(…)` row is both taller and, against the card
/// fill, nearly invisible because the rail is drawn in `BORDER`.
pub(super) fn spin_row(ui: &mut Ui, label: &str, enabled: bool, spinner: egui::DragValue<'_>) -> Response {
    labeled_row(ui, label, |ui| {
        spinner_width(ui, SPIN_W);
        ui.add_enabled(enabled, spinner);
    })
}

/// Label on the left, a right-aligned pair of spinners sharing the column edge.
pub(super) fn range_row(
    ui: &mut Ui,
    label: &str,
    enabled: bool,
    start: egui::DragValue<'_>,
    end: egui::DragValue<'_>,
) -> Response {
    ui.horizontal(|ui| {
        ui.label(label);
        let avail = ui.available_width();
        let needed = SPIN_W * 2.0 + ui.spacing().item_spacing.x;
        ui.add_space((avail - needed).max(0.0));
        spinner_width(ui, SPIN_W);
        // Left-to-right: creation order matches visual and tab order.
        ui.add_enabled(enabled, start);
        ui.add_enabled(enabled, end);
    })
    .response
}

/// Column captions for a run of [`range_row`]s.
pub(super) fn range_header(ui: &mut Ui, start: &str, end: &str) {
    let height = ui.spacing().interact_size.y;
    ui.horizontal(|ui| {
        ui.label("");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for caption in [end, start] {
                ui.add_sized([SPIN_W, height], egui::Label::new(RichText::new(caption).color(style::MUTED)));
            }
        });
    });
}

/// A muted group caption inside a card, occupying a full widget row.
///
/// A bare `ui.label` is only as tall as its galley, about 16px at body 12,
/// where every widget row is `spacing.interact_size.y` (20). One caption is
/// therefore enough to throw two columns of cards 4px out of vertical alignment,
/// which is exactly what it did to this tab. Use this rather than `style::hint`
/// for anything that heads a run of rows.
pub(super) fn caption_row(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.set_min_height(ui.spacing().interact_size.y);
        ui.label(RichText::new(text).color(style::MUTED));
    });
}

/// Two even sub-columns, for packing short checkbox runs the way the WinForms
/// group boxes did instead of stacking them into a tall single file.
pub(super) fn split(ui: &mut Ui, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    ui.columns(2, |columns| {
        // `columns` yields `top_down_justified` children; left as-is every widget
        // inside would stretch to the sub-column width.
        columns[0].with_layout(Layout::top_down(Align::Min), left);
        columns[1].with_layout(Layout::top_down(Align::Min), right);
    });
}

/// A trailing control pushed to the right edge of its own row.
///
/// The `ui.horizontal` wrapper is load-bearing: a bare `with_layout` using a
/// horizontal layout claims the *whole* remaining height of a vertical parent,
/// which silently pushes anything below it out of the pane.
pub(super) fn right_aligned(ui: &mut Ui, contents: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), contents);
    });
}

/// A feature checkbox on the left with the option it governs on the right edge.
///
/// The WinForms tab paired several toggles with a single dependent control this
/// way (solar shadows + detail, exponential fog + multiplier). Folding them back
/// onto one row is also what buys the vertical budget for the restored groups.
pub(super) fn check_row<R>(
    ui: &mut Ui,
    checked: &mut bool,
    label: &str,
    contents: impl FnOnce(&mut Ui) -> R,
) -> (Response, R) {
    ui.horizontal(|ui| {
        let checkbox = ui.checkbox(checked, label);
        let contents = ui.with_layout(Layout::right_to_left(Align::Center), contents).inner;
        (checkbox, contents)
    })
    .inner
}

/// Shared width of every spinner in the two-column tabs.
pub(super) const SPIN_W: f32 = 72.0;

pub(super) fn aspect_ratio(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        return "—".to_owned();
    }

    let (mut a, mut b) = (width, height);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    format!("{}:{}", width / a, height / a)
}

/// A read-only boxed value, styled like a disabled text box. The text is centred:
/// these hold short numeric readouts, and left-aligning them leaves a ragged gap
/// beside the fixed-width box.
///
/// The box is allocated at an exact size and painted directly rather than built
/// from a `Frame` around a `Label`, because a container sizes itself to its
/// content: the row height then follows the *text's* metrics, and these fields
/// do not all draw from the same font (`1280 × 1410` resolves U+00D7 through a
/// fallback face with taller metrics than ASCII digits). Painting also keeps the
/// stroke inside the allocation, so the box occupies exactly `width`.
pub(super) fn value_field(ui: &mut Ui, text: impl Into<String>, width: f32) -> Response {
    let height = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());

    if !ui.is_rect_visible(rect) {
        return response;
    }

    let visuals = ui.visuals();
    ui.painter().rect(
        rect,
        2.0,
        visuals.extreme_bg_color,
        visuals.widgets.inactive.bg_stroke,
        egui::StrokeKind::Inside,
    );
    // Clipped, since nothing here can grow the box to fit an overlong value.
    ui.painter().with_clip_rect(rect).text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text.into(),
        egui::TextStyle::Body.resolve(ui.style()),
        visuals.text_color(),
    );
    response
}
