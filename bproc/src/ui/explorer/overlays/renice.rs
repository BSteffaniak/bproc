//! Renice dialog: current value, a clickable / draggable -20..19 slider,
//! typed input and Cancel / Apply buttons.
use super::super::hits::Hit;
use super::super::paint::{Frame, centered, theme, truncate};
use super::super::{ExplorerState, UiRequest};
use super::OverlayOutcome;
use super::widgets::{self, Button, Target};
use crate::procs::ProcessKey;
use crate::procs::actions::{ActionRequest, NICE_RANGE};
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui::geometry::Rect;
use bmux_tui::text_width::display_width;

pub const MIN: i32 = *NICE_RANGE.start();
pub const MAX: i32 = *NICE_RANGE.end();
const STEPS: usize = (MAX - MIN + 1) as usize;

pub const DEC: u16 = 1;
pub const INC: u16 = 2;
pub const CANCEL: u16 = 3;
pub const APPLY: u16 = 4;

const HINT_ROOT: &str = "Lower values = higher priority (-20 highest, 19 lowest).";
const HINT_UNKNOWN: &str =
    "Lower values = higher priority. Without root you can only raise nice (lower priority).";

/// Whether this process may lower nice values (raise priority).
pub fn is_privileged() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub struct ReniceDialog {
    pub targets: Vec<ProcessKey>,
    pub value: i32,
    /// Nice value of the targets before the change, when known and shared.
    pub current: Option<i32>,
    /// True when the targets currently have different nice values.
    pub mixed: bool,
    /// Text typed since the last arrow / click edit.
    pub input: Option<String>,
    /// Enter presses Cancel instead of Apply.
    pub cancel_focused: bool,
    /// Running as root: any value in `MIN..=MAX` may be applied. Otherwise
    /// the nice value can only go up (macOS/BSD, Linux without
    /// CAP_SYS_NICE), so a known current value is the floor.
    pub privileged: bool,
    hover: Target,
}

impl ReniceDialog {
    /// `lower`: `Some(true)` starts one step lower priority (nice + 1),
    /// `Some(false)` one step higher, `None` at the current value.
    pub fn new(state: &ExplorerState, targets: Vec<ProcessKey>, lower: Option<bool>) -> Self {
        Self::with_privilege(state, targets, lower, is_privileged())
    }

    /// [`Self::new`] with an explicit privilege level.
    pub fn with_privilege(
        state: &ExplorerState,
        targets: Vec<ProcessKey>,
        lower: Option<bool>,
        privileged: bool,
    ) -> Self {
        let values: Vec<Option<i32>> = targets
            .iter()
            .map(|k| state.store.get(k).and_then(|r| r.entry.sample.nice))
            .collect();
        let current = values.first().copied().flatten();
        let mixed = values.iter().any(|v| Some(v) != values.first());
        let delta = match lower {
            Some(true) => 1,
            Some(false) => -1,
            None => 0,
        };
        let mut dialog = Self {
            targets,
            value: 0,
            current: if mixed { None } else { current },
            mixed,
            input: None,
            cancel_focused: false,
            privileged,
            hover: Target::Outside,
        };
        dialog.value = dialog.clamp(current.unwrap_or(0) + delta);
        dialog
    }

    /// Lowest value that can be applied: the current nice for unprivileged
    /// users (when known and shared), otherwise `MIN`.
    pub fn floor(&self) -> i32 {
        match self.current {
            Some(c) if !self.privileged => c.clamp(MIN, MAX),
            _ => MIN,
        }
    }

    /// True when the user cannot raise the targets' priority at all, so F7
    /// ("Nice-") has nothing to offer.
    pub fn cannot_raise(&self) -> bool {
        self.floor() > MIN
    }

    fn clamp(&self, value: i32) -> i32 {
        value.clamp(self.floor(), MAX)
    }

    fn hint(&self) -> String {
        if self.privileged {
            HINT_ROOT.to_string()
        } else if self.cannot_raise() {
            format!(
                "Without root you can only lower priority (raise nice): {}..{MAX}.",
                self.floor()
            )
        } else {
            HINT_UNKNOWN.to_string()
        }
    }

    fn set(&mut self, value: i32) {
        self.value = self.clamp(value);
        self.input = None;
    }

    fn type_char(&mut self, c: char) {
        let mut text = self.input.take().unwrap_or_default();
        let accept = match c {
            '-' => text.is_empty(),
            '0'..='9' => text.trim_start_matches('-').len() < 2,
            _ => false,
        };
        if accept {
            text.push(c);
        }
        if let Ok(v) = text.parse::<i32>() {
            self.value = self.clamp(v);
        }
        self.input = Some(text);
    }

    fn backspace(&mut self) {
        let floor = self.floor();
        if let Some(text) = &mut self.input {
            text.pop();
            if let Ok(v) = text.parse::<i32>() {
                self.value = v.clamp(floor, MAX);
            }
            if text.is_empty() {
                self.input = None;
            }
        }
    }

    fn apply(&self, state: &mut ExplorerState) {
        state.request(UiRequest::Action(ActionRequest::Renice {
            targets: self.targets.clone(),
            nice: self.value,
        }));
    }

    pub fn draw(
        &self,
        frame: &mut Frame<'_, '_, '_>,
        area: Rect,
        index: usize,
        state: &ExplorerState,
    ) {
        let hint = widgets::wrap(
            &self.hint(),
            62.min(area.width).saturating_sub(4) as usize,
            2,
        );
        let rect = centered(area, 62, 11 + hint.len().max(1) as u16);
        let inner = frame.panel(
            rect,
            Some("Change priority"),
            theme::panel_border(),
            theme::panel_bg(),
        );
        frame.hit(rect, Hit::OverlayBody { overlay: index });
        let Some(buttons_y) = widgets::last_row(inner) else {
            return;
        };
        let x = inner.x.saturating_add(1);
        let w = inner.width.saturating_sub(2);
        let body = widgets::body();
        let muted = widgets::muted();
        let hover = match self.hover {
            Target::Control(c) => Some(c),
            _ => None,
        };
        let rows = buttons_y.saturating_sub(inner.y);
        let row = |i: u16| (i < rows).then(|| inner.y + i);
        let text = |frame: &mut Frame<'_, '_, '_>, y: u16, s: &str, style| {
            frame.text(Rect::new(x, y, w, 1), &truncate(s, w as usize), style);
        };

        if let Some(y) = row(0) {
            let who = match self.targets.as_slice() {
                [key] => format!("{}  ·  PID {}", state.name_of(key), key.pid),
                many => format!("{} processes", many.len()),
            };
            text(frame, y, &who, body.patch(theme::bold()));
        }
        if let Some(y) = row(1) {
            let current = match (self.current, self.mixed) {
                (_, true) => "mixed".to_string(),
                (Some(v), _) => v.to_string(),
                (None, _) => "–".to_string(),
            };
            text(frame, y, &format!("Current nice: {current}"), muted);
        }
        if let Some(y) = row(3) {
            let label = "New nice:";
            text(frame, y, label, body);
            let mut cx = x.saturating_add(display_width(label) as u16 + 1);
            let spin = |frame: &mut Frame<'_, '_, '_>, cx: &mut u16, s: &str, control: u16| {
                let style = if hover == Some(control) {
                    widgets::hovered(theme::button())
                } else {
                    theme::button()
                };
                let r = Rect::new(*cx, y, 3, 1).intersection(inner);
                frame.text(r, s, style);
                frame.hit(
                    r,
                    Hit::OverlayControl {
                        overlay: index,
                        control,
                    },
                );
                *cx = cx.saturating_add(4);
            };
            spin(frame, &mut cx, " ◀ ", DEC);
            let shown = match &self.input {
                Some(t) => format!("{t:>3}▏"),
                None => format!("{:>3} ", self.value),
            };
            let field = Rect::new(cx, y, 5, 1).intersection(inner);
            frame.text(
                field,
                &format!(" {shown}"),
                theme::selected().patch(theme::bold()),
            );
            cx = cx.saturating_add(6);
            spin(frame, &mut cx, " ▶ ", INC);
            let effect = match self.current {
                Some(c) if self.value < c => "higher priority",
                Some(c) if self.value > c => "lower priority",
                Some(_) => "unchanged",
                None => "",
            };
            let ew = (x + w).saturating_sub(cx);
            if ew > 0 {
                frame.text(
                    Rect::new(cx, y, ew, 1),
                    &truncate(effect, ew as usize),
                    body.patch(theme::accent()),
                );
            }
        }
        if let Some(y) = row(4) {
            self.draw_slider(frame, Rect::new(x, y, w, 1), index, row(5));
        }
        for (i, line) in hint.iter().enumerate() {
            if let Some(y) = row(7 + i as u16) {
                text(frame, y, line, muted);
            }
        }
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
                    label: "Apply",
                    style: theme::button_primary(),
                    control: APPLY,
                },
            ],
            Some(if self.cancel_focused { CANCEL } else { APPLY }),
            hover,
        );
    }

    fn draw_slider(
        &self,
        frame: &mut Frame<'_, '_, '_>,
        r: Rect,
        index: usize,
        scale_y: Option<u16>,
    ) {
        let cells = (r.width as usize).min(STEPS);
        if cells == 0 {
            return;
        }
        let marker = value_cell(self.value, cells);
        let hovered = match self.hover {
            Target::Item(i) => Some(i),
            _ => None,
        };
        for i in 0..cells {
            let v = cell_value(i, cells);
            let blocked = v < self.floor();
            let (symbol, style) = if i == marker {
                (
                    "●",
                    widgets::body().patch(theme::accent()).patch(theme::bold()),
                )
            } else if blocked {
                // Out of reach without root.
                ("┄", widgets::muted())
            } else if i < marker {
                ("━", widgets::body().patch(theme::accent()))
            } else {
                ("─", widgets::muted())
            };
            let style = if hovered == Some((v - MIN) as usize) && i != marker && !blocked {
                style.patch(theme::hovered())
            } else {
                style
            };
            let cx = r.x + i as u16;
            frame.cell(cx, r.y, symbol, style);
            frame.hit(
                Rect::new(cx, r.y, 1, 1),
                Hit::OverlayItem {
                    overlay: index,
                    item: (v - MIN) as usize,
                },
            );
        }
        if let Some(y) = scale_y {
            let muted = widgets::muted();
            let span = cells as u16;
            frame.text(Rect::new(r.x, y, span, 1), &MIN.to_string(), muted);
            let zero = r.x + value_cell(0, cells) as u16;
            if cells >= 10 {
                frame.text(Rect::new(zero, y, 1, 1), "0", muted);
            }
            if cells >= 6 {
                frame.text(Rect::new(r.x + span - 2, y, 2, 1), &MAX.to_string(), muted);
            }
        }
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
                    if !self.cancel_focused {
                        self.apply(state);
                    }
                    return OverlayOutcome::Close;
                }
                KeyCode::Left | KeyCode::Down => self.set(self.value - 1),
                KeyCode::Right | KeyCode::Up => self.set(self.value + 1),
                KeyCode::PageDown => self.set(self.value - 5),
                KeyCode::PageUp => self.set(self.value + 5),
                KeyCode::Home => self.set(self.floor()),
                KeyCode::End => self.set(MAX),
                KeyCode::Tab => self.cancel_focused = !self.cancel_focused,
                KeyCode::Backspace | KeyCode::Delete => self.backspace(),
                KeyCode::Char(c @ ('0'..='9' | '-')) => self.type_char(c),
                _ => {}
            },
            Event::Mouse(mouse) => {
                let target = widgets::resolve(state, index, mouse.position);
                match mouse.kind {
                    MouseEventKind::Move => self.hover = target,
                    MouseEventKind::Drag(MouseButton::Left) => {
                        self.hover = target;
                        if let Target::Item(i) = target {
                            self.set(i as i32 + MIN);
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => match target {
                        Target::Item(i) => self.set(i as i32 + MIN),
                        Target::Control(DEC) => self.set(self.value - 1),
                        Target::Control(INC) => self.set(self.value + 1),
                        Target::Control(CANCEL) => return OverlayOutcome::Close,
                        Target::Control(APPLY) => {
                            self.apply(state);
                            return OverlayOutcome::Close;
                        }
                        Target::Control(_) | Target::Body => {}
                        Target::Outside => return OverlayOutcome::Close,
                    },
                    MouseEventKind::Down(_) => {
                        if target == Target::Outside {
                            return OverlayOutcome::Close;
                        }
                    }
                    MouseEventKind::ScrollUp => self.set(self.value + 1),
                    MouseEventKind::ScrollDown => self.set(self.value - 1),
                    _ => {}
                }
            }
            _ => {}
        }
        OverlayOutcome::Consumed
    }
}

/// Nice value represented by slider cell `i` of `cells`.
pub fn cell_value(i: usize, cells: usize) -> i32 {
    if cells <= 1 {
        return 0;
    }
    let span = (STEPS - 1) as f64;
    MIN + ((i as f64) * span / (cells - 1) as f64).round() as i32
}

/// Slider cell showing `value`.
pub fn value_cell(value: i32, cells: usize) -> usize {
    if cells <= 1 {
        return 0;
    }
    let span = (STEPS - 1) as f64;
    let v = (value.clamp(MIN, MAX) - MIN) as f64;
    ((v * (cells - 1) as f64 / span).round() as usize).min(cells - 1)
}
