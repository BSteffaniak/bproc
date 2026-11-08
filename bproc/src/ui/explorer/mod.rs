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

/// Network note shown while packet capture looks unavailable.
pub const CAPTURE_PRIVILEGE_NOTE: &str =
    "Per-process bandwidth needs packet-capture privileges — run with sudo";

/// Environment supplied by the runtime for drawing.
#[derive(Debug, Clone, Default)]
pub struct DrawEnv<'a> {
    /// Legacy status message (throttle/backend results).
    pub status: &'a str,
    pub interface_filter: Option<String>,
    /// Active socket mapper, for painters that show the capture source.
    #[allow(dead_code)]
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
        let firings = self.watch.evaluate(&evaluate, &self.store);
        let firings = guard_firings(firings, &self.store);
        for firing in firings {
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
            && escalates(*signal)
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
        // An idle, empty box gives its room to the toolbar tabs and chips.
        let search_w = if self.search.text.is_empty() && !self.search.focused {
            (area.width / 4).clamp(14.min(area.width), 40)
        } else {
            (area.width / 3).clamp(18.min(area.width), 48)
        };
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
                    let w = self
                        .settings
                        .dock_size
                        .saturating_mul(4)
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

    /// True while hover-freeze holds the row order (pointer over the table).
    /// Painters use it to explain why rows look out of sort order.
    pub fn hovering_table(&self) -> bool {
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
                if self.search.focused && search::handle_key(self, key) == Outcome::Consumed {
                    return Outcome::Consumed;
                }
                let consumed = match self.screen {
                    // The table is hidden behind a full-screen inspector: keys
                    // must never move its selection (it is not what is shown).
                    Screen::Processes if self.detail.is_full() => {
                        let key = inspector_key(key);
                        detail_panel::handle_key(self, &key) == Outcome::Consumed
                    }
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
        // A press always starts a fresh gesture. Finish a drag whose button
        // release was lost (released outside the window) at its last point so
        // it cannot hijack the next drag.
        if matches!(mouse.kind, MouseEventKind::Down(_))
            && let Some(stale) = self.pointer.drag.take()
        {
            let release = MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                position: stale.last,
                modifiers: mouse.modifiers,
            };
            self.route_drag(&stale, &release);
        }
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
            if matches!(
                hit,
                Hit::HeaderEdge { .. } | Hit::DetailSplitter | Hit::TableScrollbar
            ) {
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
            // The track is one column wide: keep scrolling by the pointer's
            // row wherever it drifts horizontally until the button is released.
            Hit::TableScrollbar => {
                if matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left)) {
                    let synthetic = MouseEvent {
                        kind: MouseEventKind::Drag(MouseButton::Left),
                        position: Point::new(drag.origin.x, mouse.position.y),
                        modifiers: mouse.modifiers,
                    };
                    let click = Click {
                        mouse: &synthetic,
                        hit: &Hit::TableScrollbar,
                        double: false,
                    };
                    table::handle_mouse(self, &click);
                }
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }

    /// The single process the active screen's highlight refers to: the
    /// inspected process while the inspector is full screen, else the table
    /// selection; the selected event's process on Events; the focused
    /// alerting process on Watches (none while the rule list has focus).
    pub fn screen_subject(&self) -> Option<ProcessKey> {
        match self.screen {
            Screen::Processes if self.detail.is_full() => self.detail.key(),
            Screen::Processes => self.table.selected(),
            Screen::Events => {
                let seq = self.events_view.selected?;
                self.events_view
                    .visible(&self.events)
                    .into_iter()
                    .find(|(s, _)| *s == seq)
                    .and_then(|(_, e)| e.key)
            }
            Screen::Watches => {
                let view = &self.watches_view;
                if view.focus_alerts {
                    view.alert_keys.get(view.alert_selected).copied()
                } else {
                    None
                }
            }
        }
    }

    fn is_alive(&self, key: &ProcessKey) -> bool {
        self.store.get(key).is_some_and(|r| !r.is_ghost())
    }

    /// The one rule for multi-process actions, shared by keyboard, F-keys and
    /// context menus: marks apply only when the focused row is itself marked;
    /// otherwise the action targets just the focused row.
    pub fn action_targets(&self, focus: Option<ProcessKey>) -> Vec<ProcessKey> {
        let marked = self.table.marked();
        match focus {
            Some(key) if marked.contains(&key) => marked,
            Some(key) => vec![key],
            None => marked,
        }
    }

    /// Targets for process actions on the active screen. Off the Processes
    /// screen (and behind a full-screen inspector) marks never apply: the
    /// action targets only what is highlighted.
    pub fn targets(&self) -> Vec<ProcessKey> {
        match self.screen {
            Screen::Processes if !self.detail.is_full() => {
                self.action_targets(self.table.selected())
            }
            Screen::Processes => self.detail.key().into_iter().collect(),
            Screen::Events | Screen::Watches => self
                .screen_subject()
                .filter(|k| self.is_alive(k))
                .into_iter()
                .collect(),
        }
    }

    /// How many of `targets` are not rows of the visible table (filtered
    /// out, under a collapsed node, or behind another screen).
    pub fn hidden_targets(&self, targets: &[ProcessKey]) -> usize {
        if self.screen != Screen::Processes || self.detail.is_full() {
            return 0;
        }
        targets
            .iter()
            .filter(|k| !self.table.rows().iter().any(|r| r.key == **k))
            .count()
    }

    /// Execute an action from any source (key, F-key bar, menu, button).
    pub fn run(&mut self, action: Action) {
        if self.screen != Screen::Processes
            && action.is_process_scoped()
            && !self.prepare_off_screen(action)
        {
            return;
        }
        // Off the Processes screen `prepare_off_screen` already moved the
        // table selection to the highlighted row's process.
        let selected = match self.screen {
            Screen::Processes => self.screen_subject(),
            Screen::Events | Screen::Watches => self.table.selected(),
        };
        if self.screen != Screen::Processes && action.is_process_scoped() {
            // Exactly the highlighted row's process; marks never apply here.
            return self.run_for(action, selected.into_iter().collect());
        }
        let targets = self.targets();
        self.dispatch(action, selected, targets);
    }

    /// Run a process action against explicit targets, ignoring marks and the
    /// selection (inspector buttons, toast actions).
    pub fn run_for(&mut self, action: Action, targets: Vec<ProcessKey>) {
        let selected = targets.first().copied();
        self.dispatch(action, selected, targets);
    }

    /// Resolve a process action pressed on the Events or Watches screen to
    /// the highlighted row's process. Returns false when the action was
    /// fully handled (or refused) here.
    fn prepare_off_screen(&mut self, action: Action) -> bool {
        let Some(key) = self.screen_subject() else {
            self.flash(match self.screen {
                Screen::Watches => "Tab to an alerting process to act on it",
                _ => "Select an event tied to a running process",
            });
            return false;
        };
        if !self.is_alive(&key) {
            let name = self.name_of(&key);
            self.flash(format!("{name} (PID {}) is no longer running", key.pid));
            return false;
        }
        match action {
            Action::Details | Action::FullDetails => {
                if screens::jump_to_process(self, key) && action == Action::FullDetails {
                    self.detail.set_full(true);
                }
                false
            }
            Action::Mark
            | Action::MarkTree
            | Action::Expand
            | Action::Collapse
            | Action::GoParent => {
                self.flash("Switch to Processes (1) to use that on the table");
                false
            }
            _ => {
                self.table.select(key);
                true
            }
        }
    }

    /// Where a keyboard-opened context menu appears: next to the highlighted
    /// row of the active screen.
    fn menu_anchor(&self, key: ProcessKey) -> Point {
        let rect = match self.screen {
            Screen::Processes => self.hits.rect_of(|h| *h == Hit::Row { key }),
            Screen::Events => self.events_view.selected.and_then(|seq| {
                self.hits.rect_of(|h| {
                    *h == Hit::EventRow {
                        index: seq as usize,
                    }
                })
            }),
            Screen::Watches => self.hits.rect_of(|h| *h == Hit::ProcessLink { key }),
        };
        rect.map(|r| Point::new(r.x + 8, r.y + 1))
            .unwrap_or(Point::new(self.last_area.x + 4, self.last_area.y + 4))
    }

    fn dispatch(&mut self, action: Action, selected: Option<ProcessKey>, targets: Vec<ProcessKey>) {
        use overlays::Overlay;
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
                if targets.is_empty() {
                    self.flash("Select a process first");
                } else {
                    overlays::open_renice(self, targets, action == Action::NiceUp);
                }
            }
            Action::Signal => {
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
                if targets.is_empty() {
                    self.flash("Select a process first");
                    return;
                }
                // Never act on processes the user cannot see without asking.
                self.sync_rows();
                let hidden = self.hidden_targets(&targets) > 0;
                if hidden
                    || (self.settings.confirm_signals
                        && matches!(signal, Signal::Term | Signal::Kill))
                {
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
                // Off the Processes screen the panel, search and marks are not
                // visible: Esc first returns to where they are.
                if self.screen != Screen::Processes {
                    self.screen = Screen::Processes;
                } else if !self.search.text.is_empty() {
                    self.search.clear();
                    self.table.mark_dirty();
                } else if self.detail.is_open() {
                    self.detail.close();
                } else if !self.table.marked().is_empty() {
                    self.table.clear_marks();
                } else {
                    // Nothing else wanted Esc: dismiss the newest notification.
                    toasts::dismiss_newest(self);
                }
            }
            Action::RunToast => {
                if !toasts::run_newest_action(self) {
                    self.flash("No notification with an action");
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
                    let at = self.menu_anchor(key);
                    self.overlays.push(overlays::process_menu(self, key, at));
                }
            }
            Action::Mark => {
                if let Some(key) = selected {
                    self.table.toggle_mark(key);
                    // Advance only while the table is what the user sees.
                    if !self.detail.is_full() {
                        self.table.select_next(1);
                    }
                }
            }
            Action::MarkTree => {
                if let Some(key) = selected {
                    let mut keys = vec![key];
                    keys.extend(self.store.descendants(&key));
                    // A marked bproc would become a signal target.
                    let (keys, skipped_self) = crate::procs::actions::exclude_self(keys);
                    self.table.mark_all(keys);
                    if skipped_self {
                        self.flash("bproc itself is never marked");
                    }
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
                let Some(parent) = selected.and_then(|k| self.store.ancestors(&k).first().copied())
                else {
                    if selected.is_some() {
                        self.flash("No parent process");
                    }
                    return;
                };
                if self.detail.is_full() {
                    // Behind a full-screen inspector, inspect the parent.
                    self.detail.open(parent, true);
                    self.request(UiRequest::LoadDetail(parent));
                    self.select_visible(parent);
                } else if !self.select_visible(parent) {
                    self.flash(format!(
                        "Parent {} (PID {}) is hidden by the current filter",
                        self.name_of(&parent),
                        parent.pid
                    ));
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
                    if self.store.get(&key).is_some_and(|r| r.is_ghost()) {
                        self.flash(format!("{name} (PID {}) has exited", key.pid));
                        return;
                    }
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

    /// Rebuild the table rows now if a filter, sort or snapshot changed since
    /// the last draw, so visibility checks see what the next frame shows.
    pub fn sync_rows(&mut self) {
        let hovering = self.hovering_table();
        self.table.refresh(
            &self.store,
            &self.settings,
            &self.search,
            self.traffic,
            self.uid,
            hovering,
        );
    }

    /// Select `key` only when it is a visible table row. Returns whether it
    /// was selected.
    pub fn select_visible(&mut self, key: ProcessKey) -> bool {
        self.sync_rows();
        let visible = self.table.rows().iter().any(|r| r.key == key);
        if visible {
            self.table.select(key);
        }
        visible
    }

    pub fn set_preset(&mut self, preset: crate::procs::columns::Preset) {
        self.settings.preset = preset;
        // A preset brings its own default sort (always descending, as in
        // `TableView::on_preset_changed`); drop the saved override so a
        // restart opens the preset the way it looks now.
        self.settings.sort_column = None;
        self.settings.sort_descending = true;
        self.table.on_preset_changed(&self.settings);
        self.request(UiRequest::SaveSettings);
    }
}

/// Inside the full-screen inspector, j/k scroll like Down/Up.
fn inspector_key(key: &bmux_keyboard::KeyStroke) -> bmux_keyboard::KeyStroke {
    use bmux_keyboard::KeyCode;
    let mut key = *key;
    if !key.modifiers.ctrl && !key.modifiers.alt {
        key.key = match key.key {
            KeyCode::Char('j') => KeyCode::Down,
            KeyCode::Char('k') => KeyCode::Up,
            other => other,
        };
    }
    key
}

/// Whether a delivered signal should be followed by a "still running —
/// force kill?" check. SIGHUP usually reloads a daemon, so it never escalates.
pub fn escalates(signal: Signal) -> bool {
    matches!(signal, Signal::Term | Signal::Int | Signal::Quit)
}

/// Most signals one watch rule may send per evaluation; further firings of
/// that rule are downgraded to notifications.
pub const MAX_WATCH_SIGNALS_PER_RULE: usize = 5;

/// PIDs a watch rule must never signal: PID 0/1, bproc itself and its
/// ancestors (the shell and terminal it runs in).
pub fn protected_pids(store: &ProcessStore) -> HashSet<i32> {
    let own = std::process::id() as i32;
    let mut pids: HashSet<i32> = [0, 1, own].into_iter().collect();
    #[cfg(unix)]
    pids.insert(unsafe { libc::getppid() });
    if let Some(row) = store.by_pid(own) {
        pids.extend(store.ancestors(&row.key()).iter().map(|k| k.pid));
    }
    pids
}

/// Guardrails for watch-rule signals, shared by the explorer and `bproc
/// watch`: a Signal firing becomes a notification when it targets a protected
/// process or exceeds the per-rule budget for this evaluation.
pub fn guard_firings(
    firings: Vec<crate::procs::watch::Firing>,
    store: &ProcessStore,
) -> Vec<crate::procs::watch::Firing> {
    if !firings
        .iter()
        .any(|f| matches!(f.action, WatchAction::Signal(_)))
    {
        return firings;
    }
    let protected = protected_pids(store);
    let mut sent: std::collections::HashMap<u64, usize> = Default::default();
    firings
        .into_iter()
        .map(|mut firing| {
            if let WatchAction::Signal(signal) = firing.action {
                let reason = if protected.contains(&firing.key.pid) {
                    Some("protected process")
                } else {
                    let count = sent.entry(firing.rule_id).or_default();
                    *count += 1;
                    (*count > MAX_WATCH_SIGNALS_PER_RULE).then_some("rule signal limit reached")
                };
                if let Some(reason) = reason {
                    firing.action = WatchAction::Notify;
                    firing.message =
                        format!("{} ({} skipped: {reason})", firing.message, signal.name());
                }
            }
            firing
        })
        .collect()
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
        state.handle_event(&Event::Key(KeyStroke::with_modifiers(
            KeyCode::Char('t'),
            shift,
        )));
        assert_ne!(
            state.settings.tree, tree,
            "Shift+t must toggle tree, not throttle"
        );
        assert!(
            !state
                .take_requests()
                .iter()
                .any(|r| matches!(r, UiRequest::Legacy(LegacyRequest::OpenThrottle { .. })))
        );
    }
}

#[cfg(test)]
mod routing_tests {
    use super::table::fixtures::{key, many, press, press_with, refresh, render, type_text};
    use super::*;
    use bmux_keyboard::{KeyCode, Modifiers};
    use bmux_tui::event::MouseModifiers;
    use overlays::Overlay;

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::NONE
        }
    }

    fn signal_targets(state: &ExplorerState) -> Option<Vec<ProcessKey>> {
        match state.overlays.last()? {
            Overlay::Signal(dialog) => Some(dialog.targets.clone()),
            _ => None,
        }
    }

    fn menu_targets(state: &ExplorerState) -> Option<Vec<ProcessKey>> {
        match state.overlays.last()? {
            Overlay::Menu(menu) => Some(menu.targets.clone()),
            _ => None,
        }
    }

    fn mouse(state: &mut ExplorerState, kind: MouseEventKind, x: u16, y: u16) {
        state.handle_event(&Event::Mouse(
            MouseEvent::new(kind, Point::new(x, y)).with_modifiers(MouseModifiers::NONE),
        ));
    }

    fn first_visible_row(state: &ExplorerState) -> Option<ProcessKey> {
        state
            .hits
            .committed()
            .iter()
            .filter_map(|(r, h)| match h {
                Hit::Row { key } => Some((r.y, *key)),
                _ => None,
            })
            .min_by_key(|(y, _)| *y)
            .map(|(_, k)| k)
    }

    fn on_events_screen_with_event_for(pid: i32) -> ExplorerState {
        let mut state = super::table::fixtures::state_with(many(5));
        state.table.select(key(2));
        state.events.push(ProcEvent {
            at_ms: 0,
            kind: EventKind::Alert,
            key: Some(key(pid)),
            name: format!("proc{pid}"),
            message: "hot".into(),
        });
        state.screen = Screen::Events;
        state.events_view.selected = Some(state.events.pushed() - 1);
        state
    }

    #[test]
    fn full_screen_inspector_keys_never_retarget_the_hidden_table() {
        let mut state = super::table::fixtures::state_with(many(5));
        state.table.select(key(1));
        press_with(&mut state, KeyCode::Enter, shift());
        assert!(state.detail.is_full());
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(
            state.table.selected(),
            Some(key(1)),
            "j scrolls, not selects"
        );
        press(&mut state, KeyCode::Char('x'));
        assert_eq!(signal_targets(&state), Some(vec![key(1)]));
    }

    #[test]
    fn full_screen_inspector_ignores_hidden_marks() {
        let mut state = super::table::fixtures::state_with(many(5));
        state.table.mark_all(vec![key(3), key(4)]);
        state.table.select(key(1));
        press_with(&mut state, KeyCode::Enter, shift());
        assert_eq!(state.targets(), vec![key(1)]);
        press(&mut state, KeyCode::F(9));
        assert_eq!(signal_targets(&state), Some(vec![key(1)]));
    }

    #[test]
    fn go_parent_never_selects_a_filtered_out_row() {
        let mut state = super::table::fixtures::state_with(many(5));
        press(&mut state, KeyCode::Char('/'));
        type_text(&mut state, "proc5");
        press(&mut state, KeyCode::Enter);
        state.search.focused = false;
        refresh(&mut state);
        assert_eq!(state.table.selected(), Some(key(5)));
        press(&mut state, KeyCode::Backspace);
        assert_eq!(state.table.selected(), Some(key(5)));
        assert!(
            state
                .flash
                .as_ref()
                .is_some_and(|(m, _)| m.contains("hidden"))
        );
        press(&mut state, KeyCode::Char('x'));
        assert_eq!(signal_targets(&state), Some(vec![key(5)]));
    }

    #[test]
    fn go_parent_selects_a_visible_parent() {
        let mut state = super::table::fixtures::state_with(many(5));
        state.table.select(key(4));
        press(&mut state, KeyCode::Backspace);
        assert_eq!(state.table.selected(), Some(key(1)));
    }

    #[test]
    fn events_screen_process_keys_act_on_the_selected_event() {
        let mut state = on_events_screen_with_event_for(4);
        press(&mut state, KeyCode::Char('x'));
        assert_eq!(signal_targets(&state), Some(vec![key(4)]));
        state.overlays.clear();
        press(&mut state, KeyCode::F(8));
        assert!(matches!(state.overlays.last(), Some(Overlay::Renice(_))));
        state.overlays.clear();
        press(&mut state, KeyCode::Char('.'));
        assert_eq!(menu_targets(&state), Some(vec![key(4)]));
    }

    #[test]
    fn events_screen_never_uses_marks_or_the_hidden_selection() {
        let mut state = on_events_screen_with_event_for(4);
        state.table.mark_all(vec![key(2), key(3)]);
        assert_eq!(state.targets(), vec![key(4)]);
        state.events_view.selected = None;
        assert!(state.targets().is_empty());
        press(&mut state, KeyCode::F(9));
        assert!(state.overlays.is_empty(), "no event selected → no dialog");
        assert!(state.flash.is_some());
    }

    #[test]
    fn events_screen_details_jump_to_processes() {
        let mut state = on_events_screen_with_event_for(4);
        state.run(Action::Details);
        assert_eq!(state.screen, Screen::Processes);
        assert_eq!(state.detail.key(), Some(key(4)));
    }

    #[test]
    fn watches_rule_focus_has_no_process_target() {
        let mut state = super::table::fixtures::state_with(many(5));
        state.table.select(key(2));
        state.screen = Screen::Watches;
        press(&mut state, KeyCode::Char('x'));
        assert!(state.overlays.is_empty());
        state.watches_view.alert_keys = vec![key(3)];
        state.watches_view.focus_alerts = true;
        press(&mut state, KeyCode::Char('x'));
        assert_eq!(signal_targets(&state), Some(vec![key(3)]));
    }

    #[test]
    fn hidden_marks_do_not_override_an_unmarked_selection() {
        let mut state = super::table::fixtures::state_with(many(5));
        state.table.mark_all(vec![key(2), key(3)]);
        press(&mut state, KeyCode::Char('/'));
        type_text(&mut state, "proc5");
        press(&mut state, KeyCode::Enter);
        state.search.focused = false;
        refresh(&mut state);
        assert_eq!(state.table.marked(), vec![key(2), key(3)]);
        state.table.select(key(5));
        press(&mut state, KeyCode::Char('x'));
        let keyboard = signal_targets(&state);
        state.overlays.clear();
        press(&mut state, KeyCode::Char('.'));
        let menu = menu_targets(&state);
        assert_eq!(keyboard, Some(vec![key(5)]));
        assert_eq!(keyboard, menu, "keyboard and menu agree");
    }

    #[test]
    fn marked_selection_targets_all_marks_and_confirms_hidden_ones() {
        let mut state = super::table::fixtures::state_with(many(5));
        state.settings.confirm_signals = false;
        state.table.mark_all(vec![key(2), key(5)]);
        press(&mut state, KeyCode::Char('/'));
        type_text(&mut state, "proc5");
        press(&mut state, KeyCode::Enter);
        state.search.focused = false;
        refresh(&mut state);
        state.table.select(key(5));
        assert_eq!(state.targets(), vec![key(2), key(5)]);
        assert_eq!(state.hidden_targets(&state.targets()), 1);
        state.run(Action::Suspend);
        assert!(
            matches!(state.overlays.last(), Some(Overlay::Signal(_))),
            "hidden targets always confirm"
        );
        assert!(
            !state
                .take_requests()
                .iter()
                .any(|r| matches!(r, UiRequest::Action(_)))
        );
    }

    #[test]
    fn preset_change_resets_the_saved_sort() {
        use crate::procs::columns::{ColumnId, Preset};
        let mut state = super::table::fixtures::state_with(many(5));
        super::table::sort_by(&mut state, ColumnId::Rss);
        state.set_preset(Preset::Network);
        assert_eq!(state.table.sort().column, ColumnId::NetDown);
        let restarted = table::TableView::new(&state.settings);
        assert_eq!(restarted.sort().column, ColumnId::NetDown);
        assert!(restarted.sort().descending);
    }

    #[test]
    fn scrollbar_drag_survives_horizontal_drift() {
        let mut state = super::table::fixtures::state_with(many(200));
        render(&mut state, 100, 20);
        let track = state
            .hits
            .rect_of(|h| *h == Hit::TableScrollbar)
            .expect("scrollbar");
        assert_eq!(first_visible_row(&state), Some(key(1)));
        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            track.x,
            track.y,
        );
        mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            track.x.saturating_sub(3),
            track.y + track.height - 1,
        );
        render(&mut state, 100, 20);
        assert_ne!(first_visible_row(&state), Some(key(1)));
        mouse(
            &mut state,
            MouseEventKind::Up(MouseButton::Left),
            track.x.saturating_sub(3),
            track.y + track.height - 1,
        );
        assert!(state.pointer.drag.is_none());
    }

    #[test]
    fn a_new_press_finishes_a_drag_whose_release_was_lost() {
        let mut state = super::table::fixtures::state_with(many(50));
        render(&mut state, 100, 20);
        let edge = state
            .hits
            .committed()
            .iter()
            .find(|(_, h)| matches!(h, Hit::HeaderEdge { .. }))
            .cloned()
            .expect("header edge");
        let Hit::HeaderEdge { column } = edge.1 else {
            unreachable!()
        };
        let (x, y) = (edge.0.x, edge.0.y);
        mouse(&mut state, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            x + 4,
            y,
        );
        let resized = table::column_width(&state.settings, column);
        // Release lost; the next gesture starts on a row.
        let row = state
            .hits
            .rect_of(|h| matches!(h, Hit::Row { .. }))
            .expect("row");
        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            row.x + 2,
            row.y,
        );
        assert!(state.pointer.drag.is_none());
        mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            row.x + 30,
            row.y,
        );
        assert_eq!(table::column_width(&state.settings, column), resized);
    }

    #[test]
    fn esc_off_the_processes_screen_returns_first() {
        let mut state = super::table::fixtures::state_with(many(5));
        state.table.select(key(1));
        press(&mut state, KeyCode::Enter);
        state.table.mark_all(vec![key(2), key(3)]);
        state.screen = Screen::Events;
        press(&mut state, KeyCode::Escape);
        assert_eq!(state.screen, Screen::Processes);
        assert!(state.detail.is_open());
        assert_eq!(state.table.marked().len(), 2);
    }

    #[test]
    fn hangup_never_offers_force_kill() {
        assert!(!escalates(Signal::Hup));
        assert!(!escalates(Signal::Kill));
        assert!(escalates(Signal::Term));
    }

    fn firing(rule_id: u64, pid: i32) -> crate::procs::watch::Firing {
        crate::procs::watch::Firing {
            rule_id,
            key: ProcessKey::new(pid, 1),
            name: "p".into(),
            message: "hot".into(),
            action: WatchAction::Signal(Signal::Kill),
        }
    }

    #[test]
    fn watch_signals_skip_bproc_its_ancestors_and_init() {
        let store = ProcessStore::new();
        let own = std::process::id() as i32;
        let guarded = guard_firings(vec![firing(1, own), firing(1, 1), firing(1, 0)], &store);
        assert!(
            guarded
                .iter()
                .all(|f| f.action == WatchAction::Notify && f.message.contains("skipped"))
        );
        #[cfg(unix)]
        {
            let parent = unsafe { libc::getppid() };
            let guarded = guard_firings(vec![firing(1, parent)], &store);
            assert_eq!(guarded[0].action, WatchAction::Notify);
        }
    }

    #[test]
    fn watch_signals_are_capped_per_rule() {
        let store = ProcessStore::new();
        let firings: Vec<_> = (0..20).map(|i| firing(7, 100_000 + i)).collect();
        let guarded = guard_firings(firings, &store);
        let signals = guarded
            .iter()
            .filter(|f| matches!(f.action, WatchAction::Signal(_)))
            .count();
        assert_eq!(signals, MAX_WATCH_SIGNALS_PER_RULE);
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
            let mut key = *key;
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
