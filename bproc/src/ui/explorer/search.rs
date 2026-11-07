//! Search / filter box in the toolbar. Typing filters the table live using
//! the `procs::filter` language; parse errors are shown inline.
use super::paint::Frame;
use super::{Click, ExplorerState, Outcome};
use crate::procs::filter::Filter;
use bmux_keyboard::KeyStroke;
use bmux_tui::geometry::Rect;

#[derive(Debug, Default)]
pub struct SearchBar {
    pub text: String,
    pub focused: bool,
    /// Cursor position in chars.
    pub cursor: usize,
    pub error: Option<String>,
    filter: Option<Filter>,
}

impl SearchBar {
    pub fn filter(&self) -> Option<&Filter> {
        self.filter.as_ref()
    }
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.chars().count();
        match Filter::parse(text) {
            Ok(f) => {
                self.filter = (!f.is_empty()).then_some(f);
                self.error = None;
            }
            Err(e) => self.error = Some(e.message),
        }
    }
    pub fn clear(&mut self) {
        self.set_text("");
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
    if click.is_left() {
        state.search.focused = true;
    }
    Outcome::Consumed
}

pub fn paste(state: &mut ExplorerState, text: &str) -> Outcome {
    let mut t = state.search.text.clone();
    t.push_str(text);
    state.search.set_text(&t);
    state.table.mark_dirty();
    Outcome::Consumed
}
