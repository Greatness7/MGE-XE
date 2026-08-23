use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{self, Color32, Stroke};
use rust_i18n::t;

use distantland::GenerationJob;

use crate::{
    config::{self, AppSettings, SettingsStore},
    distant::DistantLandStatus,
    job, localization,
    platform::{self, RegistrySettings},
    shaders::ShaderCatalog,
    style,
    ui::UiState,
};

/// How long a non-error status message stays on screen.
const STATUS_LIFETIME: Duration = Duration::from_secs(5);

pub struct GuiApp {
    pub store: SettingsStore,
    pub settings: AppSettings,
    pub registry: RegistrySettings,
    /// Distant-land generation settings, in the form they are persisted in:
    /// paths relative as written, never rebased. The generator window seeds its
    /// editable copy from this; consumers resolve against `store.root()` at the
    /// point of use, and a resolved job is never written back because absolute
    /// paths would make the file valid on one install only.
    pub job: GenerationJob,
    /// Malformed TOML must not be overwritten by an implicit default job.
    pub(crate) job_writes_disabled: bool,
    pub(crate) ui: UiState,
}

impl GuiApp {
    pub fn new(creation: &eframe::CreationContext<'_>, root: PathBuf) -> Result<Self> {
        style::install(&creation.egui_ctx);
        let (store, mut settings) = SettingsStore::load(root.clone())?;
        let config_diagnostic = store.diagnostic().map(str::to_owned);
        let mut load_warnings = store
            .warnings()
            .iter()
            .map(|warning| format!("{}: {}", warning.path, warning.message))
            .collect::<Vec<_>>();
        localization::apply_saved_locale(&mut settings.mge.gui.language);
        let shader_catalog = ShaderCatalog::scan(&root);
        settings.mge.shaders.chain.retain(|shader| shader_catalog.contains(shader));

        // The stored tick is only meaningful when the generated set is usable:
        // without this the tab opens fully enabled over data the runtime will
        // refuse, and the first click on the checkbox bounces back. Reconciling
        // here costs the setting on exit-save, which is honest: with no usable
        // data distant land is off in game whatever the configuration says, and
        // a successful generation run turns it straight back on below.
        let distant_status = DistantLandStatus::inspect(&root);
        settings.mge.distant_land.enabled &= distant_status.complete;

        let job_load = job::load(&root);
        let job_error = job_load.error;
        let legacy_job_present = job_load.legacy_present;
        load_warnings.extend(job_load.warnings);

        let mut app = Self {
            store,
            settings,
            registry: RegistrySettings::load(),
            job: job_load.job,
            job_writes_disabled: job_error.is_some(),
            ui: UiState::new(
                platform::display_modes(),
                shader_catalog,
                distant_status,
                Instant::now() + STATUS_LIFETIME,
            ),
        };
        app.sync_derived_settings();
        if let Some(error) = config_diagnostic {
            app.set_error(error);
        } else if let Some(error) = job_error {
            // Sticky, and deliberately louder than the routine load message it
            // replaces: the host reads this same file at game launch and stops
            // at `JobInvalid`, so the damage is not confined to the GUI.
            app.set_error(error);
        } else {
            if legacy_job_present {
                load_warnings.push(t!("generator.messages.legacy_job_notice").into_owned());
            }
            if !load_warnings.is_empty() {
                app.set_warning(load_warnings.join("\n"));
            }
        }
        Ok(app)
    }

    /// Re-derive the settings that are computed from state living outside
    /// `mgeXE.toml`. Call after any load that replaces `self.settings`: the
    /// screen resolution lives in the registry and can change without the GUI,
    /// so a stored Auto FOV value is only as good as the resolution it was
    /// written for.
    pub fn sync_derived_settings(&mut self) {
        localization::apply_saved_locale(&mut self.settings.mge.gui.language);
        config::refresh_auto_fov(&mut self.settings.mge, self.registry.width, self.registry.height);
    }

    fn allow_persistence(&mut self) -> bool {
        if platform::morrowind_is_running() {
            self.set_error(t!("messages.morrowind_running"));
            return false;
        }
        true
    }

    pub(crate) fn save_now(&mut self) {
        if !self.allow_persistence() {
            return;
        }
        let file_results = self.store.save(&self.settings);
        let registry_result = self.registry.save(self.settings.mge.runtime.disabled);
        let mut errors = Vec::new();
        if let Err(error) = file_results.toml {
            errors.push(format!("mgeXE.toml: {error:#}"));
        }
        if let Err(error) = file_results.morrowind_ini {
            errors.push(format!("Morrowind.ini: {error:#}"));
        }
        if let Err(error) = registry_result {
            errors.push(format!("Morrowind registry: {error:#}"));
        }
        if errors.is_empty() {
            self.set_success(t!("messages.settings_saved"));
        } else {
            self.set_error(t!("messages.config_save_failed", error = errors.join("\n")));
        }
    }

    pub(crate) fn reset_now(&mut self, clear: bool) {
        if !self.allow_persistence() {
            return;
        }
        match self.store.reset(clear) {
            Ok(settings) => {
                self.settings = settings;
                self.sync_derived_settings();
                self.set_success(t!("messages.defaults_restored"));
            }
            Err(error) => self.set_error(t!("messages.reset_failed", error = format!("{error:#}"))),
        }
    }

    pub(crate) fn import_now(&mut self) {
        if !self.allow_persistence() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter(t!("config.file_filter").as_ref(), &["toml"])
            .pick_file()
        else {
            return;
        };
        match self.store.import_mge(&path) {
            Ok(settings) => {
                self.settings = settings;
                self.sync_derived_settings();
                self.set_success(t!("messages.config_imported", path = path.display()));
            }
            Err(error) => self.set_error(t!("messages.import_failed", error = format!("{error:#}"))),
        }
    }

    pub(crate) fn export_now(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("mgeXE.toml")
            .add_filter(t!("config.file_filter").as_ref(), &["toml"])
            .save_file()
        else {
            return;
        };
        match self.store.export_mge(&path, &self.settings) {
            Ok(()) => self.set_success(t!("messages.config_exported", path = path.display())),
            Err(error) => self.set_error(t!("messages.export_failed", error = format!("{error:#}"))),
        }
    }

    fn set_status(&mut self, title: impl Into<String>, text: impl Into<String>, color: Color32, lifetime: Option<Duration>) {
        self.ui.feedback.title = title.into();
        self.ui.feedback.text = text.into();
        self.ui.feedback.color = color;
        self.ui.feedback.expiry = lifetime.map(|lifetime| Instant::now() + lifetime);
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        // Sticky: a failed import or save must not scroll past unnoticed.
        self.set_status(t!("feedback.error"), error, style::BAD, None);
    }

    pub fn set_success(&mut self, message: impl Into<String>) {
        self.set_status(t!("feedback.success"), message, style::GOOD, Some(STATUS_LIFETIME));
    }

    pub fn set_warning(&mut self, message: impl Into<String>) {
        self.set_status(t!("feedback.warning"), message, style::WARN, None);
    }

    /// Renders the UI feedback message as a floating toast in the bottom-right corner.
    ///
    /// Deliberately not a status strip along the bottom: every tab is laid out to
    /// fit the window exactly, so a permanent strip would put the tightest tabs
    /// into a scrollbar. An `Area` floats over the panels and costs no layout.
    fn show_status_toast(&mut self, ctx: &egui::Context) {
        if self.ui.feedback.text.is_empty() {
            return;
        }
        if let Some(expiry) = self.ui.feedback.expiry {
            let Some(remaining) = expiry.checked_duration_since(Instant::now()) else {
                self.ui.feedback.text.clear();
                return;
            };
            // Nothing else is animating once the UI settles, so without this the
            // toast would sit there until the next unrelated repaint.
            ctx.request_repaint_after(remaining);
        }

        let viewport = ctx.content_rect();
        let max_width = (viewport.width() * 0.6).min(480.0);
        let max_height = viewport.height() * 0.35;
        let mut dismiss_clicked = false;
        let toast = egui::Area::new(egui::Id::new("mge_status_toast"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-8.0, -8.0))
            .movable(false)
            .sense(egui::Sense::click())
            .show(ctx, |ui| {
                ui.set_max_width(max_width);
                egui::Frame::new()
                    .fill(style::CARD_HEADER)
                    .stroke(Stroke::new(1.0, style::BORDER))
                    .corner_radius(2.0)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        let close_size = ui.spacing().interact_size.y;
                        let header = ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&self.ui.feedback.title)
                                    .color(self.ui.feedback.color)
                                    .strong(),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let close = ui.add_sized(egui::vec2(close_size, close_size), egui::Button::new(""));
                                close.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        ui.is_enabled(),
                                        t!("messages.dismiss"),
                                    )
                                });
                                let visuals = ui.style().interact(&close);
                                let icon_rect = close.rect.shrink(5.0).expand(visuals.expansion);
                                let icon_rect = egui::Rect::from_center_size(icon_rect.center(), icon_rect.size() * 0.5);
                                ui.painter()
                                    .line_segment([icon_rect.left_top(), icon_rect.right_bottom()], visuals.fg_stroke);
                                ui.painter()
                                    .line_segment([icon_rect.right_top(), icon_rect.left_bottom()], visuals.fg_stroke);
                                dismiss_clicked = close.on_hover_text(t!("messages.dismiss")).clicked();
                            });
                        });

                        let header_height = header.response.rect.height() + ui.spacing().item_spacing.y;
                        egui::ScrollArea::vertical()
                            .max_height(max_height - header_height)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&self.ui.feedback.text).color(self.ui.feedback.color),
                                    )
                                    .wrap(),
                                );
                            });
                    });
            })
            .response;

        if dismiss_clicked || toast.on_hover_text(t!("messages.dismiss")).clicked() {
            self.ui.feedback.text.clear();
        }
    }
}

impl eframe::App for GuiApp {
    // No `logic` pass: distant-land generation pumps its own worker channel from
    // `show_generator`, and the key remapper polls its own `KeyCapture` from
    // `show_remap_dialog`. Nothing is left to pump centrally.

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.show_main_ui(ui, frame);
        self.show_dialogs(ui.ctx(), frame);
        self.show_status_toast(ui.ctx());
    }

    fn on_exit(&mut self) {
        if platform::morrowind_is_running() {
            return;
        }
        let _ = self.store.save(&self.settings);
        let _ = self.registry.save(self.settings.mge.runtime.disabled);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        style::APP_BG.to_normalized_gamma_f32()
    }
}
