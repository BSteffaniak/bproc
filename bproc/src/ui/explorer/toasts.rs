//! Non-modal notifications stacked in the top-right of the main area.
//! Toasts auto-expire; clicking dismisses; an optional action button runs a
//! queued `UiRequest` (e.g. "Force kill" after a SIGTERM).
//!
//! Keyboard: [`ACTION_KEY`] runs the newest toast's action and
//! [`dismiss_newest`] backs Esc when nothing else wants it.
//!
//! Toasts stack newest-first, so a new one pushes older ones down. Clicks on
//! a toast that appeared or moved within [`MOVE_GUARD`] are ignored so a
//! button cannot slide under the pointer and be hit by accident.
use super::hits::Hit;
use super::overlays::widgets::wrap;
use super::paint::{Frame, theme, truncate};
use super::{Click, ExplorerState, Outcome, UiRequest};
use crate::procs::actions::{ActionRequest, Signal};
use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::event::MouseEvent;
use bmux_tui::geometry::{Point, Rect};
use bmux_tui::style::{Color, Style};
use bmux_tui::text_width::display_width;
use std::time::{Duration, Instant};

/// Newest toasts drawn at once; older ones collapse into "+N more".
pub const MAX_VISIBLE: usize = 4;
/// `Hit::Toast` id of the "+N more · Clear all" line (real ids start at 1).
pub const CLEAR_ALL: u64 = 0;
/// Time left on a toast is topped up to this while the pointer rests on it.
const HOVER_GRACE: Duration = Duration::from_secs(2);
/// Clicks on a toast this soon after it appeared or moved are ignored.
pub const MOVE_GUARD: Duration = Duration::from_millis(400);
/// Key that runs the newest toast's action button (e.g. "Force kill").
pub const ACTION_KEY: char = '!';

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
    /// Row the toast was last drawn at.
    pub last_y: Option<u16>,
    /// When the toast was first drawn or last changed rows.
    pub moved_at: Option<Instant>,
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
    /// True while the toast is still settling after appearing or moving.
    pub fn settling(&self, now: Instant) -> bool {
        self.moved_at
            .is_some_and(|at| now.saturating_duration_since(at) < MOVE_GUARD)
    }
    /// Record that the toast is drawn at row `y` at `now`.
    fn place(&mut self, y: u16, now: Instant) {
        if self.last_y != Some(y) {
            self.last_y = Some(y);
            self.moved_at = Some(now);
        }
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
            last_y: None,
            moved_at: None,
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
    /// The most recent toast that has an action button.
    pub fn newest_with_action(&self) -> Option<&Toast> {
        self.items.iter().rev().find(|t| t.action.is_some())
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
        for toast in &mut state.toasts.items {
            toast.last_y = None;
        }
        return;
    }
    let width = 56.min(area.width - 2);
    let x = area.x + area.width - width - 1;
    let bottom = area.y + area.height;
    // Border + padding + icon column on the left, padding + border right.
    let text_w = width.saturating_sub(6) as usize;
    let mut y = area.y;
    let mut shown = 0;
    let mut full = false;
    let now = frame.now;
    let key_hint = state.toasts.newest_with_action().map(|t| t.id);
    for i in (0..state.toasts.items.len()).rev() {
        let toast = &state.toasts.items[i];
        let lines = toast_lines(toast, text_w);
        let h = lines.len() as u16 + 2 + u16::from(toast.action.is_some());
        full = full || shown >= MAX_VISIBLE || y.saturating_add(h) > bottom;
        if full {
            // Not on screen: showing up again later counts as a move.
            state.toasts.items[i].last_y = None;
            continue;
        }
        draw_toast(
            frame,
            Rect::new(x, y, width, h),
            toast,
            &lines,
            hovered == Some(toast.id),
            key_hint == Some(toast.id),
        );
        state.toasts.items[i].place(y, now);
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

fn toast_lines(toast: &Toast, text_w: usize) -> Vec<String> {
    let lines = wrap(&toast.text, text_w, 2);
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn draw_toast(
    frame: &mut Frame<'_, '_, '_>,
    r: Rect,
    toast: &Toast,
    lines: &[String],
    hovered: bool,
    key_hint: bool,
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
        // The newest action also answers to `ACTION_KEY`.
        let label = if key_hint {
            format!(" {label} · {ACTION_KEY} ")
        } else {
            format!(" {label} ")
        };
        let text = truncate(&label, inner.width.saturating_sub(2) as usize);
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
    if click.wheel() != 0 {
        return forward_wheel(state, click.mouse);
    }
    let id = match click.hit {
        Hit::Toast { id } | Hit::ToastAction { id } => *id,
        _ => return Outcome::Ignored,
    };
    if !(click.is_left() || click.is_right() || click.is_middle()) {
        return Outcome::Ignored;
    }
    if id == CLEAR_ALL {
        if click.is_left() {
            state.toasts.clear();
        }
        return Outcome::Consumed;
    }
    // A toast that just appeared or was pushed down by a newer one may have
    // slid under the pointer: swallow the click instead of acting on it.
    let now = Instant::now();
    if state
        .toasts
        .items
        .iter()
        .any(|t| t.id == id && t.settling(now))
    {
        return Outcome::Consumed;
    }
    let toast = state.toasts.dismiss(id);
    if matches!(click.hit, Hit::ToastAction { .. })
        && click.is_left()
        && let Some((_, request)) = toast.and_then(|t| t.action)
    {
        run_action(state, request);
    }
    Outcome::Consumed
}

/// Run a toast's action, routing ending signals through the confirmation
/// dialog when `confirm_signals` is on (as the context menu does).
fn run_action(state: &mut ExplorerState, request: UiRequest) {
    if let UiRequest::Action(ActionRequest::Signal { targets, signal }) = &request
        && state.settings.confirm_signals
        && signal.is_terminating()
        && *signal != Signal::Hup
    {
        let dialog = super::overlays::signal_dialog(state, targets.clone(), *signal);
        state.overlays.push(dialog);
        return;
    }
    state.request(request);
}

/// Scroll whatever lies under the toast at the pointer, so toasts do not
/// swallow wheel events over the table or a screen.
fn forward_wheel(state: &mut ExplorerState, mouse: &MouseEvent) -> Outcome {
    let Some(hit) = hit_below_toasts(state, mouse.position) else {
        return Outcome::Ignored;
    };
    let click = Click {
        mouse,
        hit: &hit,
        double: false,
    };
    match hit {
        Hit::Row { .. }
        | Hit::TreeToggle { .. }
        | Hit::Header { .. }
        | Hit::HeaderEdge { .. }
        | Hit::TableBody
        | Hit::TableScrollbar => super::table::handle_mouse(state, &click),
        Hit::DetailTab(_)
        | Hit::DetailAction(_)
        | Hit::DetailClose
        | Hit::DetailDockToggle
        | Hit::DetailSplitter
        | Hit::DetailBody
        | Hit::DetailRow { .. }
        | Hit::ProcessLink { .. } => super::detail_panel::handle_mouse(state, &click),
        Hit::ScreenBody
        | Hit::EventRow { .. }
        | Hit::WatchRow { .. }
        | Hit::WatchControl { .. } => super::screens::handle_mouse(state, &click),
        _ => Outcome::Ignored,
    }
}

/// Topmost committed hit at `position` that is not a toast.
fn hit_below_toasts(state: &ExplorerState, position: Point) -> Option<Hit> {
    state
        .hits
        .committed()
        .iter()
        .rev()
        .find(|(r, h)| {
            !matches!(h, Hit::Toast { .. } | Hit::ToastAction { .. })
                && super::hits::contains(*r, position)
        })
        .map(|(_, h)| h.clone())
}

/// Run the newest toast's action button (dismissing it). Returns false when
/// no toast has an action.
pub fn run_newest_action(state: &mut ExplorerState) -> bool {
    let Some(id) = state.toasts.newest_with_action().map(|t| t.id) else {
        return false;
    };
    if let Some((_, request)) = state.toasts.dismiss(id).and_then(|t| t.action) {
        run_action(state, request);
    }
    true
}

/// Dismiss the newest toast (Esc when nothing else consumed it).
pub fn dismiss_newest(state: &mut ExplorerState) -> bool {
    state.toasts.items.pop().is_some()
}

/// Toast keys: [`ACTION_KEY`] runs the newest action; Esc dismisses the
/// newest toast. Meant to be called after the focused view declined the key.
// Called from `ExplorerState::handle_event` key routing.
#[allow(dead_code)]
pub fn handle_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    let m = key.modifiers;
    if m.ctrl || m.alt {
        return Outcome::Ignored;
    }
    let handled = match key.key {
        KeyCode::Char(c) if c == ACTION_KEY => run_newest_action(state),
        KeyCode::Escape => dismiss_newest(state),
        _ => false,
    };
    if handled {
        Outcome::Consumed
    } else {
        Outcome::Ignored
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

    /// Pretend every toast has been in place longer than `MOVE_GUARD`.
    fn settle(state: &mut ExplorerState) {
        for toast in &mut state.toasts.items {
            if let Some(at) = toast.moved_at {
                toast.moved_at = at.checked_sub(MOVE_GUARD * 2);
            }
        }
    }

    fn click(state: &mut ExplorerState, at: Point) -> Outcome {
        mouse_at(state, MouseEventKind::Down(MouseButton::Left), at)
    }

    fn mouse_at(state: &mut ExplorerState, kind: MouseEventKind, at: Point) -> Outcome {
        let mouse = MouseEvent::new(kind, at);
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
        settle(&mut state);
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
        settle(&mut state);
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
        settle(&mut state);
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

    fn escalation(state: &mut ExplorerState) -> UiRequest {
        let request = UiRequest::Action(ActionRequest::Signal {
            targets: vec![crate::procs::ProcessKey::new(42, 7)],
            signal: Signal::Kill,
        });
        state.toasts.push_action(
            Severity::Warning,
            "worker is still running",
            "Force kill",
            request.clone(),
        );
        request
    }

    fn action_rect(state: &ExplorerState) -> Rect {
        state
            .hits
            .rect_of(|h| matches!(h, Hit::ToastAction { .. }))
            .expect("action button")
    }

    #[test]
    fn force_kill_confirms_when_signals_need_confirmation() {
        let mut state = ExplorerState::default();
        state.settings.confirm_signals = true;
        escalation(&mut state);
        render(&mut state, 80, 20);
        settle(&mut state);
        let r = action_rect(&state);
        assert_eq!(click(&mut state, Point::new(r.x, r.y)), Outcome::Consumed);
        assert!(
            state.take_requests().is_empty(),
            "no SIGKILL without a dialog"
        );
        match state.overlays.last() {
            Some(super::super::overlays::Overlay::Signal(d)) => {
                assert_eq!(d.signal, Signal::Kill);
                assert_eq!(d.targets, vec![crate::procs::ProcessKey::new(42, 7)]);
            }
            _ => panic!("signal dialog expected"),
        }
        assert!(state.toasts.is_empty());

        // Without confirmation the button sends directly.
        let mut state = ExplorerState::default();
        state.settings.confirm_signals = false;
        let request = escalation(&mut state);
        render(&mut state, 80, 20);
        settle(&mut state);
        let r = action_rect(&state);
        click(&mut state, Point::new(r.x, r.y));
        assert_eq!(state.take_requests(), vec![request]);
        assert!(state.overlays.is_empty());
    }

    #[test]
    fn clicks_on_toasts_that_just_moved_are_ignored() {
        let mut state = ExplorerState::default();
        state.settings.confirm_signals = false;
        escalation(&mut state);
        render(&mut state, 80, 20);
        // Freshly shown: a click right away is swallowed.
        let r = action_rect(&state);
        assert_eq!(click(&mut state, Point::new(r.x, r.y)), Outcome::Consumed);
        assert!(state.take_requests().is_empty());
        assert_eq!(state.toasts.items.len(), 1, "toast kept");
        settle(&mut state);

        // A newer toast pushes the button down; the old position now shows
        // the newer toast and the moved button is guarded.
        state.toasts.push(Severity::Info, "watch rule fired");
        render(&mut state, 80, 20);
        settle_only(&mut state, "watch rule fired");
        let moved = action_rect(&state);
        assert!(moved.y > r.y, "older toast moved down");
        click(&mut state, Point::new(moved.x, moved.y));
        assert!(state.take_requests().is_empty(), "moved button is guarded");
        assert_eq!(state.toasts.items.len(), 2);

        // Once settled it works again.
        settle(&mut state);
        click(&mut state, Point::new(moved.x, moved.y));
        assert_eq!(state.take_requests().len(), 1);
    }

    /// Settle only the toast whose text is `text`.
    fn settle_only(state: &mut ExplorerState, text: &str) {
        for toast in &mut state.toasts.items {
            if toast.text == text
                && let Some(at) = toast.moved_at
            {
                toast.moved_at = at.checked_sub(MOVE_GUARD * 2);
            }
        }
    }

    #[test]
    fn keyboard_runs_the_newest_action_and_esc_dismisses() {
        let key = |code| KeyStroke::simple(code);
        let mut state = ExplorerState::default();
        assert_eq!(
            handle_key(&mut state, &key(KeyCode::Char(ACTION_KEY))),
            Outcome::Ignored
        );
        assert_eq!(
            handle_key(&mut state, &key(KeyCode::Escape)),
            Outcome::Ignored
        );
        state.settings.confirm_signals = false;
        let request = escalation(&mut state);
        state.toasts.push(Severity::Info, "plain");
        assert_eq!(
            handle_key(&mut state, &key(KeyCode::Char(ACTION_KEY))),
            Outcome::Consumed
        );
        assert_eq!(state.take_requests(), vec![request]);
        assert_eq!(state.toasts.items.len(), 1);
        assert_eq!(
            handle_key(&mut state, &key(KeyCode::Escape)),
            Outcome::Consumed
        );
        assert!(state.toasts.is_empty());

        // With confirmation on, the key opens the dialog instead.
        state.settings.confirm_signals = true;
        escalation(&mut state);
        assert!(run_newest_action(&mut state));
        assert!(state.take_requests().is_empty());
        assert_eq!(state.overlays.len(), 1);
    }

    #[test]
    fn wheel_over_a_toast_scrolls_what_is_underneath() {
        let mut state = ExplorerState::default();
        state.toasts.push(Severity::Info, "hello");
        render(&mut state, 80, 20);
        let r = state
            .hits
            .rect_of(|h| matches!(h, Hit::Toast { .. }))
            .unwrap();
        let at = Point::new(r.x + 2, r.y + 1);
        // Nothing underneath: not consumed.
        assert_eq!(
            mouse_at(&mut state, MouseEventKind::ScrollDown, at),
            Outcome::Ignored
        );
        // Table underneath: forwarded (the table consumes wheel events).
        let toast_hits = state.hits.committed().to_vec();
        let mut hits = std::mem::take(&mut state.hits);
        hits.begin();
        hits.push(Rect::new(0, 0, 80, 20), Hit::TableBody);
        for (rect, hit) in toast_hits {
            hits.push(rect, hit);
        }
        hits.commit();
        state.hits = hits;
        assert_eq!(
            mouse_at(&mut state, MouseEventKind::ScrollDown, at),
            Outcome::Consumed
        );
        assert_eq!(state.toasts.items.len(), 1, "wheel does not dismiss");
    }
}
