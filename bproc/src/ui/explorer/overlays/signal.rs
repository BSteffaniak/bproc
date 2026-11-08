//! "Send signal" confirmation dialog: target summary, optional children,
//! a radio list of signals and Cancel / Send buttons.
use super::super::paint::{Frame, centered, theme, truncate};
use super::super::{ExplorerState, UiRequest};
use super::OverlayOutcome;
use super::widgets::{self, Button, Target};
use crate::procs::ProcessKey;
use crate::procs::actions::{self, ActionRequest, Signal};
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui::geometry::Rect;
use bmux_tui::style::Style;

pub const CHILDREN: u16 = 1;
pub const CANCEL: u16 = 2;
pub const SEND: u16 = 3;

/// Suffix for a target that is not a row of the visible table.
const HIDDEN: &str = "  (hidden by filter)";
/// Targets listed by name before collapsing into "and N more".
const LISTED: usize = 4;
/// Signalling more processes than this (with children) asks twice.
pub const LARGE_TREE: usize = 50;

/// Whether `key` is this bproc process (see [`actions::is_self`]).
pub fn is_self(key: &ProcessKey) -> bool {
    actions::is_self(key.pid)
}

/// "Kill process tree" is never offered for these (see
/// [`actions::is_system_root`]).
pub fn is_system_root(pid: i32) -> bool {
    actions::is_system_root(pid)
}

/// `keys` without bproc itself; the flag says whether it was present.
pub fn without_self(keys: Vec<ProcessKey>) -> (Vec<ProcessKey>, bool) {
    actions::exclude_self(keys)
}

pub struct SignalDialog {
    pub targets: Vec<ProcessKey>,
    pub signal: Signal,
    /// Live descendants of the targets that are not targets themselves.
    pub children: Vec<ProcessKey>,
    pub include_children: bool,
    /// Enter presses Cancel instead of Send.
    pub cancel_focused: bool,
    /// bproc itself was among the requested targets and was dropped.
    pub skipped_self: bool,
    /// Some target is a system root, so its children are not offered.
    pub tree_blocked: bool,
    /// A large send was pressed once; the next press sends.
    pub armed: bool,
    hover: Target,
}

impl SignalDialog {
    pub fn new(state: &ExplorerState, targets: Vec<ProcessKey>, signal: Signal) -> Self {
        let (targets, skipped_self) = without_self(targets);
        let mut unique: Vec<ProcessKey> = Vec::with_capacity(targets.len());
        for key in targets {
            if !unique.contains(&key) {
                unique.push(key);
            }
        }
        let tree_blocked = unique.iter().any(|k| is_system_root(k.pid));
        let mut children: Vec<ProcessKey> = Vec::new();
        if !tree_blocked {
            for key in &unique {
                for child in state.store.descendants(key) {
                    if !is_self(&child) && !unique.contains(&child) && !children.contains(&child) {
                        children.push(child);
                    }
                }
            }
        }
        Self {
            targets: unique,
            signal,
            children,
            include_children: false,
            cancel_focused: false,
            skipped_self,
            tree_blocked,
            armed: false,
            hover: Target::Outside,
        }
    }

    /// Every process the signal will be sent to.
    pub fn recipients(&self) -> Vec<ProcessKey> {
        let mut out = self.targets.clone();
        if self.include_children {
            out.extend(self.children.iter().copied());
        }
        out
    }

    /// Whether Send must be pressed twice (more than `LARGE_TREE` recipients
    /// including children).
    pub fn needs_second_press(&self) -> bool {
        self.include_children && self.recipients().len() > LARGE_TREE
    }

    /// Press Send: returns false when the dialog should stay open (a large
    /// send was armed instead of sent).
    fn press_send(&mut self, state: &mut ExplorerState) -> bool {
        if self.needs_second_press() && !self.armed {
            self.armed = true;
            return false;
        }
        let targets = self.recipients();
        if targets.is_empty() {
            state.flash("bproc does not signal itself");
            return true;
        }
        state.request(UiRequest::Action(ActionRequest::Signal {
            targets,
            signal: self.signal,
        }));
        true
    }

    fn toggle_children(&mut self) {
        if !self.children.is_empty() {
            self.include_children = !self.include_children;
            self.armed = false;
        }
    }

    /// Warning lines shown under the summary.
    fn notes(&self) -> Vec<(String, Style)> {
        let warn = widgets::body().fg(bmux_tui::style::Color::Yellow);
        let mut out = Vec::new();
        if self.skipped_self {
            out.push((
                format!(
                    "⚠ bproc itself (PID {}) is skipped; quit with q.",
                    std::process::id()
                ),
                warn,
            ));
        }
        if self.tree_blocked {
            out.push((
                "Children of a system root process are never offered.".to_string(),
                widgets::muted(),
            ));
        }
        if self.needs_second_press() {
            let n = self.recipients().len();
            let text = if self.armed {
                format!("⚠ Press Send again to signal all {n} processes.")
            } else {
                format!("⚠ This signals {n} processes.")
            };
            out.push((text, warn.patch(theme::bold())));
        }
        out
    }

    fn select(&mut self, i: usize) {
        if let Some(signal) = Signal::ALL.get(i) {
            self.signal = *signal;
        }
    }

    fn move_by(&mut self, delta: isize) {
        let i = Signal::ALL
            .iter()
            .position(|s| *s == self.signal)
            .unwrap_or(0) as isize;
        let n = Signal::ALL.len() as isize;
        self.select((i + delta).clamp(0, n - 1) as usize);
    }

    fn summary(&self, state: &ExplorerState) -> Vec<(String, Style)> {
        let text = widgets::body();
        let muted = widgets::muted();
        // Targets the user cannot see in the table are called out by name.
        let hidden = |key: &ProcessKey| state.hidden_targets(std::slice::from_ref(key)) > 0;
        if let [key] = self.targets.as_slice() {
            let row = state.store.get(key);
            let mut first = format!("{}  ·  PID {}", state.name_of(key), key.pid);
            if let Some(user) = row.and_then(|r| r.entry.record.user.as_deref()) {
                first.push_str(&format!("  ·  {user}"));
            }
            if row.is_some_and(|r| r.is_ghost()) {
                first.push_str("  (exited)");
            }
            if hidden(key) {
                first.push_str(HIDDEN);
            }
            let mut lines = vec![(first, text.patch(theme::bold()))];
            if let Some(r) = row {
                let command = r.entry.record.command();
                if command != r.name() {
                    lines.push((command, muted));
                }
            }
            return lines;
        }
        let mut header = format!("{} processes", self.targets.len());
        let hidden_count = state.hidden_targets(&self.targets);
        if hidden_count > 0 {
            header.push_str(&format!("  ({hidden_count} hidden by filter)"));
        }
        let mut lines = vec![(header, text.patch(theme::bold()))];
        for key in self.targets.iter().take(LISTED) {
            let mut line = format!("• {}  ({})", state.name_of(key), key.pid);
            if hidden(key) {
                line.push_str(HIDDEN);
            }
            lines.push((line, text));
        }
        if self.targets.len() > LISTED {
            lines.push((format!("and {} more", self.targets.len() - LISTED), muted));
        }
        lines
    }

    pub fn draw(
        &self,
        frame: &mut Frame<'_, '_, '_>,
        area: Rect,
        index: usize,
        state: &ExplorerState,
    ) {
        let mut summary = self.summary(state);
        summary.extend(self.notes());
        let children_rows = if self.children.is_empty() { 0 } else { 2 };
        let rows = summary.len() + 1 + children_rows + Signal::ALL.len() + 1 + 1;
        let h = (rows + 2).min(u16::MAX as usize) as u16;
        let rect = centered(area, 80, h);
        let title = if self.signal == Signal::Kill {
            "Force kill"
        } else {
            "Send signal"
        };
        let inner = frame.panel(rect, Some(title), theme::panel_border(), theme::panel_bg());
        frame.hit(
            rect,
            super::super::hits::Hit::OverlayBody { overlay: index },
        );
        let Some(buttons_y) = widgets::last_row(inner) else {
            return;
        };
        let x = inner.x.saturating_add(1);
        let w = inner.width.saturating_sub(2);
        let mut y = inner.y;
        let room = |y: &mut u16| {
            let ok = *y < buttons_y;
            if ok {
                *y += 1;
            }
            ok.then_some(*y - 1)
        };
        for (line, style) in &summary {
            let Some(row) = room(&mut y) else { break };
            frame.text(Rect::new(x, row, w, 1), &truncate(line, w as usize), *style);
        }
        room(&mut y);
        if !self.children.is_empty() {
            if let Some(row) = room(&mut y) {
                let n = self.children.len();
                let label = format!(
                    "{} Also signal {n} child process{}",
                    widgets::checkbox(self.include_children),
                    if n == 1 { "" } else { "es" }
                );
                let label = truncate(&label, w as usize);
                let lw = bmux_tui::text_width::display_width(&label) as u16;
                let mut style = widgets::body();
                if self.hover == Target::Control(CHILDREN) {
                    style = style.patch(theme::hovered());
                }
                let r = Rect::new(x, row, lw, 1);
                frame.text(r, &label, style);
                frame.hit(
                    r,
                    super::super::hits::Hit::OverlayControl {
                        overlay: index,
                        control: CHILDREN,
                    },
                );
            }
            room(&mut y);
        }
        for (i, signal) in Signal::ALL.iter().enumerate() {
            let Some(row) = room(&mut y) else { break };
            let chosen = *signal == self.signal;
            let mut style = widgets::body();
            if self.hover == Target::Item(i) {
                style = style.patch(theme::hovered());
            }
            let r = Rect::new(inner.x, row, inner.width, 1);
            frame.fill(r, style);
            let radio = if chosen { "●" } else { "○" };
            let strong = if chosen {
                style.patch(theme::accent()).patch(theme::bold())
            } else {
                style
            };
            frame.text(
                Rect::new(x, row, w, 1),
                &format!("{} ", i + 1),
                style.patch(theme::dim()),
            );
            let main = format!("{radio} {:<8} {:<17}", signal.name(), signal.verb());
            let main = truncate(&main, w.saturating_sub(2) as usize);
            let mw = bmux_tui::text_width::display_width(&main) as u16;
            frame.text(Rect::new(x + 2, row, w.saturating_sub(2), 1), &main, strong);
            let dx = x.saturating_add(2).saturating_add(mw).saturating_add(1);
            let dw = (x + w).saturating_sub(dx);
            if dw > 0 {
                frame.text(
                    Rect::new(dx, row, dw, 1),
                    &truncate(signal.description(), dw as usize),
                    style.patch(theme::dim()),
                );
            }
            frame.hit(
                r,
                super::super::hits::Hit::OverlayItem {
                    overlay: index,
                    item: i,
                },
            );
        }
        let send_label = if self.armed && self.needs_second_press() {
            format!(
                "Confirm {} ×{}",
                self.signal.name(),
                self.recipients().len()
            )
        } else {
            format!("Send {}", self.signal.name())
        };
        let send_style = if self.signal == Signal::Kill || self.armed {
            theme::button_danger()
        } else {
            theme::button_primary()
        };
        let hover = match self.hover {
            Target::Control(c) => Some(c),
            _ => None,
        };
        widgets::buttons(
            frame,
            inner,
            buttons_y,
            index,
            &[
                Button {
                    label: "Cancel",
                    style: theme::button(),
                    control: CANCEL,
                },
                Button {
                    label: &send_label,
                    style: send_style,
                    control: SEND,
                },
            ],
            Some(if self.cancel_focused { CANCEL } else { SEND }),
            hover,
        );
    }

    pub fn handle(
        &mut self,
        index: usize,
        event: &Event,
        state: &mut ExplorerState,
    ) -> OverlayOutcome {
        match event {
            Event::Key(key) => match key.key {
                KeyCode::Escape => return OverlayOutcome::Close,
                KeyCode::Enter => {
                    if !self.cancel_focused && !self.press_send(state) {
                        return OverlayOutcome::Consumed;
                    }
                    return OverlayOutcome::Close;
                }
                KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
                KeyCode::Home => self.select(0),
                KeyCode::End => self.select(Signal::ALL.len() - 1),
                KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                    self.cancel_focused = !self.cancel_focused;
                }
                KeyCode::Space | KeyCode::Char(' ') | KeyCode::Char('c') => {
                    self.toggle_children();
                }
                KeyCode::Char(c @ '1'..='9') => self.select(c as usize - '1' as usize),
                _ => {}
            },
            Event::Mouse(mouse) => {
                let target = widgets::resolve(state, index, mouse.position);
                match mouse.kind {
                    MouseEventKind::Move | MouseEventKind::Drag(_) => self.hover = target,
                    MouseEventKind::Down(MouseButton::Left) => match target {
                        Target::Item(i) => self.select(i),
                        Target::Control(CHILDREN) => self.toggle_children(),
                        Target::Control(CANCEL) => return OverlayOutcome::Close,
                        Target::Control(SEND) => {
                            if self.press_send(state) {
                                return OverlayOutcome::Close;
                            }
                        }
                        Target::Control(_) | Target::Body => {}
                        Target::Outside => return OverlayOutcome::Close,
                    },
                    MouseEventKind::Down(_) => {
                        if target == Target::Outside {
                            return OverlayOutcome::Close;
                        }
                    }
                    MouseEventKind::ScrollUp => self.move_by(-1),
                    MouseEventKind::ScrollDown => self.move_by(1),
                    _ => {}
                }
            }
            _ => {}
        }
        OverlayOutcome::Consumed
    }
}
