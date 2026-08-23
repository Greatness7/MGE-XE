use eframe::egui::{self, Ui};
use rust_i18n::t;

use mge_config::{RuntimeSettings, Settings};

use crate::{
    input::InputSettings,
    morrowind_profile::IniSettings,
    style,
    ui::{spin_row, split, tooltip},
};

use super::{MacroEditorState, RemapEditorState};

pub(super) fn mge_ini_card(ui: &mut Ui, settings: &mut Settings) {
    style::card(ui, "mgeXE.toml", |ui| {
        split(
            ui,
            |ui| {
                tooltip(
                    ui.checkbox(&mut settings.runtime.disabled, t!("input.mge.disable_mge")),
                    t!("input.mge.disable_mge_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.runtime.mwse_disabled, t!("input.mge.disable_mwse")),
                    t!("input.mge.disable_mwse_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.runtime.proxy_only, t!("input.mge.proxy_only")),
                    t!("input.mge.proxy_only_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.runtime.skip_intro, t!("input.mge.skip_intro")),
                    t!("input.mge.skip_intro_tip"),
                );
            },
            |ui| {
                tooltip(
                    ui.checkbox(&mut settings.runtime.menu_caching, t!("input.mge.pause_menus")),
                    t!("input.mge.pause_menus_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.runtime.crosshair_autohide, t!("input.mge.crosshair_autohide")),
                    t!("input.mge.crosshair_autohide_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.render.messages, t!("input.mge.messages")),
                    t!("input.mge.messages_tip"),
                );
            },
        );
        tooltip(
            spin_row(
                ui,
                t!("input.mge.message_duration").as_ref(),
                settings.render.messages,
                egui::DragValue::new(&mut settings.render.message_timeout_ms)
                    .range(1000..=10000)
                    .speed(10)
                    .suffix(" ms"),
            ),
            t!("input.mge.message_duration_tip"),
        );
    });
}

pub(super) fn morrowind_ini_card(ui: &mut Ui, settings: &mut IniSettings) {
    style::card(ui, "Morrowind.ini", |ui| {
        split(
            ui,
            |ui| {
                tooltip(
                    ui.checkbox(&mut settings.yes_to_all, t!("input.morrowind.yes_to_all")),
                    t!("input.morrowind.yes_to_all_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.thread_loading, t!("input.morrowind.thread_loading")),
                    t!("input.morrowind.thread_loading_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.subtitles, t!("input.morrowind.subtitles")),
                    t!("input.morrowind.subtitles_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.show_fps, t!("input.morrowind.show_fps")),
                    t!("input.morrowind.show_fps_tip"),
                );
            },
            |ui| {
                tooltip(
                    ui.checkbox(&mut settings.screenshots, t!("input.morrowind.screenshots")),
                    t!("input.morrowind.screenshots_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.high_detail_shadows, t!("input.morrowind.actor_shadows")),
                    t!("input.morrowind.actor_shadows_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.disable_audio, t!("input.morrowind.disable_audio")),
                    t!("input.morrowind.disable_audio_tip"),
                );
                tooltip(
                    ui.checkbox(&mut settings.hit_fader, t!("input.morrowind.hit_fader")),
                    t!("input.morrowind.hit_fader_tip"),
                );
            },
        );
        ui.separator();
        spin_row(
            ui,
            t!("input.morrowind.fps_limit").as_ref(),
            true,
            egui::DragValue::new(&mut settings.fps_limit)
                .range(1..=300)
                .speed(1)
                .suffix(" FPS"),
        );
    });
}

pub(super) fn camera_card(ui: &mut Ui, settings: &mut RuntimeSettings) {
    style::card(ui, t!("input.camera.title"), |ui| {
        let custom = settings.custom_camera;
        tooltip(
            ui.checkbox(&mut settings.custom_camera, t!("input.camera.custom")),
            t!("input.camera.custom_tip"),
        );
        tooltip(
            spin_row(
                ui,
                t!("input.camera.x_offset").as_ref(),
                custom,
                egui::DragValue::new(&mut settings.camera_x)
                    .range(-500.0..=500.0)
                    .speed(0.5)
                    .fixed_decimals(1),
            ),
            t!("input.camera.x_offset_tip"),
        );
        tooltip(
            spin_row(
                ui,
                t!("input.camera.y_offset").as_ref(),
                custom,
                egui::DragValue::new(&mut settings.camera_y)
                    .range(-500.0..=500.0)
                    .speed(0.5)
                    .fixed_decimals(1),
            ),
            t!("input.camera.y_offset_tip"),
        );
        tooltip(
            spin_row(
                ui,
                t!("input.camera.z_offset").as_ref(),
                custom,
                egui::DragValue::new(&mut settings.camera_z)
                    .range(-500.0..=500.0)
                    .speed(0.5)
                    .fixed_decimals(1),
            ),
            t!("input.camera.z_offset_tip"),
        );
    });
}

pub(super) fn light_attenuation_card(ui: &mut Ui, settings: &mut IniSettings) {
    style::card(ui, t!("input.lighting.title"), |ui| {
        tooltip(
            spin_row(
                ui,
                t!("input.lighting.constant").as_ref(),
                true,
                egui::DragValue::new(&mut settings.light_constant)
                    .range(0.0..=5.0)
                    .speed(0.01)
                    .fixed_decimals(3),
            ),
            t!("input.lighting.constant_tip"),
        );
        tooltip(
            spin_row(
                ui,
                t!("input.lighting.linear").as_ref(),
                true,
                egui::DragValue::new(&mut settings.light_linear)
                    .range(0.0..=5.0)
                    .speed(0.01)
                    .fixed_decimals(3),
            ),
            t!("input.lighting.linear_tip"),
        );
        tooltip(
            spin_row(
                ui,
                t!("input.lighting.quadratic").as_ref(),
                true,
                egui::DragValue::new(&mut settings.light_quadratic)
                    .range(0.0..=5.0)
                    .speed(0.01)
                    .fixed_decimals(3),
            ),
            t!("input.lighting.quadratic_tip"),
        );
        style::hint(ui, t!("input.lighting.hint").as_ref());
    });
}

pub(super) fn input_card(
    ui: &mut Ui,
    settings: &InputSettings,
    macro_editor: &mut Option<MacroEditorState>,
    remap_editor: &mut Option<RemapEditorState>,
) {
    style::card(ui, t!("input.card.title"), |ui| {
        ui.horizontal(|ui| {
            if tooltip(ui.button(t!("input.card.macro_editor")), t!("input.card.macro_editor_tip")).clicked() {
                *macro_editor = Some(MacroEditorState::new(settings));
            }
            if tooltip(ui.button(t!("input.card.key_remapper")), t!("input.card.key_remapper_tip")).clicked() {
                *remap_editor = Some(RemapEditorState::default());
            }
        });
    });
}
