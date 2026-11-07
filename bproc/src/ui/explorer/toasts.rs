//! Non-modal notifications stacked in the top-right of the main area.
//! Toasts auto-expire; clicking dismisses; an optional action button runs a
//! queued `UiRequest` (e.g. "Force kill" after a SIGTERM).
use super::paint::Frame;
use super::{Click, ExplorerState, Outcome, UiRequest};
use bmux_tui::geometry::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub severity: Severity,
    pub text: String,
    pub action: Option<(String, UiRequest)>,
    pub created: std::time::Instant,
}

#[derive(Debug, Default)]
pub struct Toasts {
    pub items: Vec<Toast>,
    next_id: u64,
}

impl Toasts {
    pub fn push(&mut self, severity: Severity, text: impl Into<String>) {
        self.add(severity, text.into(), None);
    }
    pub fn push_action(
        &mut self,
        severity: Severity,
        text: impl Into<String>,
        label: &str,
        request: UiRequest,
    ) {
        self.add(severity, text.into(), Some((label.to_string(), request)));
    }
    fn add(&mut self, severity: Severity, text: String, action: Option<(String, UiRequest)>) {
        self.next_id += 1;
        self.items.push(Toast {
            id: self.next_id,
            severity,
            text,
            action,
            created: std::time::Instant::now(),
        });
    }
}

pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    let _ = (frame, area, state);
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let _ = (state, click);
    Outcome::Ignored
}
