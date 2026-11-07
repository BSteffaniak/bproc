//! Styled line model for panel bodies: segments that may carry hit regions,
//! painted with clipping and scrolling.
use crate::ui::explorer::hits::Hit;
use crate::ui::explorer::paint::Frame;
use bmux_tui::geometry::Rect;
use bmux_tui::style::Style;
use bmux_tui::text::{Line, Span};
use bmux_tui::text_width::display_width;

/// A styled run of text; `hit` covers exactly the painted cells.
#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    pub text: String,
    pub style: Style,
    pub hit: Option<Hit>,
}

impl Seg {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            hit: None,
        }
    }
    pub fn link(text: impl Into<String>, style: Style, hit: Hit) -> Self {
        Self {
            text: text.into(),
            style,
            hit: Some(hit),
        }
    }
    pub fn width(&self) -> usize {
        display_width(&self.text)
    }
}

/// One body row; `hit` spans the full row width under any segment hits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BLine {
    pub segs: Vec<Seg>,
    pub hit: Option<Hit>,
}

impl BLine {
    pub fn new(segs: Vec<Seg>) -> Self {
        Self { segs, hit: None }
    }
    pub fn blank() -> Self {
        Self::default()
    }
    pub fn plain(text: impl Into<String>, style: Style) -> Self {
        Self::new(vec![Seg::new(text, style)])
    }
    pub fn with_hit(mut self, hit: Hit) -> Self {
        self.hit = Some(hit);
        self
    }
    pub fn text(&self) -> String {
        self.segs.iter().map(|s| s.text.as_str()).collect()
    }
}

/// Join adjacent hitless segments that share a style.
pub fn merge(segs: Vec<Seg>) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::with_capacity(segs.len());
    for seg in segs {
        match out.last_mut() {
            Some(last) if last.style == seg.style && last.hit.is_none() && seg.hit.is_none() => {
                last.text.push_str(&seg.text);
            }
            _ => out.push(seg),
        }
    }
    out
}

/// Paint `line` into the first row of `r`, pushing its hit regions.
pub fn paint_line(frame: &mut Frame<'_, '_, '_>, r: Rect, line: &BLine) {
    if r.width == 0 || r.height == 0 {
        return;
    }
    let row = Rect::new(r.x, r.y, r.width, 1);
    let spans: Vec<Span> = line
        .segs
        .iter()
        .map(|s| Span::styled(s.text.clone(), s.style))
        .collect();
    frame.line(row, &Line::from_spans(spans));
    if let Some(hit) = &line.hit {
        frame.hit(row, hit.clone());
    }
    let mut x = 0usize;
    let limit = usize::from(r.width);
    for seg in &line.segs {
        if x >= limit {
            break;
        }
        let w = seg.width().min(limit - x);
        if let Some(hit) = &seg.hit {
            frame.hit(
                Rect::new(r.x.saturating_add(x as u16), r.y, w as u16, 1),
                hit.clone(),
            );
        }
        x += seg.width();
    }
}

/// Paint `lines` starting at `scroll` into `r`, one per row.
pub fn paint_lines(frame: &mut Frame<'_, '_, '_>, r: Rect, lines: &[BLine], scroll: usize) {
    for (i, line) in lines
        .iter()
        .skip(scroll)
        .take(usize::from(r.height))
        .enumerate()
    {
        paint_line(frame, Rect::new(r.x, r.y + i as u16, r.width, 1), line);
    }
}

/// Word-wrap to `width` cells. Words longer than a line (paths, long flags)
/// break after a separator such as `/` when one fits, else mid-word.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for word in text.split(' ') {
        let ww = display_width(word);
        let sep = usize::from(!current.is_empty());
        if used + sep + ww <= width {
            if sep == 1 {
                current.push(' ');
            }
            current.push_str(word);
            used += sep + ww;
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        let mut rest = word;
        while display_width(rest) > width {
            let cut = break_point(rest, width);
            out.push(rest[..cut].to_string());
            rest = &rest[cut..];
        }
        current = rest.to_string();
        used = display_width(rest);
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

/// Byte index to split an over-long `word` at so the head fits `width`
/// cells: just after the last separator in the head, else at the width.
/// Always at least one character.
fn break_point(word: &str, width: usize) -> usize {
    let mut limit = 0usize;
    let mut used = 0usize;
    for (i, ch) in word.char_indices() {
        let cw = display_width(ch.encode_utf8(&mut [0; 4]));
        if used + cw > width && i > 0 {
            break;
        }
        used += cw;
        limit = i + ch.len_utf8();
    }
    const SEPARATORS: [char; 6] = ['/', '\\', ':', ',', '=', '-'];
    word[..limit]
        .char_indices()
        .rev()
        .find(|(i, c)| SEPARATORS.contains(c) && *i > 0)
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::explorer::paint::theme;

    #[test]
    fn wrap_prefers_word_boundaries() {
        assert_eq!(
            wrap("cargo run --release", 10),
            vec!["cargo run", "--release"]
        );
        assert_eq!(wrap("/very/long/path", 6), vec!["/very/", "long/", "path"]);
        // Paths break after a slash, never mid-name, when a slash fits.
        assert_eq!(
            wrap("/Volumes/ehdd/target/release/bclean", 16),
            vec!["/Volumes/ehdd/", "target/release/", "bclean"]
        );
        // No separator: split at the width.
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        // A short word after a long one continues on its line.
        assert_eq!(wrap("/aa/bb/cc x", 6), vec!["/aa/", "bb/cc", "x"]);
        assert_eq!(wrap("", 5), vec![""]);
        assert!(wrap("abc", 0).is_empty());
        assert_eq!(wrap("界面界面", 4), vec!["界面", "界面"]);
    }

    #[test]
    fn merge_keeps_hits_separate() {
        let a = Seg::new("a", theme::dim());
        let b = Seg::new("b", theme::dim());
        let link = Seg::link("c", theme::dim(), Hit::DetailBody);
        let merged = merge(vec![a, b, link]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "ab");
    }
}
