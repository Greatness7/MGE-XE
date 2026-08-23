use std::{
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context, Result, bail};
use regex::Regex;
use rust_i18n::t;

/// Category reported for a shader whose technique carries no `category`
/// annotation. The runtime gives these a neutral priority and the dialog keeps
/// them at the end of the chain.
pub const UNCATEGORIZED: &str = "custom";

/// Sort priority of an uncategorized shader.
pub const DEFAULT_PRIORITY: i32 = 6_000_000;

/// Category → sort priority, mirroring `ShaderPriorityValue` in
/// `d3d8/cpp/mge/postshaders.cpp`. The runtime re-sorts the chain on
/// every load, so this table decides only what the dialog *shows*; keeping it
/// equal to the runtime's is what makes the shown order honest.
pub fn category_priority(category: &str) -> i32 {
    match category {
        "scene" => 1_000_000,
        "atmosphere" => 2_000_000,
        "lens" => 3_000_000,
        "sensor" => 4_000_000,
        "tone" => 5_000_000,
        "final" => 9_000_000,
        _ => DEFAULT_PRIORITY,
    }
}

#[derive(Clone, Debug)]
pub struct ShaderInfo {
    pub name: String,
    pub path: PathBuf,
    pub category: String,
    /// `category_priority(category) + priorityAdjust`, as the runtime computes it.
    pub priority: i32,
}

#[derive(Clone, Debug, Default)]
pub struct ShaderCatalog {
    pub shaders: Vec<ShaderInfo>,
}

impl ShaderCatalog {
    pub fn scan(root: &Path) -> Self {
        let directory = root.join("Data Files").join("shaders").join("XEshaders");
        let mut shaders = Vec::new();
        if let Ok(entries) = fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("fx")) {
                    continue;
                }
                let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default().to_owned();
                let source = fs::read_to_string(&path).unwrap_or_default();
                let (category, adjust) = parse_annotations(&source);
                let category = category.unwrap_or_else(|| UNCATEGORIZED.to_owned());
                let priority = category_priority(&category) + adjust;
                shaders.push(ShaderInfo {
                    name,
                    path,
                    category,
                    priority,
                });
            }
        }
        shaders.sort_by(|left, right| left.name.cmp(&right.name));
        Self { shaders }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.shaders.iter().any(|shader| shader.name == name)
    }

    pub fn get(&self, name: &str) -> Option<&ShaderInfo> {
        self.shaders.iter().find(|shader| shader.name == name)
    }

    /// Category shown beside a chain entry; empty when the shader is not on disk.
    pub fn category_of(&self, name: &str) -> &str {
        self.get(name).map_or("", |shader| shader.category.as_str())
    }

    /// Add `name` to `active` at its sorted position.
    ///
    /// Categorized shaders land before the first categorized entry of a higher
    /// priority; uncategorized ones are appended, and are skipped over while
    /// searching for that position. Deliberately not a full sort: it leaves any
    /// manual ordering of same-priority entries alone, which the runtime's
    /// `stable_sort` then preserves.
    ///
    /// Shaders absent from the catalog are ignored; a chain loaded from TOML is
    /// pruned against the catalog at startup.
    pub fn insert_sorted(&self, active: &mut Vec<String>, name: &str) {
        if active.iter().any(|entry| entry == name) {
            return;
        }
        let Some(shader) = self.get(name) else {
            return;
        };
        if shader.category == UNCATEGORIZED {
            active.push(name.to_owned());
            return;
        }

        let mut insert_at = active.len();
        for (index, entry) in active.iter().enumerate() {
            let Some(other) = self.get(entry) else {
                continue;
            };
            if other.category == UNCATEGORIZED {
                continue;
            }
            if shader.priority < other.priority {
                insert_at = index;
                break;
            }
        }
        active.insert(insert_at, name.to_owned());
    }

    /// The chain a quality preset produces, in sorted order.
    pub fn preset_chain(&self, preset: usize) -> Vec<String> {
        let mut active = Vec::new();
        if let Some((_, names)) = PRESETS.get(preset) {
            for name in *names {
                self.insert_sorted(&mut active, name);
            }
        }
        active
    }

    /// The chain the seven feature dropdowns describe, rebuilt from scratch.
    ///
    /// The rebuild order is the legacy handler's and is load-bearing for one
    /// pair: "Underwater Interior Effects" and "Underwater Effects" are both
    /// `scene`, so only insertion order separates them.
    pub fn chain_from_effects(&self, selections: &[usize]) -> Vec<String> {
        let mut active = Vec::new();
        for &index in EFFECT_REBUILD_ORDER {
            let Some(effect) = EFFECT_OPTIONS.get(index) else {
                continue;
            };
            let Some(&selected) = selections.get(index) else {
                continue;
            };
            if selected > 0
                && let Some(name) = effect.shaders.get(selected - 1)
            {
                self.insert_sorted(&mut active, name);
            }
        }
        active
    }
}

/// One feature dropdown on the left pane: a caption, its option captions, and
/// the shader each non-`Off` option contributes. `options` is always one longer
/// than `shaders`, because option 0 is "Off".
pub struct EffectOption {
    pub label: &'static str,
    pub options: &'static [&'static str],
    pub shaders: &'static [&'static str],
}

pub const EFFECT_OPTIONS: &[EffectOption] = &[
    EffectOption {
        label: "shaders.effects.hdr",
        options: &["common.choices.off", "common.choices.on"],
        shaders: &["Eye Adaptation (HDR)"],
    },
    EffectOption {
        label: "shaders.effects.ssao",
        options: &["common.choices.off", "shaders.quality.medium", "shaders.quality.high"],
        shaders: &["SSAO Fast", "SSAO HQ"],
    },
    EffectOption {
        label: "shaders.effects.bloom",
        options: &["common.choices.off", "shaders.quality.fine", "shaders.quality.soft"],
        shaders: &["Bloom Fine", "Bloom Soft"],
    },
    EffectOption {
        label: "shaders.effects.sunshafts",
        options: &["common.choices.off", "common.choices.on"],
        shaders: &["Sunshafts"],
    },
    EffectOption {
        label: "shaders.effects.depth_of_field",
        options: &["common.choices.off", "common.choices.on"],
        shaders: &["Depth of Field"],
    },
    EffectOption {
        label: "shaders.effects.underwater_sunshafts",
        options: &["common.choices.off", "common.choices.on"],
        shaders: &["Underwater Effects"],
    },
    EffectOption {
        label: "shaders.effects.interior_caustics",
        options: &["common.choices.off", "common.choices.on"],
        shaders: &["Underwater Interior Effects"],
    },
];

/// Indices into [`EFFECT_OPTIONS`] in the order the legacy handler added them:
/// SSAO, interior caustics, underwater sunshafts, depth of field, sunshafts,
/// bloom, HDR. See [`ShaderCatalog::chain_from_effects`].
const EFFECT_REBUILD_ORDER: &[usize] = &[1, 6, 5, 4, 3, 2, 0];

/// Which option a feature dropdown shows for the current chain: 0 for "Off",
/// otherwise one past the index of whichever of its shaders is active.
pub fn effect_selection(active: &[String], effect: &EffectOption) -> usize {
    for name in active {
        if let Some(index) = effect.shaders.iter().position(|shader| shader == name) {
            return index + 1;
        }
    }
    0
}

/// Index of the preset this chain reproduces exactly, or [`CUSTOM_PRESET`].
pub fn matching_preset(active: &[String]) -> usize {
    PRESETS
        .iter()
        .position(|(_, names)| {
            names.len() == active.len() && names.iter().zip(active).all(|(name, entry)| *name == entry.as_str())
        })
        .unwrap_or(CUSTOM_PRESET)
}

/// Sentinel index of the trailing "Custom" entry in the preset dropdown. It is
/// not a preset: selecting it changes nothing, and it is what the dropdown shows
/// whenever the chain matches no preset.
pub const CUSTOM_PRESET: usize = PRESETS.len();

/// Captions for the preset dropdown, including the trailing "Custom" sentinel.
pub fn preset_labels() -> Vec<&'static str> {
    PRESETS
        .iter()
        .map(|(name, _)| *name)
        .chain(std::iter::once("shaders.presets.custom"))
        .collect()
}

/// Read the `category` and `priorityAdjust` annotations off the first
/// `technique` declaration, the way `PostShaders::initShader` does.
fn parse_annotations(source: &str) -> (Option<String>, i32) {
    static TECHNIQUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\btechnique\b[^\r\n<]*<([^>]*)>"#).unwrap());
    static CATEGORY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\bcategory\s*=\s*"(\w+)""#).unwrap());
    static PRIORITY_ADJUST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bpriorityAdjust\s*=\s*(-?\d+)").unwrap());

    let Some(annotations) = TECHNIQUE.captures(source).and_then(|captures| captures.get(1)) else {
        return (None, 0);
    };
    let annotations = annotations.as_str();

    let category = CATEGORY
        .captures(annotations)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_owned());
    let adjust = PRIORITY_ADJUST
        .captures(annotations)
        .and_then(|captures| captures.get(1))
        .and_then(|value| value.as_str().parse().ok())
        .unwrap_or(0);

    (category, adjust)
}

/// Name a document carries until it is saved somewhere.
pub const NEW_FILE_NAME: &str = "shaders.editor.new_file";

#[derive(Clone, Debug)]
pub struct ShaderEditor {
    pub name: String,
    pub path: Option<PathBuf>,
    pub source: String,
    pub dirty: bool,
    /// Contents of the message pane below the source. Carries save and flag
    /// results, and is deliberately persistent: it stays until the next
    /// operation replaces it rather than expiring like the main window's toast.
    pub message: String,
}

impl ShaderEditor {
    pub fn new() -> Self {
        Self {
            name: t!(NEW_FILE_NAME).into_owned(),
            path: None,
            source: String::new(),
            dirty: false,
            message: String::new(),
        }
    }

    pub fn open(shader: &ShaderInfo) -> Result<Self> {
        Self::open_path(shader.path.clone())
    }

    pub fn open_path(path: PathBuf) -> Result<Self> {
        let source = fs::read_to_string(&path).with_context(|| format!("read shader {}", path.display()))?;
        let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("shader").to_owned();
        Ok(Self {
            name,
            // Trim CR/LF here and re-add one trailing newline on save so a file
            // does not grow a blank line per round trip.
            source: source.trim_matches(['\r', '\n']).to_owned(),
            path: Some(path),
            dirty: false,
            message: String::new(),
        })
    }

    /// Window title in `<filename>[*] - Shader Editor` format; the asterisk is
    /// the dirty indicator.
    pub fn title(&self) -> String {
        let mark = if self.dirty { "*" } else { "" };
        t!("shaders.editor.title", name = &self.name, mark = mark).into_owned()
    }

    pub fn save(&mut self) -> Result<()> {
        let Some(path) = &self.path else {
            bail!("{}", t!("shaders.messages.choose_name"));
        };
        fs::write(path, self.on_disk_source()).with_context(|| format!("write shader {}", path.display()))?;
        self.dirty = false;
        Ok(())
    }

    pub fn save_as(&mut self, mut path: PathBuf) -> Result<()> {
        if path.extension().is_none() {
            path.set_extension("fx");
        }
        fs::write(&path, self.on_disk_source()).with_context(|| format!("write shader {}", path.display()))?;
        self.name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("shader").to_owned();
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }

    /// The source as written to disk: the buffer plus the single trailing
    /// newline the legacy editor appended.
    fn on_disk_source(&self) -> String {
        format!("{}\n", self.source.trim_end_matches(['\r', '\n']))
    }

    pub fn flags(&self) -> u32 {
        flags_regex()
            .captures(&self.source)
            .and_then(|captures| captures.get(1))
            .and_then(|value| value.as_str().parse().ok())
            .unwrap_or(0)
    }

    pub fn set_flags(&mut self, flags: u32) {
        let regex = flags_regex();
        if regex.is_match(&self.source) {
            self.source = regex.replace(&self.source, format!("int mgeflags = {flags}")).into_owned();
        } else {
            self.source = format!("int mgeflags = {flags};\n{}", self.source);
        }
        self.dirty = true;
    }
}

fn flags_regex() -> Regex {
    Regex::new(r"int[ \t]+mgeflags[ \t]*=[ \t]*([0-9]+)").unwrap()
}

pub const PRESETS: &[(&str, &[&str])] = &[
    ("shaders.presets.lowest", &["Bloom Fine"]),
    ("shaders.presets.low", &["Sunshafts", "Bloom Fine", "Eye Adaptation (HDR)"]),
    (
        "shaders.presets.medium",
        &["SSAO Fast", "Sunshafts", "Bloom Fine", "Eye Adaptation (HDR)"],
    ),
    (
        "shaders.presets.high",
        &[
            "SSAO Fast",
            "Underwater Interior Effects",
            "Underwater Effects",
            "Sunshafts",
            "Bloom Soft",
            "Eye Adaptation (HDR)",
        ],
    ),
    (
        "shaders.presets.very_high",
        &[
            "SSAO HQ",
            "Underwater Interior Effects",
            "Underwater Effects",
            "Sunshafts",
            "Bloom Soft",
            "Eye Adaptation (HDR)",
        ],
    ),
];

/// The seven `mgeflags` bits and their captions. The two parenthesised
/// captions are why the dialog is as wide as it is.
pub const SHADER_FLAGS: &[(u32, &str)] = &[
    (1, "shaders.flags.interiors"),
    (2, "shaders.flags.exteriors"),
    (4, "shaders.flags.interior_exteriors"),
    (8, "shaders.flags.underwater"),
    (16, "shaders.flags.above_water"),
    (32, "shaders.flags.sun_visible"),
    (64, "shaders.flags.sun_hidden"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn editor_with(source: &str) -> ShaderEditor {
        ShaderEditor {
            name: "test".to_owned(),
            path: Some(PathBuf::from("test.fx")),
            source: source.to_owned(),
            dirty: false,
            message: String::new(),
        }
    }

    #[test]
    fn shader_flags_update_in_place() {
        let mut editor = editor_with("int mgeflags = 3;\ntechnique main {}");
        assert_eq!(editor.flags(), 3);
        editor.set_flags(65);
        assert!(editor.source.contains("int mgeflags = 65"));
        assert!(editor.dirty);
    }

    /// A source with no declaration reads as zero and gains one on acceptance,
    /// prepended. These are the two halves of the legacy `bOK` handler.
    #[test]
    fn missing_shader_flags_read_as_zero_and_are_prepended() {
        let mut editor = editor_with("technique main {}");
        assert_eq!(editor.flags(), 0);
        editor.set_flags(6);
        assert!(editor.source.starts_with("int mgeflags = 6;\n"));
        assert!(editor.source.ends_with("technique main {}"));
        assert_eq!(editor.flags(), 6);
    }

    /// The seven bits are the powers of two the runtime tests, in order.
    #[test]
    fn shader_flag_bits_are_the_seven_runtime_conditions() {
        let bits: Vec<u32> = SHADER_FLAGS.iter().map(|(bit, _)| *bit).collect();
        assert_eq!(bits, vec![1, 2, 4, 8, 16, 32, 64]);
    }

    /// Titles carry the filename and the legacy asterisk, and nothing else.
    #[test]
    fn the_title_marks_a_dirty_document() {
        let mut editor = editor_with("");
        assert_eq!(editor.title(), "test - Shader Editor");
        editor.dirty = true;
        assert_eq!(editor.title(), "test* - Shader Editor");
        assert_eq!(
            ShaderEditor::new().title(),
            "New file - Shader Editor",
            "an unnamed document uses the legacy placeholder name"
        );
    }

    /// Open trims the surrounding blank lines and save re-adds exactly one, so a
    /// file does not grow a newline per open/save round trip.
    #[test]
    fn open_save_round_trips_without_growing_trailing_newlines() {
        let directory = std::env::temp_dir().join("mge_gui_shader_round_trip");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("round_trip.fx");
        fs::write(&path, "technique main {}\n").unwrap();

        for _ in 0..3 {
            let mut editor = ShaderEditor::open_path(path.clone()).unwrap();
            assert_eq!(editor.source, "technique main {}");
            editor.save().unwrap();
            assert!(!editor.dirty);
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "technique main {}\n");
        fs::remove_dir_all(&directory).ok();
    }

    fn shader(name: &str, category: &str, adjust: i32) -> ShaderInfo {
        ShaderInfo {
            name: name.to_owned(),
            path: PathBuf::from(format!("{name}.fx")),
            category: category.to_owned(),
            priority: category_priority(category) + adjust,
        }
    }

    /// The catalog as the shipped `XEshaders` directory produces it.
    fn stock_catalog() -> ShaderCatalog {
        ShaderCatalog {
            shaders: vec![
                shader("Bloom Fine", "sensor", 0),
                shader("Bloom Soft", "sensor", 0),
                shader("Depth of Field", "lens", 0),
                shader("Eye Adaptation (HDR)", "tone", 0),
                shader("SSAO Fast", "scene", -10_000),
                shader("SSAO HQ", "scene", -10_000),
                shader("Sunshafts", "atmosphere", 0),
                shader("Underwater Effects", "scene", 0),
                shader("Underwater Interior Effects", "scene", 0),
            ],
        }
    }

    #[test]
    fn annotations_are_read_off_the_technique() {
        let source = "technique T0 < string MGEinterface = \"MGE XE 0\"; \
             string category = \"scene\"; int priorityAdjust = -10000; > { }";
        let (category, adjust) = parse_annotations(source);
        assert_eq!(category.as_deref(), Some("scene"));
        assert_eq!(adjust, -10_000);
    }

    #[test]
    fn unannotated_shaders_are_uncategorized() {
        let (category, adjust) = parse_annotations("technique T0 { pass p0 { } }");
        assert_eq!(category, None);
        assert_eq!(adjust, 0);
        assert_eq!(category_priority(UNCATEGORIZED), DEFAULT_PRIORITY);
    }

    #[test]
    fn sorted_insert_orders_by_category() {
        let catalog = stock_catalog();
        let mut active = Vec::new();
        for name in ["Eye Adaptation (HDR)", "Bloom Soft", "Sunshafts", "SSAO Fast"] {
            catalog.insert_sorted(&mut active, name);
        }
        assert_eq!(active, vec!["SSAO Fast", "Sunshafts", "Bloom Soft", "Eye Adaptation (HDR)"]);
    }

    #[test]
    fn sorted_insert_ignores_duplicates_and_unknown_names() {
        let catalog = stock_catalog();
        let mut active = vec!["Sunshafts".to_owned()];
        catalog.insert_sorted(&mut active, "Sunshafts");
        catalog.insert_sorted(&mut active, "Not A Shader");
        assert_eq!(active, vec!["Sunshafts"]);
    }

    #[test]
    fn presets_survive_the_sorted_insert_unchanged() {
        let catalog = stock_catalog();
        for (index, (_, names)) in PRESETS.iter().enumerate() {
            assert_eq!(catalog.preset_chain(index), *names, "preset {index}");
        }
    }

    #[test]
    fn effect_selections_round_trip_through_the_chain() {
        let catalog = stock_catalog();
        for (index, (_, _)) in PRESETS.iter().enumerate() {
            let chain = catalog.preset_chain(index);
            let selections: Vec<usize> = EFFECT_OPTIONS.iter().map(|effect| effect_selection(&chain, effect)).collect();
            assert_eq!(catalog.chain_from_effects(&selections), chain, "preset {index}");
            assert_eq!(matching_preset(&chain), index);
        }
    }

    #[test]
    fn an_unrecognised_chain_reads_as_custom() {
        assert_eq!(matching_preset(&["Sunshafts".to_owned()]), CUSTOM_PRESET);
        assert_eq!(preset_labels().len(), CUSTOM_PRESET + 1);
        assert_eq!(preset_labels()[CUSTOM_PRESET], "shaders.presets.custom");
    }
}
