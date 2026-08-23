use std::array;
use std::borrow::Cow;

use mge_config::{INPUT_COUNT, MacroKind, TRIGGER_COUNT};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeyPress {
    pub code: u8,
    pub down: bool,
}

#[derive(Clone, Debug)]
pub struct Macro {
    pub kind: MacroKind,
    pub console: Vec<KeyPress>,
    pub description: String,
    pub keys: [bool; INPUT_COUNT],
    pub timer_id: u8,
    pub function: u8,
}

impl Default for Macro {
    fn default() -> Self {
        Self {
            kind: MacroKind::Unused,
            console: Vec::new(),
            description: String::new(),
            keys: [false; INPUT_COUNT],
            timer_id: 0,
            function: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Trigger {
    pub interval_ms: u32,
    pub active: bool,
    pub keys: [bool; INPUT_COUNT],
}

impl Default for Trigger {
    fn default() -> Self {
        Self {
            interval_ms: 0,
            active: false,
            keys: [false; INPUT_COUNT],
        }
    }
}

#[derive(Clone, Debug)]
pub struct InputSettings {
    pub macros: Vec<Macro>,
    pub triggers: [Trigger; TRIGGER_COUNT],
    pub remap: [u8; 256],
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            macros: vec![Macro::default(); INPUT_COUNT],
            triggers: array::from_fn(|_| Trigger::default()),
            remap: [0; 256],
        }
    }
}

impl InputSettings {
    pub fn from_config(config: &mge_config::InputSettings) -> Self {
        let mut result = Self::default();
        for item in &config.macros {
            let index = item.index as usize;
            if index >= INPUT_COUNT {
                continue;
            }
            let mut macro_item = Macro {
                kind: item.kind,
                description: item.description.clone(),
                timer_id: item.timer_id,
                function: item.function,
                ..Macro::default()
            };
            macro_item.console = item
                .key_events
                .iter()
                .filter_map(|event| u8::try_from(event.code).ok().map(|code| KeyPress { code, down: event.down }))
                .collect();
            for key in &item.keys {
                if (*key as usize) < INPUT_COUNT {
                    macro_item.keys[*key as usize] = true;
                }
            }
            result.macros[index] = macro_item;
        }
        for item in &config.triggers {
            let index = item.index as usize;
            if index >= TRIGGER_COUNT {
                continue;
            }
            let mut trigger = Trigger {
                interval_ms: item.interval_ms,
                active: item.active,
                ..Trigger::default()
            };
            for key in &item.keys {
                if (*key as usize) < INPUT_COUNT {
                    trigger.keys[*key as usize] = true;
                }
            }
            result.triggers[index] = trigger;
        }
        for (source, target) in &config.remap {
            if (*source as usize) < result.remap.len() {
                result.remap[*source as usize] = *target;
            }
        }
        result
    }

    pub fn to_config(&self) -> mge_config::InputSettings {
        let macros = self
            .macros
            .iter()
            .enumerate()
            .filter(|(_, item)| item.kind != MacroKind::Unused)
            .map(|(index, item)| mge_config::MacroSettings {
                index: index as u16,
                kind: item.kind,
                key_events: item
                    .console
                    .iter()
                    .map(|event| mge_config::KeyEvent {
                        code: event.code as u16,
                        down: event.down,
                    })
                    .collect(),
                description: item.description.clone(),
                keys: item
                    .keys
                    .iter()
                    .enumerate()
                    .filter_map(|(key, active)| active.then_some(key as u16))
                    .collect(),
                timer_id: item.timer_id,
                function: item.function,
            })
            .collect();
        let triggers = self
            .triggers
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let keys = item
                    .keys
                    .iter()
                    .enumerate()
                    .filter_map(|(key, active)| active.then_some(key as u16))
                    .collect::<Vec<_>>();
                (!keys.is_empty()).then_some(mge_config::TriggerSettings {
                    index: index as u8,
                    active: item.active,
                    interval_ms: item.interval_ms,
                    keys,
                })
            })
            .collect();
        let remap = self
            .remap
            .iter()
            .enumerate()
            .filter(|(_, target)| **target != 0)
            .map(|(source, target)| (source as u16, *target))
            .collect();
        mge_config::InputSettings { macros, triggers, remap }
    }
}

pub const GRAPHICS_FUNCTIONS: &[(u8, &str)] = &[
    (0, "input.functions.screenshot"),
    (13, "input.functions.toggle_shaders"),
    (14, "input.functions.toggle_fps"),
    (27, "input.functions.toggle_crosshair"),
    (7, "input.functions.toggle_zoom"),
    (10, "input.functions.reset_zoom"),
    (8, "input.functions.increase_zoom"),
    (9, "input.functions.decrease_zoom"),
    (11, "input.functions.toggle_messages"),
    (12, "input.functions.last_message"),
    (21, "input.functions.increase_view"),
    (22, "input.functions.decrease_view"),
    (28, "input.functions.next_music"),
    (29, "input.functions.disable_music"),
    (36, "input.functions.toggle_distant"),
    (37, "input.functions.toggle_shadows"),
    (38, "input.functions.toggle_grass"),
    (39, "input.functions.toggle_blending"),
    (40, "input.functions.toggle_lighting"),
    (5, "input.functions.toggle_transparency_aa"),
    (41, "input.functions.increase_fov"),
    (42, "input.functions.decrease_fov"),
    (30, "input.functions.haggle_plus_1"),
    (43, "input.functions.haggle_plus_10"),
    (31, "input.functions.haggle_plus_100"),
    (44, "input.functions.haggle_plus_1000"),
    (32, "input.functions.haggle_plus_10000"),
    (33, "input.functions.haggle_minus_1"),
    (45, "input.functions.haggle_minus_10"),
    (34, "input.functions.haggle_minus_100"),
    (46, "input.functions.haggle_minus_1000"),
    (35, "input.functions.haggle_minus_10000"),
    (48, "input.functions.camera_forward"),
    (49, "input.functions.camera_back"),
    (50, "input.functions.camera_left"),
    (51, "input.functions.camera_right"),
    (52, "input.functions.camera_down"),
    (53, "input.functions.camera_up"),
];

#[derive(Clone, Copy, Debug)]
pub struct KeyButton {
    pub code: usize,
    pub label: &'static str,
}

impl KeyButton {
    pub const fn new(code: usize, label: &'static str) -> Self {
        Self { code, label }
    }
}

/// DirectInput scan-code to display name, grouped into keyboard rows for
/// readability. Indices 256..265 are mouse inputs.
///
/// This is a **name table**, not a layout: both keyboard surfaces draw from
/// `MACRO_KEYS` in `ui/input.rs`, which carries the legacy absolute geometry.
/// The per-key widths this table used to hold went with the flow layout the key
/// remapper was rebuilt away from; `input_label` is the only reader left.
pub const KEYBOARD_ROWS: &[&[KeyButton]] = &[
    &[
        KeyButton::new(0x01, "Esc"),
        KeyButton::new(0x3B, "F1"),
        KeyButton::new(0x3C, "F2"),
        KeyButton::new(0x3D, "F3"),
        KeyButton::new(0x3E, "F4"),
        KeyButton::new(0x3F, "F5"),
        KeyButton::new(0x40, "F6"),
        KeyButton::new(0x41, "F7"),
        KeyButton::new(0x42, "F8"),
        KeyButton::new(0x43, "F9"),
        KeyButton::new(0x44, "F10"),
        KeyButton::new(0x57, "F11"),
        KeyButton::new(0x58, "F12"),
        KeyButton::new(0xB7, "Prt"),
        KeyButton::new(0x46, "Scr"),
        KeyButton::new(0xC5, "Pause"),
    ],
    &[
        KeyButton::new(0x29, "Grave"),
        KeyButton::new(0x02, "1"),
        KeyButton::new(0x03, "2"),
        KeyButton::new(0x04, "3"),
        KeyButton::new(0x05, "4"),
        KeyButton::new(0x06, "5"),
        KeyButton::new(0x07, "6"),
        KeyButton::new(0x08, "7"),
        KeyButton::new(0x09, "8"),
        KeyButton::new(0x0A, "9"),
        KeyButton::new(0x0B, "0"),
        KeyButton::new(0x0C, "-"),
        KeyButton::new(0x0D, "="),
        KeyButton::new(0x0E, "Back"),
        KeyButton::new(0xD2, "Ins"),
        KeyButton::new(0xC7, "Home"),
        KeyButton::new(0xC9, "PgUp"),
        KeyButton::new(0x45, "Num"),
        KeyButton::new(0xB5, "/"),
        KeyButton::new(0x37, "*"),
        KeyButton::new(0x4A, "-"),
    ],
    &[
        KeyButton::new(0x0F, "Tab"),
        KeyButton::new(0x10, "Q"),
        KeyButton::new(0x11, "W"),
        KeyButton::new(0x12, "E"),
        KeyButton::new(0x13, "R"),
        KeyButton::new(0x14, "T"),
        KeyButton::new(0x15, "Y"),
        KeyButton::new(0x16, "U"),
        KeyButton::new(0x17, "I"),
        KeyButton::new(0x18, "O"),
        KeyButton::new(0x19, "P"),
        KeyButton::new(0x1A, "["),
        KeyButton::new(0x1B, "]"),
        KeyButton::new(0x2B, "\\"),
        KeyButton::new(0xD3, "Del"),
        KeyButton::new(0xCF, "End"),
        KeyButton::new(0xD1, "PgDn"),
        KeyButton::new(0x47, "7"),
        KeyButton::new(0x48, "8"),
        KeyButton::new(0x49, "9"),
        KeyButton::new(0x4E, "+"),
    ],
    &[
        KeyButton::new(0x3A, "Caps"),
        KeyButton::new(0x1E, "A"),
        KeyButton::new(0x1F, "S"),
        KeyButton::new(0x20, "D"),
        KeyButton::new(0x21, "F"),
        KeyButton::new(0x22, "G"),
        KeyButton::new(0x23, "H"),
        KeyButton::new(0x24, "J"),
        KeyButton::new(0x25, "K"),
        KeyButton::new(0x26, "L"),
        KeyButton::new(0x27, ";"),
        KeyButton::new(0x28, "'"),
        KeyButton::new(0x1C, "Enter"),
        KeyButton::new(0x4B, "4"),
        KeyButton::new(0x4C, "5"),
        KeyButton::new(0x4D, "6"),
    ],
    &[
        KeyButton::new(0x2A, "L Shift"),
        KeyButton::new(0x2C, "Z"),
        KeyButton::new(0x2D, "X"),
        KeyButton::new(0x2E, "C"),
        KeyButton::new(0x2F, "V"),
        KeyButton::new(0x30, "B"),
        KeyButton::new(0x31, "N"),
        KeyButton::new(0x32, "M"),
        KeyButton::new(0x33, ","),
        KeyButton::new(0x34, "."),
        KeyButton::new(0x35, "/"),
        KeyButton::new(0x36, "R Shift"),
        KeyButton::new(0xC8, "↑"),
        KeyButton::new(0x4F, "1"),
        KeyButton::new(0x50, "2"),
        KeyButton::new(0x51, "3"),
        KeyButton::new(0x9C, "Enter"),
    ],
    &[
        KeyButton::new(0x1D, "L Ctrl"),
        KeyButton::new(0xDB, "L Win"),
        KeyButton::new(0x38, "L Alt"),
        KeyButton::new(0x39, "Space"),
        KeyButton::new(0xB8, "R Alt"),
        KeyButton::new(0xDC, "R Win"),
        KeyButton::new(0xDD, "Menu"),
        KeyButton::new(0x9D, "R Ctrl"),
        KeyButton::new(0xCB, "←"),
        KeyButton::new(0xD0, "↓"),
        KeyButton::new(0xCD, "→"),
        KeyButton::new(0x52, "0"),
        KeyButton::new(0x53, "."),
    ],
    &[
        KeyButton::new(256, "Mouse 1"),
        KeyButton::new(257, "Mouse 2"),
        KeyButton::new(258, "Mouse 3"),
        KeyButton::new(259, "Mouse 4"),
        KeyButton::new(260, "Mouse 5"),
        KeyButton::new(261, "Mouse 6"),
        KeyButton::new(262, "Mouse 7"),
        KeyButton::new(263, "Mouse 8"),
        KeyButton::new(264, "Wheel ↑"),
        KeyButton::new(265, "Wheel ↓"),
    ],
];

pub fn input_label(code: usize) -> Cow<'static, str> {
    // `the_name_table_covers_every_drawn_key` pins the hex fallback as unreachable for drawn keys,
    // so the borrowed arm is the one that runs and the table's `&'static str` need not be copied.
    KEYBOARD_ROWS
        .iter()
        .flat_map(|row| row.iter())
        .find(|key| key.code == code)
        .map_or_else(|| Cow::Owned(format!("0x{code:02X}")), |key| Cow::Borrowed(key.label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_persistable_macro_kind_round_trips_and_unused_is_omitted() {
        let persistable = [
            MacroKind::Console1,
            MacroKind::Console2,
            MacroKind::Hammer1,
            MacroKind::Hammer2,
            MacroKind::Unhammer,
            MacroKind::AlternateHammer1,
            MacroKind::AlternateHammer2,
            MacroKind::AlternateUnhammer,
            MacroKind::Press1,
            MacroKind::Press2,
            MacroKind::Unpress,
            MacroKind::BeginTimer,
            MacroKind::EndTimer,
            MacroKind::Graphics,
        ];
        let config = mge_config::InputSettings {
            macros: persistable
                .into_iter()
                .enumerate()
                .map(|(index, kind)| mge_config::MacroSettings {
                    index: index as u16,
                    kind,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };

        let round_trip = InputSettings::from_config(&config).to_config();
        assert_eq!(round_trip.macros, config.macros);

        let unused = mge_config::InputSettings {
            macros: vec![mge_config::MacroSettings {
                index: 42,
                kind: MacroKind::Unused,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(InputSettings::from_config(&unused).to_config().macros.is_empty());
    }

    #[test]
    fn structured_input_round_trip() {
        let config = mge_config::InputSettings {
            macros: vec![
                mge_config::MacroSettings {
                    index: 65,
                    kind: mge_config::MacroKind::Graphics,
                    function: 39,
                    ..Default::default()
                },
                mge_config::MacroSettings {
                    index: 3,
                    kind: mge_config::MacroKind::Console1,
                    description: "hello".into(),
                    key_events: vec![
                        mge_config::KeyEvent { code: 41, down: true },
                        mge_config::KeyEvent { code: 41, down: false },
                    ],
                    ..Default::default()
                },
            ],
            triggers: vec![mge_config::TriggerSettings {
                index: 0,
                active: true,
                interval_ms: 250,
                keys: vec![30, 256],
            }],
            remap: [(30, 31)].into_iter().collect(),
        };
        let loaded = InputSettings::from_config(&config);
        assert_eq!(loaded.macros[65].function, 39);
        assert_eq!(loaded.macros[3].console.len(), 2);
        assert_eq!(loaded.macros[3].description, "hello");
        assert!(loaded.triggers[0].active);
        assert_eq!(loaded.triggers[0].interval_ms, 250);
        assert_eq!(loaded.remap[30], 31);

        let round_trip = loaded.to_config();
        let mut expected = config;
        expected.macros.sort_by_key(|value| value.index);
        assert_eq!(round_trip, expected);
        assert!(round_trip.render_triggers()[0].starts_with("T0=True,250,"));
    }

    #[test]
    fn press_macro_keys_round_trip_in_order_and_drop_out_of_range_indices() {
        let config = mge_config::InputSettings {
            macros: vec![mge_config::MacroSettings {
                index: 7,
                kind: MacroKind::Press1,
                keys: vec![264, 30, INPUT_COUNT as u16, 0],
                ..Default::default()
            }],
            ..Default::default()
        };

        let loaded = InputSettings::from_config(&config);
        assert!(loaded.macros[7].keys[0]);
        assert!(loaded.macros[7].keys[30]);
        assert!(loaded.macros[7].keys[264]);
        assert_eq!(loaded.macros[7].keys.iter().filter(|active| **active).count(), 3);

        let round_trip = loaded.to_config();
        assert_eq!(round_trip.macros[0].keys, vec![0, 30, 264]);
    }
}
