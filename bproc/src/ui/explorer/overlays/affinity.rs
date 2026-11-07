//! "Set CPU affinity" dialog (Linux): a checkbox grid of logical CPUs with
//! All / None shortcuts and Cancel / Apply buttons.
use super::super::hits::Hit;
use super::super::paint::{Frame, centered, theme, truncate};
use super::super::{ExplorerState, UiRequest};
use super::OverlayOutcome;
use super::widgets::{self, Button, Target};
use crate::procs::ProcessKey;
use crate::procs::actions::{self, ActionRequest};
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui::geometry::Rect;

pub const ALL: u16 = 1;
pub const NONE: u16 = 2;
pub const CANCEL: u16 = 3;
pub const APPLY: u16 = 4;

/// Width of one grid cell: `[x] 127` plus a gap.
const CELL_W: u16 = 8;
/// Most CPU columns per row.
const MAX_COLS: usize = 8;
/// Dialog width.
const WIDTH: u16 = 2 + 2 + CELL_W * MAX_COLS as u16;

const HINT: &str = "Space toggle · a all · n none · Enter apply";

pub struct AffinityDialog {
    pub targets: Vec<ProcessKey>,
    /// One flag per logical CPU.
    pub cpus: Vec<bool>,
    /// Keyboard cursor (CPU index).
    pub cursor: usize,
    /// Enter presses Cancel instead of Apply.
    pub cancel_focused: bool,
    hover: Target,
}

impl AffinityDialog {
    /// Grid for the machine's logical CPUs, prefilled from the first
    /// target's current mask (all CPUs when it cannot be read).
    pub fn new(state: &ExplorerState, targets: Vec<ProcessKey>) -> Self {
        let current = targets
            .first()
            .and_then(|k| actions::current_affinity(k.pid).ok());
        Self::with_mask(cpu_count(state), targets, current.as_deref())
    }

    /// Grid of `count` CPUs with `allowed` checked (`None` = all).
    pub fn with_mask(count: usize, targets: Vec<ProcessKey>, allowed: Option<&[usize]>) -> Self {
        let count = count.max(1);
        let mut cpus = vec![allowed.is_none(); count];
        if let Some(allowed) = allowed {
            for &cpu in allowed {
                if let Some(slot) = cpus.get_mut(cpu) {
                    *slot = true;
                }
            }
        }
        Self {
            targets,
            cpus,
            cursor: 0,
            cancel_focused: false,
            hover: Target::Outside,
        }
    }

    /// Checked CPU indices.
    pub fn selected(&self) -> Vec<usize> {
        (0..self.cpus.len()).filter(|&i| self.cpus[i]).collect()
    }

    fn toggle(&mut self, i: usize) {
        if let Some(slot) = self.cpus.get_mut(i) {
            *slot = !*slot;
        }
    }

    fn set_all(&mut self, on: bool) {
        self.cpus.iter_mut().for_each(|c| *c = on);
    }

    /// Queue the request; false (and a flash) when no CPU is checked.
    fn apply(&self, state: &mut ExplorerState) -> bool {
        let cpus = self.selected();
        if cpus.is_empty() {
            state.flash("Select at least one CPU");
            return false;
        }
        state.request(UiRequest::Action(ActionRequest::Affinity {
            targets: self.targets.clone(),
            cpus,
        }));
        true
    }

    fn columns(&self) -> usize {
        self.cpus.len().clamp(1, MAX_COLS)
    }

    fn move_cursor(&mut self, dx: isize, dy: isize) {
        let cols = self.columns() as isize;
        let n = self.cpus.len() as isize;
        let next = self.cursor as isize + dx + dy * cols;
        self.cursor = next.clamp(0, n - 1) as usize;
    }

    pub fn draw(
        &self,
        frame: &mut Frame<'_, '_, '_>,
        area: Rect,
        index: usize,
        state: &ExplorerState,
    ) {
        let cols = self.columns();
        let grid_rows = self.cpus.len().div_ceil(cols);
        // who, blank, grid, blank, hint, blank, buttons
        let h = (grid_rows + 6 + 2).min(u16::MAX as usize) as u16;
        let rect = centered(area, WIDTH, h);
        let inner = frame.panel(
            rect,
            Some("CPU affinity"),
            theme::panel_border(),
            theme::panel_bg(),
        );
        frame.hit(rect, Hit::OverlayBody { overlay: index });
        let Some(buttons_y) = widgets::last_row(inner) else {
            return;
        };
        let x = inner.x.saturating_add(1);
        let w = inner.width.saturating_sub(2);
        let rows = buttons_y.saturating_sub(inner.y);
        let row = |i: usize| (i < rows as usize).then(|| inner.y + i as u16);
        if let Some(y) = row(0) {
            let who = match self.targets.as_slice() {
                [key] => format!("{}  ·  PID {}", state.name_of(key), key.pid),
                many => format!("{} processes", many.len()),
            };
            frame.text(
                Rect::new(x, y, w, 1),
                &truncate(&who, w as usize),
                widgets::body().patch(theme::bold()),
            );
        }
        for (i, on) in self.cpus.iter().enumerate() {
            let Some(y) = row(2 + i / cols) else { break };
            let cx = x.saturating_add((i % cols) as u16 * CELL_W);
            if cx >= x + w {
                continue;
            }
            let mut style = widgets::body();
            if self.hover == Target::Item(i) {
                style = style.patch(theme::hovered());
            }
            if i == self.cursor {
                style = style.patch(theme::accent()).patch(theme::bold());
            }
            let label = format!("{} {i}", widgets::checkbox(*on));
            let cw = (CELL_W - 1).min(x + w - cx);
            let r = Rect::new(cx, y, cw, 1);
            frame.text(r, &truncate(&label, cw as usize), style);
            frame.hit(
                r,
                Hit::OverlayItem {
                    overlay: index,
                    item: i,
                },
            );
        }
        if let Some(y) = row(3 + grid_rows) {
            frame.text(
                Rect::new(x, y, w, 1),
                &truncate(HINT, w as usize),
                widgets::muted(),
            );
        }
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
                    label: "All",
                    style: theme::button(),
                    control: ALL,
                },
                Button {
                    label: "None",
                    style: theme::button(),
                    control: NONE,
                },
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
                    if self.cancel_focused || self.apply(state) {
                        return OverlayOutcome::Close;
                    }
                }
                KeyCode::Left => self.move_cursor(-1, 0),
                KeyCode::Right => self.move_cursor(1, 0),
                KeyCode::Up => self.move_cursor(0, -1),
                KeyCode::Down => self.move_cursor(0, 1),
                KeyCode::Home => self.cursor = 0,
                KeyCode::End => self.cursor = self.cpus.len() - 1,
                KeyCode::Tab => self.cancel_focused = !self.cancel_focused,
                KeyCode::Space | KeyCode::Char(' ') => self.toggle(self.cursor),
                KeyCode::Char('a') => self.set_all(true),
                KeyCode::Char('n') => self.set_all(false),
                _ => {}
            },
            Event::Mouse(mouse) => {
                let target = widgets::resolve(state, index, mouse.position);
                match mouse.kind {
                    MouseEventKind::Move | MouseEventKind::Drag(_) => self.hover = target,
                    MouseEventKind::Down(MouseButton::Left) => match target {
                        Target::Item(i) => {
                            self.cursor = i.min(self.cpus.len() - 1);
                            self.toggle(i);
                        }
                        Target::Control(ALL) => self.set_all(true),
                        Target::Control(NONE) => self.set_all(false),
                        Target::Control(CANCEL) => return OverlayOutcome::Close,
                        Target::Control(APPLY) => {
                            if self.apply(state) {
                                return OverlayOutcome::Close;
                            }
                        }
                        Target::Control(_) | Target::Body => {}
                        Target::Outside => return OverlayOutcome::Close,
                    },
                    MouseEventKind::Down(_) if target == Target::Outside => {
                        return OverlayOutcome::Close;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        OverlayOutcome::Consumed
    }
}

/// Logical CPUs: from the latest system snapshot, else the OS.
fn cpu_count(state: &ExplorerState) -> usize {
    match state.store.system().cpus.len() {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
        n => n,
    }
}
