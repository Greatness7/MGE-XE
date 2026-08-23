use std::path::{Path, PathBuf};

use eframe::egui::{
    Align, CentralPanel, ComboBox, Context, Label, Layout, Panel, RichText, ScrollArea, TextEdit, TextStyle, Ui,
    ViewportBuilder, ViewportCommand, ViewportId,
};
use rust_i18n::t;

use crate::{
    app::GuiApp,
    localization::{self, AUTO_LOCALE},
    platform, style,
};

use super::{labeled_row, tooltip};

/// The log files the Logs card offers, all of them written into the game root.
/// `Morrowind_d3d9.log` is DXVK's own log and is absent unless the game runs
/// through it.
const LOG_FILES: [&str; 3] = ["mgeXE.log", "mgeHost64.log", "Morrowind_d3d9.log"];

/// Log-viewer window geometry. Wide enough for the runtime's longest banner
/// lines without a horizontal scroll, and freely resizable below that.
const LOG_VIEWER_SIZE: [f32; 2] = [820.0, 580.0];
const LOG_VIEWER_MIN_SIZE: [f32; 2] = [420.0, 240.0];

pub(crate) struct ConfigUiState {
    pub(crate) clear_settings_on_reset: bool,
    pub(crate) log_viewer: Option<LogViewerState>,
    pub(crate) about_open: bool,
}

/// The open log file. Like the generator and the shader windows this is a real
/// second OS window rather than a floating `egui::Window`, so its state lives
/// here and the viewport is drawn from it every frame.
pub(crate) struct LogViewerState {
    /// File name, which is also the window title.
    name: String,
    path: PathBuf,
    /// The file text, or why it could not be shown.
    body: Result<String, String>,
    /// Longest line in characters, cached because it sizes the no-wrap pane
    /// every frame and a log can be large.
    longest_line: usize,
    /// Cleared until the window has been laid out once; see `viewport_ready` on
    /// the shader windows.
    viewport_ready: bool,
    /// A viewport builder is not diffed (pitfall 24), so a *reopen* on another
    /// file has to push its title through an explicit command.
    title_synced: bool,
}

#[derive(Clone, Copy)]
enum ConfigRequest {
    Save,
    Reload,
    Import,
    Export,
    Reset(bool),
}

fn help_card(ui: &mut Ui, language: &mut String, about_open: &mut bool) -> Option<String> {
    let mut error = None;
    style::card(ui, t!("config.help.title"), |ui| {
        labeled_row(ui, t!("config.language").as_ref(), |ui| {
            let selected = if language == AUTO_LOCALE {
                localization::automatic_name()
            } else {
                localization::language_name(language)
            };
            let mut changed = false;
            ComboBox::from_id_salt("language").selected_text(selected).show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(language, AUTO_LOCALE.to_owned(), localization::automatic_name())
                    .changed();
                for locale in localization::available_locale_codes() {
                    changed |= ui
                        .selectable_value(language, locale.to_string(), localization::language_name(&locale))
                        .changed();
                }
            });
            if changed {
                localization::apply_saved_locale(language);
            }
        });
        ui.horizontal(|ui| {
            if ui.button(t!("config.help.documentation")).clicked()
                && let Err(open_error) = platform::open_url("https://github.com/Hrnchamd/MGE-XE/wiki")
            {
                error = Some(t!("messages.documentation_open_failed", error = open_error).into_owned());
            }
            if ui.button(t!("config.help.about")).clicked() {
                *about_open = true;
            }
        });
    });
    error
}

fn logs_card(ui: &mut Ui) -> Option<&'static str> {
    let mut log_to_open = None;
    style::card(ui, t!("config.logs.title"), |ui| {
        for label in LOG_FILES {
            if ui.button(label).clicked() {
                log_to_open = Some(label);
            }
        }
    });
    log_to_open
}

/// Reads a log file, mapping both failure modes onto text the window can show,
/// and measures its longest line. The toast channel belongs to the main window,
/// so a missing or unreadable log is reported inside the viewer instead.
fn read_log(path: &Path) -> (Result<String, String>, usize) {
    let body = match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(t!("messages.file_missing", path = path.display()).into_owned())
        }
        Err(error) => Err(t!("messages.log_read_failed", path = path.display(), error = error).into_owned()),
    };
    let longest_line = body.as_ref().map_or(0, |contents| {
        contents.lines().map(|line| line.chars().count()).max().unwrap_or(0)
    });
    (body, longest_line)
}

/// The log text: read-only but selectable, since `&str` implements `TextBuffer`,
/// so it can be selected and copied but not typed into.
///
/// Lines are not wrapped, the way a log is normally read. `desired_width` cannot
/// express that; sizing the enclosing `Ui` to the longest line is what turns
/// wrapping off and lets the scroll area supply the horizontal bar (pitfalls 25
/// and 26).
fn log_pane(ui: &mut Ui, contents: &str, longest_line: usize) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let glyph_w = ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
    // Measured out here: a `ScrollArea` hands its child unbounded space along
    // every scrollable axis, so `available_width` inside the closure is not the
    // viewport width.
    let viewport_w = ui.available_width();
    let content_w = ((longest_line as f32 + 2.0) * glyph_w).max(viewport_w);

    ScrollArea::both()
        .id_salt("log_viewer_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(content_w);
            let mut text = contents;
            ui.add(
                TextEdit::multiline(&mut text)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(24),
            );
        });
}

fn import_export_card(ui: &mut Ui) -> Option<ConfigRequest> {
    let mut request = None;
    style::card(ui, t!("config.import_export.title"), |ui| {
        if ui.button(t!("config.import_export.save")).clicked() {
            request = Some(ConfigRequest::Save);
        }
        if ui.button(t!("config.import_export.reload")).clicked() {
            request = Some(ConfigRequest::Reload);
        }
        if ui.button(t!("config.import_export.import")).clicked() {
            request = Some(ConfigRequest::Import);
        }
        if ui.button(t!("config.import_export.export")).clicked() {
            request = Some(ConfigRequest::Export);
        }
        style::hint(ui, t!("config.import_export.hint").as_ref());
    });
    request
}

fn reset_card(ui: &mut Ui, clear_settings: &mut bool) -> bool {
    let mut restore_defaults = false;
    style::card(ui, t!("config.reset.title"), |ui| {
        tooltip(
            ui.checkbox(clear_settings, t!("config.reset.clear_unknown").as_ref()),
            t!("config.reset.clear_unknown_tip"),
        );
        restore_defaults = ui.button(t!("config.reset.restore")).clicked();
        style::hint(ui, t!("config.reset.hint").as_ref());
    });
    restore_defaults
}

impl GuiApp {
    pub(crate) fn show_config(&mut self, ui: &mut Ui) {
        ui.columns(2, |columns| {
            if let Some(error) = help_card(
                &mut columns[0],
                &mut self.settings.mge.gui.language,
                &mut self.ui.config.about_open,
            ) {
                self.set_error(error);
            }
            columns[0].add_space(3.0);
            if let Some(log_name) = logs_card(&mut columns[0]) {
                self.open_log(log_name);
            }

            if let Some(request) = import_export_card(&mut columns[1]) {
                self.handle_config_request(request);
            }
            columns[1].add_space(3.0);
            if reset_card(&mut columns[1], &mut self.ui.config.clear_settings_on_reset) {
                self.handle_config_request(ConfigRequest::Reset(self.ui.config.clear_settings_on_reset));
            }
        });
    }

    /// Opens `name` from the game root in the log window, replacing whatever it
    /// was showing. Reopening keeps the window on screen rather than blinking it
    /// through another hidden first frame.
    pub(crate) fn open_log(&mut self, name: &str) {
        let path = self.store.root().join(name);
        let (body, longest_line) = read_log(&path);
        let viewport_ready = self.ui.config.log_viewer.as_ref().is_some_and(|viewer| viewer.viewport_ready);
        self.ui.config.log_viewer = Some(LogViewerState {
            name: name.to_owned(),
            path,
            body,
            longest_line,
            viewport_ready,
            title_synced: false,
        });
    }

    pub(crate) fn show_log_viewer(&mut self, ctx: &Context) {
        let Some((name, viewport_ready)) = self
            .ui
            .config
            .log_viewer
            .as_ref()
            .map(|viewer| (viewer.name.clone(), viewer.viewport_ready))
        else {
            return;
        };

        let mut builder = ViewportBuilder::default()
            .with_title(name)
            .with_inner_size(LOG_VIEWER_SIZE)
            .with_min_inner_size(LOG_VIEWER_MIN_SIZE)
            .with_clamp_size_to_monitor_size(true)
            .with_visible(viewport_ready);
        if let Some(icon) = crate::load_icon() {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(ViewportId::from_hash_of("mge_log_viewer"), builder, |ui, _class| {
            self.log_viewer_body(ui)
        });

        if let Some(viewer) = self.ui.config.log_viewer.as_mut()
            && !viewer.viewport_ready
        {
            viewer.viewport_ready = true;
            ctx.request_repaint();
        }
    }

    fn log_viewer_body(&mut self, ui: &mut Ui) {
        // Dropped: the window closes, and nothing here needs saving.
        let Some(mut viewer) = self.ui.config.log_viewer.take() else {
            return;
        };
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            return;
        }
        if !viewer.title_synced {
            ui.ctx().send_viewport_cmd(ViewportCommand::Title(viewer.name.clone()));
            viewer.title_synced = true;
        }

        let mut reload = false;
        let mut error = None;
        Panel::top("log_viewer_head").show(ui, |ui| {
            ui.add_space(4.0);
            // The buttons claim their width first, so the path truncates into
            // what is left instead of pushing them off the row.
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(t!("config.logs.reveal")).clicked()
                        && let Err(reveal_error) = platform::reveal_path(&viewer.path)
                    {
                        error = Some(format!("{reveal_error:#}"));
                    }
                    reload = ui.button(t!("config.logs.reload")).clicked();
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(Label::new(RichText::new(viewer.path.display().to_string()).color(style::MUTED)).truncate());
                    });
                });
            });
            ui.add_space(4.0);
        });

        if reload {
            (viewer.body, viewer.longest_line) = read_log(&viewer.path);
        }

        CentralPanel::default().show(ui, |ui| match &viewer.body {
            Ok(contents) if contents.is_empty() => {
                style::hint(ui, t!("config.logs.empty").as_ref());
            }
            Ok(contents) => log_pane(ui, contents, viewer.longest_line),
            Err(message) => style::hint(ui, message.as_str()),
        });

        self.ui.config.log_viewer = Some(viewer);
        if let Some(error) = error {
            self.set_error(error);
        }
    }

    fn handle_config_request(&mut self, request: ConfigRequest) {
        match request {
            ConfigRequest::Save => self.save_now(),
            ConfigRequest::Reload => match self.store.reload() {
                Ok(settings) => {
                    self.settings = settings;
                    self.sync_derived_settings();
                    let job_load = crate::job::load(self.store.root());
                    self.job = job_load.job;
                    self.job_writes_disabled = job_load.error.is_some();
                    if let Some(error) = job_load.error {
                        self.set_error(error);
                    } else {
                        let mut warnings = self
                            .store
                            .warnings()
                            .iter()
                            .map(|warning| format!("{}: {}", warning.path, warning.message))
                            .collect::<Vec<_>>();
                        warnings.extend(job_load.warnings);
                        if job_load.legacy_present {
                            warnings.push(t!("generator.messages.legacy_job_notice").into_owned());
                        }
                        if warnings.is_empty() {
                            self.set_success(t!("messages.config_reloaded"));
                        } else {
                            self.set_warning(warnings.join("\n"));
                        }
                    }
                }
                Err(error) => self.set_error(t!("messages.reload_failed", error = format!("{error:#}"))),
            },
            ConfigRequest::Import => self.import_now(),
            ConfigRequest::Export => self.export_now(),
            ConfigRequest::Reset(clear) => self.reset_now(clear),
        }
    }

    pub(crate) fn show_instructions(&mut self, ui: &mut Ui) {
        style::card(ui, t!("instructions.getting_started.title"), |ui| {
            ui.label(t!("instructions.getting_started.graphics"));
            ui.label(t!("instructions.getting_started.generate"));
            ui.label(t!("instructions.getting_started.adjust"));
            ui.label(t!("instructions.getting_started.save"));
        });
        ui.add_space(3.0);
        style::card(ui, t!("instructions.shaders.title"), |ui| {
            ui.label(t!("instructions.shaders.management"));
            ui.label(t!("instructions.shaders.runtime"));
        });
        ui.add_space(3.0);
        style::card(ui, t!("instructions.troubleshooting.title"), |ui| {
            ui.label(t!("instructions.troubleshooting.location"));
            ui.label(t!("instructions.troubleshooting.close_game"));
            ui.label(t!("instructions.troubleshooting.logs"));
            ui.label(t!("instructions.troubleshooting.regenerate"));
        });
    }
}
