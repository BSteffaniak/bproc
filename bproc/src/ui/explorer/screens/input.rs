//! Small input helpers shared by the screens, help and settings overlays:
//! key classification and a single-line text field.
use super::super::paint::Frame;
use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::geometry::Rect;
use bmux_tui::style::{Modifier, Style};
use bmux_tui::text::{Line, Span};
use bmux_tui::text_width::display_width;

/// Printable character for a key press, uppercased when Shift is held.
pub fn typed_char(key: &KeyStroke) -> Option<char> {
    let m = key.modifiers;
    if m.ctrl || m.alt || m.super_key || m.meta || m.hyper {
        return None;
    }
    match key.key {
        KeyCode::Char(c) if m.shift && c.is_ascii_lowercase() => Some(c.to_ascii_uppercase()),
        KeyCode::Char(c) if !c.is_control() => Some(c),
        KeyCode::Space => Some(' '),
        _ => None,
    }
}

/// True for a press of `code` without Ctrl/Alt/Super/Meta (Shift allowed).
pub fn is(key: &KeyStroke, code: KeyCode) -> bool {
    let m = key.modifiers;
    key.key == code && !m.ctrl && !m.alt && !m.super_key && !m.meta
}

pub fn is_space(key: &KeyStroke) -> bool {
    is(key, KeyCode::Space) || is(key, KeyCode::Char(' '))
}

/// Single-line editable text with a character cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    pub text: String,
    /// Cursor position in characters.
    pub cursor: usize,
}

impl TextInput {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    fn byte_index(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Insert text at the cursor; control characters become spaces.
    pub fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            let ch = if ch.is_control() { ' ' } else { ch };
            let at = self.byte_index(self.cursor);
            self.text.insert(at, ch);
            self.cursor += 1;
        }
    }

    /// Apply an editing key. Returns true when the key was an editing key.
    pub fn handle_key(&mut self, key: &KeyStroke) -> bool {
        let len = self.text.chars().count();
        if key.modifiers.ctrl && !key.modifiers.alt {
            match key.key {
                KeyCode::Char('u') => *self = Self::default(),
                KeyCode::Char('a') => self.cursor = 0,
                KeyCode::Char('e') => self.cursor = len,
                KeyCode::Char('w') | KeyCode::Backspace => self.delete_word(),
                _ => return false,
            }
            return true;
        }
        if let Some(ch) = typed_char(key) {
            self.insert_str(ch.encode_utf8(&mut [0; 4]));
            return true;
        }
        match key.key {
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte_index(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte_index(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Backspace | KeyCode::Delete => {}
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            _ => return false,
        }
        true
    }

    fn delete_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor.min(chars.len());
        while start > 0 && chars[start - 1] == ' ' {
            start -= 1;
        }
        while start > 0 && chars[start - 1] != ' ' {
            start -= 1;
        }
        let from = self.byte_index(start);
        let to = self.byte_index(self.cursor);
        self.text.replace_range(from..to, "");
        self.cursor = start;
    }

    /// Paint the field into the first row of `r`, scrolled so the cursor stays
    /// visible. Shows `placeholder` (dim) when empty and unfocused.
    pub fn draw(
        &self,
        frame: &mut Frame<'_, '_, '_>,
        r: Rect,
        style: Style,
        focused: bool,
        placeholder: &str,
    ) {
        if r.width == 0 || r.height == 0 {
            return;
        }
        let width = r.width as usize;
        if self.text.is_empty() && !focused {
            let line = Line::from_spans(vec![Span::styled(
                placeholder.to_string(),
                style.patch(Style::new().add_modifier(Modifier::DIM)),
            )]);
            frame.line_bg(r, &line, style);
            return;
        }
        let chars: Vec<char> = self.text.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let cw = |c: char| display_width(c.encode_utf8(&mut [0; 4])).max(1);
        // Leave one cell for the cursor block at the end of the text.
        let mut start = 0;
        let mut used: usize = chars[..cursor].iter().map(|c| cw(*c)).sum::<usize>() + 1;
        while used > width && start < cursor {
            used -= cw(chars[start]);
            start += 1;
        }
        let mut spans = Vec::new();
        let mut before = String::new();
        let mut taken = 0;
        for (i, &c) in chars.iter().enumerate().skip(start) {
            let w = cw(c);
            if taken + w > width {
                break;
            }
            if focused && i == cursor {
                spans.push(Span::styled(std::mem::take(&mut before), style));
                spans.push(Span::styled(
                    c.to_string(),
                    style.add_modifier(Modifier::REVERSED),
                ));
            } else {
                before.push(c);
            }
            taken += w;
        }
        spans.push(Span::styled(before, style));
        if focused && cursor == chars.len() && taken < width {
            spans.push(Span::styled(" ", style.add_modifier(Modifier::REVERSED)));
        }
        frame.line_bg(r, &Line::from_spans(spans), style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bmux_keyboard::Modifiers;

    fn k(code: KeyCode) -> KeyStroke {
        KeyStroke::simple(code)
    }

    #[test]
    fn typed_char_normalises_shifted_letters() {
        let shifted = KeyStroke::with_modifiers(
            KeyCode::Char('a'),
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
        );
        assert_eq!(typed_char(&shifted), Some('A'));
        assert_eq!(typed_char(&k(KeyCode::Space)), Some(' '));
        assert_eq!(typed_char(&k(KeyCode::Enter)), None);
        let ctrl = KeyStroke::with_modifiers(
            KeyCode::Char('a'),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        );
        assert_eq!(typed_char(&ctrl), None);
    }

    #[test]
    fn editing_keys_move_and_delete_by_character() {
        let mut t = TextInput::new("héllo");
        assert_eq!(t.cursor, 5);
        assert!(t.handle_key(&k(KeyCode::Left)));
        assert!(t.handle_key(&k(KeyCode::Backspace)));
        assert_eq!(t.text, "hélo");
        assert!(t.handle_key(&k(KeyCode::Home)));
        assert!(t.handle_key(&k(KeyCode::Delete)));
        assert_eq!(t.text, "élo");
        assert!(t.handle_key(&k(KeyCode::Char('X'))));
        assert_eq!(t.text, "Xélo");
        assert!(t.handle_key(&k(KeyCode::End)));
        assert!(t.handle_key(&k(KeyCode::Right)));
        assert_eq!(t.cursor, 4);
        assert!(!t.handle_key(&k(KeyCode::Up)));
        t.handle_key(&k(KeyCode::Home));
        assert!(t.handle_key(&k(KeyCode::Backspace)));
        assert_eq!(t.text, "Xélo");
    }

    #[test]
    fn ctrl_shortcuts_clear_and_delete_words() {
        let ctrl = |c| {
            KeyStroke::with_modifiers(
                KeyCode::Char(c),
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                },
            )
        };
        let mut t = TextInput::new("foo bar  ");
        assert!(t.handle_key(&ctrl('w')));
        assert_eq!(t.text, "foo ");
        assert!(t.handle_key(&ctrl('u')));
        assert!(t.text.is_empty());
        t.insert_str("a\nb");
        assert_eq!(t.text, "a b");
    }
}
