use eframe::egui::{
    self, Align, Button, CentralPanel, ComboBox, Context, Frame, Margin, Rect, RichText, TextEdit, Ui, UiBuilder,
    ViewportBuilder, ViewportCommand, ViewportId, WidgetText, vec2,
};
use mge_config::MacroKind;
use rust_i18n::t;

use crate::{
    app::GuiApp,
    input::{GRAPHICS_FUNCTIONS, InputSettings, KeyPress},
    style,
    ui::selectable_value,
};

use super::MACRO_KEYS;

pub struct MacroEditorState {
    pub draft: InputSettings,
    mode: MacroEditorMode,
    before_edit: Option<InputSettings>,
    trigger_delay_text: String,
    key_input: KeyInputType,
    viewport_ready: bool,
    focus_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacroEditorMode {
    Selection,
    Macro(usize),
    Trigger(usize),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum KeyInputType {
    Tap,
    Down,
    Up,
}

impl MacroEditorState {
    pub fn new(settings: &InputSettings) -> Self {
        Self {
            draft: settings.clone(),
            mode: MacroEditorMode::Selection,
            before_edit: None,
            trigger_delay_text: String::new(),
            key_input: KeyInputType::Tap,
            viewport_ready: false,
            focus_pending: false,
        }
    }

    fn title(&self) -> String {
        match self.mode {
            MacroEditorMode::Selection => t!("input.macro.title").into_owned(),
            MacroEditorMode::Macro(index) => t!("input.macro.editing_macro", index = format!("{index:x}")).into_owned(),
            MacroEditorMode::Trigger(index) => t!("input.macro.editing_trigger", index = index).into_owned(),
        }
    }

    fn start_macro(&mut self, index: usize) {
        self.before_edit = Some(self.draft.clone());
        if self.draft.macros[index].kind == MacroKind::Unused {
            self.draft.macros[index].kind = MacroKind::Graphics;
        }
        self.mode = MacroEditorMode::Macro(index);
        self.key_input = KeyInputType::Tap;
    }

    fn start_trigger(&mut self, index: usize) {
        self.before_edit = Some(self.draft.clone());
        self.mode = MacroEditorMode::Trigger(index);
        self.trigger_delay_text = self.draft.triggers[index].interval_ms.to_string();
    }

    fn save_edit(&mut self) {
        self.before_edit = None;
        self.mode = MacroEditorMode::Selection;
        self.trigger_delay_text.clear();
    }

    fn cancel_edit(&mut self) {
        if let Some(before_edit) = self.before_edit.take() {
            self.draft = before_edit;
        }
        self.mode = MacroEditorMode::Selection;
        self.trigger_delay_text.clear();
    }
}

const MACRO_TYPES: [(MacroKind, &str); 15] = [
    (MacroKind::Unused, "input.macro.types.unused"),
    (MacroKind::Console1, "input.macro.types.console1"),
    (MacroKind::Console2, "input.macro.types.console2"),
    (MacroKind::Hammer1, "input.macro.types.hammer1"),
    (MacroKind::Hammer2, "input.macro.types.hammer2"),
    (MacroKind::Unhammer, "input.macro.types.unhammer"),
    (MacroKind::AlternateHammer1, "input.macro.types.ahammer1"),
    (MacroKind::AlternateHammer2, "input.macro.types.ahammer2"),
    (MacroKind::AlternateUnhammer, "input.macro.types.aunhammer"),
    (MacroKind::Press1, "input.macro.types.press1"),
    (MacroKind::Press2, "input.macro.types.press2"),
    (MacroKind::Unpress, "input.macro.types.unpress"),
    (MacroKind::BeginTimer, "input.macro.types.start_trigger"),
    (MacroKind::EndTimer, "input.macro.types.end_trigger"),
    (MacroKind::Graphics, "input.macro.types.function"),
];

fn macro_type_short_label(kind: MacroKind) -> &'static str {
    MACRO_TYPES
        .iter()
        .find_map(|(candidate, label)| (*candidate == kind).then_some(*label))
        .expect("every macro kind has a GUI label")
}

fn key_visual_state(state: &MacroEditorState, code: usize) -> (bool, bool) {
    match state.mode {
        MacroEditorMode::Selection => (true, state.draft.macros[code].kind != MacroKind::Unused),
        MacroEditorMode::Macro(index) => {
            let macro_item = &state.draft.macros[index];
            if macro_item.kind.is_press() {
                (code < 264, macro_item.keys[code])
            } else if macro_item.kind.is_console() {
                (code < 256, false)
            } else {
                (false, false)
            }
        }
        MacroEditorMode::Trigger(index) => (code < 264, state.draft.triggers[index].keys[code]),
    }
}

fn append_console_key(command: &mut Vec<KeyPress>, code: u8, input: KeyInputType) {
    match input {
        KeyInputType::Tap if command.len() <= 253 => {
            command.extend([KeyPress { code, down: true }, KeyPress { code, down: false }])
        }
        KeyInputType::Down if command.len() < 255 => {
            command.push(KeyPress { code, down: true });
        }
        KeyInputType::Up if command.len() < 255 => {
            command.push(KeyPress { code, down: false });
        }
        _ => {}
    }
}

fn macro_keyboard(ui: &mut Ui, state: &MacroEditorState, origin: eframe::egui::Pos2) -> Option<usize> {
    let mut clicked = None;
    for key in MACRO_KEYS {
        let rect = Rect::from_min_size(origin + vec2(key.x, key.y), vec2(key.width, key.height));
        let (enabled, selected) = key_visual_state(state, key.code);
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
        if response.clicked() {
            clicked = Some(key.code);
        }
    }
    clicked
}

fn combo_at(
    ui: &mut Ui,
    rect: Rect,
    enabled: bool,
    id: &'static str,
    selected_text: impl Into<WidgetText>,
    add_contents: impl FnOnce(&mut Ui),
) {
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.add_enabled_ui(enabled, |ui| {
            ui.spacing_mut().interact_size.x = rect.width();
            ComboBox::from_id_salt(id)
                .icon(style::combo_arrow_icon)
                .width(rect.width())
                .selected_text(selected_text)
                .show_ui(ui, add_contents);
        });
    });
}

fn label_at(ui: &mut Ui, rect: Rect, text: impl Into<WidgetText>) {
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(rect)
            .layout(eframe::egui::Layout::left_to_right(Align::Center)),
        |ui| {
            ui.label(text);
        },
    );
}

impl GuiApp {
    pub(in crate::ui) fn show_macro_dialog(&mut self, ctx: &Context) {
        let Some(state) = self.ui.input.macro_editor.as_ref() else {
            return;
        };
        let title = state.title();
        let viewport_ready = state.viewport_ready;
        let mut builder = ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([764.0, 361.0])
            .with_resizable(false)
            .with_clamp_size_to_monitor_size(true)
            .with_visible(viewport_ready);
        if let Some(icon) = crate::load_icon() {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(ViewportId::from_hash_of("mge_macro_editor"), builder, |ui, _class| {
            self.macro_editor_body(ui)
        });

        if let Some(state) = self.ui.input.macro_editor.as_mut()
            && !state.viewport_ready
        {
            state.viewport_ready = true;
            state.focus_pending = true;
            ctx.request_repaint();
        }
    }

    fn macro_editor_body(&mut self, ui: &mut Ui) {
        let Some(mut state) = self.ui.input.macro_editor.take() else {
            return;
        };

        // Closing the native window is Cancel for an active edit. Previously
        // saved edits are already in `self.settings.input`.
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            return;
        }
        if state.focus_pending {
            ui.ctx().send_viewport_cmd(ViewportCommand::Focus);
            state.focus_pending = false;
        }

        let mut save = false;
        let mut cancel = false;

        CentralPanel::default()
            .frame(Frame::NONE.fill(style::APP_BG).inner_margin(Margin::same(12)))
            .show(ui, |ui| {
                ui.set_min_size(vec2(740.0, 337.0));
                let origin = ui.min_rect().min;
                let at = |x, y, width, height| Rect::from_min_size(origin + vec2(x, y), vec2(width, height));

                if let Some(code) = macro_keyboard(ui, &state, origin) {
                    match state.mode {
                        MacroEditorMode::Selection => state.start_macro(code),
                        MacroEditorMode::Macro(index) if state.draft.macros[index].kind.is_press() => {
                            state.draft.macros[index].keys[code] = !state.draft.macros[index].keys[code];
                        }
                        MacroEditorMode::Macro(index) if state.draft.macros[index].kind.is_console() && code < 256 => {
                            append_console_key(&mut state.draft.macros[index].console, code as u8, state.key_input);
                        }
                        MacroEditorMode::Trigger(index) if code < 264 => {
                            state.draft.triggers[index].keys[code] = !state.draft.triggers[index].keys[code];
                        }
                        _ => {}
                    }
                }

                let console_mode = matches!(
                    state.mode,
                    MacroEditorMode::Macro(index)
                        if state.draft.macros[index].kind.is_console()
                );
                let clear = ui
                    .scope_builder(UiBuilder::new().max_rect(at(608.0, 0.0, 128.0, 42.0)), |ui| {
                        ui.add_enabled(
                            console_mode,
                            Button::new(t!("input.macro.clear_console")).min_size(vec2(128.0, 42.0)),
                        )
                    })
                    .inner
                    .clicked();
                if clear && let MacroEditorMode::Macro(index) = state.mode {
                    state.draft.macros[index].console.clear();
                    state.draft.macros[index].description.clear();
                }

                let macro_index = match state.mode {
                    MacroEditorMode::Macro(index) => Some(index),
                    _ => None,
                };
                let macro_kind = macro_index
                    .map(|index| state.draft.macros[index].kind)
                    .unwrap_or(MacroKind::Unused);

                let mut selected_kind = macro_kind;
                combo_at(
                    ui,
                    at(0.0, 256.0, 184.0, 23.0),
                    macro_index.is_some(),
                    "macro_type",
                    t!(macro_type_short_label(macro_kind)),
                    |ui| {
                        for (kind, label) in MACRO_TYPES {
                            selectable_value(ui, &mut selected_kind, kind, t!(label));
                        }
                    },
                );
                if let Some(index) = macro_index
                    && selected_kind != macro_kind
                {
                    state.draft.macros[index].kind = selected_kind;
                }
                label_at(ui, at(190.0, 258.0, 90.0, 20.0), t!("input.macro.macro_type"));

                let function_label = macro_index
                    .and_then(|index| {
                        let function = state.draft.macros[index].function;
                        GRAPHICS_FUNCTIONS
                            .iter()
                            .find(|(id, _)| *id == function)
                            .map(|(_, label)| *label)
                    })
                    .unwrap_or("input.functions.screenshot");
                let mut selected_function = macro_index.map(|index| state.draft.macros[index].function).unwrap_or(0);
                combo_at(
                    ui,
                    at(0.0, 283.0, 184.0, 23.0),
                    macro_index.is_some() && selected_kind == MacroKind::Graphics,
                    "graphics_function",
                    t!(function_label),
                    |ui| {
                        for (id, label) in GRAPHICS_FUNCTIONS {
                            selectable_value(ui, &mut selected_function, *id, t!(*label));
                        }
                    },
                );
                if let Some(index) = macro_index
                    && selected_kind == MacroKind::Graphics
                {
                    state.draft.macros[index].function = selected_function;
                }
                label_at(ui, at(190.0, 285.0, 90.0, 20.0), t!("input.macro.function"));

                let mut trigger_choice = match state.mode {
                    MacroEditorMode::Macro(index) if selected_kind.is_timer() => {
                        usize::from(state.draft.macros[index].timer_id) + 1
                    }
                    MacroEditorMode::Trigger(index) => index + 1,
                    _ => 0,
                };
                let trigger_enabled =
                    state.mode == MacroEditorMode::Selection || macro_index.is_some_and(|_| selected_kind.is_timer());
                let trigger_text: WidgetText = if trigger_choice == 0 {
                    RichText::new(t!("input.macro.trigger_placeholder")).size(10.0).into()
                } else {
                    trigger_choice.to_string().into()
                };
                combo_at(
                    ui,
                    at(296.0, 258.0, 72.0, 23.0),
                    trigger_enabled,
                    "macro_trigger",
                    trigger_text,
                    |ui| {
                        selectable_value(ui, &mut trigger_choice, 0, t!("input.macro.trigger_placeholder"));
                        for index in 1..=4 {
                            selectable_value(ui, &mut trigger_choice, index, index.to_string());
                        }
                    },
                );
                if state.mode == MacroEditorMode::Selection && trigger_choice > 0 {
                    state.start_trigger(trigger_choice - 1);
                } else if let Some(index) = macro_index
                    && selected_kind.is_timer()
                    && trigger_choice > 0
                {
                    state.draft.macros[index].timer_id = (trigger_choice - 1) as u8;
                }
                label_at(ui, at(374.0, 260.0, 100.0, 20.0), t!("input.macro.trigger"));

                let trigger_edit = match state.mode {
                    MacroEditorMode::Trigger(index) => Some(index),
                    _ => None,
                };
                ui.scope_builder(UiBuilder::new().max_rect(at(296.0, 285.0, 72.0, 23.0)), |ui| {
                    let response = ui.add_enabled(
                        trigger_edit.is_some(),
                        TextEdit::singleline(&mut state.trigger_delay_text)
                            .desired_width(72.0)
                            .char_limit(4),
                    );
                    if response.changed() {
                        state.trigger_delay_text.retain(|character| character.is_ascii_digit());
                        if let Some(index) = trigger_edit
                            && let Ok(value) = state.trigger_delay_text.parse()
                        {
                            state.draft.triggers[index].interval_ms = value;
                        }
                    }
                });
                label_at(ui, at(374.0, 287.0, 110.0, 20.0), t!("input.macro.trigger_delay"));

                let command_length = macro_index
                    .map(|index| {
                        let macro_item = &state.draft.macros[index];
                        if macro_item.kind.is_console() {
                            macro_item.console.len()
                        } else if macro_item.kind.is_press() {
                            macro_item.keys.iter().filter(|active| **active).count()
                        } else {
                            0
                        }
                    })
                    .map(|count| count.to_string())
                    .unwrap_or_default();
                let mut command_length_shown = command_length;
                ui.scope_builder(UiBuilder::new().max_rect(at(489.0, 258.0, 102.0, 23.0)), |ui| {
                    ui.add_enabled(false, TextEdit::singleline(&mut command_length_shown).desired_width(102.0));
                });
                label_at(ui, at(597.0, 260.0, 140.0, 20.0), t!("input.macro.command_length"));

                let description_enabled = macro_index.is_some_and(|_| selected_kind.is_console());
                if let Some(index) = macro_index {
                    ui.scope_builder(UiBuilder::new().max_rect(at(489.0, 284.0, 102.0, 23.0)), |ui| {
                        ui.add_enabled(
                            description_enabled,
                            TextEdit::singleline(&mut state.draft.macros[index].description).desired_width(102.0),
                        );
                    });
                } else {
                    let mut empty = String::new();
                    ui.scope_builder(UiBuilder::new().max_rect(at(489.0, 284.0, 102.0, 23.0)), |ui| {
                        ui.add_enabled(false, TextEdit::singleline(&mut empty).desired_width(102.0));
                    });
                }
                label_at(ui, at(597.0, 286.0, 143.0, 20.0), t!("input.macro.command_description"));

                label_at(ui, at(0.0, 315.0, 96.0, 20.0), t!("input.macro.key_input_type"));
                for (x, value, text) in [
                    (98.0, KeyInputType::Tap, "input.macro.tap"),
                    (150.0, KeyInputType::Down, "input.macro.down"),
                    (212.0, KeyInputType::Up, "input.macro.up"),
                ] {
                    let clicked = ui
                        .scope_builder(UiBuilder::new().max_rect(at(x, 313.0, 58.0, 22.0)), |ui| {
                            ui.add_enabled(console_mode, egui::RadioButton::new(state.key_input == value, t!(text)))
                        })
                        .inner
                        .clicked();
                    if clicked {
                        state.key_input = value;
                    }
                }

                if let Some(index) = trigger_edit {
                    ui.put(
                        at(361.0, 313.0, 115.0, 22.0),
                        egui::Checkbox::new(&mut state.draft.triggers[index].active, t!("input.macro.trigger_enabled")),
                    );
                } else {
                    let mut active = false;
                    ui.scope_builder(UiBuilder::new().max_rect(at(361.0, 313.0, 115.0, 22.0)), |ui| {
                        ui.add_enabled(false, egui::Checkbox::new(&mut active, t!("input.macro.trigger_enabled")));
                    });
                }

                let editing = state.mode != MacroEditorMode::Selection;
                cancel = ui
                    .scope_builder(UiBuilder::new().max_rect(at(680.0, 311.0, 60.0, 25.0)), |ui| {
                        ui.add_enabled(editing, Button::new(t!("common.actions.cancel")).min_size(vec2(60.0, 25.0)))
                    })
                    .inner
                    .clicked();
                save = ui
                    .scope_builder(UiBuilder::new().max_rect(at(614.0, 311.0, 60.0, 25.0)), |ui| {
                        ui.add_enabled(editing, Button::new(t!("common.actions.save")).min_size(vec2(60.0, 25.0)))
                    })
                    .inner
                    .clicked();
            });

        if cancel {
            state.cancel_edit();
        } else if save {
            if let MacroEditorMode::Trigger(index) = state.mode
                && state.trigger_delay_text.is_empty()
            {
                state.draft.triggers[index].interval_ms = 10;
            }
            self.settings.input = state.draft.clone();
            state.save_edit();
        }

        ui.ctx().send_viewport_cmd(ViewportCommand::Title(state.title()));
        self.ui.input.macro_editor = Some(state);
    }
}

#[cfg(test)]
mod macro_editor_tests {
    use super::*;

    #[test]
    fn cancel_restores_the_macro_that_was_opened_for_editing() {
        let settings = InputSettings::default();
        let mut state = MacroEditorState::new(&settings);

        state.start_macro(0);
        assert_eq!(state.title(), "Editing macro 0x0");
        assert_eq!(state.draft.macros[0].kind, MacroKind::Graphics);

        state.cancel_edit();
        assert_eq!(state.mode, MacroEditorMode::Selection);
        assert_eq!(state.draft.macros[0].kind, MacroKind::Unused);
    }

    #[test]
    fn console_key_actions_respect_the_legacy_state_limit() {
        let mut command = Vec::new();
        append_console_key(&mut command, 0x1e, KeyInputType::Tap);
        append_console_key(&mut command, 0x30, KeyInputType::Down);
        append_console_key(&mut command, 0x2e, KeyInputType::Up);
        assert_eq!(
            command,
            vec![
                KeyPress { code: 0x1e, down: true },
                KeyPress { code: 0x1e, down: false },
                KeyPress { code: 0x30, down: true },
                KeyPress { code: 0x2e, down: false },
            ]
        );

        command.resize(254, KeyPress::default());
        append_console_key(&mut command, 0x20, KeyInputType::Tap);
        assert_eq!(command.len(), 254);
        append_console_key(&mut command, 0x20, KeyInputType::Down);
        assert_eq!(command.len(), 255);
        append_console_key(&mut command, 0x20, KeyInputType::Up);
        assert_eq!(command.len(), 255);
    }
}
