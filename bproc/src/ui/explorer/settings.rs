//! Settings overlay (F2): refresh rate, meters, dock, behaviour toggles and
//! per-preset columns. Changes apply live and persist via `SaveSettings`.
use super::ExplorerState;
use super::overlays::OverlayOutcome;
use super::paint::Frame;
use crate::config::ExplorerSettings;
use bmux_tui::event::Event;
use bmux_tui::geometry::Rect;

#[derive(Debug)]
pub struct SettingsState {
    pub selected: usize,
    pub scroll: usize,
}

impl SettingsState {
    pub fn new(settings: &ExplorerSettings) -> Self {
        let _ = settings;
        Self {
            selected: 0,
            scroll: 0,
        }
    }
}

pub fn draw(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    index: usize,
    settings: &SettingsState,
    state: &ExplorerState,
) {
    let _ = (frame, area, index, settings, state);
}

pub fn handle(
    settings: &mut SettingsState,
    index: usize,
    event: &Event,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    let _ = (settings, index, event, state);
    OverlayOutcome::Close
}
