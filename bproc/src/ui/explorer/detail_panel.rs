//! Process inspector, docked (bottom/right) or full screen. Tabs: Overview
//! (sparklines), Threads, Files, Network, Environment, Memory, Tree, Limits,
//! Events. Action buttons and ancestor breadcrumbs are clickable.
use super::paint::Frame;
use super::{Click, Drag, ExplorerState, Outcome};
use crate::procs::ProcessKey;
use crate::procs::detail::DeepDetail;
use bmux_keyboard::KeyStroke;
use bmux_tui::event::MouseEvent;
use bmux_tui::geometry::Rect;

#[derive(Debug, Default)]
pub struct DetailPanel {
    key: Option<ProcessKey>,
    full: bool,
    detail: Option<DeepDetail>,
}

impl DetailPanel {
    pub fn open(&mut self, key: ProcessKey, full: bool) {
        if self.key != Some(key) {
            self.detail = None;
        }
        self.key = Some(key);
        self.full = full;
    }
    pub fn close(&mut self) {
        self.key = None;
        self.full = false;
        self.detail = None;
    }
    pub fn key(&self) -> Option<ProcessKey> {
        self.key
    }
    pub fn is_open(&self) -> bool {
        self.key.is_some()
    }
    pub fn is_full(&self) -> bool {
        self.key.is_some() && self.full
    }
    pub fn is_docked(&self) -> bool {
        self.key.is_some() && !self.full
    }
    pub fn set_full(&mut self, full: bool) {
        self.full = full;
    }
    /// Accept a collected detail if it is for the open process.
    pub fn apply(&mut self, detail: DeepDetail) {
        if Some(detail.key) == self.key {
            self.detail = Some(detail);
        }
    }
    pub fn detail(&self) -> Option<&DeepDetail> {
        self.detail.as_ref()
    }
}

pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
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

pub fn handle_drag(state: &mut ExplorerState, drag: &Drag, mouse: &MouseEvent) -> Outcome {
    let _ = (state, drag, mouse);
    Outcome::Ignored
}
