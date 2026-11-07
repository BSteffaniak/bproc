//! Help overlay generated from the action registry; every row is clickable
//! and runs its action.
use super::ExplorerState;
use super::overlays::OverlayOutcome;
use super::paint::Frame;
use bmux_tui::event::Event;
use bmux_tui::geometry::Rect;

#[derive(Debug, Default)]
pub struct HelpState {
    pub scroll: usize,
}

pub fn draw(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    index: usize,
    help: &HelpState,
    state: &ExplorerState,
) {
    let _ = (frame, area, index, help, state);
}

pub fn handle(
    help: &mut HelpState,
    index: usize,
    event: &Event,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    let _ = (help, index, event, state);
    OverlayOutcome::Close
}
