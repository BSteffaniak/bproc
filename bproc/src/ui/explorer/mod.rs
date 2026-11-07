//! Process explorer: the htop-class, mouse-first main screen.
//!
//! Ownership map (one owner per file):
//! - `mod.rs`, `hits.rs`, `paint.rs`, `actions.rs`: state, layout, routing, action dispatch.
//! - `chrome.rs`: meters header, toolbar (screens/presets/chips), F-key bar, status line.
//! - `table.rs`, `search.rs`: process table and search/filter box.
//! - `overlays.rs`, `toasts.rs`: modal stack (menus, dialogs) and notifications.
//! - `detail_panel.rs`: docked / full-screen process inspector.
//! - `screens.rs`, `help.rs`, `settings.rs`: events + watches screens, help, settings.
//!
//! Painters never perform I/O. Side effects are queued in `outbox` as
//! `UiRequest`s which the runtime executes off the UI thread.
pub mod actions;
pub mod chrome;
pub mod detail_panel;
pub mod help;
pub mod hits;
pub mod overlays;
pub mod paint;
pub mod screens;
pub mod search;
pub mod settings;
pub mod table;
pub mod toasts;

use crate::config::{DockPosition, ExplorerSettings};
use crate::process::TrafficType;
use crate::procs::actions::{ActionReport, ActionRequest, Signal};
use crate::procs::detail::DeepDetail;
use crate::procs::export::ExportFormat;
use crate::procs::store::ProcessStore;
use crate::procs::watch::{EventKind, EventLog, ProcEvent, WatchAction, WatchEngine};
use crate::procs::{ProcSnapshot, ProcessKey};
use actions::Action;
use bmux_tui::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use bmux_tui::geometry::{Point, Rect};
use bmux_tui::paint::PaintCx;
use hits::{Hit, HitMap, Screen};
use paint::Frame;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// Requests to the legacy (network) part of the application.
#[derive(Debug, Clone, PartialEq)]
pub enum LegacyRequest {
    OpenThrottle { pid: i32, name: String },
    RemoveThrottle { pid: i32, name: String },
    Interfaces,
    TrafficMode,
    Graph,
    Backends,
}

/// Side effects queued by the explorer for the runtime to execute.
#[derive(Debug, Clone, PartialEq)]
pub enum UiRequest {
    Action(ActionRequest),
    LoadDetail(ProcessKey),
    SetRefresh(Duration),
    Pause(bool),
    RefreshNow,
    /// Persist `settings` and watch rules to the config file.
    SaveSettings,
    Export(ExportFormat),
    /// Copy text to the terminal clipboard (OSC 52).
    Copy(String),
    Legacy(LegacyRequest),
    Quit,
}

/// What the runtime should do after the explorer handled an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Consumed,
    Ignored,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drag {
    pub hit: Hit,
    pub origin: Point,
    pub last: Point,
}

#[derive(Debug, Clone, Default)]
pub struct Pointer {
    pub position: Option<Point>,
    pub moved_at: Option<Instant>,
    /// Last left press, for double-click detection.
    pub last_click: Option<(Instant, Point, Hit)>,
    pub drag: Option<Drag>,
}

pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Environment supplied by the runtime for drawing.
#[derive(Debug, Clone, Default)]
pub struct DrawEnv<'a> {
    /// Legacy status message (throttle/backend results).
    pub status: &'a str,
    pub interface_filter: Option<String>,
    pub socket_mapper: Option<String>,
}

pub struct ExplorerState {
    pub store: ProcessStore,
    pub settings: ExplorerSettings,
    pub table: table::TableView,
    pub search: search::SearchBar,
    pub overlays: Vec<overlays::Overlay>,
    pub toasts: toasts::Toasts,
    pub detail: detail_panel::DetailPanel,
    pub events: EventLog,
    pub watch: WatchEngine,
    pub events_view: screens::EventsView,
    pub watches_view: screens::WatchesView,
    pub screen: Screen,
    pub hits: HitMap,
    pub outbox: Vec<UiRequest>,
    pub paused: bool,
    pub traffic: TrafficType,
    /// Explains missing network data (e.g. capture needs root).
    pub network_note: Option<String>,
    pub pointer: Pointer,
    /// Transient status line message.
    pub flash: Option<(String, Instant)>,
    /// Current user's uid for the "only mine" filter.
    pub uid: Option<u32>,
    /// Processes currently violating a watch rule.
    pub alerting: HashSet<ProcessKey>,
    /// Pending escalation checks: (deadline, keys) after a terminating signal.
    pub escalations: Vec<(Instant, Vec<ProcessKey>)>,
    pub last_area: Rect,
}

impl Default for ExplorerState {
    fn default() -> Self {
        Self::new(ExplorerSettings::default(), Vec::new())
    }
}

impl ExplorerState {
    pub fn new(settings: ExplorerSettings, rules: Vec<crate::procs::watch::WatchRule>) -> Self {
        let table = table::TableView::new(&settings);
        Self {
            store: ProcessStore::new(),
            table,
            search: search::SearchBar::default(),
            overlays: Vec::new(),
            toasts: toasts::Toasts::default(),
            detail: detail_panel::DetailPanel::default(),
            events: EventLog::default(),
            watch: WatchEngine::new(rules),
            events_view: screens::EventsView::default(),
            watches_view: screens::WatchesView::default(),
            screen: Screen::Processes,
            hits: HitMap::default(),
            outbox: Vec::new(),
            paused: false,
            traffic: TrafficType::All,
            network_note: None,
            pointer: Pointer::default(),
            flash: None,
            uid: current_uid(),
            alerting: HashSet::new(),
            escalations: Vec::new(),
            last_area: Rect::default(),
            settings,
        }
    }

    pub fn flash(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), Instant::now()));
    }

    pub fn request(&mut self, request: UiRequest) {
        self.outbox.push(request);
    }

    pub fn take_requests(&mut self) -> Vec<UiRequest> {
        std::mem::take(&mut self.outbox)
    }

    /// Display name for a key, falling back to the PID.
    pub fn name_of(&self, key: &ProcessKey) -> String {
        self.store
            .get(key)
            .map(|r| r.name().to_string())
            .unwrap_or_else(|| format!("PID {}", key.pid))
    }

    // ---- inputs from the runtime -------------------------------------------------

    pub fn apply_snapshot(&mut self, snapshot: ProcSnapshot) {
        let evaluate = snapshot.clone();
        self.store.apply_snapshot(snapshot);
        self.events.record_snapshot(&evaluate, &self.store);
        for firing in self.watch.evaluate(&evaluate, &self.store) {
            self.events.push(ProcEvent {
                at_ms: evaluate.acquired_at_ms,
                kind: EventKind::Alert,
                key: Some(firing.key),
                name: firing.name.clone(),
                message: firing.message.clone(),
            });
            match firing.action {
                WatchAction::Log => {}
                WatchAction::Notify => self
                    .toasts
                    .push(toasts::Severity::Warning, firing.message.clone()),
                WatchAction::Signal(signal) => {
                    self.toasts.push(
                        toasts::Severity::Warning,
                        format!("{} — sending {}", firing.message, signal.name()),
                    );
                    self.request(UiRequest::Action(ActionRequest::Signal {
                        targets: vec![firing.key],
                        signal,
                    }));
                }
            }
        }
        self.alerting = self.watch.alerting().into_iter().collect();
        self.table.mark_dirty();
        self.check_escalations();
        if let Some(key) = self.detail.key()
            && self.store.get(&key).is_some_and(|r| !r.is_ghost())
        {
            self.request(UiRequest::LoadDetail(key));
        }
    }

    pub fn apply_network(&mut self, map: &crate::process::ProcessMap) {
        self.store.merge_network(map);
        self.table.mark_dirty();
    }

    pub fn apply_detail(&mut self, detail: DeepDetail) {
        self.detail.apply(detail);
    }

    pub fn apply_report(&mut self, report: ActionReport) {
        let summary = report.summary();
        let failed = report.failed().count();
        let severity = if failed == 0 {
            toasts::Severity::Success
        } else if report.succeeded() == 0 {
            toasts::Severity::Error
        } else {
            toasts::Severity::Warning
        };
        for outcome in &report.outcomes {
            self.events.push(ProcEvent {
                at_ms: crate::procs::model::now_ms(),
                kind: EventKind::Action,
                key: Some(outcome.key),
                name: outcome.name.clone(),
                message: match &outcome.result {
                    Ok(()) => format!("{} sent", report.request.label()),
                    Err(e) => format!("{} failed: {e}", report.request.label()),
                },
            });
        }
        self.toasts.push(severity, summary);
        if let ActionRequest::Signal { signal, .. } = &report.request
            && signal.is_terminating()
            && *signal != Signal::Kill
        {
            let keys: Vec<ProcessKey> = report
                .outcomes
                .iter()
                .filter(|o| o.result.is_ok())
                .map(|o| o.key)
                .collect();
            if !keys.is_empty() {
                let after = Duration::from_secs(self.settings.escalate_after_secs.max(1));
                self.escalations.push((Instant::now() + after, keys));
            }
        }
        self.request(UiRequest::RefreshNow);
    }

    /// After a terminating signal, offer SIGKILL for survivors.
    fn check_escalations(&mut self) {
        let now = Instant::now();
        let (due, pending): (Vec<_>, Vec<_>) =
            self.escalations.drain(..).partition(|(t, _)| *t <= now);
        self.escalations = pending;
        for (_, keys) in due {
            let alive: Vec<ProcessKey> = keys
                .into_iter()
                .filter(|k| self.store.get(k).is_some_and(|r| !r.is_ghost()))
                .collect();
            if !alive.is_empty() {
                let label = if alive.len() == 1 {
                    format!("{} is still running", self.name_of(&alive[0]))
                } else {
                    format!("{} processes are still running", alive.len())
                };
                self.toasts.push_action(
                    toasts::Severity::Warning,
                    label,
                    "Force kill",
                    UiRequest::Action(ActionRequest::Signal {
                        targets: alive,
                        signal: Signal::Kill,
                    }),
                );
            }
        }
    }

    // ---- layout & drawing ------------------------------------------------------

    pub fn draw(&mut self, cx: &mut PaintCx<'_, '_>, area: Rect, env: &DrawEnv<'_>) {
        self.last_area = area;
        self.table.refresh(
            &self.store,
            &self.settings,
            &self.search,
            self.traffic,
            self.uid,
            self.hovering_table(),
        );
        let mut hits = std::mem::take(&mut self.hits);
        hits.begin();
        {
            let mut frame = Frame {
                cx,
                hits: &mut hits,
                now: Instant::now(),
            };
            frame.fill(area, paint::theme::base());
            let layout = self.layout(area);
            if layout.header.height > 0 {
                chrome::draw_header(&mut frame, layout.header, self);
            }
            chrome::draw_toolbar(&mut frame, layout.toolbar, self);
            search::draw(&mut frame, layout.search, self);
            match self.screen {
                Screen::Processes => {
                    if self.detail.is_full() {
                        detail_panel::draw(&mut frame, layout.main, self);
                    } else {
                        table::draw(&mut frame, layout.table, self);
                        if layout.dock.width > 0 && layout.dock.height > 0 {
                            detail_panel::draw(&mut frame, layout.dock, self);
                        }
                    }
                }
                Screen::Events => screens::draw_events(&mut frame, layout.main, self),
                Screen::Watches => screens::draw_watches(&mut frame, layout.main, self),
            }
            chrome::draw_fkeys(&mut frame, layout.fkeys, self);
            chrome::draw_status(&mut frame, layout.status, self, env);
            toasts::draw(&mut frame, layout.main, self);
            for index in 0..self.overlays.len() {
                overlays::draw(&mut frame, area, index, self);
            }
        }
        self.hits = hits;
    }

    /// Promote this frame's hit regions (called from the presenter commit).
    pub fn commit(&mut self) {
        self.hits.commit();
    }

    pub fn layout(&self, area: Rect) -> Layout {
        let header_h = if self.settings.show_meters {
            chrome::header_height(self, area.width).min(area.height.saturating_sub(6) / 2)
        } else {
            0
        };
        let mut y = area.y;
        let header = Rect::new(area.x, y, area.width, header_h);
        y += header_h;
        let toolbar_h = u16::from(area.height > header_h + 2);
        let search_w = (area.width / 3).clamp(18.min(area.width), 48);
        let toolbar = Rect::new(area.x, y, area.width.saturating_sub(search_w), toolbar_h);
        let search = Rect::new(area.x + toolbar.width, y, search_w, toolbar_h);
        y += toolbar_h;
        let bottom_h = 2.min(area.height.saturating_sub(y - area.y));
        let main_h = area
            .height
            .saturating_sub(y - area.y)
            .saturating_sub(bottom_h);
        let main = Rect::new(area.x, y, area.width, main_h);
        let fkeys = Rect::new(area.x, y + main_h, area.width, u16::from(bottom_h > 0));
        let status = Rect::new(area.x, y + main_h + 1, area.width, u16::from(bottom_h > 1));
        let (table, dock) = if self.detail.is_docked() && self.screen == Screen::Processes {
            match self.settings.dock {
                DockPosition::Bottom => {
                    let h = self
                        .settings
                        .dock_size
                        .clamp(6, main.height.saturating_sub(5).max(6))
                        .min(main.height);
                    (
                        Rect::new(main.x, main.y, main.width, main.height - h),
                        Rect::new(main.x, main.y + main.height - h, main.width, h),
                    )
                }
                DockPosition::Right => {
                    let w = (self.settings.dock_size * 4)
                        .clamp(30, main.width.saturating_sub(40).max(30))
                        .min(main.width);
                    (
                        Rect::new(main.x, main.y, main.width - w, main.height),
                        Rect::new(main.x + main.width - w, main.y, w, main.height),
                    )
                }
            }
        } else {
            (main, Rect::new(main.x, main.y + main.height, main.width, 0))
        };
        Layout {
            header,
            toolbar,
            search,
            main,
            table,
            dock,
            fkeys,
            status,
        }
    }

    fn hovering_table(&self) -> bool {
        if !self.settings.hover_freeze {
            return false;
        }
        let (Some(pos), Some(at)) = (self.pointer.position, self.pointer.moved_at) else {
            return false;
        };
        at.elapsed() < Duration::from_secs(3)
            && matches!(
                self.hits.at(pos),
                Some(Hit::Row { .. } | Hit::TreeToggle { .. } | Hit::TableBody)
            )
    }

    // ---- input -----------------------------------------------------------------

    pub fn handle_event(&mut self, event: &Event) -> Outcome {
        let normalized = normalize_key(event);
        let event = normalized.as_ref().unwrap_or(event);
        if let Event::Mouse(mouse) = event {
            self.pointer.position = Some(mouse.position);
            if matches!(mouse.kind, MouseEventKind::Move | MouseEventKind::Drag(_)) {
                self.pointer.moved_at = Some(Instant::now());
            }
        }
        // Modal overlays own all input while open.
        if let Some(mut top) = self.overlays.pop() {
            let index = self.overlays.len();
            let outcome = overlays::handle(&mut top, index, event, self);
            match outcome {
                overlays::OverlayOutcome::Close => {}
                _ => self.overlays.insert(index.min(self.overlays.len()), top),
            }
            return Outcome::Consumed;
        }
        match event {
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Key(key) => {
                if self.search.focused {
                    if search::handle_key(self, key) == Outcome::Consumed {
                        return Outcome::Consumed;
                    }
                }
                let consumed = match self.screen {
                    Screen::Processes => {
                        (self.detail.is_open()
                            && detail_panel::handle_key(self, key) == Outcome::Consumed)
                            || table::handle_key(self, key) == Outcome::Consumed
                    }
                    Screen::Events | Screen::Watches => {
                        screens::handle_key(self, key) == Outcome::Consumed
                    }
                };
                if consumed {
                    return Outcome::Consumed;
                }
                match Action::from_key(key) {
                    Some(action) => {
                        self.run(action);
                        Outcome::Consumed
                    }
                    None => Outcome::Ignored,
                }
            }
            Event::Paste(text) if self.search.focused => search::paste(self, text),
            _ => Outcome::Ignored,
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> Outcome {
        let hit = self.hits.at(mouse.position).cloned();
        if let MouseEventKind::Drag(MouseButton::Left) = mouse.kind
            && let Some(drag) = &mut self.pointer.drag
        {
            drag.last = mouse.position;
            let drag = drag.clone();
            return self.route_drag(&drag, mouse);
        }
        if let MouseEventKind::Up(MouseButton::Left) = mouse.kind
            && let Some(drag) = self.pointer.drag.take()
        {
            return self.route_drag(&drag, mouse);
        }
        let Some(hit) = hit else {
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.search.focused = false;
            }
            return Outcome::Ignored;
        };
        let mut double = false;
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let now = Instant::now();
            double = self.pointer.last_click.as_ref().is_some_and(|(t, p, h)| {
                now.duration_since(*t) <= DOUBLE_CLICK && *p == mouse.position && *h == hit
            });
            self.pointer.last_click = if double {
                None
            } else {
                Some((now, mouse.position, hit.clone()))
            };
            if matches!(hit, Hit::HeaderEdge { .. } | Hit::DetailSplitter) {
                self.pointer.drag = Some(Drag {
                    hit: hit.clone(),
                    origin: mouse.position,
                    last: mouse.position,
                });
            }
            if !matches!(hit, Hit::SearchBox | Hit::SearchClear) {
                self.search.focused = false;
            }
        }
        let click = Click {
            mouse,
            hit: &hit,
            double,
        };
        match &hit {
            Hit::Row { .. }
            | Hit::TreeToggle { .. }
            | Hit::Header { .. }
            | Hit::HeaderEdge { .. }
            | Hit::TableBody
            | Hit::TableScrollbar => table::handle_mouse(self, &click),
            Hit::Meter(_)
            | Hit::Preset(_)
            | Hit::Chip(_)
            | Hit::FKey(_)
            | Hit::Screen(_)
            | Hit::StatusNote => chrome::handle_mouse(self, &click),
            Hit::SearchBox | Hit::SearchClear => search::handle_mouse(self, &click),
            Hit::Toast { .. } | Hit::ToastAction { .. } => toasts::handle_mouse(self, &click),
            Hit::DetailTab(_)
            | Hit::DetailAction(_)
            | Hit::DetailClose
            | Hit::DetailDockToggle
            | Hit::DetailSplitter
            | Hit::DetailBody
            | Hit::DetailRow { .. }
            | Hit::ProcessLink { .. } => detail_panel::handle_mouse(self, &click),
            Hit::ScreenBody
            | Hit::EventRow { .. }
            | Hit::WatchRow { .. }
            | Hit::WatchControl { .. } => screens::handle_mouse(self, &click),
            Hit::OverlayBackdrop { .. }
            | Hit::OverlayBody { .. }
            | Hit::OverlayItem { .. }
            | Hit::OverlayControl { .. } => Outcome::Ignored,
        }
    }

    fn route_drag(&mut self, drag: &Drag, mouse: &MouseEvent) -> Outcome {
        match drag.hit {
            Hit::HeaderEdge { .. } => table::handle_drag(self, drag, mouse),
            Hit::DetailSplitter => detail_panel::handle_drag(self, drag, mouse),
            _ => Outcome::Ignored,
        }
    }

    /// Targets for process actions: marked rows, else the selection.
    pub fn targets(&self) -> Vec<ProcessKey> {
        let marked = self.table.marked();
        if !marked.is_empty() {
            return marked;
        }
        self.table.selected().into_iter().collect()
    }

    /// Execute an action from any source (key, F-key bar, menu, button).
    pub fn run(&mut self, action: Action) {
        use overlays::Overlay;
        let selected = self.table.selected();
        match action {
            Action::Help => self
                .overlays
                .push(Overlay::Help(help::HelpState::default())),
            Action::Settings => {
                self.overlays
                    .push(Overlay::Settings(settings::SettingsState::new(
                        &self.settings,
                    )))
            }
            Action::Search => {
                self.screen = Screen::Processes;
                self.search.focused = true;
            }
            Action::FilterMenu => self.overlays.push(overlays::filter_menu(self)),
            Action::Tree => {
                self.settings.tree = !self.settings.tree;
                self.table.mark_dirty();
                self.flash(if self.settings.tree {
                    "Tree view"
                } else {
                    "List view"
                });
                self.request(UiRequest::SaveSettings);
            }
            Action::SortMenu => self.overlays.push(overlays::sort_menu(self)),
            Action::Columns => self.overlays.push(overlays::column_chooser(self)),
            Action::NiceUp | Action::NiceDown => {
                let targets = self.targets();
                if !targets.is_empty() {
                    self.overlays.push(overlays::renice_dialog(
                        self,
                        targets,
                        action == Action::NiceUp,
                    ));
                }
            }
            Action::Signal => {
                let targets = self.targets();
                if targets.is_empty() {
                    self.flash("Select a process first");
                } else {
                    self.overlays
                        .push(overlays::signal_dialog(self, targets, Signal::Term));
                }
            }
            Action::Terminate | Action::Kill | Action::Suspend | Action::Resume => {
                let signal = match action {
                    Action::Terminate => Signal::Term,
                    Action::Kill => Signal::Kill,
                    Action::Suspend => Signal::Stop,
                    _ => Signal::Cont,
                };
                let targets = self.targets();
                if targets.is_empty() {
                    return;
                }
                if self.settings.confirm_signals && matches!(signal, Signal::Term | Signal::Kill) {
                    self.overlays
                        .push(overlays::signal_dialog(self, targets, signal));
                } else {
                    self.request(UiRequest::Action(ActionRequest::Signal { targets, signal }));
                }
            }
            Action::Quit => self.request(UiRequest::Quit),
            Action::Details => {
                if let Some(key) = selected {
                    if self.detail.key() == Some(key) && self.detail.is_open() {
                        self.detail.set_full(!self.detail.is_full());
                    } else {
                        self.detail.open(key, false);
                        self.request(UiRequest::LoadDetail(key));
                    }
                }
            }
            Action::FullDetails => {
                if let Some(key) = selected {
                    self.detail.open(key, true);
                    self.request(UiRequest::LoadDetail(key));
                }
            }
            Action::CloseDetails => {
                if !self.search.text.is_empty() {
                    self.search.clear();
                    self.table.mark_dirty();
                } else if self.detail.is_open() {
                    self.detail.close();
                } else if !self.table.marked().is_empty() {
                    self.table.clear_marks();
                } else if self.screen != Screen::Processes {
                    self.screen = Screen::Processes;
                }
            }
            Action::ToggleDock => {
                self.settings.dock = match self.settings.dock {
                    DockPosition::Bottom => DockPosition::Right,
                    DockPosition::Right => DockPosition::Bottom,
                };
                self.request(UiRequest::SaveSettings);
            }
            Action::ContextMenu => {
                if let Some(key) = selected {
                    let at = self
                        .hits
                        .rect_of(|h| *h == Hit::Row { key })
                        .map(|r| Point::new(r.x + 8, r.y + 1))
                        .unwrap_or(Point::new(self.last_area.x + 4, self.last_area.y + 4));
                    self.overlays.push(overlays::process_menu(self, key, at));
                }
            }
            Action::Mark => {
                if let Some(key) = selected {
                    self.table.toggle_mark(key);
                    self.table.select_next(1);
                }
            }
            Action::MarkTree => {
                if let Some(key) = selected {
                    let mut keys = vec![key];
                    keys.extend(self.store.descendants(&key));
                    self.table.mark_all(keys);
                }
            }
            Action::ClearMarks => self.table.clear_marks(),
            Action::Pin => {
                if let Some(key) = selected {
                    let pinned = self.table.toggle_pin(key);
                    self.flash(format!(
                        "{} {}",
                        if pinned { "Pinned" } else { "Unpinned" },
                        self.name_of(&key)
                    ));
                }
            }
            Action::Freeze => {
                let frozen = self.table.toggle_freeze();
                self.flash(if frozen {
                    "Sort order frozen"
                } else {
                    "Sort order live"
                });
            }
            Action::Pause => {
                self.paused = !self.paused;
                self.request(UiRequest::Pause(self.paused));
                self.flash(if self.paused {
                    "Updates paused"
                } else {
                    "Updates resumed"
                });
            }
            Action::Expand | Action::Collapse => {
                if let Some(key) = selected {
                    self.table.set_collapsed(key, action == Action::Collapse);
                }
            }
            Action::ExpandAll => self.table.expand_all(),
            Action::CollapseAll => {
                let roots: Vec<ProcessKey> = self.store.rows().iter().map(|r| r.key()).collect();
                self.table.collapse_all(roots);
            }
            Action::GoParent => {
                if let Some(parent) =
                    selected.and_then(|k| self.store.ancestors(&k).first().copied())
                {
                    self.table.select(parent);
                }
            }
            Action::NextPreset | Action::PrevPreset => {
                use crate::procs::columns::Preset;
                let i = Preset::ALL
                    .iter()
                    .position(|p| *p == self.settings.preset)
                    .unwrap_or(0);
                let n = Preset::ALL.len();
                let next = if action == Action::NextPreset {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                self.set_preset(Preset::ALL[next]);
            }
            Action::ToggleUsers => {
                self.settings.only_mine = !self.settings.only_mine;
                self.table.mark_dirty();
                self.request(UiRequest::SaveSettings);
            }
            Action::ToggleKernelThreads => {
                self.settings.hide_kernel_threads = !self.settings.hide_kernel_threads;
                self.table.mark_dirty();
                self.request(UiRequest::SaveSettings);
            }
            Action::ToggleNetActive => {
                self.settings.net_active_only = !self.settings.net_active_only;
                self.table.mark_dirty();
                self.request(UiRequest::SaveSettings);
            }
            Action::ToggleMeters => {
                self.settings.show_meters = !self.settings.show_meters;
                self.request(UiRequest::SaveSettings);
            }
            Action::Processes => self.screen = Screen::Processes,
            Action::Events => self.screen = Screen::Events,
            Action::Watches => self.screen = Screen::Watches,
            Action::WatchProcess => {
                if let Some(key) = selected {
                    self.overlays.push(screens::watch_editor_for(self, key));
                }
            }
            Action::Export => self.overlays.push(overlays::export_menu(self)),
            Action::Throttle => {
                if let Some(key) = selected {
                    let name = self.name_of(&key);
                    self.request(UiRequest::Legacy(LegacyRequest::OpenThrottle {
                        pid: key.pid,
                        name,
                    }));
                }
            }
            Action::RemoveThrottle => {
                if let Some(key) = selected {
                    let name = self.name_of(&key);
                    self.request(UiRequest::Legacy(LegacyRequest::RemoveThrottle {
                        pid: key.pid,
                        name,
                    }));
                }
            }
            Action::Interfaces => self.request(UiRequest::Legacy(LegacyRequest::Interfaces)),
            Action::TrafficMode => self.request(UiRequest::Legacy(LegacyRequest::TrafficMode)),
            Action::Graph => self.request(UiRequest::Legacy(LegacyRequest::Graph)),
            Action::Backends => self.request(UiRequest::Legacy(LegacyRequest::Backends)),
        }
    }

    pub fn set_preset(&mut self, preset: crate::procs::columns::Preset) {
        self.settings.preset = preset;
        self.table.on_preset_changed(&self.settings);
        self.request(UiRequest::SaveSettings);
    }
}

/// A mouse event resolved against the committed hit map.
pub struct Click<'a> {
    pub mouse: &'a MouseEvent,
    pub hit: &'a Hit,
    /// Second left press on the same target within `DOUBLE_CLICK`.
    pub double: bool,
}

impl Click<'_> {
    pub fn is_left(&self) -> bool {
        matches!(self.mouse.kind, MouseEventKind::Down(MouseButton::Left))
    }
    pub fn is_right(&self) -> bool {
        matches!(self.mouse.kind, MouseEventKind::Down(MouseButton::Right))
    }
    pub fn is_middle(&self) -> bool {
        matches!(self.mouse.kind, MouseEventKind::Down(MouseButton::Middle))
    }
    pub fn wheel(&self) -> i32 {
        match self.mouse.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use bmux_keyboard::{KeyCode, KeyStroke, Modifiers};

    #[test]
    fn shifted_letters_run_uppercase_bindings() {
        let mut state = ExplorerState::default();
        let mut shift = Modifiers::NONE;
        shift.shift = true;
        let tree = state.settings.tree;
        state.handle_event(&Event::Key(KeyStroke::with_modifiers(KeyCode::Char('t'), shift)));
        assert_ne!(state.settings.tree, tree, "Shift+t must toggle tree, not throttle");
        assert!(
            !state
                .take_requests()
                .iter()
                .any(|r| matches!(r, UiRequest::Legacy(LegacyRequest::OpenThrottle { .. })))
        );
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Layout {
    pub header: Rect,
    pub toolbar: Rect,
    pub search: Rect,
    pub main: Rect,
    pub table: Rect,
    pub dock: Rect,
    pub fkeys: Rect,
    pub status: Rect,
}

/// Terminals report Shift+letter as the lowercase key plus a Shift modifier;
/// every handler expects the typed (uppercase) character instead.
fn normalize_key(event: &Event) -> Option<Event> {
    let Event::Key(key) = event else { return None };
    match key.key {
        bmux_keyboard::KeyCode::Char(c) if key.modifiers.shift && c.is_ascii_lowercase() => {
            let mut key = key.clone();
            key.key = bmux_keyboard::KeyCode::Char(c.to_ascii_uppercase());
            Some(Event::Key(key))
        }
        _ => None,
    }
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    Some(unsafe { libc::getuid() })
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}
