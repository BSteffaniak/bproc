//! Events timeline and Watches screens, plus the watch-rule editor overlay.
use super::overlays::{Overlay, OverlayOutcome};
use super::paint::Frame;
use super::{Click, ExplorerState, Outcome};
use crate::procs::ProcessKey;
use bmux_keyboard::KeyStroke;
use bmux_tui::event::Event;
use bmux_tui::geometry::Rect;

#[derive(Debug, Default)]
pub struct EventsView {}

#[derive(Debug, Default)]
pub struct WatchesView {}

#[derive(Debug)]
pub struct WatchEditor {}

pub fn draw_events(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    let _ = (frame, area, state);
}

pub fn draw_watches(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    let _ = (frame, area, state);
}

pub fn handle_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    let _ = (state, key);
    Outcome::Ignored
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let _ = (state, click);
    Outcome::Ignored
}

/// Editor pre-filled to watch `key`.
pub fn watch_editor_for(state: &ExplorerState, key: ProcessKey) -> Overlay {
    let _ = (state, key);
    Overlay::WatchEditor(WatchEditor {})
}

pub fn draw_editor(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    index: usize,
    editor: &WatchEditor,
    state: &ExplorerState,
) {
    let _ = (frame, area, index, editor, state);
}

pub fn handle_editor(
    editor: &mut WatchEditor,
    index: usize,
    event: &Event,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    let _ = (editor, index, event, state);
    OverlayOutcome::Close
}
