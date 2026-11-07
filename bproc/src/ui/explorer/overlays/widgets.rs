//! Shared overlay building blocks: backdrop, buttons, hit resolution.
use super::super::ExplorerState;
use super::super::hits::Hit;
use super::super::paint::{Frame, lr, theme, truncate};
use bmux_tui::geometry::{Point, Rect};
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text_width::display_width;

/// Capture clicks over the whole screen and dim what is underneath.
pub fn backdrop(frame: &mut Frame<'_, '_, '_>, area: Rect, index: usize) {
    frame
        .cx
        .restyle(lr(area), Style::new().fg(Color::BrightBlack));
    frame.hit(area, Hit::OverlayBackdrop { overlay: index });
}

/// What a pointer position means for overlay `index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Item(usize),
    Control(u16),
    Body,
    /// The backdrop, another overlay, or anything not yet covered by this
    /// overlay's committed hit regions.
    Outside,
}

pub fn resolve(state: &ExplorerState, index: usize, position: Point) -> Target {
    match state.hits.at(position) {
        Some(Hit::OverlayItem { overlay, item }) if *overlay == index => Target::Item(*item),
        Some(Hit::OverlayControl { overlay, control }) if *overlay == index => {
            Target::Control(*control)
        }
        Some(Hit::OverlayBody { overlay }) if *overlay == index => Target::Body,
        _ => Target::Outside,
    }
}

/// A dialog button: label, base style and control id.
pub struct Button<'a> {
    pub label: &'a str,
    pub style: Style,
    pub control: u16,
}

/// Brighter variant of a button style for hover feedback.
pub fn hovered(style: Style) -> Style {
    let bg = match style.bg {
        Some(Color::White) => Color::BrightWhite,
        Some(Color::Cyan) => Color::BrightCyan,
        Some(Color::Red) => Color::BrightRed,
        Some(other) => other,
        None => Color::BrightBlack,
    };
    style.bg(bg)
}

/// Paint `buttons` right-aligned on row `y` of `inner`, pushing a control hit
/// for each. The focused button (Enter target) is underlined.
pub fn buttons(
    frame: &mut Frame<'_, '_, '_>,
    inner: Rect,
    y: u16,
    index: usize,
    buttons: &[Button<'_>],
    focused: Option<u16>,
    hover: Option<u16>,
) {
    if inner.width == 0 || inner.height == 0 || y < inner.y || y >= inner.y + inner.height {
        return;
    }
    let labels: Vec<String> = buttons.iter().map(|b| format!(" {} ", b.label)).collect();
    let total: usize = labels.iter().map(|l| display_width(l)).sum::<usize>()
        + 2 * buttons.len().saturating_sub(1);
    let right = inner.x.saturating_add(inner.width);
    let mut x = right
        .saturating_sub(1)
        .saturating_sub(total.min(u16::MAX as usize) as u16)
        .max(inner.x);
    for (button, label) in buttons.iter().zip(&labels) {
        let room = right.saturating_sub(x) as usize;
        if room == 0 {
            break;
        }
        let text = truncate(label, room);
        let w = display_width(&text) as u16;
        let mut style = button.style;
        if hover == Some(button.control) {
            style = hovered(style);
        }
        if focused == Some(button.control) {
            style = style.add_modifier(Modifier::UNDERLINE | Modifier::BOLD);
        }
        let r = Rect::new(x, y, w, 1);
        frame.text(r, &text, style);
        frame.hit(
            r,
            Hit::OverlayControl {
                overlay: index,
                control: button.control,
            },
        );
        x = x.saturating_add(w).saturating_add(2);
    }
}

/// `[x] label` / `[ ] label`.
pub fn checkbox(checked: bool) -> &'static str {
    if checked { "[x]" } else { "[ ]" }
}

/// Bottom row of the panel interior, if any.
pub fn last_row(inner: Rect) -> Option<u16> {
    (inner.height > 0).then(|| inner.y + inner.height - 1)
}

/// Body style used inside panels.
pub fn body() -> Style {
    theme::panel_bg()
}

/// Dim text on the panel surface.
pub fn muted() -> Style {
    theme::panel_bg().patch(theme::dim())
}

/// Scroll offset that keeps `cursor` within a window of `visible` rows.
pub fn visible_scroll(stored: usize, cursor: Option<usize>, len: usize, visible: usize) -> usize {
    if visible == 0 {
        return 0;
    }
    let mut scroll = stored.min(len.saturating_sub(visible));
    if let Some(c) = cursor {
        if c < scroll {
            scroll = c;
        } else if c >= scroll + visible {
            scroll = c + 1 - visible;
        }
    }
    scroll
}

/// Greedy word wrap to `width` cells and at most `max_lines` lines; the last
/// line ends with `…` when text was cut.
pub fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut cut = false;
    'words: for word in text.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let need = if current.is_empty() {
                display_width(&word)
            } else {
                display_width(&current) + 1 + display_width(&word)
            };
            if need <= width {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(&word);
                break;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                if lines.len() == max_lines {
                    cut = true;
                    break 'words;
                }
                continue;
            }
            // A single word longer than the line: hard-split it.
            let mut head = String::new();
            let mut used = 0;
            let mut rest = String::new();
            for ch in word.chars() {
                let w = display_width(ch.encode_utf8(&mut [0; 4]));
                if rest.is_empty() && used + w <= width {
                    head.push(ch);
                    used += w;
                } else {
                    rest.push(ch);
                }
            }
            if head.is_empty() {
                // Wider than the line itself (e.g. a wide glyph in 1 cell).
                break;
            }
            lines.push(head);
            if lines.len() == max_lines {
                cut = !rest.is_empty();
                break 'words;
            }
            if rest.is_empty() {
                break;
            }
            word = rest;
        }
    }
    if !current.is_empty() {
        if lines.len() < max_lines {
            lines.push(current);
        } else {
            cut = true;
        }
    }
    if cut && let Some(last) = lines.last_mut() {
        let base = truncate(last, width.saturating_sub(1));
        let base = base.trim_end_matches('…').to_string();
        *last = format!("{base}…");
        if display_width(last) > width {
            *last = truncate(last, width);
        }
    }
    lines
}
