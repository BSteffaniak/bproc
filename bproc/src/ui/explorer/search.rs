//! Search / filter box in the toolbar. Typing filters the table live using
//! the `procs::filter` language; parse errors are shown inline.
use super::hits::Hit;
use super::paint::{Frame, theme, truncate};
use super::{Click, ExplorerState, Outcome};
use crate::procs::filter::Filter;
use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::event::{MouseButton, MouseEventKind};
use bmux_tui::geometry::{Point, Rect};
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text::{Line, Span};
use bmux_tui::text_width::display_width;

#[cfg(test)]
mod tests;

pub const PLACEHOLDER: &str = "Search  /   e.g. node  cpu>5  user:me";
const ICON: &str = "⌕ ";
const CLEAR: &str = "✕";

#[derive(Debug, Default)]
pub struct SearchBar {
    pub text: String,
    pub focused: bool,
    /// Cursor position in chars.
    pub cursor: usize,
    pub error: Option<String>,
    filter: Option<Filter>,
    /// First char shown when the text is wider than the box.
    offset: usize,
    /// Screen column where the text starts and its width (last frame).
    text_x: u16,
    text_w: u16,
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
    fn len(&self) -> usize {
        self.text.chars().count()
    }
    /// Byte index of char position `pos`.
    fn byte_at(&self, pos: usize) -> usize {
        self.text
            .char_indices()
            .nth(pos)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }
    /// Replace the text keeping the cursor at `cursor` (in chars).
    fn edit(&mut self, text: String, cursor: usize) {
        self.set_text(&text);
        self.cursor = cursor.min(self.len());
    }
    /// Insert `s` at the cursor.
    pub fn insert(&mut self, s: &str) {
        let at = self.byte_at(self.cursor);
        let mut t = self.text.clone();
        t.insert_str(at, s);
        let cursor = self.cursor + s.chars().count();
        self.edit(t, cursor);
    }
    /// Delete the char before the cursor; false when nothing changed.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let (a, b) = (self.byte_at(self.cursor - 1), self.byte_at(self.cursor));
        let mut t = self.text.clone();
        t.replace_range(a..b, "");
        let cursor = self.cursor - 1;
        self.edit(t, cursor);
        true
    }
    /// Delete the char under the cursor; false when nothing changed.
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.len() {
            return false;
        }
        let (a, b) = (self.byte_at(self.cursor), self.byte_at(self.cursor + 1));
        let mut t = self.text.clone();
        t.replace_range(a..b, "");
        let cursor = self.cursor;
        self.edit(t, cursor);
        true
    }
    /// Delete the word before the cursor (Ctrl+W); false when nothing changed.
    pub fn delete_word(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let (a, b) = (self.byte_at(start), self.byte_at(self.cursor));
        let mut t = self.text.clone();
        t.replace_range(a..b, "");
        self.edit(t, start);
        true
    }
    /// Char position for a click at screen column `x` (last drawn layout).
    pub fn cursor_for_column(&self, x: u16) -> usize {
        if x < self.text_x {
            return self.len();
        }
        let col = usize::from(x - self.text_x);
        let mut used = 0;
        for (i, ch) in self.text.chars().enumerate().skip(self.offset) {
            let w = display_width(ch.encode_utf8(&mut [0; 4]));
            if used + w > col {
                return if col - used > w / 2 && w > 1 {
                    i + 1
                } else {
                    i
                };
            }
            used += w;
        }
        self.len()
    }
    /// Keep the cursor inside a text area of `width` cells.
    fn scroll_to_cursor(&mut self, width: usize) {
        let chars: Vec<char> = self.text.chars().collect();
        self.cursor = self.cursor.min(chars.len());
        let cw = |c: char| display_width(c.encode_utf8(&mut [0; 4]));
        let total: usize = chars.iter().map(|&c| cw(c)).sum();
        if total < width {
            self.offset = 0;
            return;
        }
        self.offset = self.offset.min(self.cursor);
        // The cursor needs one free cell after the text before it.
        while self.offset < self.cursor
            && chars[self.offset..self.cursor]
                .iter()
                .map(|&c| cw(c))
                .sum::<usize>()
                + 1
                > width
        {
            self.offset += 1;
        }
    }
}

/// Apply a text edit: refilter the table from the top.
fn changed(state: &mut ExplorerState) {
    state.table.mark_dirty();
    state.table.scroll_to_top();
}

fn field_style(focused: bool) -> Style {
    if focused {
        theme::panel_bg()
    } else {
        theme::base()
    }
}

pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    if area.width == 0 || area.height == 0 {
        state.search.text_w = 0;
        return;
    }
    let area = Rect::new(area.x, area.y, area.width, 1);
    let focused = state.search.focused;
    let bg = field_style(focused);
    frame.fill(area, bg);
    frame.hit(area, Hit::SearchBox);

    let right = area.x + area.width;
    let icon_style = if focused {
        bg.patch(theme::accent()).add_modifier(Modifier::BOLD)
    } else {
        bg.patch(theme::dim())
    };
    let icon_w = (display_width(ICON) as u16).min(area.width);
    frame.text(Rect::new(area.x, area.y, icon_w, 1), ICON, icon_style);
    let text_x = area.x + icon_w;
    let mut text_end = right;

    let has_text = !state.search.text.is_empty();
    let mut clear: Option<Rect> = None;
    if has_text && area.width >= icon_w + 5 {
        let x = right - 3;
        clear = Some(Rect::new(x, area.y, 3, 1));
        text_end = x;
    }
    if has_text {
        let note = match &state.search.error {
            Some(e) => Some((e.clone(), bg.patch(theme::error()))),
            None => {
                let shown = state.table.rows().iter().filter(|r| !r.ghost).count();
                let total = state.store.len_live();
                Some((format!("{shown}/{total}"), bg.patch(theme::dim())))
            }
        };
        if let Some((note, style)) = note {
            let room = usize::from(text_end.saturating_sub(text_x));
            // The note may take at most half of the room, leaving space to type.
            let max = room.saturating_sub(8).min(room / 2);
            if max >= 3 {
                let note = truncate(&note, max);
                let w = display_width(&note) as u16;
                let x = text_end - w;
                frame.text(Rect::new(x, area.y, w, 1), &note, style);
                text_end = x.saturating_sub(1);
            }
        }
    }

    let text_w = text_end.saturating_sub(text_x);
    state.search.text_x = text_x;
    state.search.text_w = text_w;
    let text_rect = Rect::new(text_x, area.y, text_w, 1);
    if !has_text {
        state.search.offset = 0;
        if !focused || text_w > 0 {
            let hint = truncate(PLACEHOLDER, usize::from(text_w));
            frame.text(text_rect, &hint, bg.patch(theme::dim()));
        }
    } else {
        state.search.scroll_to_cursor(usize::from(text_w));
        let style = if state.search.error.is_some() {
            bg.patch(theme::error())
        } else {
            bg.patch(Style::new().fg(if focused {
                Color::BrightWhite
            } else {
                Color::Default
            }))
        };
        let visible: String = state
            .search
            .text
            .chars()
            .skip(state.search.offset)
            .collect();
        let shown = truncate_exact(&visible, usize::from(text_w));
        frame.line(
            text_rect,
            &Line::from_spans(vec![Span::styled(shown, style)]),
        );
    }
    if let Some(r) = clear {
        let style = if focused {
            bg.patch(theme::accent())
        } else {
            bg.patch(theme::dim())
        };
        frame.text(r, &format!(" {CLEAR} "), style);
        frame.hit(r, Hit::SearchClear);
    }
    if focused && text_w > 0 {
        let before: String = state
            .search
            .text
            .chars()
            .skip(state.search.offset)
            .take(state.search.cursor.saturating_sub(state.search.offset))
            .collect();
        let x = text_x
            .saturating_add(display_width(&before) as u16)
            .min(text_x + text_w - 1);
        frame.cx.set_cursor(Point::new(x, area.y), true);
    }
}

/// Clip to `width` cells without an ellipsis (the cursor scrolls the view).
fn truncate_exact(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = display_width(ch.encode_utf8(&mut [0; 4]));
        if used + w > width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out
}

pub fn handle_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    let m = key.modifiers;
    let search = &mut state.search;
    let edited = match key.key {
        KeyCode::Char('u') if m.ctrl => {
            let had = !search.text.is_empty();
            search.clear();
            had
        }
        KeyCode::Char('w') if m.ctrl => search.delete_word(),
        KeyCode::Backspace if m.alt || m.ctrl => search.delete_word(),
        KeyCode::Char('a') if m.ctrl => {
            search.cursor = 0;
            false
        }
        KeyCode::Char('e') if m.ctrl => {
            search.cursor = search.len();
            false
        }
        KeyCode::Char(_) if m.ctrl || m.alt => return Outcome::Ignored,
        KeyCode::Char(c) => {
            search.insert(c.encode_utf8(&mut [0; 4]));
            true
        }
        KeyCode::Space => {
            search.insert(" ");
            true
        }
        KeyCode::Backspace => search.backspace(),
        KeyCode::Delete => search.delete(),
        KeyCode::Left => {
            search.cursor = search.cursor.saturating_sub(1);
            false
        }
        KeyCode::Right => {
            search.cursor = (search.cursor + 1).min(search.len());
            false
        }
        KeyCode::Home => {
            search.cursor = 0;
            false
        }
        KeyCode::End => {
            search.cursor = search.len();
            false
        }
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
            let plain = KeyStroke::simple(key.key);
            super::table::handle_key(state, &plain);
            return Outcome::Consumed;
        }
        KeyCode::Enter => {
            search.focused = false;
            state.table.refresh(
                &state.store,
                &state.settings,
                &state.search,
                state.traffic,
                state.uid,
                false,
            );
            state.table.select_index(0);
            return Outcome::Consumed;
        }
        KeyCode::Escape => {
            search.focused = false;
            if !search.text.is_empty() {
                search.clear();
                changed(state);
            }
            return Outcome::Consumed;
        }
        KeyCode::Tab => {
            search.focused = false;
            return Outcome::Consumed;
        }
        _ => return Outcome::Ignored,
    };
    if edited {
        changed(state);
    }
    Outcome::Consumed
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    match click.mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if *click.hit == Hit::SearchClear {
                if !state.search.text.is_empty() {
                    state.search.clear();
                    changed(state);
                }
            } else {
                state.search.cursor = state.search.cursor_for_column(click.mouse.position.x);
            }
            state.search.focused = true;
            Outcome::Consumed
        }
        MouseEventKind::Down(_) => {
            state.search.focused = true;
            Outcome::Consumed
        }
        _ => Outcome::Consumed,
    }
}

pub fn paste(state: &mut ExplorerState, text: &str) -> Outcome {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.trim_end_matches(' ');
    if clean.is_empty() {
        return Outcome::Consumed;
    }
    state.search.insert(clean);
    changed(state);
    Outcome::Consumed
}
