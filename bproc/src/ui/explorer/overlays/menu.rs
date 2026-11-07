//! Reusable cascading menu: headers, separators, key hints, disabled rows,
//! ✓ toggles and `▸` submenus that open right (or left when there is no
//! room). Keyboard and mouse drive the same cursor.
use super::super::actions::Action;
use super::super::hits::Hit;
use super::super::paint::{Frame, fit, theme, truncate};
use super::super::{ExplorerState, UiRequest};
use super::OverlayOutcome;
use super::widgets::{self, Target, visible_scroll};
use crate::procs::ProcessKey;
use crate::procs::actions::Signal;
use crate::procs::columns::ColumnId;
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui::geometry::{Point, Rect};
use bmux_tui::text_width::display_width;

/// `Hit::OverlayItem::item` packs the menu level above this stride.
pub const LEVEL_STRIDE: usize = 1 << 16;

/// What activating an item does. Executed by `menus::execute` with the
/// menu's subject/targets.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuCommand {
    /// Run an action on the subject (selected first).
    Run(Action),
    Request(UiRequest),
    /// Copy text to the clipboard and flash "Copied {what}".
    Copy {
        text: String,
        what: String,
    },
    /// Replace the search text.
    Search(String),
    ClearSearch,
    /// Signal the targets (confirming per settings).
    Signal(Signal),
    /// Signal dialog for the targets with their descendants preselected.
    SignalTree,
    Renice,
    TogglePin,
    ToggleMark,
    ShowInTree,
    Sort(ColumnId),
    ToggleDescending,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    Command(MenuCommand),
    Submenu(Vec<MenuItem>),
    Separator,
    /// Non-interactive caption row.
    Header,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub label: String,
    /// Right-aligned dim key hint.
    pub hint: String,
    pub kind: ItemKind,
    pub enabled: bool,
    /// Draws a ✓ column; `Some(true)` shows the mark.
    pub checked: Option<bool>,
    /// Toggles keep the menu open and flip `checked` on activation.
    pub keep_open: bool,
}

impl MenuItem {
    fn new(label: impl Into<String>, kind: ItemKind) -> Self {
        Self {
            label: label.into(),
            hint: String::new(),
            kind,
            enabled: true,
            checked: None,
            keep_open: false,
        }
    }
    pub fn command(label: impl Into<String>, command: MenuCommand) -> Self {
        Self::new(label, ItemKind::Command(command))
    }
    pub fn submenu(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        let enabled = items.iter().any(MenuItem::selectable);
        Self::new(label, ItemKind::Submenu(items)).enabled(enabled)
    }
    pub fn separator() -> Self {
        Self::new("", ItemKind::Separator)
    }
    pub fn header(label: impl Into<String>) -> Self {
        Self::new(label, ItemKind::Header)
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    /// Show a ✓ when `on` (closes on activation, like a radio choice).
    pub fn check(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }
    /// An on/off toggle: ✓ when `on`, stays open when activated.
    pub fn toggle(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self.keep_open = true;
        self
    }
    pub fn selectable(&self) -> bool {
        self.enabled && matches!(self.kind, ItemKind::Command(_) | ItemKind::Submenu(_))
    }
    fn children(&self) -> Option<&[MenuItem]> {
        match &self.kind {
            ItemKind::Submenu(items) => Some(items),
            _ => None,
        }
    }
    /// Lowercase accelerator letter: first alphanumeric character.
    fn accelerator(&self) -> Option<char> {
        self.label
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_lowercase().next().unwrap_or(c))
    }
}

pub struct MenuOverlay {
    pub at: Point,
    pub items: Vec<MenuItem>,
    /// Highlighted item per open level; level `n + 1` is the submenu of
    /// level `n`'s highlighted item.
    pub cursor: Vec<Option<usize>>,
    scroll: Vec<usize>,
    /// Process a context menu was opened for.
    pub subject: Option<ProcessKey>,
    /// Processes that process actions apply to.
    pub targets: Vec<ProcessKey>,
}

/// Where a level is drawn and how far it is scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelGeometry {
    pub rect: Rect,
    pub scroll: usize,
}

impl MenuOverlay {
    pub fn new(at: Point, items: Vec<MenuItem>) -> Self {
        let first = first_selectable(&items);
        Self {
            at,
            items,
            cursor: vec![first],
            scroll: vec![0],
            subject: None,
            targets: Vec::new(),
        }
    }

    pub fn levels(&self) -> usize {
        self.cursor.len()
    }

    /// Items shown at `level` (0 = root).
    pub fn level_items(&self, level: usize) -> &[MenuItem] {
        let mut items: &[MenuItem] = &self.items;
        for l in 0..level {
            match self.cursor[l]
                .and_then(|i| items.get(i))
                .and_then(MenuItem::children)
            {
                Some(children) => items = children,
                None => return &[],
            }
        }
        items
    }

    fn level_items_mut(&mut self, level: usize) -> Option<&mut Vec<MenuItem>> {
        let mut items = &mut self.items;
        for l in 0..level {
            let i = self.cursor[l]?;
            match &mut items.get_mut(i)?.kind {
                ItemKind::Submenu(children) => items = children,
                _ => return None,
            }
        }
        Some(items)
    }

    /// The highlighted item of the deepest level.
    pub fn current(&self) -> Option<&MenuItem> {
        let level = self.levels() - 1;
        self.cursor[level].and_then(|i| self.level_items(level).get(i))
    }

    /// Rectangles of every open level, clamped inside `area`.
    pub fn geometry(&self, area: Rect) -> Vec<LevelGeometry> {
        let mut out: Vec<LevelGeometry> = Vec::new();
        for level in 0..self.levels() {
            let items = self.level_items(level);
            let (w, h) = measure(items);
            let w = w.min(area.width);
            let h = h.min(area.height);
            let (x, y) = match out.last() {
                None => {
                    let x = clamp_start(self.at.x, w, area.x, area.width);
                    let fits_below =
                        self.at.y.saturating_add(h) <= area.y.saturating_add(area.height);
                    let y = if fits_below {
                        self.at.y
                    } else {
                        self.at.y.saturating_sub(h)
                    };
                    (x, clamp_start(y, h, area.y, area.height))
                }
                Some(parent) => {
                    let p = parent.rect;
                    let parent_cursor = self.cursor[level - 1].unwrap_or(0);
                    let row_y =
                        p.y.saturating_add(1)
                            .saturating_add(parent_cursor.saturating_sub(parent.scroll) as u16);
                    let right = area.x.saturating_add(area.width);
                    let p_right = p.x.saturating_add(p.width);
                    let x = if p_right.saturating_add(w) <= right {
                        p_right
                    } else if p.x >= area.x.saturating_add(w) {
                        p.x - w
                    } else {
                        right.saturating_sub(w).max(area.x)
                    };
                    (
                        x,
                        clamp_start(row_y.saturating_sub(1), h, area.y, area.height),
                    )
                }
            };
            let visible = h.saturating_sub(2) as usize;
            let scroll =
                visible_scroll(self.scroll[level], self.cursor[level], items.len(), visible);
            out.push(LevelGeometry {
                rect: Rect::new(x, y, w, h),
                scroll,
            });
        }
        out
    }

    pub fn draw(&self, frame: &mut Frame<'_, '_, '_>, area: Rect, index: usize) {
        let geometry = self.geometry(area);
        let deepest = self.levels() - 1;
        for (level, g) in geometry.iter().enumerate() {
            self.draw_level(frame, level, *g, index, level == deepest);
        }
    }

    fn draw_level(
        &self,
        frame: &mut Frame<'_, '_, '_>,
        level: usize,
        g: LevelGeometry,
        index: usize,
        active: bool,
    ) {
        let items = self.level_items(level);
        let r = g.rect;
        let inner = frame.panel(r, None, theme::panel_border(), theme::panel_bg());
        frame.hit(r, Hit::OverlayBody { overlay: index });
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let check_w = if items.iter().any(|i| i.checked.is_some()) {
            2
        } else {
            0
        };
        let arrow_w = if items.iter().any(|i| i.children().is_some()) {
            2
        } else {
            0
        };
        let border = theme::panel_bg().patch(theme::panel_border());
        for row in 0..inner.height as usize {
            let idx = g.scroll + row;
            let Some(item) = items.get(idx) else { break };
            let y = inner.y + row as u16;
            let row_rect = Rect::new(inner.x, y, inner.width, 1);
            match &item.kind {
                ItemKind::Separator => {
                    frame.text(row_rect, &"─".repeat(inner.width as usize), border);
                    if r.width >= 2 {
                        frame.cell(r.x, y, "├", border);
                        frame.cell(r.x + r.width - 1, y, "┤", border);
                    }
                    continue;
                }
                ItemKind::Header => {
                    let text = truncate(&item.label, inner.width.saturating_sub(2) as usize);
                    frame.fill(row_rect, theme::panel_bg());
                    frame.text(
                        Rect::new(inner.x + 1, y, inner.width.saturating_sub(1), 1),
                        &text,
                        theme::panel_bg()
                            .patch(theme::accent())
                            .patch(theme::bold()),
                    );
                    continue;
                }
                _ => {}
            }
            let highlighted = self.cursor[level] == Some(idx) && item.selectable();
            let style = if highlighted && active {
                theme::selected()
            } else if highlighted {
                theme::panel_bg().patch(theme::selected_unfocused())
            } else if !item.enabled {
                widgets::muted()
            } else {
                theme::panel_bg()
            };
            let hint_style = if highlighted || !item.enabled {
                style
            } else {
                widgets::muted()
            };
            frame.fill(row_rect, style);
            let width = inner.width as usize;
            let mut x = inner.x + 1;
            if check_w > 0 && width > 2 {
                if item.checked == Some(true) {
                    frame.text(Rect::new(x, y, 1, 1), "✓", style.patch(theme::bold()));
                }
                x += check_w;
            }
            let used = (x - inner.x) as usize;
            let tail = arrow_w + 1;
            let avail = width.saturating_sub(used + tail);
            let hint_w = display_width(&item.hint);
            let label_w = display_width(&item.label);
            let show_hint = hint_w > 0 && label_w + 2 + hint_w <= avail;
            let label_room = if show_hint { avail - hint_w - 2 } else { avail };
            frame.text(
                Rect::new(x, y, label_room as u16, 1),
                &truncate(&item.label, label_room),
                style,
            );
            if show_hint {
                let hx = inner.x + (width - tail - hint_w) as u16;
                frame.text(Rect::new(hx, y, hint_w as u16, 1), &item.hint, hint_style);
            }
            if item.children().is_some() && width > 2 {
                frame.text(Rect::new(inner.x + width as u16 - 2, y, 1, 1), "▸", style);
            }
            if item.selectable() {
                frame.hit(
                    row_rect,
                    Hit::OverlayItem {
                        overlay: index,
                        item: level * LEVEL_STRIDE + idx,
                    },
                );
            }
        }
        if r.width > 4 {
            let marker_x = r.x + r.width - 3;
            if g.scroll > 0 {
                frame.cell(marker_x, r.y, "▲", border);
            }
            if g.scroll + (inner.height as usize) < items.len() {
                frame.cell(marker_x, r.y + r.height - 1, "▼", border);
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
            Event::Key(key) => {
                let outcome = self.handle_key(key.key, state);
                self.sync_scroll(state.last_area);
                outcome
            }
            Event::Mouse(mouse) => {
                let target = widgets::resolve(state, index, mouse.position);
                let item = match target {
                    Target::Item(item) => Some((item / LEVEL_STRIDE, item % LEVEL_STRIDE)),
                    _ => None,
                };
                match mouse.kind {
                    MouseEventKind::Move | MouseEventKind::Drag(_) => {
                        if let Some((level, idx)) = item {
                            self.hover(level, idx);
                        }
                        OverlayOutcome::Consumed
                    }
                    MouseEventKind::Down(MouseButton::Left | MouseButton::Right) => {
                        match (target, item) {
                            (_, Some((level, idx))) => {
                                self.hover(level, idx);
                                self.activate(level, idx, false, state)
                            }
                            (Target::Body, _) => OverlayOutcome::Consumed,
                            _ => OverlayOutcome::Close,
                        }
                    }
                    MouseEventKind::Down(_) => match target {
                        Target::Outside => OverlayOutcome::Close,
                        _ => OverlayOutcome::Consumed,
                    },
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let up = mouse.kind == MouseEventKind::ScrollUp;
                        if let Some(level) = self.level_at(mouse.position, state.last_area) {
                            self.wheel(level, up, state.last_area);
                        }
                        OverlayOutcome::Consumed
                    }
                    _ => OverlayOutcome::Consumed,
                }
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    fn handle_key(&mut self, key: KeyCode, state: &mut ExplorerState) -> OverlayOutcome {
        let level = self.levels() - 1;
        match key {
            KeyCode::Up => self.step(level, -1),
            KeyCode::Down | KeyCode::Tab => self.step(level, 1),
            KeyCode::Home => self.cursor[level] = first_selectable(self.level_items(level)),
            KeyCode::End => {
                let items = self.level_items(level);
                self.cursor[level] = (0..items.len()).rev().find(|&i| items[i].selectable());
            }
            KeyCode::PageUp => {
                for _ in 0..8 {
                    self.step_clamped(level, -1);
                }
            }
            KeyCode::PageDown => {
                for _ in 0..8 {
                    self.step_clamped(level, 1);
                }
            }
            KeyCode::Right => {
                if let Some(idx) = self.cursor[level]
                    && self.current().is_some_and(|i| i.children().is_some())
                {
                    return self.activate(level, idx, true, state);
                }
            }
            KeyCode::Enter | KeyCode::Space => {
                if let Some(idx) = self.cursor[level] {
                    return self.activate(level, idx, true, state);
                }
            }
            KeyCode::Left => self.close_level(),
            KeyCode::Escape | KeyCode::Backspace => {
                if level == 0 {
                    return OverlayOutcome::Close;
                }
                self.close_level();
            }
            KeyCode::Char(' ') => {
                if let Some(idx) = self.cursor[level] {
                    return self.activate(level, idx, true, state);
                }
            }
            KeyCode::Char(c) => return self.accelerate(level, c, state),
            _ => {}
        }
        OverlayOutcome::Consumed
    }

    /// Jump to the next item whose first letter is `c`; activate it when it
    /// is the only match.
    fn accelerate(&mut self, level: usize, c: char, state: &mut ExplorerState) -> OverlayOutcome {
        let c = c.to_lowercase().next().unwrap_or(c);
        let items = self.level_items(level);
        let matches: Vec<usize> = (0..items.len())
            .filter(|&i| items[i].selectable() && items[i].accelerator() == Some(c))
            .collect();
        match matches.as_slice() {
            [] => OverlayOutcome::Consumed,
            [only] => {
                let only = *only;
                self.cursor[level] = Some(only);
                self.activate(level, only, true, state)
            }
            many => {
                let current = self.cursor[level];
                let next = many
                    .iter()
                    .copied()
                    .find(|&i| current.is_none_or(|c| i > c))
                    .unwrap_or(many[0]);
                self.cursor[level] = Some(next);
                OverlayOutcome::Consumed
            }
        }
    }

    fn step(&mut self, level: usize, delta: isize) {
        let items = self.level_items(level);
        let n = items.len();
        if n == 0 {
            return;
        }
        let start = match self.cursor[level] {
            Some(i) => i as isize,
            None if delta > 0 => -1,
            None => n as isize,
        };
        for k in 1..=n as isize {
            let i = (start + delta * k).rem_euclid(n as isize) as usize;
            if items[i].selectable() {
                self.cursor[level] = Some(i);
                return;
            }
        }
    }

    fn step_clamped(&mut self, level: usize, delta: isize) {
        let items = self.level_items(level);
        let mut i = match self.cursor[level] {
            Some(i) => i as isize,
            None => return self.step(level, delta),
        };
        loop {
            i += delta;
            if i < 0 || i as usize >= items.len() {
                return;
            }
            if items[i as usize].selectable() {
                self.cursor[level] = Some(i as usize);
                return;
            }
        }
    }

    fn close_level(&mut self) {
        if self.levels() > 1 {
            self.cursor.pop();
            self.scroll.pop();
        }
    }

    /// Highlight `idx` at `level` from the pointer, opening its submenu.
    fn hover(&mut self, level: usize, idx: usize) {
        if level >= self.levels() {
            return;
        }
        let already_open = self.levels() > level + 1 && self.cursor[level] == Some(idx);
        if already_open {
            self.cursor.truncate(level + 2);
            self.scroll.truncate(level + 2);
            return;
        }
        self.cursor.truncate(level + 1);
        self.scroll.truncate(level + 1);
        self.cursor[level] = Some(idx);
        if self
            .level_items(level)
            .get(idx)
            .is_some_and(|i| i.selectable() && i.children().is_some())
        {
            self.cursor.push(None);
            self.scroll.push(0);
        }
    }

    fn activate(
        &mut self,
        level: usize,
        idx: usize,
        keyboard: bool,
        state: &mut ExplorerState,
    ) -> OverlayOutcome {
        let Some(item) = self.level_items(level).get(idx).cloned() else {
            return OverlayOutcome::Consumed;
        };
        if !item.selectable() {
            return OverlayOutcome::Consumed;
        }
        match item.kind {
            ItemKind::Submenu(children) => {
                self.cursor.truncate(level + 1);
                self.scroll.truncate(level + 1);
                self.cursor[level] = Some(idx);
                self.cursor.push(if keyboard {
                    first_selectable(&children)
                } else {
                    None
                });
                self.scroll.push(0);
                OverlayOutcome::Consumed
            }
            ItemKind::Command(command) => {
                let subject = self.subject;
                let targets = self.targets.clone();
                super::menus::execute(&command, subject, &targets, state);
                if item.keep_open {
                    if let Some(items) = self.level_items_mut(level)
                        && let Some(it) = items.get_mut(idx)
                    {
                        it.checked = it.checked.map(|c| !c);
                    }
                    OverlayOutcome::Consumed
                } else {
                    OverlayOutcome::Close
                }
            }
            ItemKind::Separator | ItemKind::Header => OverlayOutcome::Consumed,
        }
    }

    fn level_at(&self, position: Point, area: Rect) -> Option<usize> {
        self.geometry(area)
            .iter()
            .rposition(|g| super::super::hits::contains(g.rect, position))
    }

    fn wheel(&mut self, level: usize, up: bool, area: Rect) {
        let geometry = self.geometry(area);
        let Some(g) = geometry.get(level) else { return };
        self.cursor.truncate(level + 1);
        self.scroll.truncate(level + 1);
        self.cursor[level] = None;
        let len = self.level_items(level).len();
        let visible = g.rect.height.saturating_sub(2) as usize;
        let max = len.saturating_sub(visible);
        self.scroll[level] = if up {
            g.scroll.saturating_sub(3)
        } else {
            (g.scroll + 3).min(max)
        };
    }

    /// Persist the scroll offsets that keep keyboard cursors visible.
    fn sync_scroll(&mut self, area: Rect) {
        let geometry = self.geometry(area);
        for (level, g) in geometry.iter().enumerate() {
            self.scroll[level] = g.scroll;
        }
    }
}

fn first_selectable(items: &[MenuItem]) -> Option<usize> {
    items.iter().position(MenuItem::selectable)
}

/// Outer size of a level (borders included).
fn measure(items: &[MenuItem]) -> (u16, u16) {
    let check = if items.iter().any(|i| i.checked.is_some()) {
        2
    } else {
        0
    };
    let arrow = if items.iter().any(|i| i.children().is_some()) {
        2
    } else {
        0
    };
    let label = items
        .iter()
        .filter(|i| !matches!(i.kind, ItemKind::Header))
        .map(|i| display_width(&i.label))
        .max()
        .unwrap_or(0);
    let header = items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Header))
        .map(|i| display_width(&i.label) + 1)
        .max()
        .unwrap_or(0);
    let hint = items
        .iter()
        .map(|i| display_width(&i.hint))
        .max()
        .unwrap_or(0);
    let hint = if hint > 0 { hint + 2 } else { 0 };
    let inner = (1 + check + label + hint + arrow + 1)
        .max(header + 1)
        .max(10);
    let w = (inner + 2).min(u16::MAX as usize) as u16;
    let h = (items.len() + 2).min(u16::MAX as usize) as u16;
    (w, h)
}

/// Start coordinate so a span of `len` starting near `want` stays within
/// `[origin, origin + extent)`.
fn clamp_start(want: u16, len: u16, origin: u16, extent: u16) -> u16 {
    let max = origin.saturating_add(extent.saturating_sub(len));
    want.clamp(origin, max.max(origin))
}

/// Pad a column label for menus: `TITLE   description`.
pub fn column_label(column: ColumnId) -> String {
    let desc = column.description();
    let short = desc
        .split(" (")
        .next()
        .unwrap_or(desc)
        .split(", ")
        .next()
        .unwrap_or(desc);
    format!("{} {}", fit(column.title(), 7, false), truncate(short, 30))
}
