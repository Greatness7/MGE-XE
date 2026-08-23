//! Widget vocabulary shared by the generator's settings pages.
//!
//! A caption, a control, and a grey hint wrapped underneath it: the repeated
//! shape the pages are built from. The generator's local dialect of
//! [`crate::ui::widgets`]; the main window's `labeled_row` idiom does not fit
//! because these pages are two narrow columns and nearly every field carries a
//! hint.

use eframe::egui::{Align, ComboBox, DragValue, Id, Layout, Ui, vec2};

use crate::{style, ui::SPIN_W, ui::spinner_width};

/// Re-exported so the generator's pages reach the jitter-free selectables
/// through their usual `super::widgets::` path. See
/// [`crate::ui::widgets::selectable_label`] for why the egui originals are not
/// used directly.
pub(super) use crate::ui::widgets::{selectable_label, selectable_value};

/// Width of the Advanced page's label column: every control starts on one
/// vertical line whatever its label's length.
///
/// Wide enough to clear the longest label on the page (`Control-map memory
/// limit`); an overflowing label pushes its own control off the shared line.
const LABEL_W: f32 = 170.0;

/// A settings field: caption, control row, then a hint.
///
/// The hint wraps, so the caption goes above the control rather than beside it.
/// A few fields carry no hint; pass `""` for those rather than inventing one.
pub(super) fn field(ui: &mut Ui, label: &str, hint: &str, control: impl FnOnce(&mut Ui)) {
    ui.label(label);
    ui.horizontal(control);
    if !hint.is_empty() {
        style::hint(ui, hint);
    }
}

/// A settings row: fixed-width label column, control, then a hint spanning the
/// group beneath it.
///
/// This is the Advanced page's form, where one full-width group stacks rows of
/// differing label lengths; [`field`] is the form for the two narrow columns on
/// Landscape and Statics, where a label beside its control would not fit.
pub(super) fn row(ui: &mut Ui, label: &str, hint: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            vec2(LABEL_W, ui.spacing().interact_size.y),
            Layout::left_to_right(Align::Center),
            |ui| {
                // Load-bearing: `allocate_ui_with_layout`'s size argument is only
                // an upper bound. It allocates the child's `min_rect`, so a
                // short label would otherwise shrink the cell and every row would
                // start its control at a different x. `set_min_width` is what
                // turns the cell into a fixed column.
                ui.set_min_width(LABEL_W);
                ui.label(label);
            },
        );
        control(ui);
    });
    if !hint.is_empty() {
        style::hint(ui, hint);
    }
}

/// `count` even sub-columns, each with a plain top-down layout.
///
/// `ui.columns` hands out `top_down_justified` children, so everything inside
/// would stretch to the column width.
/// [`crate::ui::split`] fixes that for two columns via two separate closures;
/// these pages need one closure instead, because the fields it dispatches to
/// are disjoint borrows of a single `GenerationSettings`, and two closures could
/// not both hold it.
pub(super) fn columns(ui: &mut Ui, count: usize, mut column: impl FnMut(&mut Ui, usize)) {
    ui.columns(count, |columns| {
        for (index, ui) in columns.iter_mut().enumerate() {
            ui.with_layout(Layout::top_down(Align::Min), |ui| column(ui, index));
        }
    });
}

/// [`columns`] for a row of [`style::card_sized`] cards, every card padded down
/// to the bottom of the tallest one.
///
/// A column is laid out before its neighbour exists, so the height to pad to is
/// carried in `ctx` under `id` from the previous frame and a repaint is
/// requested whenever it moves — the settle is one frame and only ever visible
/// on the very first. `column` receives that height and returns the height its
/// card took *without* the padding, which is what lets the cached maximum fall
/// again when the window widens and a wrapped hint unwraps.
pub(super) fn columns_equal_height(ui: &mut Ui, id: Id, count: usize, mut column: impl FnMut(&mut Ui, usize, f32) -> f32) {
    let target = ui.ctx().data(|data| data.get_temp::<f32>(id)).unwrap_or(0.0);
    let mut tallest: f32 = 0.0;
    ui.columns(count, |columns| {
        for (index, ui) in columns.iter_mut().enumerate() {
            let natural = ui
                .with_layout(Layout::top_down(Align::Min), |ui| column(ui, index, target))
                .inner;
            tallest = tallest.max(natural);
        }
    });
    // Sub-pixel churn would repaint forever without a deadband.
    if (tallest - target).abs() > 0.5 {
        ui.ctx().data_mut(|data| data.insert_temp(id, tallest));
        ui.ctx().request_repaint();
    }
}

/// A float spinner bounded below by `min` and above by `f32::MAX`.
///
/// The bounds are the ones `GenerationSettings::validate` enforces, so a value
/// the host would refuse at launch cannot be entered here. `f32::MAX` rather
/// than infinity as the ceiling because `validate` also requires *finite*, and
/// an `inf` typed in here would otherwise only surface much later as a refused
/// run.
pub(super) fn float_spinner(ui: &mut Ui, value: &mut f32, min: f64, speed: f64) {
    spinner_width(ui, SPIN_W);
    ui.add(
        DragValue::new(value)
            .range(min..=f64::from(f32::MAX))
            .speed(speed)
            .min_decimals(1)
            .max_decimals(4),
    );
}

/// A combo over a fixed list of sizes, bound to the value rather than an index.
///
/// The lists are `distantland`'s `SUPPORTED_*` constants, which is what
/// `validate_for_generation` checks against. Offering exactly those is what
/// stops a user choosing a size the host rejects at launch.
///
/// A job carrying an unsupported size still *displays* it, because the selected
/// text is formatted from the value and not looked up in the list. That is
/// deliberate: showing the real value and letting the user pick a valid one
/// beats silently rewriting a file the user has not asked to change.
pub(super) fn size_combo(ui: &mut Ui, id: &'static str, value: &mut u32, options: &[u32], width: f32) {
    ComboBox::from_id_salt(id)
        .selected_text(value.to_string())
        .icon(style::combo_arrow_icon)
        .width(width)
        .show_ui(ui, |ui| {
            for &option in options {
                selectable_value(ui, value, option, option.to_string());
            }
        });
}

/// `options` trimmed to entries the detected adapter can actually create.
///
/// Falls back to the full list when nothing was detected, or when the detected
/// size is below every option. An empty picker would be worse than one that
/// might offer a size the adapter turns out not to support.
pub(super) fn capped_size_options(options: &'static [u32], max_texture_dimension: Option<u32>) -> Vec<u32> {
    let Some(max) = max_texture_dimension else {
        return options.to_vec();
    };
    let capped: Vec<u32> = options.iter().copied().filter(|&size| size <= max).collect();
    if capped.is_empty() { options.to_vec() } else { capped }
}
