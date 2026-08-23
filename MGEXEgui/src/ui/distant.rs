//! Distant Land UI: the settings tab and the two dialog windows (`weather`,
//! `lighting`) which share the layout vocabulary in `dialog_layout`.

mod dialog_layout;
mod lighting;
mod page;
mod weather;

use crate::{distant::DistantLandStatus, ui::generate::GeneratorState};

pub(crate) struct DistantUiState {
    /// Written back after every generation run (`ui/generate.rs`), read by the
    /// settings tab.
    pub(crate) status: DistantLandStatus,
    /// The weather fog/wind window, when open. `None` while closed.
    weather: Option<weather::WeatherEditorState>,
    /// The per-pixel lighting window, when open. `None` while closed.
    lighting: Option<lighting::LightingEditorState>,
    /// The distant-land generation window, when open. `None` while closed.
    /// Owned by `ui/generate.rs`.
    pub(crate) generator: Option<GeneratorState>,
}

impl DistantUiState {
    pub(crate) fn new(status: DistantLandStatus) -> Self {
        Self {
            status,
            weather: None,
            lighting: None,
            generator: None,
        }
    }
}
