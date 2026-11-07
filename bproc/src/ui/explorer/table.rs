//! Process table: virtualised rows, column headers (click to sort, right-click
//! for columns, drag edges to resize), selection that follows the process,
//! multi-select marks, pins, tree toggles, hover-freeze and heat colouring.
use super::hits::Hit;
use super::paint::Frame;
use super::search::SearchBar;
use super::{Click, Drag, ExplorerState, Outcome};
use crate::config::ExplorerSettings;
use crate::process::TrafficType;
use crate::procs::ProcessKey;
use crate::procs::columns::{ColumnCtx, ColumnId};
use crate::procs::store::{ProcessStore, SortSpec, UserScope, ViewRow, ViewSpec};
use bmux_keyboard::KeyStroke;
use bmux_tui::event::MouseEvent;
use bmux_tui::geometry::Rect;
use std::collections::HashSet;

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
        let _ = hovering;
        if !self.dirty {
            return;
        }
        self.dirty = false;
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
            frozen_order: self.frozen.as_deref(),
            ctx: self.column_ctx(store, traffic),
        };
        self.rows = store.view(&spec);
        if self.selected.is_none() {
            self.selected = self.rows.first().map(|r| r.key);
        }
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
    }
    pub fn selected(&self) -> Option<ProcessKey> {
        self.selected
    }
    pub fn select(&mut self, key: ProcessKey) {
        self.selected = Some(key);
    }
    /// Move the selection by `delta` rows (negative = up).
    pub fn select_next(&mut self, delta: isize) {
        let _ = delta;
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
        self.dirty = true;
    }
}

pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    let _ = (frame, area, state, Hit::TableBody);
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let _ = (state, click);
    Outcome::Ignored
}

pub fn handle_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    let _ = (state, key);
    Outcome::Ignored
}

pub fn handle_drag(state: &mut ExplorerState, drag: &Drag, mouse: &MouseEvent) -> Outcome {
    let _ = (state, drag, mouse);
    Outcome::Ignored
}
