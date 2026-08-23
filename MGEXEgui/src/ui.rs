mod config;
mod distant;
mod generate;
mod graphics;
mod input;
mod shaders;
mod tabs;
mod widgets;

use std::time::Instant;

use eframe::egui::{Align, Button, Color32, Context, Id, Key, Layout, Modal, RichText};
use rust_i18n::t;

use crate::{
    app::GuiApp,
    config::{CONFIG_SCHEMA_VERSION, VERSION_NUMBER, VERSION_STRING},
    distant::DistantLandStatus,
    platform::DisplayMode,
    shaders::ShaderCatalog,
    style,
};

use config::ConfigUiState;
use distant::DistantUiState;
use graphics::GraphicsUiState;
use input::InputDialogs;
use shaders::ShaderDialogs;
pub(crate) use tabs::Pane;
use widgets::*;

/// Width of the About modal, matching the resolution dialog's client area.
const ABOUT_DIALOG_W: f32 = 370.0;
/// Width of the About modal's `Close` button, the same width the other
/// dialogs give their `OK` / `Cancel` buttons.
const ABOUT_BTN_W: f32 = 85.0;

pub(crate) struct FeedbackState {
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) color: Color32,
    pub(crate) expiry: Option<Instant>,
}

pub(crate) struct UiState {
    pub(crate) selected_pane: Pane,
    pub(crate) feedback: FeedbackState,
    pub(crate) graphics: GraphicsUiState,
    pub(crate) distant: DistantUiState,
    pub(crate) input: InputDialogs,
    pub(crate) shaders: ShaderDialogs,
    pub(crate) config: ConfigUiState,
}

impl UiState {
    pub(crate) fn new(
        display_modes: Vec<DisplayMode>,
        shader_catalog: ShaderCatalog,
        distant_status: DistantLandStatus,
        status_expiry: Instant,
    ) -> Self {
        Self {
            selected_pane: Pane::Graphics,
            feedback: FeedbackState {
                title: t!("feedback.information").into_owned(),
                text: t!("messages.settings_loaded").into_owned(),
                color: style::MUTED,
                expiry: Some(status_expiry),
            },
            graphics: GraphicsUiState {
                display_modes,
                resolution_editor: None,
            },
            distant: DistantUiState::new(distant_status),
            input: InputDialogs {
                macro_editor: None,
                remap_editor: None,
            },
            shaders: ShaderDialogs {
                catalog: shader_catalog,
                setup: None,
                editor: None,
            },
            config: ConfigUiState {
                clear_settings_on_reset: false,
                log_viewer: None,
                about_open: false,
            },
        }
    }
}

impl GuiApp {
    pub(crate) fn show_dialogs(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.show_generator(ctx);
        self.show_weather_dialog(ctx);
        self.show_lighting_dialog(ctx);
        self.show_resolution_dialog(ctx);
        self.show_macro_dialog(ctx);
        self.show_remap_dialog(ctx);
        self.show_shader_setup_dialog(ctx);
        self.show_shader_editor_dialog(ctx);
        self.show_log_viewer(ctx);
        self.show_about_dialog(ctx);
    }

    fn show_about_dialog(&mut self, ctx: &Context) {
        if !self.ui.config.about_open {
            return;
        }
        let mut open = true;
        let response = Modal::new(Id::new("about_modal")).show(ctx, |ui| {
            ui.set_width(ABOUT_DIALOG_W);
            ui.label(RichText::new(t!("about.title")).strong());
            ui.add_space(6.0);
            ui.heading(t!("application.title"));
            ui.label(t!(
                "about.version",
                version_string = VERSION_STRING,
                version_number = VERSION_NUMBER
            ));
            ui.add_space(8.0);
            ui.label(t!("about.description"));
            ui.label(t!("about.schema", version = CONFIG_SCHEMA_VERSION));
            ui.label(t!("about.license"));
            ui.hyperlink_to(t!("about.project_home"), "https://github.com/Hrnchamd/MGE-XE");
            ui.add_space(10.0);
            let height = ui.spacing().interact_size.y;
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_sized([ABOUT_BTN_W, height], Button::new(t!("common.actions.close")))
                        .clicked()
                    {
                        open = false;
                    }
                });
            });
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                open = false;
            }
        });
        if response.should_close() {
            open = false;
        }
        self.ui.config.about_open = open;
    }
}
