mod macro_editor;
mod page;
mod remapper;

use eframe::egui::Ui;

use crate::app::GuiApp;

use macro_editor::MacroEditorState;
use page::{camera_card, input_card, light_attenuation_card, mge_ini_card, morrowind_ini_card};
use remapper::RemapEditorState;

pub(crate) struct InputDialogs {
    pub(super) macro_editor: Option<MacroEditorState>,
    pub(super) remap_editor: Option<RemapEditorState>,
}

#[derive(Clone, Copy)]
struct KeyVisual {
    code: usize,
    label: &'static str,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

const fn key(code: usize, label: &'static str, x: f32, y: f32, width: f32, height: f32) -> KeyVisual {
    KeyVisual {
        code,
        label,
        x,
        y,
        width,
        height,
    }
}

// Legacy WinForms geometry, translated so (0, 0) is the 12 px client margin.
// Shared by the macro editor and the key remapper; the remapper drops the two
// mouse rows (see `remap_keyboard`).
//
// Escape is `0x01`, not the legacy `0x00`. Both WinForms forms named that button
// `b00` and derived the code from the name, but `DIK_ESCAPE` is `0x01` and
// DirectInput never reports offset 0: `RemapWrapper::GetDeviceState` in
// `d3d8/cpp/mge/mgedinput.cpp` loops `b2[RemappedKeys[i]] |= bytes[i]`
// over `i in 0..256` and `bytes[0]` is always clear. Transcribing `b00` faithfully
// made every Escape binding a silent no-op in both surfaces.
const MACRO_KEYS: &[KeyVisual] = &[
    key(0x01, "Esc", 0.0, 0.0, 32.0, 32.0),
    key(0x3B, "F1", 64.0, 0.0, 32.0, 32.0),
    key(0x3C, "F2", 96.0, 0.0, 32.0, 32.0),
    key(0x3D, "F3", 128.0, 0.0, 32.0, 32.0),
    key(0x3E, "F4", 160.0, 0.0, 32.0, 32.0),
    key(0x3F, "F5", 208.0, 0.0, 32.0, 32.0),
    key(0x40, "F6", 240.0, 0.0, 32.0, 32.0),
    key(0x41, "F7", 272.0, 0.0, 32.0, 32.0),
    key(0x42, "F8", 304.0, 0.0, 32.0, 32.0),
    key(0x43, "F9", 352.0, 0.0, 32.0, 32.0),
    key(0x44, "F10", 384.0, 0.0, 32.0, 32.0),
    key(0x57, "F11", 416.0, 0.0, 32.0, 32.0),
    key(0x58, "F12", 448.0, 0.0, 32.0, 32.0),
    key(0xB7, "Prt\nScr", 496.0, 0.0, 32.0, 32.0),
    key(0x46, "Scr\nLck", 528.0, 0.0, 32.0, 32.0),
    key(0xC5, "Ps", 560.0, 0.0, 32.0, 32.0),
    key(0x29, "`", 0.0, 48.0, 32.0, 32.0),
    key(0x02, "1", 32.0, 48.0, 32.0, 32.0),
    key(0x03, "2", 64.0, 48.0, 32.0, 32.0),
    key(0x04, "3", 96.0, 48.0, 32.0, 32.0),
    key(0x05, "4", 128.0, 48.0, 32.0, 32.0),
    key(0x06, "5", 160.0, 48.0, 32.0, 32.0),
    key(0x07, "6", 192.0, 48.0, 32.0, 32.0),
    key(0x08, "7", 224.0, 48.0, 32.0, 32.0),
    key(0x09, "8", 256.0, 48.0, 32.0, 32.0),
    key(0x0A, "9", 288.0, 48.0, 32.0, 32.0),
    key(0x0B, "0", 320.0, 48.0, 32.0, 32.0),
    key(0x0C, "-", 352.0, 48.0, 32.0, 32.0),
    key(0x0D, "=", 384.0, 48.0, 32.0, 32.0),
    key(0x0E, "<-----", 416.0, 48.0, 64.0, 32.0),
    key(0xD2, "Ins", 496.0, 48.0, 32.0, 32.0),
    key(0xC7, "Hm", 528.0, 48.0, 32.0, 32.0),
    key(0xC9, "Pg\nUp", 560.0, 48.0, 32.0, 32.0),
    key(0x45, "Nm\nLck", 608.0, 48.0, 32.0, 32.0),
    key(0xB5, "/", 640.0, 48.0, 32.0, 32.0),
    key(0x37, "*", 672.0, 48.0, 32.0, 32.0),
    key(0x4A, "-", 704.0, 48.0, 32.0, 32.0),
    key(0x0F, "Tab", 0.0, 80.0, 48.0, 32.0),
    key(0x10, "q", 48.0, 80.0, 32.0, 32.0),
    key(0x11, "w", 80.0, 80.0, 32.0, 32.0),
    key(0x12, "e", 112.0, 80.0, 32.0, 32.0),
    key(0x13, "r", 144.0, 80.0, 32.0, 32.0),
    key(0x14, "t", 176.0, 80.0, 32.0, 32.0),
    key(0x15, "y", 208.0, 80.0, 32.0, 32.0),
    key(0x16, "u", 240.0, 80.0, 32.0, 32.0),
    key(0x17, "i", 272.0, 80.0, 32.0, 32.0),
    key(0x18, "o", 304.0, 80.0, 32.0, 32.0),
    key(0x19, "p", 336.0, 80.0, 32.0, 32.0),
    key(0x1A, "[", 368.0, 80.0, 32.0, 32.0),
    key(0x1B, "]", 400.0, 80.0, 32.0, 32.0),
    key(0x2B, "\\", 432.0, 80.0, 48.0, 32.0),
    key(0xD3, "Del", 496.0, 80.0, 32.0, 32.0),
    key(0xCF, "End", 528.0, 80.0, 32.0, 32.0),
    key(0xD1, "Pg\nDn", 560.0, 80.0, 32.0, 32.0),
    key(0x47, "7", 608.0, 80.0, 32.0, 32.0),
    key(0x48, "8", 640.0, 80.0, 32.0, 32.0),
    key(0x49, "9", 672.0, 80.0, 32.0, 32.0),
    key(0x4E, "+", 704.0, 80.0, 32.0, 64.0),
    key(0x3A, "Caps\nLock", 0.0, 112.0, 56.0, 32.0),
    key(0x1E, "a", 56.0, 112.0, 32.0, 32.0),
    key(0x1F, "s", 88.0, 112.0, 32.0, 32.0),
    key(0x20, "d", 120.0, 112.0, 32.0, 32.0),
    key(0x21, "f", 152.0, 112.0, 32.0, 32.0),
    key(0x22, "g", 184.0, 112.0, 32.0, 32.0),
    key(0x23, "h", 216.0, 112.0, 32.0, 32.0),
    key(0x24, "j", 248.0, 112.0, 32.0, 32.0),
    key(0x25, "k", 280.0, 112.0, 32.0, 32.0),
    key(0x26, "l", 312.0, 112.0, 32.0, 32.0),
    key(0x27, ";", 344.0, 112.0, 32.0, 32.0),
    key(0x28, "'", 376.0, 112.0, 32.0, 32.0),
    key(0x1C, "Ret", 408.0, 112.0, 72.0, 32.0),
    key(0x4B, "4", 608.0, 112.0, 32.0, 32.0),
    key(0x4C, "5", 640.0, 112.0, 32.0, 32.0),
    key(0x4D, "6", 672.0, 112.0, 32.0, 32.0),
    key(0x2A, "L Shift", 0.0, 144.0, 72.0, 32.0),
    key(0x2C, "z", 72.0, 144.0, 32.0, 32.0),
    key(0x2D, "x", 104.0, 144.0, 32.0, 32.0),
    key(0x2E, "c", 136.0, 144.0, 32.0, 32.0),
    key(0x2F, "v", 168.0, 144.0, 32.0, 32.0),
    key(0x30, "b", 200.0, 144.0, 32.0, 32.0),
    key(0x31, "n", 232.0, 144.0, 32.0, 32.0),
    key(0x32, "m", 264.0, 144.0, 32.0, 32.0),
    key(0x33, ",", 296.0, 144.0, 32.0, 32.0),
    key(0x34, ".", 328.0, 144.0, 32.0, 32.0),
    key(0x35, "/", 360.0, 144.0, 32.0, 32.0),
    key(0x36, "R Shift", 392.0, 144.0, 88.0, 32.0),
    key(0xC8, "↑", 528.0, 144.0, 32.0, 32.0),
    key(0x4F, "1", 608.0, 144.0, 32.0, 32.0),
    key(0x50, "2", 640.0, 144.0, 32.0, 32.0),
    key(0x51, "3", 672.0, 144.0, 32.0, 32.0),
    key(0x9C, "Entr", 704.0, 144.0, 32.0, 64.0),
    key(0x1D, "L Ctrl", 0.0, 176.0, 40.0, 32.0),
    key(0xDB, "L Win", 40.0, 176.0, 40.0, 32.0),
    key(0x38, "L Alt", 80.0, 176.0, 40.0, 32.0),
    key(0x39, "Space", 120.0, 176.0, 200.0, 32.0),
    key(0xB8, "R Alt", 320.0, 176.0, 40.0, 32.0),
    key(0xDC, "R Win", 360.0, 176.0, 40.0, 32.0),
    key(0xDD, "App", 400.0, 176.0, 40.0, 32.0),
    key(0x9D, "R Ctrl", 440.0, 176.0, 40.0, 32.0),
    key(0xCB, "←", 496.0, 176.0, 32.0, 32.0),
    key(0xD0, "↓", 528.0, 176.0, 32.0, 32.0),
    key(0xCD, "→", 560.0, 176.0, 32.0, 32.0),
    key(0x52, "0", 608.0, 176.0, 64.0, 32.0),
    key(0x53, ".", 672.0, 176.0, 32.0, 32.0),
    key(256, "Mouse 1", 0.0, 214.0, 70.0, 32.0),
    key(257, "Mouse 2", 74.0, 214.0, 70.0, 32.0),
    key(258, "Mouse 3", 148.0, 214.0, 70.0, 32.0),
    key(259, "Mouse 4", 222.0, 214.0, 70.0, 32.0),
    key(260, "Mouse 5", 296.0, 214.0, 70.0, 32.0),
    key(261, "Mouse 6", 370.0, 214.0, 70.0, 32.0),
    key(262, "Mouse 7", 444.0, 214.0, 70.0, 32.0),
    key(263, "Mouse 8", 518.0, 214.0, 70.0, 32.0),
    key(264, "Mouse\nwheel up", 592.0, 214.0, 70.0, 32.0),
    key(265, "Mouse\nwheel down", 666.0, 214.0, 70.0, 32.0),
];

impl GuiApp {
    pub(crate) fn show_in_game(&mut self, ui: &mut Ui) {
        ui.columns(2, |columns| {
            mge_ini_card(&mut columns[0], &mut self.settings.mge);
            columns[0].add_space(3.0);
            morrowind_ini_card(&mut columns[0], &mut self.settings.ini);

            camera_card(&mut columns[1], &mut self.settings.mge.runtime);
            columns[1].add_space(3.0);
            light_attenuation_card(&mut columns[1], &mut self.settings.ini);
            columns[1].add_space(3.0);
            input_card(
                &mut columns[1],
                &self.settings.input,
                &mut self.ui.input.macro_editor,
                &mut self.ui.input.remap_editor,
            );
        });
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use crate::input::{KEYBOARD_ROWS, input_label};
    use mge_config::INPUT_COUNT;

    #[test]
    fn escape_uses_the_directinput_scan_code() {
        // Legacy named this button `b00` and derived the code from the name, so
        // Escape bindings landed on an offset DirectInput never reports. Both
        // tables must agree on `DIK_ESCAPE`.
        let escape = MACRO_KEYS
            .iter()
            .find(|key| key.label == "Esc")
            .expect("the keyboard has an Escape key");
        assert_eq!(escape.code, 0x01);
        assert_eq!(input_label(0x01), "Esc");
    }

    #[test]
    fn every_key_has_a_distinct_code_within_the_macro_range() {
        let mut seen = vec![false; INPUT_COUNT];
        for key in MACRO_KEYS {
            assert!(key.code < INPUT_COUNT, "{} is out of range", key.label);
            assert!(!seen[key.code], "{:#04x} appears twice", key.code);
            seen[key.code] = true;
        }
    }

    #[test]
    fn the_name_table_covers_every_drawn_key() {
        // `input_label` falls back to a bare hex code, which is what a remapped
        // key would show in its hover text if the two tables drifted apart.
        for key in MACRO_KEYS {
            assert_ne!(
                input_label(key.code),
                format!("0x{:02X}", key.code),
                "{} ({:#04x}) is missing from KEYBOARD_ROWS",
                key.label,
                key.code,
            );
        }
    }

    #[test]
    fn the_remapper_draws_only_remappable_keys() {
        // `RemapWrapper::GetDeviceState` walks a 256-entry keyboard table, so
        // the mouse codes carried for the macro editor must not be offered.
        let remappable = MACRO_KEYS.iter().filter(|key| key.code < 256).count();
        assert_eq!(remappable + 10, MACRO_KEYS.len());
        assert!(KEYBOARD_ROWS.iter().flat_map(|row| row.iter()).count() >= remappable);
    }
}
