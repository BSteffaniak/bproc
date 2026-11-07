//! Column chooser: toggle and reorder the current preset's columns. Every
//! change applies live and persists.
use super::super::hits::Hit;
use super::super::paint::{Frame, centered, fit, theme, truncate};
use super::super::{ExplorerState, UiRequest};
use super::OverlayOutcome;
use super::widgets::{self, Button, Target, visible_scroll};
use crate::procs::columns::{ALL_COLUMNS, ColumnId, Preset};
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui::geometry::Rect;

pub const MOVE_UP: u16 = 1;
pub const MOVE_DOWN: u16 = 2;
pub const RESET: u16 = 3;
pub const DONE: u16 = 4;

const HINT: &str = "Space toggle · Shift+↑↓ reorder · Enter done";

pub struct ColumnChooser {
    pub preset: Preset,
    /// Enabled columns in display order.
    pub columns: Vec<ColumnId>,
    pub focus: ColumnId,
    scroll: usize,
    hover: Target,
}

struct Geometry {
    rect: Rect,
    inner: Rect,
    list: Rect,
    scroll: usize,
}

impl ColumnChooser {
    pub fn new(state: &ExplorerState) -> Self {
        let columns = state.table.columns().to_vec();
        let focus = columns.first().copied().unwrap_or(ALL_COLUMNS[0]);
        Self {
            preset: state.settings.preset,
            columns,
            focus,
            scroll: 0,
            hover: Target::Outside,
        }
    }

    /// Enabled columns first (in order), then the rest in registry order.
    pub fn display(&self) -> Vec<ColumnId> {
        let mut out = self.columns.clone();
        out.extend(ALL_COLUMNS.iter().filter(|c| !self.columns.contains(c)));
        out
    }

    fn focus_index(&self) -> usize {
        self.display()
            .iter()
            .position(|c| *c == self.focus)
            .unwrap_or(0)
    }

    fn geometry(&self, area: Rect) -> Geometry {
        let rows = self.display().len() as u16;
        let rect = centered(area, 64, rows.saturating_add(6));
        let inner = if rect.width >= 2 && rect.height >= 2 {
            Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2)
        } else {
            Rect::new(rect.x, rect.y, 0, 0)
        };
        let footer = if inner.height >= 8 { 4 } else { 1 };
        let list = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(footer),
        );
        let scroll = visible_scroll(
            self.scroll,
            Some(self.focus_index()),
            rows as usize,
            list.height as usize,
        );
        Geometry {
            rect,
            inner,
            list,
            scroll,
        }
    }

    fn apply(&self, state: &mut ExplorerState) {
        state.table.set_columns(self.columns.clone());
        state
            .settings
            .preset_columns
            .insert(self.preset.label().to_string(), self.columns.clone());
        state.table.mark_dirty();
        state.request(UiRequest::SaveSettings);
    }

    pub fn toggle(&mut self, column: ColumnId, state: &mut ExplorerState) {
        if let Some(i) = self.columns.iter().position(|c| *c == column) {
            if self.columns.len() == 1 {
                state.flash("At least one column is required");
                return;
            }
            self.columns.remove(i);
        } else {
            self.columns.push(column);
        }
        self.focus = column;
        self.apply(state);
    }

    /// Move the focused column within the enabled set.
    pub fn reorder(&mut self, delta: isize, state: &mut ExplorerState) {
        let Some(i) = self.columns.iter().position(|c| *c == self.focus) else {
            return;
        };
        let j = i as isize + delta;
        if j < 0 || j as usize >= self.columns.len() {
            return;
        }
        self.columns.swap(i, j as usize);
        self.apply(state);
    }

    fn reset(&mut self, state: &mut ExplorerState) {
        self.columns = self.preset.default_columns();
        if !self.columns.contains(&self.focus) {
            self.focus = self.columns.first().copied().unwrap_or(ALL_COLUMNS[0]);
        }
        state.settings.preset_columns.remove(self.preset.label());
        state.table.set_columns(self.columns.clone());
        state.table.mark_dirty();
        state.request(UiRequest::SaveSettings);
        state.flash(format!("{} columns reset", self.preset.label()));
    }

    fn move_focus(&mut self, delta: isize) {
        let display = self.display();
        let i = (self.focus_index() as isize + delta).clamp(0, display.len() as isize - 1);
        self.focus = display[i as usize];
    }

    pub fn draw(&self, frame: &mut Frame<'_, '_, '_>, area: Rect, index: usize) {
        let g = self.geometry(area);
        let title = format!("Columns · {}", self.preset.label());
        frame.panel(
            g.rect,
            Some(&title),
            theme::panel_border(),
            theme::panel_bg(),
        );
        frame.hit(g.rect, Hit::OverlayBody { overlay: index });
        let inner = g.inner;
        let Some(buttons_y) = widgets::last_row(inner) else {
            return;
        };
        let display = self.display();
        let x = inner.x.saturating_add(1);
        let w = inner.width.saturating_sub(2);
        let focus = self.focus_index();
        let position = self.columns.iter().position(|c| *c == self.focus);
        for row in 0..g.list.height as usize {
            let i = g.scroll + row;
            let Some(column) = display.get(i).copied() else {
                break;
            };
            let y = g.list.y + row as u16;
            let enabled = self.columns.contains(&column);
            let style = if i == focus {
                theme::selected()
            } else if self.hover == Target::Item(i) {
                widgets::body().patch(theme::hovered())
            } else {
                widgets::body()
            };
            let r = Rect::new(inner.x, y, inner.width, 1);
            frame.fill(r, style);
            frame.hit(
                r,
                Hit::OverlayItem {
                    overlay: index,
                    item: i,
                },
            );
            let line = format!(
                "   {} {} {}",
                widgets::checkbox(enabled),
                fit(column.title(), 8, false),
                column.description()
            );
            let title_style = if enabled {
                style.patch(theme::bold())
            } else {
                style
            };
            frame.text(
                Rect::new(x, y, w, 1),
                &truncate(&line, w as usize),
                title_style,
            );
            if i == focus
                && let Some(p) = position
                && w >= 3
            {
                let arrows = [
                    ("▲", MOVE_UP, p > 0),
                    ("▼", MOVE_DOWN, p + 1 < self.columns.len()),
                ];
                for (k, (symbol, control, active)) in arrows.into_iter().enumerate() {
                    let ar = Rect::new(x + k as u16, y, 1, 1);
                    let mut s = if active {
                        style.patch(theme::bold())
                    } else {
                        style.patch(theme::dim())
                    };
                    if active && self.hover == Target::Control(control) {
                        s = widgets::hovered(theme::button());
                    }
                    frame.text(ar, symbol, s);
                    if active {
                        frame.hit(
                            ar,
                            Hit::OverlayControl {
                                overlay: index,
                                control,
                            },
                        );
                    }
                }
            }
        }
        if inner.height >= 8 {
            let sep_y = g.list.y + g.list.height;
            let border = theme::panel_bg().patch(theme::panel_border());
            frame.text(
                Rect::new(inner.x, sep_y, inner.width, 1),
                &"─".repeat(inner.width as usize),
                border,
            );
            frame.cell(g.rect.x, sep_y, "├", border);
            frame.cell(g.rect.x + g.rect.width - 1, sep_y, "┤", border);
            let description = format!("{}: {}", self.focus.title(), self.focus.description());
            frame.text(
                Rect::new(x, sep_y + 1, w, 1),
                &truncate(&description, w as usize),
                widgets::body().patch(theme::accent()),
            );
            frame.text(
                Rect::new(x, sep_y + 2, w, 1),
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
                    label: "Reset to defaults",
                    style: theme::button(),
                    control: RESET,
                },
                Button {
                    label: "Done",
                    style: theme::button_primary(),
                    control: DONE,
                },
            ],
            Some(DONE),
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
            Event::Key(key) => {
                let shift = key.modifiers.shift;
                match key.key {
                    KeyCode::Escape | KeyCode::Enter => return OverlayOutcome::Close,
                    KeyCode::Up if shift => self.reorder(-1, state),
                    KeyCode::Down if shift => self.reorder(1, state),
                    KeyCode::Up | KeyCode::Char('k') => self.move_focus(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.move_focus(1),
                    KeyCode::Char('K') => self.reorder(-1, state),
                    KeyCode::Char('J') => self.reorder(1, state),
                    KeyCode::PageUp => self.move_focus(-10),
                    KeyCode::PageDown => self.move_focus(10),
                    KeyCode::Home => self.move_focus(-(ALL_COLUMNS.len() as isize)),
                    KeyCode::End => self.move_focus(ALL_COLUMNS.len() as isize),
                    KeyCode::Space | KeyCode::Char(' ') | KeyCode::Char('x') => {
                        self.toggle(self.focus, state)
                    }
                    _ => {}
                }
                self.scroll = self.geometry(state.last_area).scroll;
            }
            Event::Mouse(mouse) => {
                let target = widgets::resolve(state, index, mouse.position);
                match mouse.kind {
                    MouseEventKind::Move | MouseEventKind::Drag(_) => self.hover = target,
                    MouseEventKind::Down(MouseButton::Left) => match target {
                        Target::Item(i) => {
                            if let Some(column) = self.display().get(i).copied() {
                                self.toggle(column, state);
                                self.scroll = self.geometry(state.last_area).scroll;
                            }
                        }
                        Target::Control(MOVE_UP) => self.reorder(-1, state),
                        Target::Control(MOVE_DOWN) => self.reorder(1, state),
                        Target::Control(RESET) => self.reset(state),
                        Target::Control(DONE) => return OverlayOutcome::Close,
                        Target::Control(_) | Target::Body => {}
                        Target::Outside => return OverlayOutcome::Close,
                    },
                    MouseEventKind::Down(_) => {
                        if target == Target::Outside {
                            return OverlayOutcome::Close;
                        }
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        self.wheel(mouse.kind == MouseEventKind::ScrollUp, state.last_area);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        OverlayOutcome::Consumed
    }

    /// Scroll the list, dragging the focus along so it stays visible.
    fn wheel(&mut self, up: bool, area: Rect) {
        let g = self.geometry(area);
        let len = self.display().len();
        let visible = g.list.height as usize;
        if visible == 0 {
            return;
        }
        let max = len.saturating_sub(visible);
        let scroll = if up {
            g.scroll.saturating_sub(3)
        } else {
            (g.scroll + 3).min(max)
        };
        let focus = self.focus_index().clamp(scroll, scroll + visible - 1);
        if let Some(column) = self.display().get(focus) {
            self.focus = *column;
        }
        self.scroll = scroll;
    }
}
