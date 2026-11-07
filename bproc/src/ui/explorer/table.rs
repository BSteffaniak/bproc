//! Process table: virtualised rows, column headers (click to sort, right-click
//! for columns, drag edges to resize), selection that follows the process,
//! multi-select marks, pins, tree toggles, hover-freeze and heat colouring.
use super::hits::Hit;
use super::paint::{Frame, fit, theme, truncate};
use super::search::SearchBar;
use super::{Click, Drag, ExplorerState, Outcome, UiRequest};
use crate::config::ExplorerSettings;
use crate::process::TrafficType;
use crate::procs::ProcessKey;
use crate::procs::columns::{Align, ColumnCtx, ColumnId};
use crate::procs::model::fmt;
use crate::procs::store::{ProcessStore, Row, SortSpec, UserScope, ViewRow, ViewSpec};
use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::event::{MouseButton, MouseEvent, MouseEventKind};
use bmux_tui::geometry::Rect;
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text::{Line, Span};
use bmux_tui::text_width::display_width;
use std::collections::HashSet;

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;

/// Narrowest width a flexible column is squeezed to before others are dropped.
const FLEX_MIN: u16 = 8;
/// Below this table width the flexible column may be dropped too.
const FLEX_KEEP: u16 = 20;
pub const MIN_COLUMN_WIDTH: u16 = 3;
pub const MAX_COLUMN_WIDTH: u16 = 80;
const GLYPH_COLLAPSED: &str = "▸ ";
const GLYPH_EXPANDED: &str = "▾ ";
const GLYPH_LEAF: &str = "  ";

/// A column placed on screen by [`layout_columns`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnSlot {
    pub column: ColumnId,
    pub x: u16,
    pub width: u16,
}

pub struct TableView {
    rows: Vec<ViewRow>,
    columns: Vec<ColumnId>,
    sort: SortSpec,
    selected: Option<ProcessKey>,
    marked: Vec<ProcessKey>,
    pinned: Vec<ProcessKey>,
    collapsed: HashSet<ProcessKey>,
    frozen: Option<Vec<ProcessKey>>,
    scroll: usize,
    dirty: bool,
    /// Body rows painted last frame (paging and wheel clamping).
    body_height: usize,
    /// Scroll so the selection is visible on the next draw.
    reveal: bool,
    /// Last plain-clicked row, the start of shift-click ranges.
    anchor: Option<ProcessKey>,
    /// Column and width when a header-edge drag started.
    resize: Option<(ColumnId, u16)>,
    /// Scrollbar track painted last frame.
    scrollbar: Option<Rect>,
}

impl TableView {
    pub fn new(settings: &ExplorerSettings) -> Self {
        Self {
            rows: Vec::new(),
            columns: settings.columns_for(settings.preset),
            sort: SortSpec {
                column: settings
                    .sort_column
                    .unwrap_or(settings.preset.default_sort()),
                descending: settings.sort_descending,
            },
            selected: None,
            marked: Vec::new(),
            pinned: Vec::new(),
            collapsed: HashSet::new(),
            frozen: None,
            scroll: 0,
            dirty: true,
            body_height: 0,
            reveal: false,
            anchor: None,
            resize: None,
            scrollbar: None,
        }
    }
    /// Column context for the current store state.
    pub fn column_ctx(&self, store: &ProcessStore, traffic: TrafficType) -> ColumnCtx {
        ColumnCtx {
            traffic,
            now_ms: store.acquired_at_ms(),
            mem_total: store.system().mem_total,
            cpu_count: store.system().cpus.len().max(1),
        }
    }
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
    /// Rebuild `rows` from the store when dirty (or always when cheap).
    /// `hovering` pauses re-sorting while the pointer is over the table.
    pub fn refresh(
        &mut self,
        store: &ProcessStore,
        settings: &ExplorerSettings,
        search: &SearchBar,
        traffic: TrafficType,
        uid: Option<u32>,
        hovering: bool,
    ) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let previous_index = self.selected_index();
        let hover_order: Option<Vec<ProcessKey>> =
            (hovering && self.frozen.is_none()).then(|| self.rows.iter().map(|r| r.key).collect());
        self.marked.retain(|k| store.get(k).is_some());
        let spec = ViewSpec {
            sort: self.sort,
            filter: search.filter(),
            tree: settings.tree,
            collapsed: &self.collapsed,
            hide_kernel_threads: settings.hide_kernel_threads,
            users: match (settings.only_mine, uid) {
                (true, Some(uid)) => UserScope::Uid(uid),
                _ => UserScope::All,
            },
            net_active_only: settings.net_active_only,
            pinned: &self.pinned,
            frozen_order: self.frozen.as_deref().or(hover_order.as_deref()),
            ctx: self.column_ctx(store, traffic),
        };
        self.rows = store.view(&spec);
        let still_present = self
            .selected
            .is_some_and(|key| self.rows.iter().any(|r| r.key == key));
        if !still_present {
            self.selected = match previous_index {
                Some(i) if !self.rows.is_empty() => Some(self.rows[i.min(self.rows.len() - 1)].key),
                _ => self.rows.first().map(|r| r.key),
            };
        }
        self.clamp_scroll();
    }
    pub fn rows(&self) -> &[ViewRow] {
        &self.rows
    }
    pub fn columns(&self) -> &[ColumnId] {
        &self.columns
    }
    pub fn set_columns(&mut self, columns: Vec<ColumnId>) {
        self.columns = columns;
    }
    pub fn sort(&self) -> SortSpec {
        self.sort
    }
    pub fn set_sort(&mut self, sort: SortSpec) {
        self.sort = sort;
        self.dirty = true;
        self.reveal = true;
    }
    pub fn selected(&self) -> Option<ProcessKey> {
        self.selected
    }
    /// Select `key` and scroll it into view on the next draw.
    pub fn select(&mut self, key: ProcessKey) {
        self.selected = Some(key);
        self.reveal = true;
    }
    /// Index of the selection in `rows`.
    pub fn selected_index(&self) -> Option<usize> {
        let key = self.selected?;
        self.rows.iter().position(|r| r.key == key)
    }
    /// Select the row at `index` (clamped); no-op when there are no rows.
    pub fn select_index(&mut self, index: usize) {
        if let Some(row) = self.rows.get(index.min(self.rows.len().saturating_sub(1))) {
            self.selected = Some(row.key);
            self.reveal = true;
        }
    }
    /// Move the selection by `delta` rows (negative = up).
    pub fn select_next(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let target = match self.selected_index() {
            Some(i) => i.saturating_add_signed(delta),
            None if delta < 0 => self.rows.len() - 1,
            None => 0,
        };
        self.select_index(target);
    }
    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }
    /// Scroll the viewport without moving the selection.
    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
        self.reveal = false;
        self.clamp_scroll();
    }
    /// Rows moved by PageUp / PageDown.
    pub fn page(&self) -> usize {
        self.body_height.saturating_sub(1).max(1)
    }
    fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.body_height.max(1))
    }
    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(self.max_scroll());
    }
    fn reveal_selection(&mut self) {
        if !std::mem::take(&mut self.reveal) || self.body_height == 0 {
            return;
        }
        if let Some(i) = self.selected_index() {
            if i < self.scroll {
                self.scroll = i;
            } else if i >= self.scroll + self.body_height {
                self.scroll = i + 1 - self.body_height;
            }
        }
    }
    pub fn marked(&self) -> Vec<ProcessKey> {
        self.marked.clone()
    }
    pub fn toggle_mark(&mut self, key: ProcessKey) {
        if let Some(i) = self.marked.iter().position(|k| *k == key) {
            self.marked.remove(i);
        } else {
            self.marked.push(key);
        }
    }
    pub fn mark_all(&mut self, keys: Vec<ProcessKey>) {
        for key in keys {
            if !self.marked.contains(&key) {
                self.marked.push(key);
            }
        }
    }
    pub fn clear_marks(&mut self) {
        self.marked.clear();
    }
    /// Mark every row between the shift-click anchor and `key` (inclusive).
    pub fn mark_range(&mut self, key: ProcessKey) {
        let to = self.rows.iter().position(|r| r.key == key);
        let from = self
            .anchor
            .and_then(|a| self.rows.iter().position(|r| r.key == a))
            .or(self.selected_index());
        let (Some(from), Some(to)) = (from, to) else {
            self.mark_all(vec![key]);
            return;
        };
        let (lo, hi) = (from.min(to), from.max(to));
        let keys = self.rows[lo..=hi].iter().map(|r| r.key).collect();
        self.mark_all(keys);
    }
    pub fn pinned(&self) -> &[ProcessKey] {
        &self.pinned
    }
    /// Returns true when the key is now pinned.
    pub fn toggle_pin(&mut self, key: ProcessKey) -> bool {
        self.dirty = true;
        if let Some(i) = self.pinned.iter().position(|k| *k == key) {
            self.pinned.remove(i);
            false
        } else {
            self.pinned.push(key);
            true
        }
    }
    pub fn is_frozen(&self) -> bool {
        self.frozen.is_some()
    }
    /// Returns true when now frozen.
    pub fn toggle_freeze(&mut self) -> bool {
        self.frozen = match self.frozen {
            Some(_) => None,
            None => Some(self.rows.iter().map(|r| r.key).collect()),
        };
        self.dirty = true;
        self.frozen.is_some()
    }
    pub fn set_collapsed(&mut self, key: ProcessKey, collapsed: bool) {
        if collapsed {
            self.collapsed.insert(key);
        } else {
            self.collapsed.remove(&key);
        }
        self.dirty = true;
    }
    /// Flip a tree node; returns true when it is now collapsed.
    pub fn toggle_collapsed(&mut self, key: ProcessKey) -> bool {
        let collapse = !self.collapsed.contains(&key);
        self.set_collapsed(key, collapse);
        collapse
    }
    pub fn expand_all(&mut self) {
        self.collapsed.clear();
        self.dirty = true;
    }
    pub fn collapse_all(&mut self, keys: Vec<ProcessKey>) {
        self.collapsed.extend(keys);
        self.dirty = true;
    }
    pub fn on_preset_changed(&mut self, settings: &ExplorerSettings) {
        self.columns = settings.columns_for(settings.preset);
        self.sort = SortSpec {
            column: settings.preset.default_sort(),
            descending: true,
        };
        self.scroll = 0;
        self.reveal = true;
        self.dirty = true;
    }
}

/// Effective width of a non-flexible column (user override or default).
pub fn column_width(settings: &ExplorerSettings, column: ColumnId) -> u16 {
    settings
        .column_widths
        .get(column.filter_key())
        .copied()
        .unwrap_or(column.width())
        .clamp(1, MAX_COLUMN_WIDTH)
}

/// Place `columns` in `width` cells starting at `x`. Columns separated by one
/// space; the flexible column absorbs slack; when too narrow the columns with
/// the highest `priority()` are dropped first (PID and the flexible column are
/// kept as long as possible).
pub fn layout_columns(
    columns: &[ColumnId],
    settings: &ExplorerSettings,
    x: u16,
    width: u16,
) -> Vec<ColumnSlot> {
    let keep_flex = width >= FLEX_KEEP;
    let mut cols: Vec<(ColumnId, u16)> = columns
        .iter()
        .map(|&c| {
            let w = if c.is_flexible() {
                0
            } else {
                column_width(settings, c)
            };
            (c, w)
        })
        .collect();
    let rank = |c: ColumnId| -> u8 {
        if c.is_flexible() {
            if keep_flex { 0 } else { u8::MAX }
        } else if c == ColumnId::Pid {
            0
        } else {
            c.priority()
        }
    };
    let needed = |cols: &[(ColumnId, u16)]| -> usize {
        cols.iter()
            .map(|(c, w)| usize::from(if c.is_flexible() { FLEX_MIN } else { *w }))
            .sum::<usize>()
            + cols.len().saturating_sub(1)
    };
    while cols.len() > 1 && needed(&cols) > usize::from(width) {
        let victim = cols
            .iter()
            .enumerate()
            .filter(|(_, (c, _))| !(keep_flex && c.is_flexible()))
            .max_by_key(|(i, (c, _))| (rank(*c), *i))
            .map(|(i, _)| i);
        match victim {
            Some(i) => {
                cols.remove(i);
            }
            None => break,
        }
    }
    let fixed: usize = cols
        .iter()
        .filter(|(c, _)| !c.is_flexible())
        .map(|(_, w)| usize::from(*w))
        .sum::<usize>()
        + cols.len().saturating_sub(1);
    let flex_count = cols.iter().filter(|(c, _)| c.is_flexible()).count();
    let slack = usize::from(width).saturating_sub(fixed);
    if let Some(share) = slack.checked_div(flex_count) {
        let mut extra = slack - share * flex_count;
        for (_, w) in cols.iter_mut().filter(|(c, _)| c.is_flexible()) {
            let bonus = usize::from(extra > 0);
            extra = extra.saturating_sub(1);
            *w = u16::try_from(share + bonus).unwrap_or(u16::MAX);
        }
    }
    let right = x.saturating_add(width);
    let mut slots = Vec::with_capacity(cols.len());
    let mut cx = x;
    for (column, w) in cols {
        if cx >= right {
            break;
        }
        let w = w.min(right - cx);
        if w > 0 {
            slots.push(ColumnSlot {
                column,
                x: cx,
                width: w,
            });
        }
        cx = cx.saturating_add(w).saturating_add(1);
    }
    slots
}

/// Cell the draggable edge of `slot` occupies: the separator after it, or
/// its last cell when nothing follows inside `right`.
fn edge_x(slot: &ColumnSlot, right: u16) -> u16 {
    let after = slot.x.saturating_add(slot.width);
    if after < right {
        after
    } else {
        after.saturating_sub(1)
    }
}

/// Segments of styled text fitted (left aligned) to exactly `width` cells,
/// ending with `…` when cut.
fn fit_spans(segments: Vec<(String, Style)>, width: usize, pad: Style) -> Vec<Span> {
    let total: usize = segments.iter().map(|(t, _)| display_width(t)).sum();
    if total <= width {
        let mut spans: Vec<Span> = segments
            .into_iter()
            .filter(|(t, _)| !t.is_empty())
            .map(|(t, s)| Span::styled(t, s))
            .collect();
        if total < width {
            spans.push(Span::styled(" ".repeat(width - total), pad));
        }
        return spans;
    }
    if width == 0 {
        return Vec::new();
    }
    let budget = width - 1;
    let mut used = 0;
    let mut spans = Vec::new();
    let mut last_style = pad;
    'outer: for (text, style) in segments {
        last_style = style;
        let mut part = String::new();
        for ch in text.chars() {
            let w = display_width(ch.encode_utf8(&mut [0; 4]));
            if used + w > budget {
                if !part.is_empty() {
                    spans.push(Span::styled(part, style));
                }
                break 'outer;
            }
            used += w;
            part.push(ch);
        }
        if !part.is_empty() {
            spans.push(Span::styled(part, style));
        }
    }
    spans.push(Span::styled("…", last_style));
    used += 1;
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), pad));
    }
    spans
}

/// `text` split into plain / highlighted segments by char ranges.
fn highlight_segments(
    text: &str,
    ranges: &[(usize, usize)],
    base: Style,
    mark: Style,
) -> Vec<(String, Style)> {
    if ranges.is_empty() {
        return vec![(text.to_string(), base)];
    }
    let mut out: Vec<(String, Style)> = Vec::new();
    for (i, ch) in text.chars().enumerate() {
        let lit = ranges.iter().any(|&(a, b)| i >= a && i < b);
        let style = if lit { mark } else { base };
        match out.last_mut() {
            Some((t, s)) if *s == style => t.push(ch),
            _ => out.push((ch.to_string(), style)),
        }
    }
    out
}

/// Cell text for a collapsed node's summed usage.
fn aggregate_text(column: ColumnId, row: &ViewRow, ctx: &ColumnCtx) -> Option<(String, f64)> {
    let agg = row.aggregate.as_ref()?;
    let mem_pct = if ctx.mem_total > 0 {
        agg.mem_rss as f64 * 100.0 / ctx.mem_total as f64
    } else {
        0.0
    };
    Some(match column {
        ColumnId::Cpu => (
            format!("Σ{}", fmt::percent(agg.cpu_percent)),
            f64::from(agg.cpu_percent),
        ),
        ColumnId::Mem => (format!("Σ{}", fmt::percent(mem_pct as f32)), mem_pct),
        ColumnId::Rss => (format!("Σ{}", fmt::bytes(agg.mem_rss)), 0.0),
        ColumnId::NetDown => (format!("Σ{}", fmt::rate(agg.net_down)), 0.0),
        ColumnId::NetUp => (format!("Σ{}", fmt::rate(agg.net_up)), 0.0),
        ColumnId::DiskRead => (format!("Σ{}", fmt::rate(agg.disk_read_rate)), 0.0),
        ColumnId::DiskWrite => (format!("Σ{}", fmt::rate(agg.disk_write_rate)), 0.0),
        _ => return None,
    })
}

fn throttled(row: &Row) -> bool {
    row.net.as_ref().is_some_and(|n| n.throttle_limit.is_some())
}

fn throttle_style() -> Style {
    Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
}

/// Which visual state wins for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowLook {
    Selected,
    SelectedUnfocused,
    Hovered,
    Marked,
    Alert,
    Ghost,
    Base,
}

impl RowLook {
    fn style(self) -> Style {
        match self {
            Self::Selected => theme::selected(),
            Self::SelectedUnfocused => theme::base().patch(theme::selected_unfocused()),
            Self::Hovered => theme::base().patch(theme::hovered()),
            Self::Marked => theme::base().patch(theme::marked()),
            Self::Alert => theme::base().patch(theme::alert()),
            Self::Ghost => theme::base().patch(theme::ghost()),
            Self::Base => theme::base(),
        }
    }
    /// Whether per-cell accent colours (heat, highlights) may override fg.
    fn colourful(self) -> bool {
        matches!(self, Self::Hovered | Self::Base | Self::SelectedUnfocused)
    }
}

pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    if area.width == 0 || area.height == 0 {
        state.table.body_height = 0;
        state.table.scrollbar = None;
        return;
    }
    let body_h = usize::from(area.height - 1);
    state.table.body_height = body_h;
    state.table.clamp_scroll();
    state.table.reveal_selection();
    state.table.clamp_scroll();

    let rows_len = state.table.rows.len();
    let show_bar = rows_len > body_h && area.width >= 4 && body_h > 0;
    let gutter = u16::from(area.width >= 4);
    let content_x = area.x + gutter;
    let content_w = area.width - gutter - u16::from(show_bar);
    let right = content_x + content_w;
    let slots = layout_columns(&state.table.columns, &state.settings, content_x, content_w);

    draw_header(frame, area, &slots, right, state);
    if body_h == 0 {
        state.table.scrollbar = None;
        return;
    }
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    frame.hit(body, Hit::TableBody);

    if rows_len == 0 {
        state.table.scrollbar = None;
        draw_empty(frame, body, state);
        return;
    }

    let ctx = state.table.column_ctx(&state.store, state.traffic);
    let focused = !state.search.focused && state.overlays.is_empty();
    let hover = state.pointer.position.filter(|_| state.overlays.is_empty());
    let row_w = area.width - u16::from(show_bar);
    let first = state.table.scroll;
    let last = (first + body_h).min(rows_len);
    for (i, index) in (first..last).enumerate() {
        let y = body.y + i as u16;
        let row_rect = Rect::new(area.x, y, row_w, 1);
        let view = &state.table.rows[index];
        let Some(row) = state.store.get(&view.key) else {
            continue;
        };
        let key = view.key;
        let hovered =
            hover.is_some_and(|p| p.y == y && p.x >= row_rect.x && p.x < row_rect.x + row_w);
        let look = if state.table.selected == Some(key) {
            if focused {
                RowLook::Selected
            } else {
                RowLook::SelectedUnfocused
            }
        } else if hovered {
            RowLook::Hovered
        } else if state.table.marked.contains(&key) {
            RowLook::Marked
        } else if state.alerting.contains(&key) {
            RowLook::Alert
        } else if view.ghost {
            RowLook::Ghost
        } else {
            RowLook::Base
        };
        let style = look.style();
        frame.fill(row_rect, style);
        if gutter > 0 {
            let marker = if state.table.marked.contains(&key) {
                Some(("●", theme::marked()))
            } else if view.pinned || state.table.pinned.contains(&key) {
                Some(("↑", theme::accent().add_modifier(Modifier::BOLD)))
            } else if throttled(row) {
                Some(("⇣", throttle_style()))
            } else if state.alerting.contains(&key) {
                Some(("!", theme::alert()))
            } else {
                None
            };
            if let Some((symbol, marker_style)) = marker {
                let s = if look == RowLook::Selected {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style.patch(Style {
                        bg: None,
                        ..marker_style
                    })
                };
                frame.cell(area.x, y, symbol, s);
            }
        }
        let mut toggle: Option<u16> = None;
        for slot in &slots {
            let cell = Rect::new(slot.x, y, slot.width, 1);
            let width = usize::from(slot.width);
            let spans = if slot.column == ColumnId::Command {
                let mut segments = Vec::new();
                if state.settings.tree {
                    let glyph = if view.has_children {
                        if view.collapsed {
                            GLYPH_COLLAPSED
                        } else {
                            GLYPH_EXPANDED
                        }
                    } else {
                        GLYPH_LEAF
                    };
                    let prefix_w = display_width(&view.tree_prefix);
                    if view.has_children && prefix_w + 1 < width {
                        toggle = Some(slot.x + prefix_w as u16);
                    }
                    let tree_style = if look.colourful() {
                        style.patch(theme::dim())
                    } else {
                        style
                    };
                    segments.push((view.tree_prefix.clone(), tree_style));
                    let glyph_style = if look.colourful() {
                        style.patch(theme::accent())
                    } else {
                        style
                    };
                    segments.push((glyph.to_string(), glyph_style));
                }
                let mark = if look.colourful() {
                    style.patch(
                        Style::new()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINE),
                    )
                } else {
                    style.add_modifier(Modifier::BOLD | Modifier::UNDERLINE)
                };
                segments.extend(highlight_segments(
                    &row.entry.record.command(),
                    &view.highlights,
                    style,
                    mark,
                ));
                fit_spans(segments, width, style)
            } else {
                let (text, heat) = match aggregate_text(slot.column, view, &ctx) {
                    Some((t, h)) => (t, Some(h)),
                    None => (
                        slot.column.text(row, &ctx),
                        match slot.column {
                            ColumnId::Cpu => Some(f64::from(row.entry.sample.cpu_percent)),
                            ColumnId::Mem => Some(f64::from(row.entry.sample.mem_percent)),
                            _ => None,
                        },
                    ),
                };
                let mut cell_style = style;
                if look.colourful() {
                    if let Some(h) = heat.filter(|_| state.settings.heat_colors) {
                        let c = theme::heat(h);
                        if c != Color::Default {
                            cell_style = cell_style.fg(c);
                        }
                    }
                    if slot.column == ColumnId::Throttle && !text.is_empty() {
                        cell_style = cell_style.patch(throttle_style());
                    }
                }
                let right_align = slot.column.align() == Align::Right;
                vec![Span::styled(fit(&text, width, right_align), cell_style)]
            };
            frame.line(cell, &Line::from_spans(spans));
        }
        frame.hit(row_rect, Hit::Row { key });
        if let Some(x) = toggle {
            frame.hit(Rect::new(x, y, 2, 1), Hit::TreeToggle { key });
        }
    }

    if show_bar {
        let track = Rect::new(area.x + area.width - 1, body.y, 1, body.height);
        draw_scrollbar(frame, track, state.table.scroll, rows_len, body_h);
        frame.hit(track, Hit::TableScrollbar);
        state.table.scrollbar = Some(track);
    } else {
        state.table.scrollbar = None;
    }
}

fn draw_header(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    slots: &[ColumnSlot],
    right: u16,
    state: &ExplorerState,
) {
    let header = Rect::new(area.x, area.y, area.width, 1);
    frame.fill(header, theme::header());
    let sort = state.table.sort;
    let hovered_edge = state.pointer.position.and_then(|p| {
        (p.y == area.y)
            .then(|| state.hits.at(p))
            .flatten()
            .and_then(|h| match h {
                Hit::HeaderEdge { column } => Some(*column),
                _ => None,
            })
    });
    let dragging = state.table.resize.map(|(c, _)| c).or_else(|| {
        state.pointer.drag.as_ref().and_then(|d| match d.hit {
            Hit::HeaderEdge { column } => Some(column),
            _ => None,
        })
    });
    for slot in slots {
        let cell = Rect::new(slot.x, area.y, slot.width, 1);
        let active = slot.column == sort.column;
        let style = if active {
            theme::header_active()
        } else {
            theme::header()
        };
        let width = usize::from(slot.width);
        let title = slot.column.title();
        let label = if active {
            let arrow = if sort.descending { "▼" } else { "▲" };
            let full = format!("{title}{arrow}");
            if display_width(&full) <= width {
                full
            } else if width <= 2 {
                arrow.to_string()
            } else {
                format!("{}{arrow}", truncate(title, width - 1))
            }
        } else {
            title.to_string()
        };
        frame.text(
            cell,
            &fit(&label, width, slot.column.align() == Align::Right),
            style,
        );
        frame.hit(
            cell,
            Hit::Header {
                column: slot.column,
            },
        );
    }
    for slot in slots.iter().filter(|s| !s.column.is_flexible()) {
        let x = edge_x(slot, right);
        let lit = hovered_edge == Some(slot.column) || dragging == Some(slot.column);
        if lit {
            frame.cell(x, area.y, "┃", theme::header().add_modifier(Modifier::BOLD));
        }
        frame.hit(
            Rect::new(x, area.y, 1, 1),
            Hit::HeaderEdge {
                column: slot.column,
            },
        );
    }
}

fn draw_empty(frame: &mut Frame<'_, '_, '_>, body: Rect, state: &ExplorerState) {
    let message = if state.store.rows().is_empty() {
        "Collecting process data…".to_string()
    } else if !state.search.text.trim().is_empty() {
        format!(
            "No processes match “{}” · Esc clears",
            state.search.text.trim()
        )
    } else {
        "No processes match the current view filters".to_string()
    };
    let text = truncate(&message, usize::from(body.width));
    let w = display_width(&text) as u16;
    let r = super::paint::centered(body, w, 1);
    frame.text(r, &text, theme::dim());
}

/// Thumb geometry `(offset, length)` within a track of `track` cells.
pub fn scrollbar_thumb(scroll: usize, rows: usize, visible: usize, track: usize) -> (usize, usize) {
    if track == 0 || rows <= visible || rows == 0 {
        return (0, track);
    }
    let len = (track * visible / rows).clamp(1, track);
    let max_scroll = rows - visible;
    let span = track - len;
    let offset = (scroll.min(max_scroll) * span + max_scroll / 2) / max_scroll;
    (offset.min(span), len)
}

fn draw_scrollbar(
    frame: &mut Frame<'_, '_, '_>,
    track: Rect,
    scroll: usize,
    rows: usize,
    visible: usize,
) {
    let h = usize::from(track.height);
    let (offset, len) = scrollbar_thumb(scroll, rows, visible, h);
    for i in 0..h {
        let y = track.y + i as u16;
        if i >= offset && i < offset + len {
            frame.cell(track.x, y, "┃", theme::accent());
        } else {
            frame.cell(track.x, y, "│", theme::dim());
        }
    }
}

/// Scroll offset for a click at row `y` of the scrollbar track.
fn scroll_for_track(table: &TableView, y: u16) -> Option<usize> {
    let track = table.scrollbar?;
    let max = table.max_scroll();
    let h = usize::from(track.height);
    let pos = usize::from(y.saturating_sub(track.y)).min(h.saturating_sub(1));
    if h <= 1 {
        return Some(0);
    }
    Some((pos * max + (h - 1) / 2) / (h - 1))
}

/// Sort by `column`: same column flips direction, a new one uses its default.
pub fn sort_by(state: &mut ExplorerState, column: ColumnId) {
    let current = state.table.sort();
    let descending = if current.column == column {
        !current.descending
    } else {
        column.default_descending()
    };
    state.table.set_sort(SortSpec { column, descending });
    state.settings.sort_column = Some(column);
    state.settings.sort_descending = descending;
    state.request(UiRequest::SaveSettings);
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let wheel = click.wheel();
    if wheel != 0 {
        state.table.scroll_by(wheel as isize);
        return Outcome::Consumed;
    }
    let mods = click.mouse.modifiers;
    match click.hit {
        Hit::Row { key } => {
            let key = *key;
            if click.is_left() {
                if mods.ctrl || mods.alt {
                    state.table.toggle_mark(key);
                    state.table.anchor = Some(key);
                    state.table.select(key);
                } else if mods.shift {
                    state.table.mark_range(key);
                    state.table.select(key);
                } else {
                    state.table.select(key);
                    state.table.anchor = Some(key);
                    if click.double {
                        state.run(super::actions::Action::Details);
                    }
                }
                Outcome::Consumed
            } else if click.is_right() {
                state.table.select(key);
                let menu = super::overlays::process_menu(state, key, click.mouse.position);
                state.overlays.push(menu);
                Outcome::Consumed
            } else if click.is_middle() {
                state.table.toggle_mark(key);
                Outcome::Consumed
            } else {
                Outcome::Ignored
            }
        }
        Hit::TreeToggle { key } => {
            let key = *key;
            if click.is_left() {
                state.table.toggle_collapsed(key);
                state.table.select(key);
                state.table.anchor = Some(key);
                Outcome::Consumed
            } else if click.is_right() {
                state.table.select(key);
                let menu = super::overlays::process_menu(state, key, click.mouse.position);
                state.overlays.push(menu);
                Outcome::Consumed
            } else {
                Outcome::Ignored
            }
        }
        Hit::Header { column } => {
            if click.is_left() {
                sort_by(state, *column);
                Outcome::Consumed
            } else if click.is_right() {
                let chooser = super::overlays::column_chooser(state);
                state.overlays.push(chooser);
                Outcome::Consumed
            } else {
                Outcome::Ignored
            }
        }
        Hit::HeaderEdge { column } => {
            if click.is_left() && click.double {
                // Double-clicking an edge restores the default width.
                state.table.resize = None;
                if state
                    .settings
                    .column_widths
                    .remove(column.filter_key())
                    .is_some()
                {
                    state.request(UiRequest::SaveSettings);
                }
                Outcome::Consumed
            } else if click.is_right() {
                let chooser = super::overlays::column_chooser(state);
                state.overlays.push(chooser);
                Outcome::Consumed
            } else if click.is_left() {
                Outcome::Consumed
            } else {
                Outcome::Ignored
            }
        }
        Hit::TableScrollbar => {
            if matches!(
                click.mouse.kind,
                MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
            ) {
                if let Some(scroll) = scroll_for_track(&state.table, click.mouse.position.y) {
                    state.table.scroll = scroll;
                    state.table.reveal = false;
                    state.table.clamp_scroll();
                }
                Outcome::Consumed
            } else {
                Outcome::Ignored
            }
        }
        Hit::TableBody => {
            if click.is_left() || click.is_right() {
                Outcome::Consumed
            } else {
                Outcome::Ignored
            }
        }
        _ => Outcome::Ignored,
    }
}

pub fn handle_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    let m = key.modifiers;
    if m.ctrl || m.alt {
        return Outcome::Ignored;
    }
    let table = &mut state.table;
    let page = table.page() as isize;
    match key.key {
        KeyCode::Up | KeyCode::Char('k') => table.select_next(-1),
        KeyCode::Down | KeyCode::Char('j') => table.select_next(1),
        KeyCode::PageUp => table.select_next(-page),
        KeyCode::PageDown => table.select_next(page),
        KeyCode::Home => table.select_index(0),
        KeyCode::End => table.select_index(usize::MAX),
        _ => return Outcome::Ignored,
    }
    Outcome::Consumed
}

pub fn handle_drag(state: &mut ExplorerState, drag: &Drag, mouse: &MouseEvent) -> Outcome {
    let Hit::HeaderEdge { column } = drag.hit else {
        return Outcome::Ignored;
    };
    let start = match state.table.resize {
        Some((c, w)) if c == column => w,
        _ => {
            let w = column_width(&state.settings, column);
            state.table.resize = Some((column, w));
            w
        }
    };
    let delta = i32::from(mouse.position.x) - i32::from(drag.origin.x);
    let width = (i32::from(start) + delta)
        .clamp(i32::from(MIN_COLUMN_WIDTH), i32::from(MAX_COLUMN_WIDTH)) as u16;
    let key = column.filter_key().to_string();
    // Leave the settings free of no-op overrides while at the default width.
    if state.settings.column_widths.contains_key(&key) || width != column.width() {
        state.settings.column_widths.insert(key, width);
    }
    if matches!(mouse.kind, MouseEventKind::Up(_)) {
        state.table.resize = None;
        if width != start {
            state.request(UiRequest::SaveSettings);
        }
    }
    Outcome::Consumed
}
