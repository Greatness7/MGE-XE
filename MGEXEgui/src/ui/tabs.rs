use eframe::egui;
use rust_i18n::t;

use crate::app::GuiApp;

use super::widgets::selectable_value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Pane {
    Graphics,
    DistantLand,
    InGame,
    Config,
    Instructions,
}

impl Pane {
    fn translation_key(self) -> &'static str {
        match self {
            Self::Graphics => "tabs.graphics",
            Self::DistantLand => "tabs.distant_land",
            Self::InGame => "tabs.in_game",
            Self::Config => "tabs.config",
            Self::Instructions => "tabs.instructions",
        }
    }
}

impl GuiApp {
    pub(crate) fn show_main_ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        ui.scope(|ui| {
            let body_item_spacing_y = ui.spacing().item_spacing.y;
            ui.spacing_mut().item_spacing.y = 0.0;

            ui.horizontal(|ui| {
                ui.spacing_mut().interact_size.y = 24.0;
                ui.spacing_mut().button_padding.x = 10.0;

                for pane in [
                    Pane::Graphics,
                    Pane::DistantLand,
                    Pane::InGame,
                    Pane::Config,
                    Pane::Instructions,
                ] {
                    selectable_value(ui, &mut self.ui.selected_pane, pane, t!(pane.translation_key()));
                }
            });

            let selected_pane = self.ui.selected_pane;
            egui::ScrollArea::vertical()
                .content_margin(egui::Margin::symmetric(6, 6))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = body_item_spacing_y;
                    match selected_pane {
                        Pane::Graphics => self.show_graphics(ui),
                        Pane::DistantLand => self.show_distant_land(ui, frame),
                        Pane::InGame => self.show_in_game(ui),
                        Pane::Config => self.show_config(ui),
                        Pane::Instructions => self.show_instructions(ui),
                    }
                });
        });
    }
}
