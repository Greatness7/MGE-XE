//! The **Set active shaders** window.
//!
//! A preset-and-feature pane on the left and the manual chain editor on the
//! right behind the `Modding >>>` toggle. The seven feature dropdowns are
//! *derived* from the chain rather than stored beside it, so changing one
//! rebuilds the whole chain from the seven values.
//!
//! The modding pane's `Editor` / `New` / `Open` buttons open the shader source
//! editor (a separate window in [`editor`]).

mod editor;

use eframe::egui::{
    Align, Button, CentralPanel, Context, DragValue, Frame, Label, Layout, Margin, Panel, RichText, ScrollArea, Ui, Vec2,
    ViewportBuilder, ViewportCommand, ViewportId, vec2,
};
use rust_i18n::t;

use crate::{
    app::GuiApp,
    shaders::{
        CUSTOM_PRESET, EFFECT_OPTIONS, ShaderCatalog, ShaderEditor, effect_selection, matching_preset, preset_labels,
    },
    style,
};

use super::widgets::{
    combo_index_localized_sized, labeled_row, preview_job, right_aligned, selectable_label, spinner_width, tooltip,
};
use editor::ShaderEditorState;

/// Left pane width, sized for translated effect names beside the fixed-width
/// combos.
const LEFT_W: f32 = 360.0;
/// Feature and preset dropdown width (legacy: 110–125 px).
const COMBO_W: f32 = 118.0;
/// Width of the `sec` unit label beside the exposure spinner.
const UNIT_W: f32 = 24.0;
/// Shared width of every command button (legacy: 79–95 px).
const BTN_W: f32 = 86.0;
/// List heights, sized so the shipped catalog and the largest preset chain
/// both fit without scrolling.
const AVAIL_H: f32 = 168.0;
const ACTIVE_H: f32 = 156.0;
/// Name-column width of the active-chain rows (legacy `lvActive` column 0).
const NAME_COL_W: f32 = 180.0;
/// Inner margin between a list box's border and its rows.
const LIST_MARGIN: f32 = 2.0;

/// Collapsed and expanded window sizes, the two states `Modding >>>` toggles
/// between. They differ in height as well as width: the modding pane is the
/// taller of the two, and denser egui rows would otherwise leave the collapsed
/// state with dead air above its buttons. The collapsed width is wide enough
/// for Polish and Russian effect labels beside their combo boxes.
const COLLAPSED_SIZE: [f32; 2] = [382.0, 344.0];
const EXPANDED_SIZE: [f32; 2] = [776.0, 456.0];

/// Combo ids, one per entry of `EFFECT_OPTIONS`. The combo helper wants a
/// `&'static str` salt, so these cannot be generated from the loop index.
const EFFECT_COMBO_IDS: &[&str] = &[
    "shader_effect_hdr",
    "shader_effect_ssao",
    "shader_effect_bloom",
    "shader_effect_sunshafts",
    "shader_effect_dof",
    "shader_effect_water_sunshafts",
    "shader_effect_caustics",
];
const EFFECT_TIP_KEYS: &[&str] = &[
    "shaders.effects.hdr_tip",
    "shaders.effects.ssao_tip",
    "shaders.effects.bloom_tip",
    "shaders.effects.sunshafts_tip",
    "shaders.effects.depth_of_field_tip",
    "shaders.effects.underwater_sunshafts_tip",
    "shaders.effects.interior_caustics_tip",
];

pub(crate) struct ShaderDialogs {
    pub(crate) catalog: ShaderCatalog,
    pub(crate) setup: Option<ShaderSetupState>,
    pub(crate) editor: Option<ShaderEditorState>,
}

pub struct ShaderSetupState {
    pub active: Vec<String>,
    /// Draft exposure time, committed to the settings by `Save` only, so
    /// `Cancel` discards it along with the chain.
    pub hdr_time: f32,
    pub selected_available: Option<usize>,
    pub selected_active: Option<usize>,
    /// Whether the modding pane is shown; drives the window width.
    pub expanded: bool,
    /// The first native viewport frame is rendered while hidden on Windows, then
    /// the next frame reveals it to avoid the non-root viewport white flash.
    pub viewport_ready: bool,
}

/// What the window body decided this frame.
enum Act {
    Save,
    Close,
}

/// A source-editor request raised from the modding pane.
enum EditorRequest {
    Catalog(usize),
    New,
    Browse,
}

impl GuiApp {
    pub(crate) fn open_shader_setup(&mut self) {
        self.ui.shaders.setup = Some(ShaderSetupState {
            active: self.settings.mge.shaders.chain.clone(),
            hdr_time: self.settings.mge.render.hdr_reaction_time,
            selected_available: None,
            selected_active: None,
            expanded: false,
            viewport_ready: false,
        });
    }

    /// Called every frame from `show_dialogs`; renders the child window while
    /// the setup state exists.
    pub(super) fn show_shader_setup_dialog(&mut self, ctx: &Context) {
        let Some(state) = self.ui.shaders.setup.as_ref() else {
            return;
        };

        let size = if state.expanded { EXPANDED_SIZE } else { COLLAPSED_SIZE };
        let viewport_ready = state.viewport_ready;
        let mut builder = ViewportBuilder::default()
            .with_title(t!("shaders.setup.title"))
            .with_inner_size(size)
            .with_resizable(false)
            .with_clamp_size_to_monitor_size(true)
            .with_visible(viewport_ready);
        if let Some(icon) = crate::load_icon() {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(ViewportId::from_hash_of("mge_shader_setup"), builder, |ui, _class| {
            self.shader_setup_body(ui)
        });

        if let Some(state) = self.ui.shaders.setup.as_mut()
            && !state.viewport_ready
        {
            state.viewport_ready = true;
            ctx.request_repaint();
        }
    }

    fn shader_setup_body(&mut self, ui: &mut Ui) {
        let Some(mut state) = self.ui.shaders.setup.take() else {
            return;
        };

        // Dropped: the window closes and the draft goes with it, which is what
        // the legacy form's `CancelButton` did.
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            return;
        }

        let mut act = None;
        let mut request = None;
        let mut toggled = false;
        let editor_busy = self.ui.shaders.editor.is_some();

        let row_h = ui.spacing().interact_size.y;
        // The `ui.horizontal` wrapper is load-bearing here; see pitfall 23.
        Panel::bottom("shader_setup_footer").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_sized([BTN_W, row_h], Button::new(t!("common.actions.cancel")))
                        .clicked()
                    {
                        act = Some(Act::Close);
                    }
                    if ui.add_sized([BTN_W, row_h], Button::new(t!("common.actions.save"))).clicked() {
                        act = Some(Act::Save);
                    }
                });
            });
            ui.add_space(4.0);
        });

        CentralPanel::default().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let height = ui.available_height();
                ui.allocate_ui_with_layout(vec2(LEFT_W, height), Layout::top_down(Align::Min), |ui| {
                    // `allocate_ui_with_layout` treats its size as an upper
                    // bound, so the pane needs its width pinning from the
                    // inside or it shrinks to its content (pitfall 21).
                    ui.set_min_width(LEFT_W);
                    toggled = options_pane(ui, &self.ui.shaders.catalog, &mut state);
                });
                if state.expanded {
                    let width = ui.available_width();
                    ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Min), |ui| {
                        ui.set_min_width(width);
                        request = modding_pane(ui, &self.ui.shaders.catalog, &mut state, editor_busy);
                    });
                }
            });
        });

        if toggled {
            state.expanded = !state.expanded;
            // An immediate viewport's builder is replaced wholesale each frame
            // rather than diffed into commands, so the resize has to be asked
            // for explicitly.
            let size = if state.expanded { EXPANDED_SIZE } else { COLLAPSED_SIZE };
            ui.ctx().send_viewport_cmd(ViewportCommand::InnerSize(Vec2::from(size)));
        }

        // Native file dialogs and filesystem reads stay outside the render pass.
        match request {
            Some(EditorRequest::New) => self.ui.shaders.editor = Some(ShaderEditorState::new(ShaderEditor::new())),
            Some(EditorRequest::Catalog(index)) => {
                if let Some(shader) = self.ui.shaders.catalog.shaders.get(index).cloned() {
                    match ShaderEditor::open(&shader) {
                        Ok(doc) => self.ui.shaders.editor = Some(ShaderEditorState::new(doc)),
                        Err(error) => {
                            self.set_error(t!("shaders.messages.open_failed", error = format!("{error:#}")).into_owned())
                        }
                    }
                }
            }
            Some(EditorRequest::Browse) => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_directory(self.store.root().join("Data Files").join("shaders").join("XEshaders"))
                    .add_filter(t!("shaders.file.effect").as_ref(), &["fx"])
                    .pick_file()
                {
                    match ShaderEditor::open_path(path) {
                        Ok(doc) => self.ui.shaders.editor = Some(ShaderEditorState::new(doc)),
                        Err(error) => {
                            self.set_error(t!("shaders.messages.open_failed", error = format!("{error:#}")).into_owned())
                        }
                    }
                }
            }
            None => {}
        }

        match act {
            Some(Act::Save) => {
                self.settings.mge.render.hdr_reaction_time = state.hdr_time;
                self.settings.mge.shaders.chain = state.active;
                self.settings.mge.render.enable_shaders = !self.settings.mge.shaders.chain.is_empty();
                self.set_success(t!("shaders.messages.chain_applied").into_owned());
            }
            Some(Act::Close) => {}
            None => self.ui.shaders.setup = Some(state),
        }
    }
}

/// The left pane: quality preset, the seven feature dropdowns, the exposure
/// spinner, and the pane toggle. Returns whether the toggle was clicked.
///
/// Neither the preset nor the feature selections are stored: both are read back
/// out of the chain every frame, so a chain edited by hand on the right is
/// described correctly here without a sync step. A changed dropdown rebuilds
/// the chain from all seven values and drops anything else that was in it.
fn options_pane(ui: &mut Ui, catalog: &ShaderCatalog, state: &mut ShaderSetupState) -> bool {
    style::card(ui, t!("shaders.setup.options"), |ui| {
        let labels = preset_labels();
        let mut preset = matching_preset(&state.active);
        let shown_preset = preset;
        tooltip(
            labeled_row(ui, t!("shaders.setup.quality_preset").as_ref(), |ui| {
                combo_index_localized_sized(ui, "shader_preset", &mut preset, labels.as_slice(), Some(COMBO_W));
            }),
            t!("shaders.setup.quality_preset_tip"),
        );
        if preset != shown_preset && preset != CUSTOM_PRESET {
            state.active = catalog.preset_chain(preset);
            state.selected_active = None;
        }

        ui.add_space(8.0);

        let mut selections: Vec<usize> = EFFECT_OPTIONS
            .iter()
            .map(|effect| effect_selection(&state.active, effect))
            .collect();
        let shown_selections = selections.clone();

        for (index, effect) in EFFECT_OPTIONS.iter().enumerate() {
            tooltip(
                labeled_row(ui, t!(effect.label).as_ref(), |ui| {
                    combo_index_localized_sized(
                        ui,
                        EFFECT_COMBO_IDS[index],
                        &mut selections[index],
                        effect.options,
                        Some(COMBO_W),
                    );
                }),
                t!(EFFECT_TIP_KEYS[index]),
            );
            // The legacy form drew the exposure spinner directly under the HDR
            // combo it belongs to, not with the other numeric settings.
            if index == 0 {
                tooltip(
                    labeled_row(ui, t!("shaders.setup.exposure_time").as_ref(), |ui| {
                        // Spinner plus unit span exactly one combo width, so the
                        // spinner's left edge lands on the dropdowns' left edge
                        // rather than floating between the two columns.
                        let spacing = ui.spacing().item_spacing.x;
                        ui.add_sized(
                            [UNIT_W, ui.spacing().interact_size.y],
                            Label::new(RichText::new(t!("shaders.setup.seconds")).color(style::MUTED)),
                        );
                        spinner_width(ui, COMBO_W - UNIT_W - spacing);
                        ui.add(
                            DragValue::new(&mut state.hdr_time)
                                .speed(0.1)
                                .range(0.01..=30.0)
                                .fixed_decimals(2),
                        );
                    }),
                    t!("shaders.setup.exposure_time_tip"),
                );
            }
        }

        if selections != shown_selections {
            state.active = catalog.chain_from_effects(&selections);
            state.selected_active = None;
        }
    });

    ui.add_space(8.0);
    let row_h = ui.spacing().interact_size.y;
    let mut toggled = false;
    right_aligned(ui, |ui| {
        let label = if state.expanded {
            t!("shaders.setup.modding_collapse")
        } else {
            t!("shaders.setup.modding_expand")
        };
        if ui.add_sized([98.0, row_h], Button::new(label)).clicked() {
            toggled = true;
        }
    });
    toggled
}

/// The right pane: the available-shader list, the active chain, and their
/// command buttons. Returns a source-editor request, which the caller services
/// after the render pass.
fn modding_pane(
    ui: &mut Ui,
    catalog: &ShaderCatalog,
    state: &mut ShaderSetupState,
    editor_busy: bool,
) -> Option<EditorRequest> {
    let mut request = None;
    let row_h = ui.spacing().interact_size.y;

    style::card(ui, t!("shaders.setup.available"), |ui| {
        ui.horizontal_top(|ui| {
            let list_w = ui.available_width() - BTN_W - ui.spacing().item_spacing.x * 2.0;
            list_box(ui, "available_shaders", list_w, AVAIL_H, |ui| {
                // Every shader stays listed, active or not: hiding active ones
                // would put the `Editor` button out of reach of exactly the
                // shaders in use.
                for (index, shader) in catalog.shaders.iter().enumerate() {
                    let response = selectable_label(ui, state.selected_available == Some(index), shader.name.as_str());
                    if response.clicked() {
                        state.selected_available = Some(index);
                    }
                    if response.double_clicked() {
                        state.selected_available = Some(index);
                        catalog.insert_sorted(&mut state.active, &shader.name);
                    }
                }
            });
            ui.vertical(|ui| {
                let selected = state.selected_available.filter(|index| *index < catalog.shaders.len());
                if ui
                    .add_enabled(
                        !editor_busy && selected.is_some(),
                        Button::new(t!("shaders.setup.editor")).min_size(vec2(BTN_W, row_h)),
                    )
                    .clicked()
                    && let Some(index) = selected
                {
                    request = Some(EditorRequest::Catalog(index));
                }
                if ui
                    .add_enabled(
                        !editor_busy,
                        Button::new(t!("common.actions.new")).min_size(vec2(BTN_W, row_h)),
                    )
                    .clicked()
                {
                    request = Some(EditorRequest::New);
                }
                if ui
                    .add_enabled(
                        !editor_busy,
                        Button::new(t!("common.actions.open")).min_size(vec2(BTN_W, row_h)),
                    )
                    .clicked()
                {
                    request = Some(EditorRequest::Browse);
                }
            });
        });
    });

    style::card(ui, t!("shaders.setup.active"), |ui| {
        ui.horizontal_top(|ui| {
            let list_w = ui.available_width() - BTN_W - ui.spacing().item_spacing.x * 2.0;
            let mut remove = None;
            list_box(ui, "active_shaders", list_w, ACTIVE_H, |ui| {
                for (index, name) in state.active.iter().enumerate() {
                    // Two columns: name, then category, as the legacy
                    // `SysListView32` had, with its headers hidden.
                    let job = preview_job(ui, name, catalog.category_of(name), NAME_COL_W);
                    let response = selectable_label(ui, state.selected_active == Some(index), job);
                    if response.clicked() {
                        state.selected_active = Some(index);
                    }
                    if response.double_clicked() {
                        remove = Some(index);
                    }
                }
            });
            if let Some(index) = remove {
                state.active.remove(index);
                state.selected_active = None;
            }

            ui.vertical(|ui| {
                let selected = state.selected_active;
                if ui
                    .add_enabled(
                        selected.is_some_and(|index| index > 0),
                        Button::new(t!("common.actions.move_up")).min_size(vec2(BTN_W, row_h)),
                    )
                    .clicked()
                    && let Some(index) = selected
                {
                    state.active.swap(index, index - 1);
                    state.selected_active = Some(index - 1);
                }
                if ui
                    .add_enabled(
                        selected.is_some_and(|index| index + 1 < state.active.len()),
                        Button::new(t!("common.actions.move_down")).min_size(vec2(BTN_W, row_h)),
                    )
                    .clicked()
                    && let Some(index) = selected
                {
                    state.active.swap(index, index + 1);
                    state.selected_active = Some(index + 1);
                }
                ui.add_space(row_h);
                if ui
                    .add_enabled(
                        !state.active.is_empty(),
                        Button::new(t!("common.actions.clear")).min_size(vec2(BTN_W, row_h)),
                    )
                    .clicked()
                {
                    state.active.clear();
                    state.selected_active = None;
                }
            });
        });
    });

    style::hint(ui, t!("shaders.setup.double_click_hint"));
    request
}

/// A bordered, fixed-size scrolling list, standing in for the WinForms `ListBox`
/// and `ListView`.
///
/// Three things here are load-bearing. The box is pinned with `set_min_size` as
/// well as `set_max_size`, because an allocation is only an upper bound and a
/// short list would otherwise collapse the box to its contents (pitfall 21).
/// The layout is re-established as `top_down_justified` rather than inherited:
/// these lists sit inside a `horizontal_top` row, so without it the rows lay out
/// left to right. The `_justified` variant is what makes a selected row highlight the
/// full width, as the `ListView`'s `FullRowSelect` did.
fn list_box(ui: &mut Ui, id: &'static str, width: f32, height: f32, contents: impl FnOnce(&mut Ui)) {
    let stroke = ui.visuals().widgets.inactive.bg_stroke;
    let padding = 2.0 * (LIST_MARGIN + stroke.width);
    let outer = vec2(width, height);
    ui.allocate_ui_with_layout(outer, Layout::top_down(Align::Min), |ui| {
        ui.set_min_size(outer);
        ui.set_max_size(outer);
        Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(stroke)
            .corner_radius(2.0)
            .inner_margin(Margin::same(LIST_MARGIN as i8))
            .show(ui, |ui| {
                let inner = vec2(width - padding, height - padding);
                ui.set_min_size(inner);
                ui.set_max_size(inner);
                ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
                    ui.with_layout(Layout::top_down_justified(Align::Min), contents);
                });
            });
    });
}
