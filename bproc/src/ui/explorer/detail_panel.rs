//! Process inspector, docked (bottom/right) or full screen. Tabs: Overview
//! (sparklines), Threads, Files, Network, Environment, Memory, Tree, Limits,
//! Events. Action buttons and ancestor breadcrumbs are clickable.
mod graph;
mod tabs;
#[cfg(test)]
mod tests;
mod text;

use super::actions::Action;
use super::hits::Hit;
use super::paint::{Frame, centered, theme, truncate};
use super::{Click, Drag, ExplorerState, Outcome, UiRequest};
use crate::config::DockPosition;
use crate::procs::ProcessKey;
use crate::procs::detail::DeepDetail;
use crate::procs::model::ProcState;
use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::event::{MouseButton, MouseEvent, MouseEventKind};
use bmux_tui::geometry::Rect;
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text_width::display_width;
use tabs::Body;
use text::{BLine, Seg, paint_line, paint_lines};

/// Inspector tabs in display order; `Hit::DetailTab(i)` indexes `Tab::ALL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Tab {
    #[default]
    Overview,
    Threads,
    Files,
    Network,
    Env,
    Memory,
    Tree,
    Limits,
    Events,
}

impl Tab {
    pub const ALL: [Tab; 9] = [
        Tab::Overview,
        Tab::Threads,
        Tab::Files,
        Tab::Network,
        Tab::Env,
        Tab::Memory,
        Tab::Tree,
        Tab::Limits,
        Tab::Events,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Threads => "Threads",
            Self::Files => "Files",
            Self::Network => "Network",
            Self::Env => "Env",
            Self::Memory => "Memory",
            Self::Tree => "Tree",
            Self::Limits => "Limits",
            Self::Events => "Events",
        }
    }
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }
}

#[derive(Debug, Default)]
pub struct DetailPanel {
    key: Option<ProcessKey>,
    full: bool,
    detail: Option<DeepDetail>,
    tab: Tab,
    scroll: [usize; Tab::ALL.len()],
    /// Scrollable rows and visible body rows at the last paint (for clamping).
    content_rows: usize,
    body_rows: usize,
    /// Area painted last frame.
    area: Rect,
    /// Panel extent (rows or cells) and saved `dock_size` when a splitter
    /// drag began.
    drag_from: Option<(u16, u16)>,
    /// Keyboard row cursor per tab (full screen): an index into `targets`.
    cursor: [Option<usize>; Tab::ALL.len()],
    /// Actionable rows of the current tab at the last paint: scroll-line
    /// index and the click action that row performs.
    targets: Vec<(usize, Hit)>,
}

impl DetailPanel {
    pub fn open(&mut self, key: ProcessKey, full: bool) {
        if self.key != Some(key) {
            self.detail = None;
            self.scroll = Default::default();
            self.cursor = Default::default();
            self.targets.clear();
        }
        self.key = Some(key);
        self.full = full;
    }
    pub fn close(&mut self) {
        self.key = None;
        self.full = false;
        self.detail = None;
        self.drag_from = None;
    }
    pub fn key(&self) -> Option<ProcessKey> {
        self.key
    }
    pub fn is_open(&self) -> bool {
        self.key.is_some()
    }
    pub fn is_full(&self) -> bool {
        self.key.is_some() && self.full
    }
    pub fn is_docked(&self) -> bool {
        self.key.is_some() && !self.full
    }
    pub fn set_full(&mut self, full: bool) {
        self.full = full;
    }
    /// Accept a collected detail if it is for the open process.
    pub fn apply(&mut self, detail: DeepDetail) {
        if Some(detail.key) == self.key {
            self.detail = Some(detail);
        }
    }
    pub fn detail(&self) -> Option<&DeepDetail> {
        self.detail.as_ref()
    }
    pub fn tab(&self) -> Tab {
        self.tab
    }
    pub fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.content_rows = usize::MAX;
            self.targets.clear();
        }
    }
    /// Cycle tabs by `delta`, wrapping.
    pub fn cycle_tab(&mut self, delta: isize) {
        let n = Tab::ALL.len() as isize;
        let i = (self.tab.index() as isize + delta).rem_euclid(n);
        self.set_tab(Tab::from_index(i as usize));
    }
    /// Scroll offset of the current tab.
    pub fn scroll(&self) -> usize {
        self.scroll[self.tab.index()]
    }
    fn max_scroll(&self) -> usize {
        self.content_rows.saturating_sub(self.body_rows)
    }
    pub fn scroll_by(&mut self, delta: isize) {
        let i = self.tab.index();
        let next = self.scroll[i].saturating_add_signed(delta);
        self.scroll[i] = next.min(self.max_scroll());
    }
    pub fn scroll_to(&mut self, offset: usize) {
        let i = self.tab.index();
        self.scroll[i] = offset.min(self.max_scroll());
    }
    fn page(&self) -> isize {
        self.body_rows.saturating_sub(1).max(1) as isize
    }
    /// Scroll-line index of the keyboard cursor in the current tab, if any.
    pub fn cursor_line(&self) -> Option<usize> {
        let c = self.cursor[self.tab.index()]?;
        self.targets.get(c).map(|(line, _)| *line)
    }
    /// Click action of the row under the keyboard cursor.
    fn cursor_hit(&self) -> Option<Hit> {
        let c = self.cursor[self.tab.index()]?;
        self.targets.get(c).map(|(_, hit)| hit.clone())
    }
    /// Scroll so scroll-line `line` is visible.
    fn reveal(&mut self, line: usize) {
        let top = self.scroll();
        if line < top {
            self.scroll_to(line);
        } else if self.body_rows > 0 && line >= top + self.body_rows {
            self.scroll_to(line + 1 - self.body_rows);
        }
    }
    /// Move the row cursor by `delta` actionable rows. Tabs without
    /// actionable rows scroll instead.
    fn move_cursor(&mut self, delta: isize) {
        let n = self.targets.len();
        if n == 0 {
            self.scroll_by(delta);
            return;
        }
        let i = self.tab.index();
        let next = match self.cursor[i].filter(|c| *c < n) {
            Some(c) => c.saturating_add_signed(delta).min(n - 1),
            // First press: start on the first visible actionable row.
            None => {
                let top = self.scroll();
                self.targets
                    .iter()
                    .position(|(line, _)| *line >= top)
                    .unwrap_or(0)
            }
        };
        self.cursor[i] = Some(next);
        if next == 0 && delta < 0 {
            // Reveal any heading above the first row.
            self.scroll_to(0);
        }
        self.reveal(self.targets[next].0);
    }
    /// After a scroll, keep the cursor on a visible row when one exists.
    fn snap_cursor(&mut self) {
        let i = self.tab.index();
        let Some(c) = self.cursor[i] else {
            return;
        };
        if self.targets.is_empty() {
            self.cursor[i] = None;
            return;
        }
        let c = c.min(self.targets.len() - 1);
        self.cursor[i] = Some(c);
        let top = self.scroll();
        let bottom = top + self.body_rows;
        let line = self.targets[c].0;
        if (top..bottom).contains(&line) {
            return;
        }
        let visible = |(_, (l, _)): &(usize, &(usize, Hit))| (top..bottom).contains(l);
        let snapped = if line < top {
            self.targets.iter().enumerate().find(visible)
        } else {
            self.targets.iter().enumerate().rev().find(visible)
        };
        if let Some((j, _)) = snapped {
            self.cursor[i] = Some(j);
        }
    }
}

/// Collection caveats worth surfacing above the tab body: limited access
/// (not root) and truncated sections.
pub fn notice(detail: &DeepDetail) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(access) = detail.fact("Access") {
        parts.push(format!("Access {access}"));
    }
    if detail.fact("Truncated").is_some() {
        parts.extend(
            detail
                .facts
                .iter()
                .filter(|(name, _)| name == "Truncated")
                .map(|(_, v)| v.clone()),
        );
    }
    (!parts.is_empty()).then(|| format!(" ⚠ {}", parts.join(" · ")))
}

fn notice_style() -> Style {
    Style::new().fg(Color::Yellow).add_modifier(Modifier::DIM)
}

/// Rows of the panel, top to bottom.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Plan {
    title: Rect,
    crumbs: Rect,
    tabs: Rect,
    banner: Rect,
    notice: Rect,
    body: Rect,
    actions: Rect,
}

fn plan(inner: Rect, ghost: bool, notice: bool) -> Plan {
    let h = inner.height;
    let row = |y: u16, on: bool| Rect::new(inner.x, y, inner.width, u16::from(on));
    let mut y = inner.y;
    let title = row(y, h >= 1);
    y = y.saturating_add(title.height);
    let crumbs = row(y, h >= 8);
    y = y.saturating_add(crumbs.height);
    let tabs = row(y, h >= 2);
    y = y.saturating_add(tabs.height);
    let banner = row(y, ghost && h >= 5);
    y = y.saturating_add(banner.height);
    let notice = row(y, notice && h >= 7);
    y = y.saturating_add(notice.height);
    let actions_on = h >= 6;
    let bottom = inner.y.saturating_add(h);
    let actions = Rect::new(
        inner.x,
        bottom.saturating_sub(1),
        inner.width,
        u16::from(actions_on),
    );
    let body_end = bottom.saturating_sub(actions.height);
    let body = Rect::new(inner.x, y, inner.width, body_end.saturating_sub(y));
    Plan {
        title,
        crumbs,
        tabs,
        banner,
        notice,
        body,
        actions,
    }
}

/// Docked panel follows the table selection.
fn follow_selection(state: &mut ExplorerState) {
    if !state.detail.is_docked() {
        return;
    }
    if let Some(selected) = state.table.selected()
        && state.detail.key() != Some(selected)
    {
        state.detail.open(selected, false);
        state.request(UiRequest::LoadDetail(selected));
    }
}

fn is_ghost(state: &ExplorerState, key: &ProcessKey) -> bool {
    state.store.get(key).is_none_or(|r| r.is_ghost())
}

pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    follow_selection(state);
    let Some(key) = state.detail.key() else {
        return;
    };
    state.detail.area = area;
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.fill(area, theme::base());
    let docked = state.detail.is_docked();
    let mut inner = area;
    if docked && state.settings.dock == DockPosition::Right {
        for y in area.y..area.y.saturating_add(area.height) {
            frame.cell(area.x, y, "┃", theme::panel_border());
        }
        frame.hit(
            Rect::new(area.x, area.y, 1, area.height),
            Hit::DetailSplitter,
        );
        inner = Rect::new(area.x + 1, area.y, area.width - 1, area.height);
        if inner.width == 0 {
            return;
        }
    }
    let ghost = is_ghost(state, &key);
    let caveat = state.detail.detail().and_then(notice);
    let p = plan(inner, ghost, caveat.is_some());
    draw_title(frame, p.title, state, key, ghost);
    draw_crumbs(frame, p.crumbs, state, key);
    draw_tabs(frame, p.tabs, state, key);
    if p.banner.height > 0 {
        frame.fill(p.banner, theme::error().add_modifier(Modifier::BOLD));
        frame.text(
            p.banner,
            &truncate(
                " ✕ Process exited — showing its last known state",
                usize::from(p.banner.width),
            ),
            theme::error().add_modifier(Modifier::BOLD),
        );
    }
    if let Some(caveat) = caveat.filter(|_| p.notice.height > 0) {
        frame.text(
            p.notice,
            &truncate(&caveat, usize::from(p.notice.width)),
            notice_style(),
        );
    }
    draw_body(frame, p.body, state, key);
    draw_actions(frame, p.actions, state, key, ghost);
}

fn draw_title(
    frame: &mut Frame<'_, '_, '_>,
    r: Rect,
    state: &ExplorerState,
    key: ProcessKey,
    ghost: bool,
) {
    if r.height == 0 || r.width == 0 {
        return;
    }
    let style = theme::header();
    frame.fill(r, style);
    let docked = state.detail.is_docked();
    let bottom_dock = docked && state.settings.dock == DockPosition::Bottom;
    if bottom_dock {
        frame.hit(r, Hit::DetailSplitter);
    }
    let close = "[✕]";
    let toggle = if state.detail.is_full() {
        "[⤡]"
    } else {
        "[⤢]"
    };
    let bw = display_width(close) as u16;
    let mut right = r.x.saturating_add(r.width);
    if r.width >= bw + 6 {
        right = right.saturating_sub(bw + 1);
        let at = Rect::new(right, r.y, bw, 1);
        frame.text(at, close, theme::header_active());
        frame.hit(at, Hit::DetailClose);
        if r.width >= bw * 2 + 14 {
            right = right.saturating_sub(bw + 1);
            let at = Rect::new(right, r.y, bw, 1);
            frame.text(at, toggle, theme::header_active());
            frame.hit(at, Hit::DetailDockToggle);
        }
    }
    let row = state.store.get(&key);
    let name = state.name_of(&key);
    let user = row
        .and_then(|r| r.entry.record.user.clone())
        .or_else(|| row.and_then(|r| r.entry.record.uid.map(|u| format!("uid {u}"))))
        .unwrap_or_else(|| "–".into());
    let status = if ghost {
        "exited".to_string()
    } else {
        row.map(|r| r.entry.sample.state.label().to_lowercase())
            .unwrap_or_default()
    };
    let grip = if bottom_dock { " ⠿ " } else { " " };
    let avail = usize::from(right.saturating_sub(r.x).saturating_sub(1));
    let rest = format!(" · PID {} · {user} · {status}", key.pid);
    let line = BLine::new(vec![
        Seg::new(grip, style),
        Seg::new(name, style.add_modifier(Modifier::BOLD)),
        Seg::new(rest, style),
    ]);
    let full: String = line.text();
    let clipped = truncate(&full, avail);
    if clipped == full {
        paint_line(frame, Rect::new(r.x, r.y, avail as u16, 1), &line);
    } else {
        frame.text(
            Rect::new(r.x, r.y, avail as u16, 1),
            &clipped,
            style.add_modifier(Modifier::BOLD),
        );
    }
}

fn draw_crumbs(frame: &mut Frame<'_, '_, '_>, r: Rect, state: &ExplorerState, key: ProcessKey) {
    if r.height == 0 || r.width < 4 {
        return;
    }
    let mut chain = state.store.ancestors(&key);
    chain.reverse();
    let sep = Seg::new(" › ", theme::dim());
    let me = Seg::new(truncate(&state.name_of(&key), 32), theme::bold());
    let width = usize::from(r.width).saturating_sub(1);
    // Keep the nearest ancestors; elide the oldest with "…".
    let mut kept: Vec<Seg> = Vec::new();
    let mut used = me.width();
    let mut elided = false;
    for ancestor in chain.iter().rev() {
        let seg = Seg::link(
            truncate(&state.name_of(ancestor), 24),
            theme::accent(),
            Hit::ProcessLink { key: *ancestor },
        );
        let need = seg.width() + sep.width();
        if used + need + 4 > width {
            elided = true;
            break;
        }
        used += need;
        kept.push(seg);
    }
    let mut segs = vec![Seg::new(" ", theme::base())];
    if elided {
        segs.push(Seg::new("…", theme::dim()));
        segs.push(sep.clone());
    }
    for seg in kept.into_iter().rev() {
        segs.push(seg);
        segs.push(sep.clone());
    }
    segs.push(me);
    paint_line(frame, r, &BLine::new(segs));
}

fn tab_label(state: &ExplorerState, key: &ProcessKey, tab: Tab) -> String {
    match tabs::count(state, key, tab) {
        Some(n) => format!(" {} {n} ", tab.label()),
        None => format!(" {} ", tab.label()),
    }
}

fn draw_tabs(frame: &mut Frame<'_, '_, '_>, r: Rect, state: &ExplorerState, key: ProcessKey) {
    if r.height == 0 || r.width == 0 {
        return;
    }
    let labels: Vec<String> = Tab::ALL
        .iter()
        .map(|t| tab_label(state, &key, *t))
        .collect();
    let widths: Vec<usize> = labels.iter().map(|l| display_width(l) + 1).collect();
    let active = state.detail.tab.index();
    let width = usize::from(r.width);
    // First visible tab such that the active one fits (with room for arrows).
    let mut first = 0;
    let total: usize = widths.iter().sum();
    if total > width {
        while first < active && widths[first..=active].iter().sum::<usize>() + 4 > width {
            first += 1;
        }
    }
    let mut x = r.x;
    let end = r.x.saturating_add(r.width);
    if first > 0 {
        frame.text(Rect::new(x, r.y, 2, 1), "‹ ", theme::accent());
        frame.hit(Rect::new(x, r.y, 1, 1), Hit::DetailTab(first - 1));
        x = x.saturating_add(2);
    }
    let mut last_shown = first;
    for (i, label) in labels.iter().enumerate().skip(first) {
        let w = display_width(label) as u16;
        let more = i + 1 < labels.len();
        let reserve = if more { 2 } else { 0 };
        if x.saturating_add(w).saturating_add(reserve) > end && i != active {
            break;
        }
        let w = w.min(end.saturating_sub(x));
        if w == 0 {
            break;
        }
        let style = if i == active {
            theme::header_active()
        } else {
            theme::base()
        };
        let at = Rect::new(x, r.y, w, 1);
        frame.text(at, label, style);
        frame.hit(at, Hit::DetailTab(i));
        last_shown = i;
        x = x.saturating_add(w);
        if x < end && more {
            frame.cell(x, r.y, "│", theme::dim());
            x = x.saturating_add(1);
        }
    }
    if last_shown + 1 < labels.len() && x < end {
        let at = Rect::new(end - 1, r.y, 1, 1);
        frame.text(at, "›", theme::accent());
        frame.hit(at, Hit::DetailTab(last_shown + 1));
    }
}

fn draw_body(frame: &mut Frame<'_, '_, '_>, r: Rect, state: &mut ExplorerState, key: ProcessKey) {
    if r.height == 0 || r.width == 0 {
        state.detail.body_rows = 0;
        return;
    }
    frame.hit(r, Hit::DetailBody);
    let content = Rect::new(
        r.x.saturating_add(1),
        r.y,
        r.width.saturating_sub(2).max(1).min(r.width),
        r.height,
    );
    let body = {
        let shared: &ExplorerState = state;
        let cx = tabs::Ctx {
            state: shared,
            key,
            row: shared.store.get(&key),
            detail: shared.detail.detail(),
            width: usize::from(content.width),
            height: usize::from(content.height),
        };
        tabs::build(&cx, shared.detail.tab())
    };
    let mut body = body;
    let panel = &mut state.detail;
    panel.content_rows = body.scroll_rows();
    panel.body_rows = usize::from(content.height);
    let scroll = panel.scroll().min(panel.max_scroll());
    panel.scroll[panel.tab.index()] = scroll;
    // The row cursor is for list tabs; the overview's arrows keep scrolling
    // its (possibly long) fact list.
    panel.targets = if panel.tab == Tab::Overview {
        Vec::new()
    } else {
        targets(&body)
    };
    panel.snap_cursor();
    if panel.is_full()
        && let Some(line) = panel.cursor_line()
        && let Some(lines) = body.scroll_lines_mut()
        && let Some(target) = lines.get_mut(line)
    {
        highlight(target, usize::from(content.width));
    }
    match &body {
        Body::Lines(lines) => paint_lines(frame, content, lines, scroll),
        Body::Columns {
            left,
            right,
            left_width,
        } => {
            let lw = (*left_width).min(content.width);
            paint_lines(
                frame,
                Rect::new(content.x, content.y, lw, content.height),
                left,
                scroll,
            );
            let gap = 2u16;
            let rx = content.x.saturating_add(lw).saturating_add(gap);
            let rw = content.width.saturating_sub(lw + gap);
            if rw > 0 {
                for y in content.y..content.y.saturating_add(content.height) {
                    frame.cell(rx - 1, y, "│", theme::dim());
                }
                paint_lines(
                    frame,
                    Rect::new(rx, content.y, rw, content.height),
                    right,
                    0,
                );
            }
        }
        Body::Message(message) => {
            let lines: Vec<String> = message
                .lines()
                .flat_map(|l| text::wrap(l, usize::from(content.width)))
                .collect();
            let h = (lines.len() as u16).min(content.height);
            let block = centered(content, content.width, h);
            for (i, line) in lines.iter().take(usize::from(h)).enumerate() {
                let w = (display_width(line) as u16).min(block.width);
                let at = centered(Rect::new(block.x, block.y + i as u16, block.width, 1), w, 1);
                let style = if i == 0 {
                    theme::dim().add_modifier(Modifier::BOLD)
                } else {
                    theme::dim()
                };
                frame.text(at, line, style);
            }
        }
    }
    if panel.content_rows > panel.body_rows && content.height > 0 {
        draw_scrollbar(frame, r, panel.content_rows, panel.body_rows, scroll);
    }
}

/// Actionable rows of `body`: a row hit, else its first link segment.
fn targets(body: &Body) -> Vec<(usize, Hit)> {
    let Some(lines) = body.scroll_lines() else {
        return Vec::new();
    };
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let hit = line
                .hit
                .clone()
                .or_else(|| line.segs.iter().find_map(|s| s.hit.clone()))?;
            matches!(hit, Hit::DetailRow { .. } | Hit::ProcessLink { .. }).then_some((i, hit))
        })
        .collect()
}

/// Paint `line` as the keyboard cursor row, padded across `width`.
fn highlight(line: &mut BLine, width: usize) {
    let sel = theme::selected();
    let mut used = 0;
    for seg in &mut line.segs {
        seg.style = seg.style.patch(sel);
        used += seg.width();
    }
    if used < width {
        line.segs.push(Seg::new(" ".repeat(width - used), sel));
    }
}

fn draw_scrollbar(frame: &mut Frame<'_, '_, '_>, r: Rect, total: usize, visible: usize, at: usize) {
    let h = usize::from(r.height);
    if h == 0 || total == 0 {
        return;
    }
    let x = r.x + r.width - 1;
    let thumb = (visible * h / total).clamp(1, h);
    let max_at = total.saturating_sub(visible).max(1);
    let top = (at * (h - thumb)) / max_at;
    for i in 0..h {
        let symbol = if i >= top && i < top + thumb {
            "┃"
        } else {
            "│"
        };
        let style = if symbol == "┃" {
            theme::accent()
        } else {
            theme::dim()
        };
        frame.cell(x, r.y + i as u16, symbol, style);
    }
}

/// Button row: label, action, emphasis style.
fn buttons(
    state: &ExplorerState,
    key: &ProcessKey,
) -> Vec<(&'static str, Action, bmux_tui::style::Style)> {
    let stopped = state
        .store
        .get(key)
        .is_some_and(|r| r.entry.sample.state == ProcState::Stopped);
    let pinned = state.table.pinned().contains(key);
    vec![
        ("Terminate", Action::Terminate, theme::button()),
        ("Kill", Action::Kill, theme::button_danger()),
        if stopped {
            ("Resume", Action::Resume, theme::button_primary())
        } else {
            ("Suspend", Action::Suspend, theme::button())
        },
        ("Renice", Action::NiceUp, theme::button()),
        ("Throttle", Action::Throttle, theme::button()),
        ("Watch", Action::WatchProcess, theme::button()),
        (
            if pinned { "Unpin" } else { "Pin" },
            Action::Pin,
            theme::button(),
        ),
    ]
}

fn draw_actions(
    frame: &mut Frame<'_, '_, '_>,
    r: Rect,
    state: &ExplorerState,
    key: ProcessKey,
    ghost: bool,
) {
    if r.height == 0 || r.width == 0 {
        return;
    }
    let end = r.x.saturating_add(r.width);
    let mut x = r.x.saturating_add(1);
    for (label, action, style) in buttons(state, &key) {
        let text = format!(" {label} ");
        let w = display_width(&text) as u16;
        if x.saturating_add(w) > end {
            break;
        }
        let at = Rect::new(x, r.y, w, 1);
        if ghost {
            frame.text(at, &text, theme::dim().add_modifier(Modifier::CROSSED_OUT));
        } else {
            frame.text(at, &text, style);
            frame.hit(at, Hit::DetailAction(action));
        }
        x = x.saturating_add(w + 1);
    }
    let hint = if !state.detail.is_full() {
        "{ } tabs · ⇧↑↓ scroll · Enter full screen"
    } else if state.detail.tab() == Tab::Env {
        "Tab tabs · ↑↓ row · y copy · Esc dock"
    } else if !state.detail.targets.is_empty() {
        "Tab tabs · ↑↓ row · Enter open · Esc dock"
    } else {
        "Tab/⇧Tab tabs · ↑↓ scroll · Esc dock"
    };
    let hw = display_width(hint) as u16;
    if x.saturating_add(hw).saturating_add(2) <= end {
        frame.text(Rect::new(end - hw - 1, r.y, hw, 1), hint, theme::dim());
    }
}

// ---- input -----------------------------------------------------------------

/// Keys: `{`/`}` cycle tabs and Shift+↑↓/PgUp/PgDn scroll in either mode.
/// Full screen also takes Tab/←→ (tabs), ↑↓ (row cursor on tabs with
/// actionable rows, else scroll), PgUp/PgDn/Home/End (scroll), Enter
/// (activate the cursor row), `y` (copy the cursor's env var) and Esc.
pub fn handle_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    if !state.detail.is_open() || key.modifiers.ctrl || key.modifiers.alt {
        return Outcome::Ignored;
    }
    let shift = key.modifiers.shift;
    let panel = &mut state.detail;
    match key.key {
        KeyCode::Char('{') => panel.cycle_tab(-1),
        KeyCode::Char('}') => panel.cycle_tab(1),
        KeyCode::Up if shift => panel.scroll_by(-1),
        KeyCode::Down if shift => panel.scroll_by(1),
        KeyCode::PageUp if shift => panel.scroll_by(-panel.page()),
        KeyCode::PageDown if shift => panel.scroll_by(panel.page()),
        _ if !panel.is_full() => return Outcome::Ignored,
        KeyCode::Tab if shift => panel.cycle_tab(-1),
        KeyCode::Tab | KeyCode::Right => panel.cycle_tab(1),
        KeyCode::Left => panel.cycle_tab(-1),
        KeyCode::Up => panel.move_cursor(-1),
        KeyCode::Down => panel.move_cursor(1),
        KeyCode::PageUp => panel.scroll_by(-panel.page()),
        KeyCode::PageDown => panel.scroll_by(panel.page()),
        KeyCode::Home => panel.scroll_to(0),
        KeyCode::End => panel.scroll_to(usize::MAX),
        KeyCode::Enter => {
            let Some(hit) = panel.cursor_hit() else {
                return Outcome::Ignored;
            };
            return activate(state, &hit);
        }
        KeyCode::Char('y') => {
            return match panel.cursor_hit() {
                Some(hit @ Hit::DetailRow { .. }) => activate(state, &hit),
                _ if panel.tab() == Tab::Env => {
                    state.flash("Select a variable with ↑↓, then press y to copy it");
                    Outcome::Consumed
                }
                _ => Outcome::Ignored,
            };
        }
        KeyCode::Escape => {
            panel.set_full(false);
            if let Some(k) = panel.key() {
                state.table.select(k);
            }
        }
        _ => return Outcome::Ignored,
    }
    Outcome::Consumed
}

/// Perform a body row's click action: copy an env var or follow a link.
fn activate(state: &mut ExplorerState, hit: &Hit) -> Outcome {
    match hit {
        Hit::DetailRow { row } => {
            if state.detail.tab() != Tab::Env {
                return Outcome::Ignored;
            }
            let Some(detail) = state.detail.detail() else {
                return Outcome::Ignored;
            };
            let Some((k, v)) = tabs::sorted_env(detail).into_iter().nth(*row) else {
                return Outcome::Ignored;
            };
            state.request(UiRequest::Copy(format!("{k}={v}")));
            state.flash(format!("Copied {k} to clipboard"));
            Outcome::Consumed
        }
        Hit::ProcessLink { key } => {
            navigate(state, *key);
            Outcome::Consumed
        }
        _ => Outcome::Ignored,
    }
}

/// Inspect another process (breadcrumb / tree / PPID link).
fn navigate(state: &mut ExplorerState, key: ProcessKey) {
    state.table.select(key);
    let full = state.detail.is_full();
    state.detail.open(key, full);
    state.request(UiRequest::LoadDetail(key));
}

/// Run `action` on the inspected process only, leaving marks untouched.
fn run_for(state: &mut ExplorerState, key: ProcessKey, action: Action) {
    let marks = state.table.marked();
    state.table.clear_marks();
    state.table.select(key);
    state.run(action);
    state.table.mark_all(marks);
}

fn toggle_full(state: &mut ExplorerState) {
    let full = !state.detail.is_full();
    state.detail.set_full(full);
    if !full && let Some(key) = state.detail.key() {
        state.table.select(key);
    }
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let Some(key) = state.detail.key() else {
        return Outcome::Ignored;
    };
    let wheel = click.wheel();
    let left = click.is_left();
    match click.hit {
        Hit::DetailTab(i) => {
            if wheel != 0 {
                state.detail.cycle_tab(wheel.signum() as isize);
            } else if left {
                state.detail.set_tab(Tab::from_index(*i));
            } else {
                return Outcome::Ignored;
            }
        }
        Hit::DetailBody | Hit::DetailRow { .. } | Hit::ProcessLink { .. } if wheel != 0 => {
            state.detail.scroll_by(wheel as isize);
        }
        Hit::DetailClose if left => state.detail.close(),
        Hit::DetailDockToggle if left => toggle_full(state),
        Hit::DetailSplitter if left => {
            if click.double {
                state.detail.drag_from = None;
                toggle_full(state);
            } else {
                let area = state.detail.area;
                let extent = match state.settings.dock {
                    DockPosition::Bottom => area.height,
                    DockPosition::Right => area.width,
                };
                state.detail.drag_from = Some((extent, state.settings.dock_size));
            }
        }
        Hit::DetailRow { .. } | Hit::ProcessLink { .. } if left => {
            return activate(state, click.hit);
        }
        Hit::DetailAction(action) if left => {
            if is_ghost(state, &key) {
                state.flash("Process has exited");
            } else {
                run_for(state, key, *action);
            }
        }
        _ => return Outcome::Ignored,
    }
    Outcome::Consumed
}

/// Bounds for `dock_size` given the current main area.
fn dock_bounds(state: &ExplorerState) -> (u16, u16) {
    let main = state.layout(state.last_area).main;
    match state.settings.dock {
        DockPosition::Bottom => (6, main.height.saturating_sub(5).max(6)),
        DockPosition::Right => (8, (main.width.saturating_sub(40) / 4).max(8)),
    }
}

pub fn handle_drag(state: &mut ExplorerState, drag: &Drag, mouse: &MouseEvent) -> Outcome {
    if drag.hit != Hit::DetailSplitter || !state.detail.is_docked() {
        state.detail.drag_from = None;
        return Outcome::Ignored;
    }
    let (extent, saved) = state.detail.drag_from.unwrap_or_else(|| {
        let size = state.settings.dock_size;
        match state.settings.dock {
            DockPosition::Bottom => (size, size),
            DockPosition::Right => (size.saturating_mul(4), size),
        }
    });
    if mouse.position == drag.origin {
        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            state.detail.drag_from = None;
        }
        return Outcome::Consumed;
    }
    let (lo, hi) = dock_bounds(state);
    let size = match state.settings.dock {
        DockPosition::Bottom => {
            let delta = i32::from(drag.origin.y) - i32::from(mouse.position.y);
            (i32::from(extent) + delta).clamp(i32::from(lo), i32::from(hi)) as u16
        }
        DockPosition::Right => {
            let delta = i32::from(drag.origin.x) - i32::from(mouse.position.x);
            let cells = (i32::from(extent) + delta).max(0);
            ((cells + 3) / 4).clamp(i32::from(lo), i32::from(hi)) as u16
        }
    };
    state.settings.dock_size = size;
    if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
        state.detail.drag_from = None;
        if size != saved {
            state.request(UiRequest::SaveSettings);
        }
    }
    Outcome::Consumed
}
