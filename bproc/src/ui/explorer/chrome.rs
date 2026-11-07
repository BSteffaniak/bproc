//! Screen chrome: meters header, toolbar (screen tabs, preset tabs, chips),
//! clickable F-key bar and status line.
use super::paint::Frame;
use super::{Click, DrawEnv, ExplorerState, Outcome};
use bmux_tui::geometry::Rect;

/// Rows the meters header needs at `width` (0 hides it).
pub fn header_height(state: &ExplorerState, width: u16) -> u16 {
    let _ = (state, width);
    2
}

pub fn draw_header(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &ExplorerState) {
    let _ = (frame, area, state);
}

pub fn draw_toolbar(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &ExplorerState) {
    let _ = (frame, area, state);
}

pub fn draw_fkeys(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &ExplorerState) {
    let _ = (frame, area, state);
}

pub fn draw_status(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    state: &ExplorerState,
    env: &DrawEnv<'_>,
) {
    let _ = (frame, area, state, env);
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let _ = (state, click);
    Outcome::Ignored
}
