//! Non-modal notifications stacked in the top-right of the main area.
//! Toasts auto-expire; clicking dismisses; an optional action button runs a
//! queued `UiRequest` (e.g. "Force kill" after a SIGTERM).
use super::hits::Hit;
use super::overlays::widgets::wrap;
use super::paint::{Frame, theme, truncate};
use super::{Click, ExplorerState, Outcome, UiRequest};
use bmux_tui::geometry::Rect;
use bmux_tui::style::{Color, Style};
use bmux_tui::text_width::display_width;
use std::time::{Duration, Instant};

/// Newest toasts drawn at once; older ones collapse into "+N more".
pub const MAX_VISIBLE: usize = 4;
/// `Hit::Toast` id of the "+N more · Clear all" line (real ids start at 1).
pub const CLEAR_ALL: u64 = 0;
/// Time left on a toast is topped up to this while the pointer rests on it.
const HOVER_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

impl Severity {
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Info => "ℹ",
            Self::Success => "✓",
            Self::Warning => "⚠",
            Self::Error => "✗",
        }
    }
    pub const fn color(self) -> Color {
        match self {
            Self::Info => Color::Cyan,
            Self::Success => Color::Green,
            Self::Warning => Color::Yellow,
            Self::Error => Color::BrightRed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub severity: Severity,
    pub text: String,
    pub action: Option<(String, UiRequest)>,
    pub created: std::time::Instant,
}

impl Toast {
    /// How long the toast stays up.
    pub fn ttl(&self) -> Duration {
        if self.action.is_some() {
            return Duration::from_secs(12);
        }
        Duration::from_secs(match self.severity {
            Severity::Info | Severity::Success => 4,
            Severity::Warning => 8,
            Severity::Error => 10,
        })
    }
    pub fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created) >= self.ttl()
    }
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
    /// Remove toast `id`, returning it.
    pub fn dismiss(&mut self, id: u64) -> Option<Toast> {
        let i = self.items.iter().position(|t| t.id == id)?;
        Some(self.items.remove(i))
    }
    pub fn clear(&mut self) {
        self.items.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    /// Drop expired toasts. The `hovered` toast never has less than
    /// `HOVER_GRACE` left, so it cannot vanish under the pointer.
    pub fn prune(&mut self, now: Instant, hovered: Option<u64>) {
        for toast in &mut self.items {
            if Some(toast.id) == hovered {
                let keep = toast.ttl().saturating_sub(HOVER_GRACE);
                if now.saturating_duration_since(toast.created) > keep {
                    toast.created = now.checked_sub(keep).unwrap_or(now);
                }
            }
        }
        self.items.retain(|t| !t.expired(now));
    }
}

/// Toast id under the pointer according to the committed hit map.
fn hovered(state: &ExplorerState) -> Option<u64> {
    let position = state.pointer.position?;
    match state.hits.at(position) {
        Some(Hit::Toast { id } | Hit::ToastAction { id }) => Some(*id),
        _ => None,
    }
}

pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    let hovered = hovered(state);
    state.toasts.prune(frame.now, hovered);
    if state.toasts.is_empty() || area.width < 8 || area.height < 3 {
        return;
    }
    let width = 56.min(area.width - 2);
    let x = area.x + area.width - width - 1;
    let bottom = area.y + area.height;
    // Border + padding + icon column on the left, padding + border right.
    let text_w = width.saturating_sub(6) as usize;
    let mut y = area.y;
    let mut shown = 0;
    for toast in state.toasts.items.iter().rev().take(MAX_VISIBLE) {
        let lines = wrap(&toast.text, text_w, 2);
        let lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        let h = lines.len() as u16 + 2 + u16::from(toast.action.is_some());
        if y.saturating_add(h) > bottom {
            break;
        }
        draw_toast(
            frame,
            Rect::new(x, y, width, h),
            toast,
            &lines,
            hovered == Some(toast.id),
        );
        y += h;
        shown += 1;
    }
    let hidden = state.toasts.items.len() - shown;
    if hidden > 0 && y < bottom {
        let label = truncate(&format!(" +{hidden} more · Clear all "), width as usize);
        let w = display_width(&label) as u16;
        let r = Rect::new(x + width - w, y, w, 1);
        let style = if hovered == Some(CLEAR_ALL) {
            theme::panel_bg().patch(theme::hovered())
        } else {
            theme::panel_bg().patch(theme::dim())
        };
        frame.text(r, &label, style);
        frame.hit(r, Hit::Toast { id: CLEAR_ALL });
    }
}

fn draw_toast(
    frame: &mut Frame<'_, '_, '_>,
    r: Rect,
    toast: &Toast,
    lines: &[String],
    hovered: bool,
) {
    let color = toast.severity.color();
    let background = if hovered {
        theme::panel_bg().bg(Color::Rgb(34, 37, 45))
    } else {
        theme::panel_bg()
    };
    let inner = frame.panel(r, None, Style::new().fg(color), background);
    frame.hit(r, Hit::Toast { id: toast.id });
    if inner.width < 3 {
        return;
    }
    frame.text(
        Rect::new(inner.x + 1, inner.y, 1, 1),
        toast.severity.icon(),
        background.fg(color).patch(theme::bold()),
    );
    for (i, line) in lines.iter().enumerate() {
        frame.text(
            Rect::new(
                inner.x + 3,
                inner.y + i as u16,
                inner.width.saturating_sub(4),
                1,
            ),
            line,
            background,
        );
    }
    if r.width > 4 {
        frame.cell(r.x + r.width - 3, r.y, "×", background.fg(color));
    }
    if let Some((label, _)) = &toast.action {
        let text = truncate(
            &format!(" {label} "),
            inner.width.saturating_sub(2) as usize,
        );
        let w = display_width(&text) as u16;
        let y = inner.y + lines.len() as u16;
        let br = Rect::new(inner.x + inner.width - w - 1, y, w, 1);
        let style = match toast.severity {
            Severity::Warning | Severity::Error => theme::button_danger(),
            Severity::Info | Severity::Success => theme::button_primary(),
        };
        frame.text(br, &text, style);
        frame.hit(br, Hit::ToastAction { id: toast.id });
    }
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    match click.hit {
        Hit::ToastAction { id } if click.is_left() => {
            if let Some((_, request)) = state.toasts.dismiss(*id).and_then(|t| t.action) {
                state.request(request);
            }
            Outcome::Consumed
        }
        Hit::Toast { id: CLEAR_ALL } if click.is_left() => {
            state.toasts.clear();
            Outcome::Consumed
        }
        Hit::Toast { id } | Hit::ToastAction { id }
            if click.is_left() || click.is_right() || click.is_middle() =>
        {
            state.toasts.dismiss(*id);
            Outcome::Consumed
        }
        _ => Outcome::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::explorer::Click;
    use bmux_tui::buffer::Buffer;
    use bmux_tui::event::{MouseButton, MouseEvent, MouseEventKind};
    use bmux_tui::geometry::Point;
    use bmux_tui::paint::PaintCx;

    fn render(state: &mut ExplorerState, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buffer = Buffer::empty(area);
        let mut bf = bmux_tui::frame::Frame::new(&mut buffer);
        let mut cx = PaintCx::new(&mut bf);
        let mut hits = std::mem::take(&mut state.hits);
        hits.begin();
        {
            let mut frame = Frame {
                cx: &mut cx,
                hits: &mut hits,
                now: Instant::now(),
            };
            draw(&mut frame, area, state);
        }
        hits.commit();
        state.hits = hits;
        buffer
    }

    fn text(buffer: &Buffer) -> String {
        (0..buffer.area().height)
            .filter_map(|y| buffer.row_symbols(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn click(state: &mut ExplorerState, at: Point) -> Outcome {
        let mouse = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), at);
        let hit = state.hits.at(at).cloned().expect("hit");
        handle_mouse(
            state,
            &Click {
                mouse: &mouse,
                hit: &hit,
                double: false,
            },
        )
    }

    #[test]
    fn expiry_depends_on_severity_and_action() {
        let mut toasts = Toasts::default();
        toasts.push(Severity::Info, "info");
        toasts.push(Severity::Warning, "warn");
        toasts.push(Severity::Error, "err");
        toasts.push_action(Severity::Info, "act", "Do", UiRequest::RefreshNow);
        let start = Instant::now();
        for t in &mut toasts.items {
            t.created = start;
        }
        toasts.prune(start + Duration::from_secs(5), None);
        assert_eq!(toasts.items.len(), 3);
        toasts.prune(start + Duration::from_secs(9), None);
        assert_eq!(
            toasts
                .items
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>(),
            ["err", "act"]
        );
        toasts.prune(start + Duration::from_secs(11), None);
        assert_eq!(toasts.items.len(), 1);
        toasts.prune(start + Duration::from_secs(12), None);
        assert!(toasts.is_empty());
    }

    #[test]
    fn hovering_keeps_a_toast_alive() {
        let mut toasts = Toasts::default();
        toasts.push(Severity::Info, "hello");
        let id = toasts.items[0].id;
        let start = Instant::now();
        toasts.items[0].created = start;
        toasts.prune(start + Duration::from_secs(3), Some(id));
        toasts.prune(start + Duration::from_secs(6), Some(id));
        assert_eq!(toasts.items.len(), 1);
        // After the pointer leaves, the grace period runs out.
        toasts.prune(start + Duration::from_secs(9), None);
        assert!(toasts.is_empty());
    }

    #[test]
    fn dismiss_and_clear() {
        let mut toasts = Toasts::default();
        toasts.push(Severity::Info, "a");
        toasts.push(Severity::Info, "b");
        let first = toasts.items[0].id;
        assert_eq!(toasts.dismiss(first).map(|t| t.text), Some("a".into()));
        assert!(toasts.dismiss(first).is_none());
        toasts.clear();
        assert!(toasts.is_empty());
    }

    #[test]
    fn draws_newest_first_with_overflow_and_icons() {
        let mut state = ExplorerState::default();
        for i in 1..=6 {
            state.toasts.push(Severity::Success, format!("message {i}"));
        }
        state.toasts.push(Severity::Error, "boom");
        let buffer = render(&mut state, 80, 30);
        let screen = text(&buffer);
        let boom = screen.find("boom").expect("newest drawn");
        let m6 = screen.find("message 6").expect("second newest drawn");
        assert!(boom < m6, "newest toast is on top");
        assert!(screen.contains("✗") && screen.contains("✓"));
        assert!(!screen.contains("message 3"), "only {MAX_VISIBLE} visible");
        assert!(screen.contains("+3 more · Clear all"));
        let clear = state
            .hits
            .rect_of(|h| *h == Hit::Toast { id: CLEAR_ALL })
            .unwrap();
        click(&mut state, Point::new(clear.x, clear.y));
        assert!(state.toasts.is_empty());
        // Toasts sit in the top-right with a one-cell margin.
        let rect = state
            .hits
            .rect_of(|h| matches!(h, Hit::Toast { .. }))
            .unwrap();
        assert_eq!(rect.x + rect.width, 79);
        assert_eq!(rect.width, 56);
        assert_eq!(rect.y, 0);
    }

    #[test]
    fn long_text_wraps_to_two_lines() {
        let mut state = ExplorerState::default();
        state.toasts.push(Severity::Info, "word ".repeat(40));
        let buffer = render(&mut state, 40, 10);
        let rect = state
            .hits
            .rect_of(|h| matches!(h, Hit::Toast { .. }))
            .unwrap();
        assert_eq!(rect.height, 4);
        assert_eq!(rect.width, 38);
        assert!(text(&buffer).contains('…'));
    }

    #[test]
    fn clicking_dismisses_and_action_runs_request() {
        let mut state = ExplorerState::default();
        state.toasts.push(Severity::Info, "plain");
        state.toasts.push_action(
            Severity::Warning,
            "still running",
            "Force kill",
            UiRequest::RefreshNow,
        );
        render(&mut state, 80, 20);
        let action = state
            .hits
            .rect_of(|h| matches!(h, Hit::ToastAction { .. }))
            .unwrap();
        assert_eq!(
            click(&mut state, Point::new(action.x, action.y)),
            Outcome::Consumed
        );
        assert_eq!(state.take_requests(), vec![UiRequest::RefreshNow]);
        assert_eq!(state.toasts.items.len(), 1);
        render(&mut state, 80, 20);
        let toast = state
            .hits
            .rect_of(|h| matches!(h, Hit::Toast { .. }))
            .unwrap();
        click(&mut state, Point::new(toast.x + 2, toast.y + 1));
        assert!(state.toasts.is_empty());
        assert!(state.take_requests().is_empty());
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let mut state = ExplorerState::default();
        state.toasts.push_action(
            Severity::Error,
            "x".repeat(200),
            "Retry",
            UiRequest::RefreshNow,
        );
        for (w, h) in [(1, 1), (2, 2), (8, 3), (9, 4), (12, 5), (3, 40)] {
            render(&mut state, w, h);
        }
    }
}
