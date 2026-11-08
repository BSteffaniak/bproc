//! Small direct-painting helpers and the shared colour theme. Everything here
//! is absolute-coordinate (`Rect`) based; the root `PaintCx` is at (0,0).
use super::hits::{Hit, HitMap};
use bmux_tui::{
    geometry::Rect,
    paint::{LocalRect, PaintCx},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    text_width::display_width,
};

/// Paint target handed to every explorer painter.
pub struct Frame<'a, 'b, 'c> {
    pub cx: &'a mut PaintCx<'b, 'c>,
    pub hits: &'a mut HitMap,
    pub now: std::time::Instant,
}

impl Frame<'_, '_, '_> {
    pub fn fill(&mut self, r: Rect, style: Style) {
        self.cx.fill(lr(r), " ", style);
    }
    /// Write one line at the top row of `r` (clipped to its width).
    pub fn line(&mut self, r: Rect, line: &Line) {
        self.cx
            .write_line(lr(Rect::new(r.x, r.y, r.width, r.height.min(1))), line);
    }
    /// Fill `r`'s first row with `style`, then write `line` over it.
    pub fn line_bg(&mut self, r: Rect, line: &Line, style: Style) {
        self.cx.write_line_with_fallback_style(
            lr(Rect::new(r.x, r.y, r.width, r.height.min(1))),
            line,
            style,
        );
    }
    pub fn text(&mut self, r: Rect, text: &str, style: Style) {
        self.line(
            r,
            &Line::from_spans(vec![Span::styled(text.to_string(), style)]),
        );
    }
    pub fn cell(&mut self, x: u16, y: u16, symbol: &str, style: Style) {
        self.cx.set_cell(i32::from(x), i64::from(y), symbol, style);
    }
    pub fn hit(&mut self, r: Rect, hit: Hit) {
        self.hits.push(r, hit);
    }
    /// Opaque rounded panel with an optional title; returns the inner area.
    pub fn panel(
        &mut self,
        r: Rect,
        title: Option<&str>,
        border: Style,
        background: Style,
    ) -> Rect {
        if r.width < 2 || r.height < 2 {
            self.fill(r, background);
            return Rect::new(r.x, r.y, 0, 0);
        }
        self.fill(r, background);
        let b = background.patch(border);
        let right = r.x + r.width - 1;
        let bottom = r.y + r.height - 1;
        self.cell(r.x, r.y, "╭", b);
        self.cell(right, r.y, "╮", b);
        self.cell(r.x, bottom, "╰", b);
        self.cell(right, bottom, "╯", b);
        for x in r.x + 1..right {
            self.cell(x, r.y, "─", b);
            self.cell(x, bottom, "─", b);
        }
        for y in r.y + 1..bottom {
            self.cell(r.x, y, "│", b);
            self.cell(right, y, "│", b);
        }
        if let Some(title) = title {
            let t = truncate(&format!(" {title} "), r.width.saturating_sub(4) as usize);
            self.text(
                Rect::new(r.x + 2, r.y, r.width.saturating_sub(4), 1),
                &t,
                b.add_modifier(Modifier::BOLD),
            );
        }
        Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2)
    }
}

pub const fn lr(r: Rect) -> LocalRect {
    LocalRect::new(r.x as i32, r.y as i64, r.width, r.height)
}

/// Truncate to `width` display cells, ending with `…` when cut.
pub fn truncate(s: &str, width: usize) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = display_width(ch.encode_utf8(&mut [0; 4]));
        if used + w > width - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Pad/truncate to exactly `width` cells.
pub fn fit(s: &str, width: usize, right: bool) -> String {
    let t = truncate(s, width);
    let pad = width.saturating_sub(display_width(&t));
    if right {
        format!("{}{}", " ".repeat(pad), t)
    } else {
        format!("{}{}", t, " ".repeat(pad))
    }
}

/// Centered rectangle of at most `w`×`h` inside `area`.
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// Shared palette. Terminal default background is kept for the main surface
/// so bproc respects the user's theme; panels use an explicit dark surface.
pub mod theme {
    use super::*;

    pub const ACCENT: Color = Color::Cyan;
    pub fn base() -> Style {
        Style::new().fg(Color::Default).bg(Color::Default)
    }
    pub fn dim() -> Style {
        Style::new().fg(Color::BrightBlack)
    }
    pub fn accent() -> Style {
        Style::new().fg(ACCENT)
    }
    pub fn bold() -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }
    pub fn header() -> Style {
        Style::new().fg(Color::Black).bg(Color::Cyan)
    }
    pub fn header_active() -> Style {
        Style::new()
            .fg(Color::Black)
            .bg(Color::BrightCyan)
            .add_modifier(Modifier::BOLD)
    }
    pub fn selected() -> Style {
        Style::new().fg(Color::Black).bg(Color::BrightCyan)
    }
    pub fn selected_unfocused() -> Style {
        Style::new()
            .fg(Color::BrightWhite)
            .bg(Color::Rgb(58, 62, 72))
    }
    pub fn hovered() -> Style {
        Style::new().bg(Color::Rgb(40, 44, 52))
    }
    pub fn marked() -> Style {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    }
    pub fn ghost() -> Style {
        Style::new()
            .fg(Color::BrightBlack)
            .add_modifier(Modifier::CROSSED_OUT)
    }
    pub fn alert() -> Style {
        Style::new()
            .fg(Color::BrightRed)
            .add_modifier(Modifier::BOLD)
    }
    pub fn panel_bg() -> Style {
        Style::new()
            .fg(Color::BrightWhite)
            .bg(Color::Rgb(24, 26, 32))
    }
    pub fn panel_border() -> Style {
        Style::new().fg(Color::Cyan)
    }
    pub fn key() -> Style {
        Style::new().fg(Color::Black).bg(Color::Cyan)
    }
    pub fn key_label() -> Style {
        Style::new().fg(Color::Default).bg(Color::Default)
    }
    pub fn button() -> Style {
        Style::new().fg(Color::Black).bg(Color::White)
    }
    pub fn button_primary() -> Style {
        Style::new()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }
    pub fn button_danger() -> Style {
        Style::new()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD)
    }
    pub fn chip_on() -> Style {
        Style::new().fg(Color::Black).bg(Color::Green)
    }
    pub fn chip_off() -> Style {
        Style::new().fg(Color::White).bg(Color::Rgb(50, 54, 62))
    }
    pub fn error() -> Style {
        Style::new().fg(Color::BrightRed)
    }
    /// Non-fatal advice (e.g. a search hint).
    pub fn warn() -> Style {
        Style::new().fg(Color::Yellow)
    }
    pub fn ok() -> Style {
        Style::new().fg(Color::Green)
    }

    /// Heat colour for a 0..=100 utilisation value.
    pub fn heat(percent: f64) -> Color {
        if percent >= 90.0 {
            Color::BrightRed
        } else if percent >= 60.0 {
            Color::Red
        } else if percent >= 30.0 {
            Color::Yellow
        } else if percent >= 5.0 {
            Color::Green
        } else {
            Color::Default
        }
    }

    pub fn download() -> Style {
        Style::new().fg(Color::Green)
    }
    pub fn upload() -> Style {
        Style::new().fg(Color::Yellow)
    }
}
