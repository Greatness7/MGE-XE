//! The distant-land generation dialog: a native child window opened from the
//! Distant Land tab.
//!
//! On backends without multi-viewport support the same code renders as an
//! embedded `egui::Window`.

mod advanced;
mod dirs;
mod grass;
mod grass_scan;
mod landscape;
mod plugins;
mod statics;
mod widgets;

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use anyhow::{Result, bail};
use distantland::{
    DistantLandGpuMemoryEstimate, GenerationJob, OutputPaths, resolve_generation_job_paths, sync_plugins_from_load_order,
};
use eframe::egui::{
    Align, CentralPanel, Color32, Context, Layout, Panel, ProgressBar, RichText, ScrollArea, Ui, ViewportBuilder,
    ViewportCommand, ViewportId,
};
use rust_i18n::t;

use crate::{
    app::GuiApp,
    config,
    distant::DistantLandStatus,
    generate::{self, Message, Outcome},
    job,
    plugins::{PluginUniverse, SortMode},
    precheck::{self, ExistingTree},
    style,
    ui::tooltip,
};

use dirs::PluginDirsEditor;
use grass_scan::GrassScan;

use super::widgets::selectable_value;

/// The generator's tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenTab {
    Plugins,
    Grass,
    Landscape,
    Statics,
    Advanced,
}

pub(crate) enum Phase {
    /// An incompatible tree is already installed: explain what a run would
    /// replace and ask for consent before showing the settings at all.
    Consent { message: &'static str },
    /// The installed tree is from a newer format this build must not replace.
    Blocked { message: &'static str },
    /// Editing settings across the tabs; footer active.
    Editing,
    /// A run is in flight: controls gone, close vetoed (no cancellation).
    Generating {
        rx: Receiver<Message>,
        index: u32,
        label: String,
    },
    /// A run finished and left a valid tree.
    Finished {
        warnings: Vec<String>,
        gpu_memory: Option<DistantLandGpuMemoryEstimate>,
        /// Dedicated VRAM of the detected GPU, for the memory-pressure warning.
        /// `None` when no adapter could be enumerated (rare, e.g. some virtual
        /// machines); the warning then falls back to fixed byte thresholds.
        vram_bytes: Option<u64>,
    },
    /// A run failed; the window explains why and offers a way back to editing.
    Failed { message: String },
}

pub(crate) struct GeneratorState {
    /// The working copy the tabs edit, in persisted (relative-path) form. The run
    /// form (absolute `output_root`, pinned `Morrowind.ini`, `force_rebuild`) is
    /// derived at Generate time and never written back.
    pub(crate) job: GenerationJob,
    /// The plugin universe the Plugins tab edits. It is the live selection state,
    /// folded back into `job.plugins`/`job.data_dirs` by `commit_plugins` on the
    /// way to a save or a run. The job carries only the chosen subset, while the
    /// page needs every plugin on disk.
    pub(crate) universe: PluginUniverse,
    /// The `Data directories…` dialog's working copy, when open. Reachable from
    /// both the Plugins and Grass tabs, because the directories are one shared list.
    pub(crate) dirs_editor: Option<PluginDirsEditor>,
    /// Background groundcover classification of the whole universe, feeding the
    /// Grass tab's filter and the Plugins tab's annotations.
    pub(crate) grass_scan: GrassScan,
    /// The Grass tab's own view order. Separate from `universe.sort` because
    /// `apply_sort` reorders `entries` in place, so a shared field would make
    /// each tab silently reorder the other.
    pub(crate) grass_sort: SortMode,
    /// Selected row of the Statics tab's override-file list. Purely a view
    /// concern: the files themselves live in `job.settings.override_files`,
    /// and the index is re-bounds-checked on use rather than kept in step.
    pub(crate) override_selected: Option<usize>,
    /// Footer "Force rebuild": a run-only modifier, never persisted. Cleared from
    /// the saved file by `job::save`'s `finalize_for_persist`.
    pub(crate) force_rebuild: bool,
    /// Set when the user consented to replacing an incompatible live tree. That
    /// tree must not then serve as a generation cache, so the run, and only the
    /// run, is forced to rebuild.
    pub(crate) cache_source_incompatible: bool,
    /// The first native viewport frame is rendered while hidden on Windows, then
    /// the next frame reveals it. This mirrors the pending upstream eframe fix
    /// for white flashes when creating non-root viewports.
    pub(crate) viewport_ready: bool,
    pub(crate) tab: GenTab,
    pub(crate) phase: Phase,
    /// In-window error line (validation / save failure). Empty means none,
    /// shown in the footer so the message is not stranded on the parent window
    /// behind this one.
    pub(crate) error: String,
    /// Largest square texture the default adapter can create, queried once per
    /// window open. `None` when it could not be queried (rare). Caps the
    /// atlas/control-map size pickers on the Landscape, Statics, and Advanced
    /// tabs. See [`crate::platform::max_texture_dimension`].
    pub(crate) max_texture_dimension: Option<u32>,
}

impl GeneratorState {
    /// Open the generator seeded from the app's loaded job, in whichever phase
    /// the already-installed distant land calls for.
    ///
    /// Both the classification and the plugin scan happen here, once per open
    /// rather than once per frame. The scan is also where the job's saved
    /// selection is reconciled against what is actually installed.
    pub(crate) fn open(root: &Path, job: &GenerationJob) -> Self {
        let paths = OutputPaths::new(root.join(job::RUNTIME_OUTPUT_ROOT));
        let phase = match precheck::classify(&paths) {
            ExistingTree::Reuse => Phase::Editing,
            ExistingTree::PromptOldCorrupt => Phase::Consent {
                message: precheck::MSG_OLD_OR_CORRUPT,
            },
            ExistingTree::PromptDifferent => Phase::Consent {
                message: precheck::MSG_DIFFERENT_VERSION,
            },
            ExistingTree::RefuseNewer => Phase::Blocked {
                message: precheck::MSG_NEWER_FORMAT,
            },
        };

        let universe = PluginUniverse::load(root, job);
        // Kicked here rather than when the Grass tab is first shown: the Plugins
        // tab annotates rows from the same result, and it is the tab that opens.
        let mut grass_scan = GrassScan::default();
        grass_scan.start(universe.plugin_paths(), universe.data_dirs());

        Self {
            job: job.clone(),
            universe,
            dirs_editor: None,
            grass_scan,
            grass_sort: SortMode::Name,
            override_selected: None,
            force_rebuild: false,
            cache_source_incompatible: false,
            viewport_ready: false,
            tab: GenTab::Plugins,
            phase,
            error: String::new(),
            max_texture_dimension: crate::platform::max_texture_dimension(),
        }
    }

    /// Fold the live plugin selection back into the job. Called on the way to a
    /// save or a run, so the two always agree with what the page is showing.
    ///
    /// Under `auto_sync_plugins` the plugin list is then re-derived from the live
    /// `Morrowind.ini`, discarding the list `write_into` just produced. That keeps
    /// **one producer** of a synced list: `mgeHost64` calls the same helper with
    /// the same `data_dirs`, and the two must agree byte-for-byte or their cache
    /// fingerprints diverge and each launch regenerates over the other's output.
    /// Deriving it here rather than in `begin_generation` is also what keeps
    /// Save-and-close from persisting a stale list.
    ///
    /// `data_dirs` is read back off the job rather than recomputed, so the layers
    /// fed to the helper are exactly the ones just written to the file, the same
    /// value the host will load.
    ///
    /// # Errors
    ///
    /// Returns an error if the load order cannot be read. Save and Generate both
    /// abort: silently generating from an empty or stale selection is worse than
    /// refusing, and `seed_from_active`'s quiet fallback to an empty set is only
    /// appropriate for the editor's opening state.
    fn commit_plugins(&mut self, root: &Path) -> Result<()> {
        self.universe.write_into(&mut self.job);
        if self.job.auto_sync_plugins {
            let data_dirs = self.job.data_dirs.clone();
            sync_plugins_from_load_order(&mut self.job, &root.join("Morrowind.ini"), data_dirs.as_deref())?;
        }
        Ok(())
    }
}

/// The in-window error line for a failed load-order sync. The window stays open:
/// the fix is on disk (an unreadable `Morrowind.ini`), not in the settings.
fn plugin_sync_error(error: &anyhow::Error) -> String {
    t!("generator.messages.plugin_sync_failed", error = format!("{error:#}")).into_owned()
}

/// A footer/decision outcome bubbled out of the pure render helpers so the
/// `&mut self` actions (save, generate, close) run after the `GeneratorState`
/// borrow is released.
enum Act {
    Generate,
    Close,
    /// Return from the `Failed` state to editing.
    Back,
    /// Consent given to replace an incompatible live tree: proceed to editing,
    /// with the old tree barred from serving as a cache.
    AcceptIncompatible,
}

impl GuiApp {
    /// Called every frame from `show_dialogs`. Drains worker events, then renders
    /// the child window while it is open.
    pub(crate) fn show_generator(&mut self, ctx: &Context) {
        if self.ui.distant.generator.is_none() {
            return;
        }
        self.pump_generator(ctx);
        self.pump_grass_scan(ctx);
        let Some(generator) = self.ui.distant.generator.as_ref() else {
            return;
        };
        let viewport_ready = generator.viewport_ready;

        let mut builder = ViewportBuilder::default()
            .with_title(t!("generator.title"))
            .with_inner_size([580.0, 500.0])
            .with_min_inner_size([520.0, 420.0])
            .with_clamp_size_to_monitor_size(true)
            .with_visible(viewport_ready);
        if let Some(icon) = crate::load_icon() {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(ViewportId::from_hash_of("mge_distant_generator"), builder, |ui, _class| {
            self.generator_body(ui)
        });

        if let Some(generator) = self.ui.distant.generator.as_mut()
            && !generator.viewport_ready
        {
            generator.viewport_ready = true;
            ctx.request_repaint();
        }
    }

    /// Open the generator from the Distant Land tab's button. No-op if already
    /// open (the button is disabled meanwhile, but guard anyway).
    pub(crate) fn open_generator(&mut self) {
        if self.ui.distant.generator.is_some() {
            return;
        }
        self.ui.distant.generator = Some(GeneratorState::open(self.store.root(), &self.job));
    }

    /// Fold in a finished groundcover scan. Unlike [`Self::pump_generator`] this
    /// runs in every phase. The scan is kicked as the window opens, which may be
    /// while the precheck consent prompt is still up.
    fn pump_grass_scan(&mut self, ctx: &Context) {
        let Some(generator) = self.ui.distant.generator.as_mut() else {
            return;
        };
        if generator.grass_scan.poll() {
            // Nothing else drives a repaint while the user is only reading, so
            // without this the annotations would wait for the next input event.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Drain the worker channel and drive terminal transitions. Kept separate from
    /// rendering so the `&mut self` reconciliation runs without holding a borrow of
    /// the generator state.
    fn pump_generator(&mut self, ctx: &Context) {
        let mut result: Option<Outcome> = None;
        {
            let Some(generator) = self.ui.distant.generator.as_mut() else {
                return;
            };
            let Phase::Generating { rx, index, label } = &mut generator.phase else {
                return;
            };
            loop {
                match rx.try_recv() {
                    Ok(Message::Progress(stage)) => {
                        let (stage_index, key) = generate::stage_progress(stage);
                        *index = stage_index;
                        *label = t!(key).into_owned();
                    }
                    Ok(Message::Done(outcome)) => {
                        result = Some(outcome);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        result = Some(Outcome::Failure {
                            message: t!("generator.messages.worker_stopped").into_owned(),
                        });
                        break;
                    }
                }
            }
        }

        match result {
            // Still running: keep the loop alive so progress advances without input.
            None => ctx.request_repaint_after(Duration::from_millis(100)),
            Some(Outcome::Success { warnings, gpu_memory }) => {
                self.reconcile_after_generation();
                if let Some(generator) = self.ui.distant.generator.as_mut() {
                    generator.error.clear();
                    generator.phase = Phase::Finished {
                        warnings,
                        gpu_memory,
                        vram_bytes: crate::platform::gpu_dedicated_video_memory_bytes(),
                    };
                }
                self.set_success(t!("generator.messages.generated").into_owned());
            }
            Some(Outcome::OutputInUse) => {
                if let Some(generator) = self.ui.distant.generator.as_mut() {
                    generator.error.clear();
                    generator.phase = Phase::Failed {
                        message: t!("generator.messages.output_in_use").into_owned(),
                    };
                }
            }
            Some(Outcome::Failure { message }) => {
                if let Some(generator) = self.ui.distant.generator.as_mut() {
                    generator.error.clear();
                    generator.phase = Phase::Failed { message };
                }
            }
        }
    }

    /// Re-read the generated set and reconcile the Distant Land tab: a run that
    /// reports success but leaves the set incomplete must not leave the tick
    /// behind, and a successful run activates distant statics.
    ///
    /// The tree's baked-in minimum static size only *bounds* the tier minimums
    /// from below via `normalize_distances`; it must not be adopted as the Far
    /// minimum outright, or it stomps any higher user setting.
    fn reconcile_after_generation(&mut self) {
        self.ui.distant.status = DistantLandStatus::inspect(self.store.root());
        self.settings.mge.distant_land.enabled = self.ui.distant.status.complete;
        if self.ui.distant.status.min_static_size.is_some() {
            self.settings.mge.distant_land.statics = true;
        }
        config::normalize_distances(&mut self.settings.mge, self.ui.distant.status.min_static_size);
    }

    fn generator_body(&mut self, ui: &mut Ui) {
        let Some(mut generator) = self.ui.distant.generator.take() else {
            return;
        };

        // Close vetoing: refuse `WM_CLOSE` mid-run, and say why instead of
        // silently ignoring the click. Otherwise let the window close.
        let close_requested = ui.ctx().input(|i| i.viewport().close_requested());
        if close_requested {
            if matches!(generator.phase, Phase::Generating { .. }) {
                ui.ctx().send_viewport_cmd(ViewportCommand::CancelClose);
            } else {
                return; // dropped: the window closes
            }
        }

        let act = match &mut generator.phase {
            Phase::Consent { message } => render_consent(ui, message),
            Phase::Blocked { message } => render_blocked(ui, message),
            Phase::Editing => render_editing(ui, &mut generator),
            Phase::Generating { index, label, .. } => {
                render_generating(ui, *index, label);
                None
            }
            Phase::Finished {
                warnings,
                gpu_memory,
                vram_bytes,
            } => render_finished(ui, warnings, *gpu_memory, *vram_bytes),
            Phase::Failed { message } => render_failed(ui, message),
        };

        match act {
            None => self.ui.distant.generator = Some(generator),
            Some(Act::Back) => {
                generator.phase = Phase::Editing;
                generator.error.clear();
                self.ui.distant.generator = Some(generator);
            }
            Some(Act::AcceptIncompatible) => {
                generator.cache_source_incompatible = true;
                generator.phase = Phase::Editing;
                self.ui.distant.generator = Some(generator);
            }
            Some(Act::Close) => {} // dropped: the window closes next frame
            Some(Act::Generate) => {
                let root = self.store.root().to_owned();
                if let Err(error) = generator.commit_plugins(&root) {
                    generator.error = plugin_sync_error(&error);
                } else {
                    self.begin_generation(&mut generator);
                }
                self.ui.distant.generator = Some(generator);
            }
        }
    }

    fn persist_generator_job(&mut self, generation_job: &GenerationJob) -> Result<Option<String>> {
        if self.job_writes_disabled {
            bail!("{}", t!("generator.messages.job_save_blocked_invalid"));
        }
        let document = job::serialize_for_persist(generation_job)?;
        self.store.save_generation_job(&document)?;
        self.job = generation_job.clone();
        Ok(job::remove_legacy(self.store.root())
            .err()
            .map(|error| t!("generator.messages.legacy_cleanup_failed", error = format!("{error:#}")).into_owned()))
    }

    /// Save the settings, derive the run job, and spawn the worker. The
    /// persisted file keeps relative paths and no `force_rebuild`, while the
    /// in-memory run job gets the absolute output root, the discovered
    /// `Morrowind.ini`, and the footer's `force_rebuild`.
    fn begin_generation(&mut self, generator: &mut GeneratorState) {
        generator.error.clear();
        let root = self.store.root().to_owned();

        // Persist first; a save failure aborts the run.
        match self.persist_generator_job(&generator.job) {
            Ok(Some(warning)) => self.set_warning(warning),
            Ok(None) => {}
            Err(error) => {
                generator.error = t!("generator.messages.settings_save_blocked", error = format!("{error:#}")).into_owned();
                return;
            }
        }

        // Pin generation to the install the GUI is editing so the VFS resolves the
        // same load order and data dirs, and generate straight into the live
        // `Data Files` root (publication is guarded by the writer lock).
        let mut run = generator.job.clone();
        let ini = root.join("Morrowind.ini");
        if ini.is_file() {
            run.morrowind_ini = Some(ini);
        }
        run.output_root = Some(root.join(job::RUNTIME_OUTPUT_ROOT));
        resolve_generation_job_paths(&mut run, &root);
        // The footer checkbox, plus the precheck's verdict: an incompatible live
        // tree the user agreed to replace must not also be read back as a cache.
        // Both apply to the run job only, because `finalize_for_persist` already
        // stripped `force_rebuild` from the file, so startup generation never
        // inherits either.
        run.settings.force_rebuild = generator.force_rebuild || generator.cache_source_incompatible;

        if let Err(error) = run.validate_for_generation() {
            generator.error = t!("generator.messages.invalid_settings", error = format!("{error:#}")).into_owned();
            return;
        }

        let rx = generate::spawn(run);
        generator.phase = Phase::Generating {
            rx,
            index: 0,
            label: t!("generator.stages.starting").into_owned(),
        };
    }
}

/// The footer used by every phase whose controls are a single right-aligned
/// button row. `render_editing` builds its own because it also carries
/// left-aligned controls and the error line.
///
/// The `ui.horizontal` wrapper is load-bearing: a bare
/// `right_to_left(Align::Center)` layout placed straight into the panel expands
/// to the panel's full height, and a bottom panel sized from its own content
/// then feeds that height back in, so the footer grows every frame. Wrapping in
/// a horizontal row makes the height content-driven and therefore stable.
fn button_footer(ui: &mut Ui, buttons: impl FnOnce(&mut Ui)) {
    Panel::bottom("gen_footer").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), buttons);
        });
        ui.add_space(4.0);
    });
}

/// Opening state for an incompatible live tree: what is installed, what a run
/// would do to it, and the choice. Nothing has been touched at this point, and
/// declining closes the window without touching anything either.
fn render_consent(ui: &mut Ui, message: &str) -> Option<Act> {
    let mut act = None;
    button_footer(ui, |ui| {
        if ui.button(t!("common.actions.cancel")).clicked() {
            act = Some(Act::Close);
        }
        if ui.button(t!("common.actions.continue")).clicked() {
            act = Some(Act::AcceptIncompatible);
        }
    });
    CentralPanel::default().show(ui, |ui| {
        ui.add_space(12.0);
        ui.heading(t!("generator.precheck.found_title"));
        ui.add_space(8.0);
        ui.colored_label(style::WARN, t!(message));
    });
    act
}

/// The installed tree is newer than this build understands; the window stays up to say why.
fn render_blocked(ui: &mut Ui, message: &str) -> Option<Act> {
    let mut act = None;
    button_footer(ui, |ui| {
        if ui.button(t!("common.actions.close")).clicked() {
            act = Some(Act::Close);
        }
    });
    CentralPanel::default().show(ui, |ui| {
        ui.add_space(12.0);
        ui.heading(t!("generator.precheck.blocked_title"));
        ui.add_space(8.0);
        ui.colored_label(style::BAD, t!(message));
    });
    act
}

fn render_editing(ui: &mut Ui, generator: &mut GeneratorState) -> Option<Act> {
    let mut act = None;

    Panel::bottom("gen_footer").show_separator_line(true).show(ui, |ui| {
        ui.add_space(4.0);
        if !generator.error.is_empty() {
            ui.colored_label(style::BAD, generator.error.as_str());
        }
        ui.horizontal(|ui| {
            tooltip(
                ui.checkbox(&mut generator.force_rebuild, t!("generator.actions.force_rebuild")),
                t!("generator.actions.force_rebuild_tip"),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t!("common.actions.cancel")).clicked() {
                    act = Some(Act::Close);
                }
                if ui.button(t!("generator.actions.save_generate")).clicked() {
                    act = Some(Act::Generate);
                }
            });
        });
        ui.add_space(4.0);
    });

    CentralPanel::default().show(ui, |ui| {
        ui.horizontal(|ui| {
            selectable_value(ui, &mut generator.tab, GenTab::Plugins, t!("generator.tabs.plugins"));
            selectable_value(ui, &mut generator.tab, GenTab::Grass, t!("generator.tabs.grass"));
            selectable_value(ui, &mut generator.tab, GenTab::Landscape, t!("generator.tabs.landscape"));
            selectable_value(ui, &mut generator.tab, GenTab::Statics, t!("generator.tabs.statics"));
            selectable_value(ui, &mut generator.tab, GenTab::Advanced, t!("generator.tabs.advanced"));
        });
        ui.separator();
        // Each page owns its own scrolling: Plugins fills the body height and
        // scrolls only its list, Landscape is short enough to need none, and
        // Statics and Advanced are whole-page scrollers. A shared outer
        // `ScrollArea` would make `available_height` meaningless for the first
        // kind.
        match generator.tab {
            GenTab::Plugins => plugins::page(ui, generator),
            GenTab::Grass => grass::page(ui, generator),
            GenTab::Landscape => landscape::page(ui, generator),
            GenTab::Statics => statics::page(ui, generator),
            GenTab::Advanced => advanced::page(ui, generator),
        }
    });

    act
}

fn render_generating(ui: &mut Ui, index: u32, label: &str) {
    CentralPanel::default().show(ui, |ui| {
        ui.add_space(12.0);
        ui.heading(t!("generator.generating.title"));
        ui.add_space(8.0);
        // "stage N started" reads as that fraction complete; the bar reaches full
        // on the last stage and is pinned there on success.
        let fraction = (index as f32 + 1.0) / generate::NUM_STAGES as f32;
        ui.add(ProgressBar::new(fraction.min(1.0)).text(label));
        ui.add_space(8.0);
        ui.label(t!("generator.generating.hint"));
    });
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Fallback thresholds for when no GPU could be detected (rare, such as some
/// virtual machines). Otherwise at risk of running low once Morrowind's own resources,
/// MGE's render targets, and driver overhead are added on top of the always-resident
/// part of the generated set; picked to roughly match a 4 GB / 6 GB card, the low end
/// still in use.
const HIGH_MEMORY_BYTES: u64 = 5 * GIB / 2;
const VERY_HIGH_MEMORY_BYTES: u64 = 4 * GIB;

/// How much the always-resident part of the generated set is likely to strain
/// the detected GPU. The thresholds are GUI policy: `distantland` reports bytes
/// and takes no view on hardware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryPressure {
    Normal,
    High,
    VeryHigh,
}

impl MemoryPressure {
    /// Fraction of detected VRAM the always-resident part of the generated set
    /// should stay under, leaving room for Morrowind itself, MGE's render
    /// targets, and driver overhead.
    const HIGH_VRAM_FRACTION: f64 = 0.5;
    const VERY_HIGH_VRAM_FRACTION: f64 = 0.75;

    /// Bytes that stay in VRAM for the whole session.
    ///
    /// Static geometry is excluded: the runtime streams merged statics in and
    /// out under its own VRAM cap, so that row is elastic and cannot cause the
    /// shortfall this warns about. The ordinary (unmerged) statics inside it do
    /// stay resident, but they are a small remainder — 107 MiB against 3.15 GiB
    /// of merged geometry on the largest set measured — far below the
    /// resolution of these thresholds, and `distantland` does not report the
    /// split here.
    fn resident_bytes(estimate: DistantLandGpuMemoryEstimate) -> u64 {
        estimate
            .static_texture_bytes
            .saturating_add(estimate.terrain_geometry_bytes)
            .saturating_add(estimate.terrain_texture_bytes)
    }

    fn classify(estimate: DistantLandGpuMemoryEstimate, vram_bytes: Option<u64>) -> Self {
        let (high, very_high) = match vram_bytes {
            Some(vram) if vram > 0 => (
                (vram as f64 * Self::HIGH_VRAM_FRACTION) as u64,
                (vram as f64 * Self::VERY_HIGH_VRAM_FRACTION) as u64,
            ),
            _ => (HIGH_MEMORY_BYTES, VERY_HIGH_MEMORY_BYTES),
        };
        let resident_bytes = Self::resident_bytes(estimate);
        if resident_bytes >= very_high {
            Self::VeryHigh
        } else if resident_bytes >= high {
            Self::High
        } else {
            Self::Normal
        }
    }

    /// Colour and localized warning line, or `None` to show none. Cites the
    /// detected VRAM size when known, otherwise falls back to generic wording.
    fn warning(self, vram_bytes: Option<u64>) -> Option<(Color32, String)> {
        let (color, known_key, generic_key) = match self {
            Self::Normal => return None,
            Self::High => (
                style::WARN,
                "generator.finished.memory_high",
                "generator.finished.memory_high_generic",
            ),
            Self::VeryHigh => (
                style::BAD,
                "generator.finished.memory_very_high",
                "generator.finished.memory_very_high_generic",
            ),
        };
        let message = match vram_bytes {
            Some(vram) if vram > 0 => t!(known_key, vram = format_binary_size(vram)).into_owned(),
            _ => t!(generic_key).into_owned(),
        };
        Some((color, message))
    }
}

/// Binary units, matching DXVK's own reporting so the two can be compared directly.
/// Sub-MiB values round up rather than to `0 MiB`, which would read as "nothing".
fn format_binary_size(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes == 0 {
        "0 MiB".to_string()
    } else {
        format!("{} MiB", bytes.div_ceil(MIB))
    }
}

/// One label/value row, value flush against the right edge.
fn memory_row(ui: &mut Ui, label: RichText, value: RichText) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(value);
        });
    });
}

fn render_memory_report(ui: &mut Ui, gpu_memory: Option<DistantLandGpuMemoryEstimate>, vram_bytes: Option<u64>) {
    let Some(estimate) = gpu_memory else {
        style::hint(ui, t!("generator.finished.memory_unavailable"));
        return;
    };

    ui.group(|ui| {
        memory_row(
            ui,
            RichText::new(t!("generator.finished.memory_title")).strong(),
            RichText::new(format_binary_size(estimate.total_bytes())).strong(),
        );
        ui.add_space(4.0);
        for (key, bytes) in [
            ("generator.finished.memory_static_geometry", estimate.static_geometry_bytes),
            ("generator.finished.memory_static_textures", estimate.static_texture_bytes),
            ("generator.finished.memory_terrain_geometry", estimate.terrain_geometry_bytes),
            ("generator.finished.memory_terrain_textures", estimate.terrain_texture_bytes),
        ] {
            memory_row(ui, RichText::new(t!(key)), RichText::new(format_binary_size(bytes)));
        }
    });

    if let Some((color, message)) = MemoryPressure::classify(estimate, vram_bytes).warning(vram_bytes) {
        ui.add_space(8.0);
        ui.colored_label(color, message);
    }
    ui.add_space(4.0);
    style::hint(ui, t!("generator.finished.memory_note"));
}

fn render_finished(
    ui: &mut Ui,
    warnings: &[String],
    gpu_memory: Option<DistantLandGpuMemoryEstimate>,
    vram_bytes: Option<u64>,
) -> Option<Act> {
    let mut act = None;
    button_footer(ui, |ui| {
        if ui.button(t!("common.actions.close")).clicked() {
            act = Some(Act::Close);
        }
    });
    CentralPanel::default().show(ui, |ui| {
        ui.add_space(12.0);
        ui.heading(t!("generator.finished.title"));
        ui.add_space(8.0);
        ui.label(t!("generator.finished.message"));
        ui.add_space(8.0);
        // One scroll area for everything below the message. The memory card is a fixed
        // few rows but the warning list is unbounded, and nesting two vertical scroll
        // areas makes both awkward to drive.
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            render_memory_report(ui, gpu_memory, vram_bytes);
            if !warnings.is_empty() {
                ui.add_space(8.0);
                ui.label(t!("generator.finished.warnings", count = warnings.len()));
                for warning in warnings {
                    ui.colored_label(style::WARN, warning);
                }
            }
        });
    });
    act
}

fn render_failed(ui: &mut Ui, message: &str) -> Option<Act> {
    let mut act = None;
    button_footer(ui, |ui| {
        if ui.button(t!("common.actions.close")).clicked() {
            act = Some(Act::Close);
        }
        if ui.button(t!("generator.actions.back_to_settings")).clicked() {
            act = Some(Act::Back);
        }
    });
    CentralPanel::default().show(ui, |ui| {
        ui.add_space(12.0);
        ui.heading(t!("generator.failed.title"));
        ui.add_space(8.0);
        ScrollArea::vertical().show(ui, |ui| {
            ui.colored_label(style::BAD, message);
        });
    });
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_sizes_switch_units_at_one_gibibyte() {
        assert_eq!(format_binary_size(0), "0 MiB");
        assert_eq!(format_binary_size(1), "1 MiB");
        assert_eq!(format_binary_size(MIB), "1 MiB");
        assert_eq!(format_binary_size(MIB + 1), "2 MiB");
        assert_eq!(format_binary_size(231 * MIB), "231 MiB");
        assert_eq!(format_binary_size(GIB - 1), "1024 MiB");
        assert_eq!(format_binary_size(GIB), "1.00 GiB");
        assert_eq!(format_binary_size(GIB + GIB / 4), "1.25 GiB");
        assert_eq!(format_binary_size(u64::MAX), "17179869184.00 GiB");
    }

    /// An estimate whose always-resident part is exactly `resident`, with a
    /// large streamed static-geometry row that must not affect the outcome.
    fn resident(resident: u64) -> DistantLandGpuMemoryEstimate {
        DistantLandGpuMemoryEstimate {
            static_geometry_bytes: 8 * GIB,
            static_texture_bytes: resident,
            terrain_geometry_bytes: 0,
            terrain_texture_bytes: 0,
        }
    }

    #[test]
    fn memory_pressure_falls_back_to_fixed_bytes_without_a_detected_gpu() {
        assert_eq!(MemoryPressure::classify(resident(0), None), MemoryPressure::Normal);
        assert_eq!(
            MemoryPressure::classify(resident(HIGH_MEMORY_BYTES - 1), None),
            MemoryPressure::Normal
        );
        assert_eq!(
            MemoryPressure::classify(resident(HIGH_MEMORY_BYTES), None),
            MemoryPressure::High
        );
        assert_eq!(
            MemoryPressure::classify(resident(VERY_HIGH_MEMORY_BYTES - 1), None),
            MemoryPressure::High
        );
        assert_eq!(
            MemoryPressure::classify(resident(VERY_HIGH_MEMORY_BYTES), None),
            MemoryPressure::VeryHigh
        );
        assert_eq!(MemoryPressure::classify(resident(u64::MAX), None), MemoryPressure::VeryHigh);
        // A GPU report of exactly zero bytes is treated the same as "unknown"
        // rather than making every generated set look infinitely oversized.
        assert_eq!(MemoryPressure::classify(resident(1), Some(0)), MemoryPressure::Normal);
    }

    #[test]
    fn memory_pressure_thresholds_scale_with_detected_vram() {
        let vram = 8 * GIB;
        assert_eq!(MemoryPressure::classify(resident(0), Some(vram)), MemoryPressure::Normal);
        assert_eq!(
            MemoryPressure::classify(resident(vram / 2 - 1), Some(vram)),
            MemoryPressure::Normal
        );
        assert_eq!(MemoryPressure::classify(resident(vram / 2), Some(vram)), MemoryPressure::High);
        assert_eq!(
            MemoryPressure::classify(resident(vram * 3 / 4 - 1), Some(vram)),
            MemoryPressure::High
        );
        assert_eq!(
            MemoryPressure::classify(resident(vram * 3 / 4), Some(vram)),
            MemoryPressure::VeryHigh
        );
    }

    /// Streamed static geometry is the largest row in a big generated set and
    /// must not decide the warning on its own.
    #[test]
    fn static_geometry_alone_never_raises_the_pressure() {
        let estimate = DistantLandGpuMemoryEstimate {
            static_geometry_bytes: u64::MAX,
            ..Default::default()
        };
        assert_eq!(MemoryPressure::classify(estimate, Some(4 * GIB)), MemoryPressure::Normal);
        assert_eq!(MemoryPressure::classify(estimate, None), MemoryPressure::Normal);
    }

    #[test]
    fn only_the_pressured_classifications_show_a_warning() {
        assert!(MemoryPressure::Normal.warning(Some(6 * GIB)).is_none());
        let (color, message) = MemoryPressure::High.warning(Some(6 * GIB)).unwrap();
        assert_eq!(color, style::WARN);
        assert!(message.contains("6.00 GiB"));
        let (color, message) = MemoryPressure::VeryHigh.warning(None).unwrap();
        assert_eq!(color, style::BAD);
        assert!(!message.is_empty());
    }

    /// A concrete worked example, and the regression this classification
    /// exists to prevent: a 4.33 GiB set is 108% of a 4 GiB card and used to
    /// raise the red warning there, but 89% of it is static geometry the
    /// runtime streams. Only 726 MiB has to stay resident, which that card
    /// takes comfortably. A 1 GiB card is where the resident part starts to
    /// crowd out the game.
    #[test]
    fn a_four_gibibyte_set_is_judged_by_the_part_that_stays_resident() {
        let estimate = DistantLandGpuMemoryEstimate {
            static_geometry_bytes: 3_886_192_000,
            static_texture_bytes: 242_221_056,
            terrain_geometry_bytes: 456_130_560,
            terrain_texture_bytes: 62_914_560,
        };

        assert_eq!(format_binary_size(estimate.total_bytes()), "4.33 GiB");
        assert_eq!(format_binary_size(MemoryPressure::resident_bytes(estimate)), "726 MiB");
        assert_eq!(MemoryPressure::classify(estimate, Some(6 * GIB)), MemoryPressure::Normal);
        assert_eq!(MemoryPressure::classify(estimate, Some(4 * GIB)), MemoryPressure::Normal);
        assert_eq!(MemoryPressure::classify(estimate, Some(GIB)), MemoryPressure::High);
    }
}
