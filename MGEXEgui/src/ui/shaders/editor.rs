//! The **shader source editor** window, laid out against the retired WinForms
//! `ShaderEd` form. A third native window on the same immediate-viewport idiom
//! as the setup window, with the runtime-flags dialog and the dirty-document
//! prompt as true modals *inside* it.

use eframe::egui::{
    Align, Button, CentralPanel, Context, Id, Key, KeyboardShortcut, Layout, MenuBar, Modal, Modifiers, Panel, RichText,
    ScrollArea, TextEdit, Ui, ViewportBuilder, ViewportCommand, ViewportId, text::CCursor, text::CCursorRange,
    text_edit::TextEditState, vec2,
};
use rust_i18n::t;

use crate::{
    app::GuiApp,
    platform,
    shaders::{NEW_FILE_NAME, SHADER_FLAGS, ShaderCatalog, ShaderEditor},
    style,
    ui::widgets::{right_aligned, tooltip},
};

/// Source-editor window geometry, from the legacy form's 96-DPI designer
/// values: a `664 × 561` client area with a `520 × 440` floor. Unlike the two
/// other child windows this one is resizable, and the source pane absorbs the
/// growth.
const EDITOR_SIZE: [f32; 2] = [664.0, 561.0];
const EDITOR_MIN_SIZE: [f32; 2] = [520.0, 440.0];
/// Width of the right-docked `Edit shader flags` button (legacy `bShaderFlags`).
const FLAGS_BTN_W: f32 = 140.0;
/// Height of the message pane (legacy `tbMessage`, a fixed 50 px table row).
const MESSAGE_H: f32 = 50.0;
/// Side of a file-toolbar button (legacy `FileToolStrip` items are 26 × 26).
const TOOL_BTN: f32 = 26.0;
/// Width of the flags dialog, from its `389 px` legacy client area. The two
/// parenthesised captions are what set it.
const FLAGS_W: f32 = 389.0;
/// Width of the flags dialog's `OK` / `Cancel` (legacy `bOK` / `bCancel`).
const DIALOG_BTN_W: f32 = 85.0;

/// Id of the source `TextEdit`. Held as a constant because the Edit menu drives
/// the widget through its stored [`TextEditState`] rather than through events.
const SOURCE_ID: &str = "shader_source_text";

/// The open document plus the window state keyed to it. All of the latter dies
/// with the editor, which is what keeps `reset_shader_editor` to one assignment.
pub(crate) struct ShaderEditorState {
    doc: ShaderEditor,
    /// Draft flag bits while the modal flags dialog is open, `None` when it is
    /// closed. The legacy dialog wrote the source from `bOK` only, so the draft
    /// deliberately does not write through as the boxes are ticked.
    flags_draft: Option<u32>,
    /// The New / Open / Close waiting behind the dirty-document prompt.
    pending: Option<Pending>,
    /// Last title handed to the editor viewport. An immediate viewport's builder
    /// is replaced wholesale rather than diffed (pitfall 24), so the dirty
    /// asterisk only appears if the change is also sent as a command.
    title_shown: String,
    /// The first native viewport frame is rendered while hidden on Windows, then
    /// the next frame reveals it to avoid the non-root viewport white flash.
    viewport_ready: bool,
}

impl ShaderEditorState {
    pub(super) fn new(doc: ShaderEditor) -> Self {
        Self {
            doc,
            flags_draft: None,
            pending: None,
            title_shown: String::new(),
            viewport_ready: false,
        }
    }
}

/// A destructive action that the dirty-document prompt is standing in front of.
/// All three take the same legacy Save / Discard / Cancel prompt, and a
/// cancelled Save As cancels the action behind it too.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    New,
    Open,
    Close,
}

impl Pending {
    /// The command this prompt was standing in front of.
    fn command(self) -> EditorCommand {
        match self {
            Self::New => EditorCommand::New,
            Self::Open => EditorCommand::Open,
            Self::Close => EditorCommand::Close,
        }
    }
}

/// What the editor's menus, toolbar, and buttons asked for this frame. Every one
/// is serviced after the render pass: they open native file dialogs, touch the
/// filesystem, or replace the document out from under the widgets drawing it.
enum EditorCommand {
    New,
    Open,
    Save,
    SaveAs,
    Close,
    OpenFlags,
    Reveal,
    Edit(EditAction),
}

/// The six legacy `Edit` menu commands.
///
/// These are applied directly to the source buffer and the source `TextEdit`'s
/// stored [`TextEditState`], *not* by synthesizing input events: a menu click
/// moves focus off the text box, so an event-based command would arrive at a
/// widget that is no longer listening.
#[derive(Clone, Copy)]
enum EditAction {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

impl GuiApp {
    /// Hosts the source editor's own OS window while an editor exists.
    pub(in crate::ui) fn show_shader_editor_dialog(&mut self, ctx: &Context) {
        let Some(state) = self.ui.shaders.editor.as_ref() else {
            return;
        };

        let viewport_ready = state.viewport_ready;
        let mut builder = ViewportBuilder::default()
            .with_title(state.doc.title())
            .with_inner_size(EDITOR_SIZE)
            .with_min_inner_size(EDITOR_MIN_SIZE)
            .with_clamp_size_to_monitor_size(true)
            .with_visible(viewport_ready);
        if let Some(icon) = crate::load_icon() {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(ViewportId::from_hash_of("mge_shader_editor"), builder, |ui, _class| {
            self.shader_editor_body(ui)
        });

        if let Some(state) = self.ui.shaders.editor.as_mut()
            && !state.viewport_ready
        {
            state.viewport_ready = true;
            ctx.request_repaint();
        }
    }

    /// The six legacy layers: menus, file toolbar, the flags command row, the
    /// source, the message pane, and the bottom action row.
    fn shader_editor_body(&mut self, ui: &mut Ui) {
        let Some(mut state) = self.ui.shaders.editor.take() else {
            return;
        };

        // The window title carries the dirty asterisk, and the builder above is
        // only read when the viewport is created (pitfall 24).
        let title = state.doc.title();
        if title != state.title_shown {
            ui.ctx().send_viewport_cmd(ViewportCommand::Title(title.clone()));
            state.title_shown = title;
        }

        // The modal prompt owns the close decision, so a dirty document vetoes
        // the native close button and raises the prompt instead of vanishing.
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            if state.doc.dirty {
                ui.ctx().send_viewport_cmd(ViewportCommand::CancelClose);
                state.pending = Some(Pending::Close);
            } else {
                self.reset_shader_editor(ui.ctx());
                return;
            }
        }

        // A `Modal` dims and input-blocks everything beneath it on its own, so
        // the surface below is drawn normally rather than gated a second time.
        // One local per panel: `EditorCommand` is not `Copy`, so a single
        // accumulator cannot be read inside more than one closure.
        let mut head = None;
        let mut foot = None;

        Panel::top("shader_editor_head").show(ui, |ui| {
            head = editor_menu_bar(ui, &state.doc);
            head = head.take().or_else(|| editor_toolbar(ui));
            ui.add_space(2.0);
            // The legacy command row holds this one right-docked button and
            // nothing else; the numeric flag value is never shown.
            right_aligned(ui, |ui| {
                let height = ui.spacing().interact_size.y;
                if ui
                    .add_sized([FLAGS_BTN_W, height], Button::new(t!("shaders.editor.edit_flags")))
                    .clicked()
                {
                    head = Some(EditorCommand::OpenFlags);
                }
            });
            ui.add_space(3.0);
        });

        Panel::bottom("shader_editor_foot").show(ui, |ui| {
            ui.add_space(3.0);
            message_pane(ui, &state.doc.message);
            ui.add_space(4.0);
            foot = editor_actions(ui, &state.doc);
            ui.add_space(4.0);
        });

        CentralPanel::default().show(ui, |ui| {
            source_pane(ui, &mut state.doc);
        });

        // Shortcuts are read after the widgets, so a keystroke the source pane
        // wanted (Ctrl+Z, Ctrl+A) has already been taken by the time these run.
        let mut command = head.or(foot).or_else(|| editor_shortcuts(ui.ctx()));

        // Modals last, so they paint over the surface they belong to.
        if let Some(draft) = state.flags_draft {
            match flags_modal(ui.ctx(), draft) {
                FlagsOutcome::Editing(draft) => state.flags_draft = Some(draft),
                FlagsOutcome::Accepted(flags) => {
                    state.flags_draft = None;
                    state.doc.set_flags(flags);
                    state.doc.message = t!("shaders.messages.flags_set", flags = flags).into_owned();
                }
                FlagsOutcome::Cancelled => state.flags_draft = None,
            }
        }
        if let Some(pending) = state.pending {
            match save_prompt_modal(ui.ctx()) {
                PromptOutcome::Waiting => {}
                PromptOutcome::Cancel => state.pending = None,
                // "No": drop the changes and let the action through.
                PromptOutcome::Discard => {
                    state.pending = None;
                    state.doc.dirty = false;
                    command = Some(pending.command());
                }
                // "Yes": save first, and abort the action if that does not
                // complete. A cancelled Save As cancels what it was standing
                // in front of, as the legacy prompt flow did.
                PromptOutcome::Save => {
                    state.pending = None;
                    let unnamed = state.doc.path.is_none();
                    if self.save_shader(&mut state.doc, unnamed) {
                        command = Some(pending.command());
                    }
                }
            }
        }

        // Native file dialogs and filesystem access stay outside the render pass.
        self.run_editor_command(ui.ctx(), command, state);
    }

    /// Services one command and decides whether the editor survives the frame.
    fn run_editor_command(&mut self, ctx: &Context, command: Option<EditorCommand>, mut state: ShaderEditorState) {
        let dirty = state.doc.dirty;
        match command {
            Some(EditorCommand::New) if dirty => state.pending = Some(Pending::New),
            Some(EditorCommand::Open) if dirty => state.pending = Some(Pending::Open),
            Some(EditorCommand::Close) if dirty => state.pending = Some(Pending::Close),

            Some(EditorCommand::New) => {
                state.doc = ShaderEditor::new();
                self.reset_source_widget(ctx);
            }
            Some(EditorCommand::Open) => {
                if let Some(path) = self.shader_file_dialog().pick_file() {
                    match ShaderEditor::open_path(path) {
                        Ok(opened) => {
                            state.doc = opened;
                            self.reset_source_widget(ctx);
                        }
                        Err(error) => {
                            state.doc.message =
                                t!("shaders.messages.open_failed", error = format!("{error:#}")).into_owned();
                        }
                    }
                }
            }
            Some(EditorCommand::Save) => {
                let unnamed = state.doc.path.is_none();
                self.save_shader(&mut state.doc, unnamed);
            }
            Some(EditorCommand::SaveAs) => {
                self.save_shader(&mut state.doc, true);
            }
            Some(EditorCommand::Close) => {
                self.reset_shader_editor(ctx);
                ctx.send_viewport_cmd(ViewportCommand::Close);
                return;
            }
            Some(EditorCommand::OpenFlags) => {
                state.flags_draft = Some(state.doc.flags());
            }
            Some(EditorCommand::Reveal) => {
                if let Some(path) = &state.doc.path
                    && let Err(error) = platform::reveal_path(path)
                {
                    state.doc.message = t!("shaders.messages.explorer_failed", error = format!("{error:#}")).into_owned();
                }
            }
            Some(EditorCommand::Edit(action)) => {
                let changed = apply_edit_action(ctx, Id::new(SOURCE_ID), &mut state.doc.source, action);
                state.doc.dirty |= changed;
            }
            None => {}
        }
        self.ui.shaders.editor = Some(state);
    }

    /// Saves, refreshes the catalog, and reports into the message pane. Returns
    /// whether the document ended up clean. A cancelled Save As returns false,
    /// which is what aborts a pending New / Open / Close.
    fn save_shader(&mut self, editor: &mut ShaderEditor, as_new: bool) -> bool {
        let result = if as_new {
            let suggested = if editor.path.is_none() {
                format!("{}.fx", t!(NEW_FILE_NAME))
            } else {
                format!("{}.fx", editor.name)
            };
            let Some(path) = self.shader_file_dialog().set_file_name(suggested).save_file() else {
                return false;
            };
            editor.save_as(path)
        } else {
            editor.save()
        };

        match result {
            Ok(()) => {
                editor.message = t!("shaders.messages.saved", name = &editor.name).into_owned();
                self.ui.shaders.catalog = ShaderCatalog::scan(self.store.root());
                self.set_success(t!("shaders.messages.saved_shader", name = &editor.name).into_owned());
                true
            }
            Err(error) => {
                let message = t!("shaders.messages.save_failed", error = format!("{error:#}")).into_owned();
                editor.message = message.clone();
                self.set_error(message);
                false
            }
        }
    }

    fn shader_file_dialog(&self) -> rfd::FileDialog {
        rfd::FileDialog::new()
            .set_directory(self.store.root().join("Data Files").join("shaders").join("XEshaders"))
            .add_filter(t!("shaders.file.effect_fx").as_ref(), &["fx"])
    }

    /// Drops the editor and everything keyed to it.
    fn reset_shader_editor(&mut self, ctx: &Context) {
        self.ui.shaders.editor = None;
        self.reset_source_widget(ctx);
    }

    /// Clears the source widget's cursor and undo history.
    ///
    /// The `TextEdit` is keyed by a constant id, so its state outlives the
    /// document in it. Without this, undo in a freshly opened file walks back
    /// into the *previous* file's text.
    fn reset_source_widget(&self, ctx: &Context) {
        let id = Id::new(SOURCE_ID);
        if let Some(mut state) = TextEditState::load(ctx, id) {
            state.clear_undoer();
            state.cursor.set_char_range(None);
            state.store(ctx, id);
        }
    }
}

/// The `File` and `Edit` menus, with the legacy shortcut captions.
///
/// Unlike the legacy form, unavailable edit commands are disabled: `Save`
/// follows the document state and the clipboard commands follow the selection.
fn editor_menu_bar(ui: &mut Ui, editor: &ShaderEditor) -> Option<EditorCommand> {
    let mut command = None;
    let has_selection = source_selection(ui.ctx()).is_some_and(|(start, end)| start != end);

    MenuBar::new().ui(ui, |ui| {
        ui.menu_button(t!("shaders.editor.menu.file"), |ui| {
            if menu_item(ui, t!("common.actions.new").as_ref(), "Ctrl+N", true) {
                command = Some(EditorCommand::New);
            }
            if menu_item(ui, t!("common.actions.open").as_ref(), "Ctrl+O", true) {
                command = Some(EditorCommand::Open);
            }
            ui.separator();
            if menu_item(
                ui,
                t!("common.actions.save").as_ref(),
                "Ctrl+S",
                editor.dirty || editor.path.is_none(),
            ) {
                command = Some(EditorCommand::Save);
            }
            if menu_item(ui, t!("common.actions.save_as").as_ref(), "Ctrl+Shift+S", true) {
                command = Some(EditorCommand::SaveAs);
            }
            ui.separator();
            // Not a legacy item; the cheapest way to reach the file being edited.
            if menu_item(
                ui,
                t!("shaders.editor.menu.show_in_explorer").as_ref(),
                "",
                editor.path.is_some(),
            ) {
                command = Some(EditorCommand::Reveal);
            }
            ui.separator();
            if menu_item(ui, t!("shaders.editor.menu.exit").as_ref(), "", true) {
                command = Some(EditorCommand::Close);
            }
        });
        ui.menu_button(t!("shaders.editor.menu.edit"), |ui| {
            if menu_item(ui, t!("shaders.editor.menu.undo").as_ref(), "Ctrl+Z", true) {
                command = Some(EditorCommand::Edit(EditAction::Undo));
            }
            if menu_item(ui, t!("shaders.editor.menu.redo").as_ref(), "Ctrl+Y", true) {
                command = Some(EditorCommand::Edit(EditAction::Redo));
            }
            ui.separator();
            if menu_item(ui, t!("shaders.editor.menu.cut").as_ref(), "Ctrl+X", has_selection) {
                command = Some(EditorCommand::Edit(EditAction::Cut));
            }
            if menu_item(ui, t!("shaders.editor.menu.copy").as_ref(), "Ctrl+C", has_selection) {
                command = Some(EditorCommand::Edit(EditAction::Copy));
            }
            if menu_item(ui, t!("shaders.editor.menu.paste").as_ref(), "Ctrl+V", true) {
                command = Some(EditorCommand::Edit(EditAction::Paste));
            }
            ui.separator();
            if menu_item(
                ui,
                t!("shaders.editor.menu.select_all").as_ref(),
                "Ctrl+A",
                !editor.source.is_empty(),
            ) {
                command = Some(EditorCommand::Edit(EditAction::SelectAll));
            }
        });
    });
    command
}

/// One menu row: caption left, shortcut right in the muted colour, as Win32
/// menus arrange them.
fn menu_item(ui: &mut Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let clicked = ui
        .add_enabled(
            enabled,
            Button::new(label).shortcut_text(RichText::new(shortcut).color(style::MUTED)),
        )
        .clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// The four file-toolbar buttons.
///
/// No icon assets in this crate, so these are drawn from the `mge_symbols`
/// family with the item text as a tooltip.
fn editor_toolbar(ui: &mut Ui) -> Option<EditorCommand> {
    let mut command = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let mut tool = |glyph: &str, tip: &str| {
            let button = Button::new(RichText::new(glyph).font(style::symbol_font(15.0))).min_size(vec2(TOOL_BTN, TOOL_BTN));
            tooltip(ui.add(button), tip).clicked()
        };
        if tool("🗋", t!("common.actions.new").as_ref()) {
            command = Some(EditorCommand::New);
        }
        if tool("🗀", t!("common.actions.open").as_ref()) {
            command = Some(EditorCommand::Open);
        }
        if tool("🖫", t!("common.actions.save").as_ref()) {
            command = Some(EditorCommand::Save);
        }
        if tool("🖬", t!("common.actions.save_as").as_ref()) {
            command = Some(EditorCommand::SaveAs);
        }
    });
    command
}

/// The bottom action row: save actions on the left, close on the right. The
/// legacy Validate / Preview buttons all needed a DirectX device in the GUI
/// process; the in-game runtime owns compilation and preview now.
fn editor_actions(ui: &mut Ui, editor: &ShaderEditor) -> Option<EditorCommand> {
    let mut command = None;
    let height = ui.spacing().interact_size.y;
    ui.horizontal(|ui| {
        if ui.add_sized([80.0, height], Button::new(t!("common.actions.save"))).clicked() {
            command = Some(EditorCommand::Save);
        }
        if ui
            .add_sized([80.0, height], Button::new(t!("common.actions.save_as_plain")))
            .clicked()
        {
            command = Some(EditorCommand::SaveAs);
        }
        if editor.dirty {
            ui.label(RichText::new(t!("shaders.editor.modified")).color(style::WARN));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_sized([80.0, height], Button::new(t!("common.actions.close")))
                .clicked()
            {
                command = Some(EditorCommand::Close);
            }
        });
    });
    command
}

/// The `File` menu's accelerators. `Ctrl+Z` / `Ctrl+Y` / `Ctrl+A` are absent on
/// purpose: the source `TextEdit` implements those itself, and intercepting them
/// here would take them away from the widget that is already handling them.
fn editor_shortcuts(ctx: &Context) -> Option<EditorCommand> {
    const NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
    const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
    const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
    const SAVE_AS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);

    ctx.input_mut(|input| {
        // Save As first: `Ctrl+Shift+S` also matches nothing else, but reading
        // the plain `Ctrl+S` first would consume the key before the modifier
        // combination is tested.
        if input.consume_shortcut(&SAVE_AS) {
            Some(EditorCommand::SaveAs)
        } else if input.consume_shortcut(&SAVE) {
            Some(EditorCommand::Save)
        } else if input.consume_shortcut(&NEW) {
            Some(EditorCommand::New)
        } else if input.consume_shortcut(&OPEN) {
            Some(EditorCommand::Open)
        } else {
            None
        }
    })
}

/// The fixed-height compiler/message pane.
///
/// Read-only but selectable and scrollable (`&str` implements `TextBuffer`, so a
/// `TextEdit` over one cannot be typed into). Persistent by design: output stays
/// until the next operation replaces it.
fn message_pane(ui: &mut Ui, message: &str) {
    let mut text = message;
    ui.add_sized(
        [ui.available_width(), MESSAGE_H],
        TextEdit::multiline(&mut text).desired_rows(2),
    );
}

/// The source pane, filling everything between the command row and the message
/// pane.
fn source_pane(ui: &mut Ui, editor: &mut ShaderEditor) {
    // Tab inserts four spaces, as the legacy editor's key handler did. The key
    // event is rewritten into a text event *before* the widget reads it: that
    // also removes it from egui's focus-navigation, so Tab cannot jump out of
    // the source box.
    ui.ctx().input_mut(|input| {
        for event in &mut input.events {
            if let eframe::egui::Event::Key {
                key: Key::Tab,
                pressed: true,
                modifiers,
                ..
            } = event
                && modifiers.is_none()
            {
                *event = eframe::egui::Event::Text("    ".to_owned());
            }
        }
    });

    // `WordWrap = false`. `desired_width` alone cannot express that: `TextEdit`
    // clamps it to `ui.available_width()` and wraps the galley at the result, so
    // an infinite request still wraps at the viewport edge. What actually turns
    // wrapping off is making the `Ui` inside the scroll area as wide as the
    // widest line. Then the clamp is a no-op and the scroll area supplies the
    // horizontal scrollbar. The face is monospaced, so that width is one glyph
    // measurement rather than a galley per line.
    let font = eframe::egui::TextStyle::Monospace.resolve(ui.style());
    let glyph_w = ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
    let longest = editor.source.lines().map(|line| line.chars().count()).max().unwrap_or(0);
    // Measured out here on purpose: a `ScrollArea` hands its child *unbounded*
    // space along every scrollable axis, so `available_width` inside the closure
    // is not the viewport width. Sizing the child from that instead produced a
    // widget kilometres wide, whose visible part then took no clicks at all, so
    // the source pane could be read but not typed into.
    let viewport_w = ui.available_width();
    let content_w = ((longest as f32 + 2.0) * glyph_w).max(viewport_w);

    ScrollArea::both()
        .id_salt("shader_source_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(content_w);
            let response = ui.add(
                TextEdit::multiline(&mut editor.source)
                    .id(Id::new(SOURCE_ID))
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(24),
            );
            if response.changed() {
                editor.dirty = true;
            }
        });
}

/// Outcome of one frame of the modal flags dialog.
enum FlagsOutcome {
    Editing(u32),
    Accepted(u32),
    Cancelled,
}

/// A fixed, compact modal. The draft is only written back to the source by
/// `OK`, so `Cancel` leaves the document untouched.
fn flags_modal(ctx: &Context, draft: u32) -> FlagsOutcome {
    let mut flags = draft;
    let mut outcome = None;

    let response = Modal::new(Id::new("shader_flags_modal")).show(ctx, |ui| {
        ui.set_width(FLAGS_W);
        ui.label(RichText::new(t!("shaders.flags.title")).strong());
        ui.add_space(6.0);
        for (bit, label) in SHADER_FLAGS {
            let mut set = flags & *bit != 0;
            // 25 px of vertical rhythm per row, as the legacy table had.
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = 21.0;
                if ui.checkbox(&mut set, t!(*label)).changed() {
                    if set {
                        flags |= *bit;
                    } else {
                        flags &= !*bit;
                    }
                }
            });
        }
        ui.add_space(10.0);
        let height = ui.spacing().interact_size.y;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_sized([DIALOG_BTN_W, height], Button::new(t!("common.actions.cancel")))
                    .clicked()
                {
                    outcome = Some(FlagsOutcome::Cancelled);
                }
                if ui
                    .add_sized([DIALOG_BTN_W, height], Button::new(t!("common.actions.ok")))
                    .clicked()
                {
                    outcome = Some(FlagsOutcome::Accepted(flags));
                }
            });
        });
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            outcome = Some(FlagsOutcome::Accepted(flags));
        }
    });

    // `should_close` is Escape or a click on the dimmed backdrop; both cancel.
    outcome.unwrap_or(if response.should_close() {
        FlagsOutcome::Cancelled
    } else {
        FlagsOutcome::Editing(flags)
    })
}

/// Outcome of one frame of the dirty-document prompt.
enum PromptOutcome {
    Waiting,
    Save,
    Discard,
    Cancel,
}

/// The legacy three-way `Save current shader file?` prompt, shared by New, Open,
/// and Close.
fn save_prompt_modal(ctx: &Context) -> PromptOutcome {
    let mut outcome = None;
    let response = Modal::new(Id::new("shader_save_prompt")).show(ctx, |ui| {
        ui.set_width(300.0);
        ui.label(RichText::new(t!("shaders.editor.save_prompt")).strong());
        ui.add_space(10.0);
        let height = ui.spacing().interact_size.y;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_sized([DIALOG_BTN_W, height], Button::new(t!("common.actions.cancel")))
                    .clicked()
                {
                    outcome = Some(PromptOutcome::Cancel);
                }
                if ui
                    .add_sized([DIALOG_BTN_W, height], Button::new(t!("common.choices.no")))
                    .clicked()
                {
                    outcome = Some(PromptOutcome::Discard);
                }
                if ui
                    .add_sized([DIALOG_BTN_W, height], Button::new(t!("common.choices.yes")))
                    .clicked()
                {
                    outcome = Some(PromptOutcome::Save);
                }
            });
        });
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            outcome = Some(PromptOutcome::Save);
        }
    });

    outcome.unwrap_or(if response.should_close() {
        PromptOutcome::Cancel
    } else {
        PromptOutcome::Waiting
    })
}

/// The source `TextEdit`'s selection as sorted character indices.
fn source_selection(ctx: &Context) -> Option<(usize, usize)> {
    let state = TextEditState::load(ctx, Id::new(SOURCE_ID))?;
    let range = state.cursor.char_range()?;
    let sorted = range.as_sorted_char_range();
    Some((sorted.start.0, sorted.end.0))
}

/// Applies one `Edit` menu command to the buffer and the widget's stored state.
/// Returns whether the source text changed.
///
/// Undo and redo go through the `TextEdit`'s own [`Undoer`](eframe::egui::util::undoer::Undoer),
/// which is the same history the widget's Ctrl+Z uses, so the two cannot drift
/// apart.
fn apply_edit_action(ctx: &Context, id: Id, source: &mut String, action: EditAction) -> bool {
    let Some(mut state) = TextEditState::load(ctx, id) else {
        return false;
    };
    let range = state.cursor.char_range().unwrap_or_default();
    let sorted = range.as_sorted_char_range();
    let (start, end) = (sorted.start.0, sorted.end.0);
    let (start_byte, end_byte) = (byte_index(source, start), byte_index(source, end));
    let selected = source[start_byte..end_byte].to_owned();
    let mut changed = false;

    match action {
        EditAction::SelectAll => {
            let last = source.chars().count();
            state
                .cursor
                .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(last))));
        }
        EditAction::Copy => {
            if !selected.is_empty() {
                ctx.copy_text(selected);
            }
        }
        EditAction::Cut | EditAction::Paste => {
            let insert = match action {
                EditAction::Cut => {
                    if selected.is_empty() {
                        return false;
                    }
                    ctx.copy_text(selected);
                    String::new()
                }
                _ => match platform::clipboard_text() {
                    Some(text) => text.replace("\r\n", "\n").replace('\r', "\n"),
                    None => return false,
                },
            };
            let mut undoer = state.undoer();
            undoer.add_undo(&(range, source.clone()));
            state.set_undoer(undoer);

            source.replace_range(start_byte..end_byte, &insert);
            let caret = start + insert.chars().count();
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(caret))));
            changed = true;
        }
        EditAction::Undo | EditAction::Redo => {
            let mut undoer = state.undoer();
            let current = (range, source.clone());
            let restored = match action {
                EditAction::Undo => undoer.undo(&current).cloned(),
                _ => undoer.redo(&current).cloned(),
            };
            state.set_undoer(undoer);
            if let Some((restored_range, text)) = restored {
                *source = text;
                state.cursor.set_char_range(Some(restored_range));
                changed = true;
            }
        }
    }

    state.store(ctx, id);
    changed
}

/// Byte offset of a character offset, clamped to the end of the string.
fn byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices().nth(char_index).map_or(text.len(), |(index, _)| index)
}
