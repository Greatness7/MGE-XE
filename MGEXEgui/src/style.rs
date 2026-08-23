use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Margin, Stroke, TextStyle, Theme,
    Vec2,
};

pub const APP_BG: Color32 = Color32::from_rgb(32, 32, 32);
pub const PANEL: Color32 = Color32::from_rgb(32, 32, 32);
pub const CARD: Color32 = Color32::from_rgb(43, 43, 43);
pub const CARD_HEADER: Color32 = Color32::from_rgb(50, 50, 50);
pub const CARD_HOVER: Color32 = Color32::from_rgb(61, 61, 61);
pub const BORDER: Color32 = Color32::from_rgb(61, 61, 61);
pub const TEXT: Color32 = Color32::from_rgb(225, 229, 235);
pub const MUTED: Color32 = Color32::from_rgb(155, 162, 174);
pub const ACCENT: Color32 = Color32::from_rgb(61, 61, 61);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(56, 56, 56);
pub const GOOD: Color32 = Color32::from_rgb(71, 195, 142);
pub const WARN: Color32 = Color32::from_rgb(245, 181, 76);
pub const BAD: Color32 = Color32::from_rgb(244, 100, 100);

pub fn install(ctx: &egui::Context) {
    install_font(ctx);
    ctx.set_theme(Theme::Dark);

    let mut style = ctx.style_of(Theme::Dark).as_ref().clone();
    style.spacing.item_spacing = Vec2::new(6.0, 3.0);
    style.spacing.button_padding = Vec2::new(7.0, 2.0);
    style.spacing.interact_size.y = 20.0;
    style.spacing.slider_width = 120.0;
    style.spacing.combo_height = 240.0;
    style.spacing.icon_width = 15.0;
    style.spacing.icon_width_inner = 8.0;
    style.spacing.scroll.foreground_color = false;
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = PANEL;
    style.visuals.window_fill = PANEL;
    style.visuals.extreme_bg_color = APP_BG;
    style.visuals.faint_bg_color = CARD;
    style.visuals.code_bg_color = APP_BG;
    style.visuals.selection.bg_fill = ACCENT_DARK;
    style.visuals.selection.stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.noninteractive.bg_fill = CARD;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.inactive.bg_fill = CARD;
    style.visuals.widgets.inactive.weak_bg_fill = CARD;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.hovered.bg_fill = CARD_HOVER;
    style.visuals.widgets.hovered.weak_bg_fill = CARD_HOVER;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.bg_fill = ACCENT_DARK;
    style.visuals.widgets.active.weak_bg_fill = ACCENT_DARK;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.hyperlink_color = ACCENT;

    // Corner Radius
    style.visuals.menu_corner_radius = CornerRadius::from(2.0);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::from(2.0);
    style.visuals.widgets.active.corner_radius = CornerRadius::from(2.0);

    style
        .text_styles
        .insert(TextStyle::Heading, FontId::new(15.0, FontFamily::Proportional));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::new(12.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(12.0, FontFamily::Proportional), //
    );
    // The shader source editor is the only monospaced surface. 12.0 is the same
    // 9 pt-at-96-DPI baseline as `Body`, which is what the legacy editor used
    // (Courier New 9 pt). See `install_font` for the face itself.
    style
        .text_styles
        .insert(TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace));
    ctx.set_style_of(Theme::Dark, style);
}

/// Font family used by the window-location pad. Segoe UI covers the cardinal
/// arrows but not the diagonals (U+2196..U+2199), so without a dedicated symbol
/// face egui falls back to its emoji font for four of the nine glyphs and the
/// pad renders in two visibly different styles.
pub const SYMBOL_FAMILY: &str = "mge_symbols";

pub fn symbol_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SYMBOL_FAMILY.into()))
}

/// Paints a compact Windows-style down chevron for combo boxes. egui's default
/// icon is a filled triangle; `ComboBox::icon` is the only supported override,
/// so every combo constructor routes through this painter.
pub fn combo_arrow_icon(ui: &egui::Ui, rect: egui::Rect, visuals: &egui::style::WidgetVisuals, _is_open: bool) {
    let center = rect.center();
    let half_width = 3.5;
    let half_height = 1.75;
    ui.painter().add(egui::Shape::line(
        vec![
            egui::pos2(center.x - half_width, center.y - half_height),
            egui::pos2(center.x, center.y + half_height),
            egui::pos2(center.x + half_width, center.y - half_height),
        ],
        Stroke::new(1.0, visuals.fg_stroke.color),
    ));
}

fn install_font(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut load = |name: &str, file: &str| {
        let path = std::path::Path::new(r"C:\Windows\Fonts").join(file);
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
        true
    };
    let has_ui = load("Segoe UI", "segoeui.ttf");
    let has_symbol = load("Segoe UI Symbol", "seguisym.ttf");
    // The shader editor's source pane. Consolas first because it is the face
    // Windows itself codes in; Courier New behind it is what the legacy editor
    // used, and is present on every Windows install. Both are prepended, so
    // egui's bundled monospace remains the tail and the family is never empty.
    let has_courier = load("Courier New", "cour.ttf");
    let has_consolas = load("Consolas", "consola.ttf");

    if has_ui && let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.insert(0, "Segoe UI".to_owned());
    }
    if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
        if has_courier {
            family.insert(0, "Courier New".to_owned());
        }
        if has_consolas {
            family.insert(0, "Consolas".to_owned());
        }
    }
    let mut symbols = Vec::new();
    if has_symbol {
        symbols.push("Segoe UI Symbol".to_owned());
    }
    // Always keep the proportional chain as a tail so the family is never empty.
    symbols.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Name(SYMBOL_FAMILY.into()), symbols);
    ctx.set_fonts(fonts);
}

/// A titled panel. The title sits in its own slightly lighter band, which reads
/// as a group-box caption instead of just another label in the body.
pub fn card<R>(ui: &mut egui::Ui, title: impl Into<egui::WidgetText>, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_enabled(ui, true, title, add_contents)
}

/// A card whose caption greys out along with its contents, for the group boxes
/// that a master checkbox switches off wholesale.
///
/// The gate deliberately wraps only the caption and the body, not the two
/// frames: egui disables by fading everything the painter emits, so gating the
/// card from the outside would wash out the fills as well and leave the panel
/// looking like a rendering glitch rather than a disabled group.
pub fn card_enabled<R>(
    ui: &mut egui::Ui,
    enabled: bool,
    title: impl Into<egui::WidgetText>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    card_sized(ui, enabled, 0.0, title, add_contents).0
}

/// Vertical padding inside a card body, both edges.
const CARD_BODY_MARGIN: i8 = 5;

/// A card padded out to `min_height`, returning both its contents' value and
/// the height it would have taken unpadded.
///
/// Side-by-side cards whose bodies differ in length otherwise end at different
/// depths, which reads as a misalignment rather than as two independent groups.
/// egui lays a column out before it can know how tall its neighbour will be, so
/// the caller carries the tallest height across frames; see
/// [`crate::ui::generate::widgets::columns_equal_height`]. The unpadded height
/// is what lets that cached value shrink again when a wrapped hint unwraps.
pub fn card_sized<R>(
    ui: &mut egui::Ui,
    enabled: bool,
    min_height: f32,
    title: impl Into<egui::WidgetText>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, f32) {
    let item_spacing = ui.spacing().item_spacing;
    let mut natural_height = 0.0;
    let inner = Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(2.0)
        .show(ui, |ui| {
            // The header band and the body must not be separated by item spacing.
            ui.spacing_mut().item_spacing.y = 0.0;
            let header = Frame::new()
                .fill(CARD_HEADER)
                .corner_radius(CornerRadius {
                    nw: 3,
                    ne: 3,
                    sw: 0,
                    se: 0,
                })
                .inner_margin(Margin {
                    left: 8,
                    right: 8,
                    top: 2,
                    bottom: 2,
                })
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add_enabled_ui(enabled, |ui| {
                        ui.label(title.into().strong().color(TEXT));
                    });
                });
            let header_height = header.response.rect.height();
            Frame::new()
                .inner_margin(Margin::symmetric(8, CARD_BODY_MARGIN))
                .show(ui, |ui| {
                    // `Ui::columns` hands out `top_down_justified` children, and a
                    // card must not inherit that: it stretches every button inside
                    // to the full column width.
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing = item_spacing;
                        // Without this a card shrinks to its content and
                        // neighbouring cards end up ragged.
                        ui.set_min_width(ui.available_width());
                        let inner = ui.add_enabled_ui(enabled, add_contents).inner;
                        // Measured before the padding is added, so the caller's
                        // cached maximum can fall as well as rise.
                        natural_height = header_height + ui.min_rect().height() + 2.0 * CARD_BODY_MARGIN as f32;
                        ui.add_space((min_height - natural_height).max(0.0));
                        inner
                    })
                    .inner
                })
                .inner
        })
        .inner;
    (inner, natural_height)
}

pub fn hint(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.label(text.into().color(MUTED));
}
