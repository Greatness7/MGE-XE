use std::path::Path;

use eframe::egui::{self, Align, Button, ComboBox, Context, Id, Key, Layout, Modal, RichText, Ui};
use mge_config::{Alignment, RenderSettings, Settings};
use rust_i18n::t;

use crate::{
    app::GuiApp,
    config::{
        self, AA_VALUES, ANISO_VALUES, FOG_VALUES, FOV_MAX, FOV_MIN, SS_FORMAT_VALUES, SS_SUFFIX_PREVIEWS, SS_SUFFIX_VALUES,
        VSYNC_VALUES,
    },
    platform::{DisplayMode, RegistrySettings},
    style,
};

use super::{
    aspect_ratio, combo_value_localized_sized, combo_value_preview_localized, combo_value_sized, control_cell,
    selectable_label, spinner_width, tooltip, value_field, vertical_rule,
};

pub(crate) struct GraphicsUiState {
    pub(crate) display_modes: Vec<DisplayMode>,
    pub(crate) resolution_editor: Option<ResolutionState>,
}

pub struct ResolutionState {
    pub draft: RegistrySettings,
}

/// Legacy `ResolutionForm` geometry, read straight off its designer block.
/// `ClientSize` `394 × 186` with a 12 px client margin, Segoe UI 9 pt at 96 DPI.
/// `TextStyle::Body` at 12.0 reproduces that font.
const RES_DIALOG_W: f32 = 370.0;
/// `cmbRes`: `(12, 12)`, `104 × 23`.
const RES_COMBO_W: f32 = 104.0;
/// `cmbRefreshRate`: `(221, 12)`, `70 × 23`.
const REFRESH_COMBO_W: f32 = 70.0;
const REFRESH_COMBO_X: f32 = 209.0;
/// `tbWidth` / `tbHeight`: `(46, 48)` and `(46, 74)`, `70 × 23`. They end on
/// `cmbRes`'s right edge rather than starting on its left one.
const RES_FIELD_W: f32 = 70.0;
const RES_FIELD_INDENT: f32 = RES_COMBO_W - RES_FIELD_W;
/// `lResolution`, `lScrWdth`, and `lScrHght` all start at x = 122.
const RES_LABEL_X: f32 = 110.0;
/// The gap a label keeps from the control it annotates.
const RES_LABEL_GAP: f32 = 6.0;
/// `bOK` / `bCancel`: `85 × 23`, the same width the shader dialogs use.
const RES_BTN_W: f32 = 85.0;
const REFRESH_DEFAULT: &str = "graphics.display.default";

fn display_card(
    ui: &mut Ui,
    settings: &mut Settings,
    registry: &mut RegistrySettings,
    resolution_editor: &mut Option<ResolutionState>,
) {
    style::card(ui, t!("graphics.display.title"), |ui| {
        const LABEL_W: f32 = 62.0;
        const RES_W: f32 = 100.0;
        const SMALL_W: f32 = 64.0;
        const CHECKS_W: f32 = 138.0;
        const LOCATION_W: f32 = 96.0;
        // One width for all three pipeline combos so they share both edges.
        // "Immediate" is the widest entry and sets the floor.
        const PIPE_COMBO_W: f32 = 64.0;
        const SEP_H: f32 = 7.0;

        let row_height = ui.spacing().interact_size.y;
        let gap = ui.spacing().item_spacing.x;
        let available = ui.available_width();
        let left_width = (available - CHECKS_W - LOCATION_W - gap * 4.0 - 20.0).max(300.0);
        // The left column is the tallest thing in the card, so it sets the
        // height of the divider and of the two blocks beside it. Derived from
        // the tokens rather than hardcoded: five rows, the gaps between them,
        // and the rule dividing the two groups.
        let block_h = row_height * 5.0 + ui.spacing().item_spacing.y * 5.0 + SEP_H;

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(left_width, block_h), Layout::top_down(Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized([LABEL_W, row_height], egui::Label::new(t!("graphics.display.resolution")));
                    tooltip(
                        value_field(
                            ui,
                            t!(
                                "graphics.display.resolution_value",
                                width = registry.width,
                                height = registry.height
                            ),
                            RES_W,
                        ),
                        t!("graphics.display.resolution_tip"),
                    );
                    tooltip(
                        value_field(ui, aspect_ratio(registry.width, registry.height), SMALL_W),
                        t!("graphics.display.aspect_ratio_tip"),
                    );
                    ui.label(t!("graphics.display.aspect_ratio"));
                });
                ui.horizontal(|ui| {
                    let select_resolution = tooltip(
                        ui.add_sized(
                            [LABEL_W + RES_W + gap, row_height],
                            egui::Button::new(t!("graphics.display.select_resolution")),
                        ),
                        t!("graphics.display.select_resolution_tip"),
                    );
                    if select_resolution.clicked() {
                        *resolution_editor = Some(ResolutionState { draft: *registry });
                    }
                    tooltip(
                        value_field(
                            ui,
                            if registry.refresh > 0 {
                                t!("graphics.display.refresh_value", rate = registry.refresh).into_owned()
                            } else {
                                t!(REFRESH_DEFAULT).into_owned()
                            },
                            SMALL_W,
                        ),
                        t!("graphics.display.refresh_rate_tip"),
                    );
                    ui.label(t!("graphics.display.refresh_rate"));
                });

                // The pipeline settings sit under the display settings as a
                // second group in the same column, one combo per row.
                ui.add(egui::Separator::default().spacing(SEP_H));

                let row = ui.horizontal(|ui| {
                    combo_value_localized_sized(
                        ui,
                        "anti_alias",
                        &mut settings.graphics.anti_aliasing,
                        &AA_VALUES,
                        Some(PIPE_COMBO_W),
                    );
                    ui.label(t!("graphics.display.antialiasing"));
                });
                tooltip(row.response, t!("graphics.display.antialiasing_tip"));
                let row = ui.horizontal(|ui| {
                    combo_value_localized_sized(
                        ui,
                        "anisotropy",
                        &mut settings.graphics.anisotropy,
                        &ANISO_VALUES,
                        Some(PIPE_COMBO_W),
                    );
                    ui.label(t!("graphics.display.anisotropic_filtering"));
                });
                tooltip(row.response, t!("graphics.display.anisotropic_filtering_tip"));
                let row = ui.horizontal(|ui| {
                    combo_value_localized_sized(
                        ui,
                        "vsync",
                        &mut settings.graphics.vsync,
                        &VSYNC_VALUES,
                        Some(PIPE_COMBO_W),
                    );
                    ui.label(t!("graphics.display.vsync"));
                });
                tooltip(row.response, t!("graphics.display.vsync_tip"));
            });

            vertical_rule(ui, block_h);

            ui.allocate_ui_with_layout(egui::vec2(CHECKS_W, block_h), Layout::top_down(Align::Min), |ui| {
                tooltip(
                    ui.checkbox(&mut registry.windowed, t!("graphics.display.windowed")),
                    t!("graphics.display.windowed_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.graphics.borderless, t!("graphics.display.borderless")),
                    t!("graphics.display.borderless_tip"),
                );
            });

            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.allocate_ui_with_layout(egui::vec2(LOCATION_W, block_h), Layout::top_down(Align::Center), |ui| {
                    ui.label(t!("graphics.display.window_location"));
                    ui.add_space(2.0);
                    // `Grid` derives its column width from `interact_size.x`,
                    // which would otherwise scatter these buttons far apart.
                    const CELL_W: f32 = 24.0;
                    const CELL_H: f32 = 20.0;
                    const PAD: f32 = 2.0;
                    const GRID_W: f32 = CELL_W * 3.0 + PAD * 2.0;
                    const GRID_H: f32 = CELL_H * 3.0 + PAD * 2.0;
                    const ARROWS: [[&str; 3]; 3] = [["↖", "↑", "↗"], ["←", "▪", "→"], ["↙", "↓", "↘"]];
                    const ALIGNMENTS: [Alignment; 3] = [Alignment::Left, Alignment::Center, Alignment::Right];
                    // Allocating the pad at its exact size is what lets the
                    // enclosing centring layout actually centre it; a plain
                    // `scope` would claim the full column width and pin the
                    // buttons to its left edge.
                    ui.allocate_ui_with_layout(egui::vec2(GRID_W, GRID_H), Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(PAD, PAD);
                        ui.spacing_mut().button_padding = egui::vec2(2.0, 1.0);
                        ui.spacing_mut().interact_size = egui::vec2(CELL_W, CELL_H);
                        egui::Grid::new("window_location_grid")
                            .spacing([PAD, PAD])
                            .min_col_width(CELL_W)
                            .max_col_width(CELL_W)
                            .min_row_height(CELL_H)
                            .show(ui, |ui| {
                                for (align_y, row) in ALIGNMENTS.into_iter().zip(ARROWS) {
                                    for (align_x, symbol) in ALIGNMENTS.into_iter().zip(row) {
                                        let selected = settings.render.window_align_x == align_x
                                            && settings.render.window_align_y == align_y;
                                        let label = RichText::new(symbol).font(style::symbol_font(12.0));
                                        let response = tooltip(
                                            ui.add_sized([CELL_W, CELL_H], egui::Button::new(label).selected(selected)),
                                            t!("graphics.display.window_location_tip"),
                                        );
                                        if response.clicked() {
                                            settings.render.window_align_x = align_x;
                                            settings.render.window_align_y = align_y;
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                });
            });
        });
    });
}

fn renderer_card(ui: &mut Ui, settings: &mut Settings, fps_limit: &mut u32, screen: (u32, u32)) -> bool {
    let mut open_shader_setup = false;
    style::card(ui, t!("graphics.renderer.title"), |ui| {
        // Every control in the right-hand band shares one right alignment edge:
        // the narrow spinners sit flush against it and the wider Fog mode combo
        // grows leftward into otherwise unused space. The band itself is anchored
        // to the card's right edge, with a rule delimiting it from the shader
        // controls, so the slack between the two groups is deliberate.
        const BAND_W: f32 = 158.0;
        const LEFT_W: f32 = 366.0;
        const BLOCK_H: f32 = 92.0;
        const FOV_SPIN_W: f32 = 66.0;
        // `ComboBox::width` sizes the interior only; the arrow and padding add
        // roughly 24px on top, so keep this well inside `BAND_W`.
        const COMBO_W: f32 = 108.0;
        let font_id = egui::TextStyle::Body.resolve(ui.style());
        let right_label_w = [
            "graphics.renderer.horizontal_fov",
            "graphics.renderer.fps_limiter",
            "graphics.renderer.ui_scaling",
            "graphics.renderer.fog_mode",
        ]
        .into_iter()
        .map(|key| {
            ui.painter()
                .layout_no_wrap(t!(key).into_owned(), font_id.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        })
        .fold(92.0_f32, f32::max)
        .min(130.0);
        let right_w = BAND_W + ui.spacing().item_spacing.x + right_label_w;

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(LEFT_W, BLOCK_H), Layout::top_down(Align::Min), |ui| {
                ui.horizontal(|ui| {
                    tooltip(
                        ui.checkbox(&mut settings.render.enable_shaders, t!("graphics.renderer.enable_shaders")),
                        t!("graphics.renderer.enable_shaders_tip"),
                    );
                    if tooltip(
                        ui.button(t!("graphics.renderer.shader_setup")),
                        t!("graphics.renderer.shader_setup_tip"),
                    )
                    .clicked()
                    {
                        open_shader_setup = true;
                    }
                });
                tooltip(
                    ui.checkbox(&mut settings.render.fps_counter, t!("graphics.renderer.display_fps")),
                    t!("graphics.renderer.display_fps_tip"),
                );
                ui.add_space(2.0);
            });

            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.allocate_ui_with_layout(egui::vec2(right_w, BLOCK_H), Layout::top_down(Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        control_cell(ui, BAND_W, |ui| {
                            ui.add_enabled_ui(!settings.gui.match_fov_to_aspect_ratio, |ui| {
                                spinner_width(ui, FOV_SPIN_W);
                                tooltip(
                                    ui.add(
                                        egui::DragValue::new(&mut settings.render.fov)
                                            .range(FOV_MIN..=FOV_MAX)
                                            .speed(0.1)
                                            .fixed_decimals(1),
                                    ),
                                    t!("graphics.renderer.horizontal_fov_tip"),
                                );
                            });
                            if tooltip(
                                ui.checkbox(&mut settings.gui.match_fov_to_aspect_ratio, t!("graphics.renderer.auto_fov")),
                                t!("graphics.renderer.auto_fov_tip"),
                            )
                            .changed()
                            {
                                // Ticking it derives the value immediately;
                                // unticking leaves the derived value in
                                // place as the starting point for manual
                                // editing, as the legacy GUI did.
                                config::refresh_auto_fov(settings, screen.0, screen.1);
                            }
                        });
                        ui.label(t!("graphics.renderer.horizontal_fov"));
                    });

                    let row = ui.horizontal(|ui| {
                        control_cell(ui, BAND_W, |ui| {
                            spinner_width(ui, FOV_SPIN_W);
                            ui.add(egui::DragValue::new(&mut *fps_limit).range(1..=300).speed(1));
                        });
                        ui.label(t!("graphics.renderer.fps_limiter"));
                    });
                    tooltip(row.response, t!("graphics.renderer.fps_limiter_tip"));

                    let row = ui.horizontal(|ui| {
                        control_cell(ui, BAND_W, |ui| {
                            spinner_width(ui, FOV_SPIN_W);
                            ui.add(
                                egui::DragValue::new(&mut settings.render.ui_scale)
                                    .range(0.5..=5.0)
                                    .speed(0.05)
                                    .fixed_decimals(2),
                            );
                        });
                        ui.label(t!("graphics.renderer.ui_scaling"));
                    });
                    tooltip(row.response, t!("graphics.renderer.ui_scaling_tip"));

                    let row = ui.horizontal(|ui| {
                        control_cell(ui, BAND_W, |ui| {
                            combo_value_localized_sized(
                                ui,
                                "fog_mode",
                                &mut settings.render.fog_mode,
                                &FOG_VALUES,
                                Some(COMBO_W),
                            );
                        });
                        ui.label(t!("graphics.renderer.fog_mode"));
                    });
                    tooltip(row.response, t!("graphics.renderer.fog_mode_tip"));
                });
            });
        });
    });

    open_shader_setup
}

fn screenshots_card(ui: &mut Ui, render: &mut RenderSettings, root: &Path) {
    style::card(ui, t!("graphics.screenshots.title"), |ui| {
        // All three rows share one label column, one field width, and one
        // trailing cell, so their left and right edges line up exactly.
        const SS_TRAIL_W: f32 = 130.0;
        let row_height = ui.spacing().interact_size.y;
        let gap = ui.spacing().item_spacing.x;
        let font_id = egui::TextStyle::Body.resolve(ui.style());
        let label_w = [
            "graphics.screenshots.file_prefix",
            "graphics.screenshots.file_format",
            "graphics.screenshots.output_directory",
        ]
        .into_iter()
        .map(|key| {
            ui.painter()
                .layout_no_wrap(t!(key).into_owned(), font_id.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        })
        .fold(96.0_f32, f32::max)
        .min(160.0);
        let field_w = (ui.available_width() - label_w - SS_TRAIL_W - gap * 2.0).max(150.0);

        ui.horizontal(|ui| {
            ui.add_sized(
                [label_w, row_height],
                egui::Label::new(t!("graphics.screenshots.file_prefix")),
            );
            tooltip(
                ui.add_sized([field_w, row_height], egui::TextEdit::singleline(&mut render.screenshot_name)),
                t!("graphics.screenshots.file_prefix_tip"),
            );
            ui.allocate_ui_with_layout(
                egui::vec2(SS_TRAIL_W, row_height),
                Layout::right_to_left(Align::Center),
                |ui| {
                    if let Some(response) = combo_value_sized(
                        ui,
                        "screenshot_format",
                        &mut render.screenshot_format,
                        &SS_FORMAT_VALUES,
                        Some(50.0),
                    ) {
                        tooltip(response, t!("graphics.screenshots.format_tip"));
                    }
                    ui.label(t!("graphics.screenshots.format"));
                },
            );
        });

        let row = ui.horizontal(|ui| {
            ui.add_sized(
                [label_w, row_height],
                egui::Label::new(t!("graphics.screenshots.file_format")),
            );
            combo_value_preview_localized(
                ui,
                "screenshot_suffix",
                &mut render.screenshot_suffix,
                &SS_SUFFIX_VALUES,
                &SS_SUFFIX_PREVIEWS,
                field_w,
            );
        });
        tooltip(row.response, t!("graphics.screenshots.file_format_tip"));

        ui.horizontal(|ui| {
            ui.add_sized(
                [label_w, row_height],
                egui::Label::new(t!("graphics.screenshots.output_directory")),
            );
            tooltip(
                ui.add_sized(
                    [field_w, row_height],
                    egui::TextEdit::singleline(&mut render.screenshot_directory)
                        .hint_text(t!("graphics.screenshots.directory_hint")),
                ),
                t!("graphics.screenshots.output_directory_tip"),
            );
            // Fill the trailing cell with two equal buttons. The enclosing
            // horizontal layout supplies the standard gap after the text edit
            // and between the buttons.
            let button_w = ((ui.available_width() - gap) / 2.0).max(0.0);
            if ui
                .add_sized([button_w, row_height], egui::Button::new(t!("common.actions.browse")))
                .clicked()
                && let Some(path) = rfd::FileDialog::new().set_directory(root).pick_folder()
            {
                render.screenshot_directory = path.to_string_lossy().into_owned();
            }
            if tooltip(
                ui.add_sized([button_w, row_height], egui::Button::new(t!("common.actions.clear"))),
                t!("graphics.screenshots.clear_directory_tip"),
            )
            .clicked()
            {
                render.screenshot_directory.clear();
            }
        });

        ui.add_space(2.0);

        style::hint(ui, t!("graphics.screenshots.hint").as_ref());
    });
}

impl GuiApp {
    pub(crate) fn show_graphics(&mut self, ui: &mut Ui) {
        display_card(
            ui,
            &mut self.settings.mge,
            &mut self.registry,
            &mut self.ui.graphics.resolution_editor,
        );

        ui.add_space(3.0);
        if renderer_card(
            ui,
            &mut self.settings.mge,
            &mut self.settings.ini.fps_limit,
            (self.registry.width, self.registry.height),
        ) {
            self.open_shader_setup();
        }

        ui.add_space(3.0);
        screenshots_card(ui, &mut self.settings.mge.render, self.store.root());
    }

    /// The **Select Resolution** overlay: a fixed, centred modal reproducing the
    /// legacy `ResolutionForm`, with the resolution and refresh-rate combos on one
    /// row, `Screen Width` / `Screen Height` indented under them, and
    /// `OK` / `Cancel` on the bottom right. Only `OK` writes. `Cancel`, Escape,
    /// and a backdrop click discard the draft.
    pub(super) fn show_resolution_dialog(&mut self, ctx: &Context) {
        let Some(mut state) = self.ui.graphics.resolution_editor.take() else {
            return;
        };
        // `Windowed mode` lives on the Display card, exactly as the legacy tab
        // carried it outside this form. The dialog only *reads* it, to gate the
        // arbitrary-dimension fields the way `Fullscreen` gated the text boxes.
        let windowed = self.registry.windowed;
        let modes = &self.ui.graphics.display_modes;
        let resolutions = distinct_resolutions(modes);
        let mut accepted = false;
        let mut cancelled = false;

        let response = Modal::new(Id::new("resolution_modal")).show(ctx, |ui| {
            ui.set_width(RES_DIALOG_W);
            // Sampled before the fields are drawn: a `DragValue` surrenders focus
            // on the same Enter that commits its text, so asking afterwards always
            // reports nothing focused.
            let editing = ui.memory(|m| m.focused().is_some());
            ui.label(RichText::new(t!("graphics.resolution.title")).strong());
            ui.add_space(6.0);

            let draft = &mut state.draft;
            let rates = refresh_rates(modes, draft.width, draft.height);

            ui.horizontal(|ui| {
                // Cell widths are the legacy control origins, so the spacing token
                // must not add a gap on top of them.
                ui.spacing_mut().item_spacing.x = 0.0;
                res_cell(ui, RES_LABEL_X, |ui| {
                    ComboBox::from_id_salt("resolution_mode")
                        .icon(style::combo_arrow_icon)
                        .selected_text(format!("{} × {}", draft.width, draft.height))
                        .width(RES_COMBO_W)
                        .show_ui(ui, |ui| {
                            for (width, height) in &resolutions {
                                let selected = draft.width == *width && draft.height == *height;
                                if selectable_label(ui, selected, format!("{width} × {height}")).clicked() {
                                    draft.width = *width;
                                    draft.height = *height;
                                    // `cmbRes_SelectedIndexChanged` rebuilt the rate
                                    // list and kept the previous rate only when the
                                    // new resolution still offered it.
                                    if draft.refresh != 0 && !refresh_rates(modes, *width, *height).contains(&draft.refresh)
                                    {
                                        draft.refresh = 0;
                                    }
                                }
                            }
                        });
                });
                res_cell(ui, REFRESH_COMBO_X - RES_LABEL_X, |ui| {
                    ui.label(t!("graphics.display.resolution"));
                });
                res_cell(ui, REFRESH_COMBO_W + RES_LABEL_GAP, |ui| {
                    // Salted with the resolution: the popup's stored size is keyed
                    // by id, so a rate list that grows under a *stable* id keeps
                    // the height it was first measured at. Opening the dialog on
                    // an arbitrary windowed size (no enumerated rates, so one
                    // `Default` row) and then picking a real resolution otherwise
                    // leaves a three-row scroller over a ten-entry list.
                    ComboBox::from_id_salt(("refresh_rate", draft.width, draft.height))
                        .icon(style::combo_arrow_icon)
                        .selected_text(refresh_caption(draft.refresh))
                        .width(REFRESH_COMBO_W)
                        .show_ui(ui, |ui| {
                            if selectable_label(ui, draft.refresh == 0, t!(REFRESH_DEFAULT)).clicked() {
                                draft.refresh = 0;
                            }
                            for rate in &rates {
                                if selectable_label(ui, draft.refresh == *rate, rate.to_string()).clicked() {
                                    draft.refresh = *rate;
                                }
                            }
                        });
                });
                ui.label(t!("graphics.display.refresh_rate"));
            });

            ui.add_space(10.0);
            let dimension_row = |ui: &mut Ui, label: &str, value: &mut u32| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    res_cell(ui, RES_LABEL_X, |ui| {
                        // `tbWidth`/`tbHeight` are narrower than the combo above and
                        // share its right edge, so they start indented.
                        ui.add_space(RES_FIELD_INDENT);
                        spinner_width(ui, RES_FIELD_W);
                        ui.add_enabled(windowed, egui::DragValue::new(value).range(1..=u32::MAX).speed(1));
                    });
                    ui.label(label);
                });
            };
            dimension_row(ui, t!("graphics.resolution.screen_width").as_ref(), &mut draft.width);
            dimension_row(ui, t!("graphics.resolution.screen_height").as_ref(), &mut draft.height);

            if !windowed {
                ui.add_space(8.0);
                style::hint(ui, t!("graphics.resolution.windowed_only").as_ref());
            }

            ui.add_space(10.0);
            let height = ui.spacing().interact_size.y;
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_sized([RES_BTN_W, height], Button::new(t!("common.actions.cancel")))
                        .clicked()
                    {
                        cancelled = true;
                    }
                    if ui
                        .add_sized([RES_BTN_W, height], Button::new(t!("common.actions.ok")))
                        .clicked()
                    {
                        accepted = true;
                    }
                });
            });
            // Enter accepts, as the shader dialogs do, but not while a dimension
            // field is being typed into, where Enter means "commit this number".
            // Unguarded, it closed the dialog on the first of the two dimensions.
            if !editing && ui.input(|i| i.key_pressed(Key::Enter)) {
                accepted = true;
            }
        });

        // `should_close` is Escape or a click on the dimmed backdrop; both cancel.
        if response.should_close() {
            cancelled = true;
        }

        if accepted {
            // `ShowDialog` wrote back only Screen Width / Height / Refresh Rate.
            // `Fullscreen` and `Adapter` are not this dialog's to change.
            self.registry.width = state.draft.width;
            self.registry.height = state.draft.height;
            self.registry.refresh = state.draft.refresh;
            self.settings.mge.graphics.refresh_rate = state.draft.refresh.min(240) as u8;
            config::refresh_auto_fov(&mut self.settings.mge, state.draft.width, state.draft.height);
            self.set_success(t!("messages.display_mode_applied"));
        } else if !cancelled {
            self.ui.graphics.resolution_editor = Some(state);
        }
    }
}

/// The distinct resolutions offered by the enumerated display modes, largest
/// first: `DXMain.GetResolutions()` followed by the legacy `Resolutions.Reverse()`.
fn distinct_resolutions(modes: &[DisplayMode]) -> Vec<(u32, u32)> {
    let mut list: Vec<(u32, u32)> = modes.iter().map(|m| (m.width, m.height)).collect();
    list.sort_unstable();
    list.dedup();
    list.reverse();
    list
}

/// The refresh rates one resolution supports, per `DXMain.GetRefreshRates(w, h)`.
fn refresh_rates(modes: &[DisplayMode], width: u32, height: u32) -> Vec<u32> {
    let mut list: Vec<u32> = modes
        .iter()
        .filter(|m| m.width == width && m.height == height)
        .map(|m| m.refresh)
        .collect();
    list.sort_unstable();
    list.dedup();
    list
}

/// Rate `0` is the registry's "let the driver pick", which the legacy combo
/// carried as its first entry.
fn refresh_caption(refresh: u32) -> String {
    if refresh == 0 {
        t!(REFRESH_DEFAULT).into_owned()
    } else {
        refresh.to_string()
    }
}

/// A fixed-width column in one of the dialog's rows.
fn res_cell(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(egui::vec2(width, height), Layout::left_to_right(Align::Center), |ui| {
        // `allocate_ui_with_layout` allocates the child's `min_rect`, so a cell
        // holding short content shrinks and the next column edge drifts
        // (pitfall 21).
        ui.set_min_width(width);
        add(ui);
    });
}
