//! Events timeline and Watches screens, plus the watch-rule editor overlay.
pub mod editor;
pub mod input;

use super::hits::{Hit, Screen};
use super::overlays::{self, Overlay, OverlayOutcome};
use super::paint::{Frame, fit, theme, truncate};
use super::{Click, ExplorerState, Outcome, UiRequest};
use crate::procs::ProcessKey;
use crate::procs::model::fmt;
use crate::procs::store::Row;
use crate::procs::watch::{EventKind, EventLog, ProcEvent, WatchRule, WatchTarget};
use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::event::Event;
use bmux_tui::geometry::{Point, Rect};
use bmux_tui::style::{Color, Style};
use bmux_tui::text::{Line, Span};
use bmux_tui::text_width::display_width;
pub use editor::{EditorForm, WatchEditor};
use input::{is, is_space, typed_char};

/// `WatchControl.index` sentinels for controls that are not rule-specific.
const EVENTS_CTRL: usize = usize::MAX;
const WATCH_BUTTONS: usize = usize::MAX - 1;
const ALERT_ROWS: usize = usize::MAX - 2;
/// `WatchControl.control` for a rule's enable checkbox.
const CHECKBOX: u16 = 100;
/// `WatchControl.control` for the events "resume following" note.
const FOLLOW: u16 = 10;

// ---- events ----------------------------------------------------------------------

/// Event filter chips; none selected means "All".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventChip {
    All,
    Started,
    Exited,
    Alerts,
    Actions,
}

impl EventChip {
    pub const ALL: [EventChip; 5] = [
        Self::All,
        Self::Started,
        Self::Exited,
        Self::Alerts,
        Self::Actions,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Started => "▶ Started",
            Self::Exited => "■ Exited",
            Self::Alerts => "⚠ Alerts",
            Self::Actions => "⚡ Actions",
        }
    }
    const fn kind(self) -> Option<EventKind> {
        match self {
            Self::All => None,
            Self::Started => Some(EventKind::Started),
            Self::Exited => Some(EventKind::Exited),
            Self::Alerts => Some(EventKind::Alert),
            Self::Actions => Some(EventKind::Action),
        }
    }
    fn index(self) -> usize {
        Self::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }
}

pub fn kind_icon(kind: EventKind) -> (&'static str, &'static str, Style) {
    match kind {
        EventKind::Started => ("▶", "started", Style::new().fg(Color::Green)),
        EventKind::Exited => ("■", "exited", theme::dim()),
        EventKind::Alert => ("⚠", "alert", theme::alert()),
        EventKind::Action => ("⚡", "action", Style::new().fg(Color::Yellow)),
        EventKind::Info => ("ℹ", "info", theme::accent()),
    }
}

/// Local wall-clock `HH:MM:SS` for an epoch-milliseconds timestamp.
pub fn clock(ms: u64) -> String {
    use chrono::TimeZone;
    i64::try_from(ms)
        .ok()
        .and_then(|ms| chrono::Local.timestamp_millis_opt(ms).single())
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".into())
}

#[derive(Debug, Default)]
pub struct EventsView {
    /// Enabled kind chips (Started, Exited, Alerts, Actions); none = All.
    pub chips: [bool; 4],
    /// Selected event by sequence number (stable while new events arrive).
    pub selected: Option<u64>,
    /// First visible row of the newest-first list.
    pub scroll: usize,
    /// The user moved away from the newest event; stop auto-following.
    pub detached: bool,
    /// `EventLog::pushed` when the screen was last drawn.
    pub seen: u64,
    /// Visible rows in the last frame (page size for PgUp/PgDn).
    pub page: usize,
}

impl EventsView {
    /// Events pushed since the Events screen was last viewed (toolbar badge).
    pub fn unseen(&self, events: &EventLog) -> u64 {
        events.pushed().saturating_sub(self.seen)
    }

    pub fn is_all(&self) -> bool {
        !self.chips.iter().any(|c| *c)
    }

    pub fn chip_on(&self, chip: EventChip) -> bool {
        match chip {
            EventChip::All => self.is_all(),
            other => self.chips[other.index() - 1],
        }
    }

    /// Click semantics: All clears the filter; a kind chip toggles.
    pub fn toggle_chip(&mut self, chip: EventChip) {
        match chip {
            EventChip::All => self.chips = [false; 4],
            other => {
                let i = other.index() - 1;
                self.chips[i] = !self.chips[i];
            }
        }
        self.reset_position();
    }

    /// Keyboard semantics: step through single-kind filters.
    pub fn cycle_chip(&mut self, delta: isize) {
        let current = if self.is_all() {
            0
        } else {
            self.chips.iter().position(|c| *c).map_or(0, |i| i + 1)
        };
        let next = (current as isize + delta).rem_euclid(EventChip::ALL.len() as isize) as usize;
        self.chips = [false; 4];
        if next > 0 {
            self.chips[next - 1] = true;
        }
        self.reset_position();
    }

    fn reset_position(&mut self) {
        self.scroll = 0;
        self.selected = None;
        self.detached = false;
    }

    pub fn matches(&self, event: &ProcEvent) -> bool {
        if self.is_all() {
            return true;
        }
        EventChip::ALL[1..]
            .iter()
            .zip(self.chips)
            .any(|(chip, on)| on && chip.kind() == Some(event.kind))
    }

    /// Matching events newest first, with their sequence numbers.
    pub fn visible<'a>(&self, events: &'a EventLog) -> Vec<(u64, &'a ProcEvent)> {
        let first = events.pushed().saturating_sub(events.len() as u64);
        events
            .iter()
            .enumerate()
            .rev()
            .map(|(i, e)| (first + i as u64, e))
            .filter(|(_, e)| self.matches(e))
            .collect()
    }

    fn selected_index(&self, list: &[(u64, &ProcEvent)]) -> Option<usize> {
        let seq = self.selected?;
        list.iter().position(|(s, _)| *s == seq)
    }

    /// Move the selection by `delta` rows (positive = older).
    fn move_selection(&mut self, list: &[(u64, &ProcEvent)], delta: isize) {
        if list.is_empty() {
            return;
        }
        let current = self.selected_index(list).unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, list.len() as isize - 1) as usize;
        self.select_index(list, next);
    }

    fn select_index(&mut self, list: &[(u64, &ProcEvent)], index: usize) {
        let Some((seq, _)) = list.get(index) else {
            return;
        };
        self.selected = Some(*seq);
        let page = self.page.max(1);
        if index < self.scroll {
            self.scroll = index;
        } else if index >= self.scroll + page {
            self.scroll = index + 1 - page;
        }
        self.detached = !(index == 0 && self.scroll == 0);
    }

    fn scroll_by(&mut self, len: usize, delta: i32) {
        let max = len.saturating_sub(self.page.max(1));
        self.scroll = (self.scroll as i64 + i64::from(delta)).clamp(0, max as i64) as usize;
        if self.scroll > 0 {
            self.detached = true;
        }
    }

    fn follow(&mut self) {
        self.detached = false;
        self.scroll = 0;
        self.selected = None;
    }
}

pub fn draw_events(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    frame.hit(area, Hit::ScreenBody);
    let pushed = state.events.pushed();
    let previous = state.events_view.seen;
    state.events_view.seen = pushed;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let view = &mut state.events_view;
    let list = view.visible(&state.events);
    // Keep the viewport anchored on the same events while detached.
    if view.detached && pushed > previous {
        let fresh = list.iter().take_while(|(s, _)| *s >= previous).count();
        view.scroll = view.scroll.saturating_add(fresh);
    }
    let body_h = area.height.saturating_sub(2) as usize;
    view.page = body_h.max(1);
    view.scroll = view.scroll.min(list.len().saturating_sub(body_h));
    if !view.detached {
        view.scroll = 0;
        view.selected = list.first().map(|(s, _)| *s);
    }
    draw_event_chips(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        state,
        &list,
    );
    if area.height < 2 {
        return;
    }
    let view = &state.events_view;
    let header = Rect::new(area.x, area.y + 1, area.width, 1);
    let cols = EventCols::new(area.width);
    frame.line_bg(header, &cols.header(), theme::header());
    let body = Rect::new(area.x, area.y + 2, area.width, body_h as u16);
    if list.is_empty() {
        let text = if state.events.is_empty() {
            "No events yet — process starts, exits, alerts and actions appear here"
        } else {
            "No events match this filter — click All to show everything"
        };
        let y = body.y + body.height / 2;
        let w = (display_width(text) as u16).min(body.width);
        let x = body.x + (body.width - w) / 2;
        frame.text(
            Rect::new(x, y, w, 1),
            &truncate(text, w as usize),
            theme::dim(),
        );
        return;
    }
    let scrollbar = list.len() > body_h && body.width > 10;
    let row_w = body.width.saturating_sub(u16::from(scrollbar));
    for (i, (seq, event)) in list.iter().skip(view.scroll).take(body_h).enumerate() {
        let r = Rect::new(body.x, body.y + i as u16, row_w, 1);
        let selected = view.selected == Some(*seq);
        let hovered = state
            .pointer
            .position
            .is_some_and(|p| super::hits::contains(r, p));
        let base = if selected {
            theme::selected()
        } else if hovered {
            theme::hovered()
        } else {
            theme::base()
        };
        frame.line_bg(r, &cols.row(event, selected, &state.store), base);
        frame.hit(
            r,
            Hit::EventRow {
                index: *seq as usize,
            },
        );
    }
    if scrollbar {
        draw_scrollbar(
            frame,
            Rect::new(body.right() - 1, body.y, 1, body.height),
            view.scroll,
            body_h,
            list.len(),
        );
    }
}

fn draw_event_chips(
    frame: &mut Frame<'_, '_, '_>,
    r: Rect,
    state: &ExplorerState,
    list: &[(u64, &ProcEvent)],
) {
    let view = &state.events_view;
    let mut counts = [0usize; 5];
    for event in state.events.iter() {
        counts[0] += 1;
        for chip in &EventChip::ALL[1..] {
            if chip.kind() == Some(event.kind) {
                counts[chip.index()] += 1;
            }
        }
    }
    let mut x = r.x;
    for chip in EventChip::ALL {
        let text = format!(" {} {} ", chip.label(), counts[chip.index()]);
        let w = (display_width(&text) as u16).min(r.right().saturating_sub(x));
        if w == 0 {
            break;
        }
        let style = if view.chip_on(chip) {
            theme::chip_on()
        } else {
            theme::chip_off()
        };
        let rect = Rect::new(x, r.y, w, 1);
        frame.text(rect, &truncate(&text, w as usize), style);
        frame.hit(
            rect,
            Hit::WatchControl {
                index: EVENTS_CTRL,
                control: chip.index() as u16,
            },
        );
        x = x.saturating_add(w + 1);
    }
    let (note, style) = if view.detached {
        let newer = list
            .iter()
            .take_while(|(s, _)| Some(*s) != view.selected)
            .count()
            .max(view.scroll);
        (
            format!(" ⏸ {newer} newer · Home to follow "),
            theme::button(),
        )
    } else {
        (" ● live ".to_string(), theme::ok())
    };
    let w = display_width(&note) as u16;
    if x.saturating_add(w) < r.right() {
        let rect = Rect::new(r.right() - w, r.y, w, 1);
        frame.text(rect, &note, style);
        if view.detached {
            frame.hit(
                rect,
                Hit::WatchControl {
                    index: EVENTS_CTRL,
                    control: FOLLOW,
                },
            );
        }
    }
}

struct EventCols {
    kind_w: usize,
    pid_w: usize,
    name_w: usize,
}

impl EventCols {
    fn new(width: u16) -> Self {
        let width = width as usize;
        Self {
            kind_w: if width >= 60 { 10 } else { 2 },
            pid_w: if width >= 40 { 7 } else { 0 },
            name_w: if width >= 80 {
                18
            } else if width >= 50 {
                12
            } else {
                0
            },
        }
    }

    fn header(&self) -> Line {
        let mut text = format!(" {} ", fit("TIME", 8, false));
        text.push_str(&fit(
            if self.kind_w > 2 { "KIND" } else { "" },
            self.kind_w,
            false,
        ));
        if self.pid_w > 0 {
            text.push_str(&fit("PID", self.pid_w, true));
            text.push(' ');
        }
        if self.name_w > 0 {
            text.push(' ');
            text.push_str(&fit("NAME", self.name_w, false));
        }
        text.push_str(" MESSAGE");
        Line::from_spans(vec![Span::styled(text, theme::header())])
    }

    fn row(
        &self,
        event: &ProcEvent,
        selected: bool,
        store: &crate::procs::store::ProcessStore,
    ) -> Line {
        let (icon, word, kind_style) = kind_icon(event.kind);
        let pick = |s: Style| if selected { Style::new() } else { s };
        let alive = event
            .key
            .is_some_and(|k| store.get(&k).is_some_and(|r| !r.is_ghost()));
        let kind = if self.kind_w > 2 {
            format!("{icon} {word}")
        } else {
            icon.to_string()
        };
        let mut spans = vec![
            Span::styled(format!(" {} ", clock(event.at_ms)), pick(theme::dim())),
            Span::styled(fit(&kind, self.kind_w, false), pick(kind_style)),
        ];
        if self.pid_w > 0 {
            let pid = event.key.map(|k| k.pid.to_string()).unwrap_or_default();
            spans.push(Span::styled(
                format!("{} ", fit(&pid, self.pid_w, true)),
                pick(if alive { theme::accent() } else { theme::dim() }),
            ));
        }
        if self.name_w > 0 {
            spans.push(Span::styled(
                format!(" {}", fit(&event.name, self.name_w, false)),
                pick(theme::bold()),
            ));
        }
        spans.push(Span::styled(format!(" {}", event.message), Style::new()));
        Line::from_spans(spans)
    }
}

fn draw_scrollbar(frame: &mut Frame<'_, '_, '_>, r: Rect, scroll: usize, page: usize, len: usize) {
    if r.height == 0 || len == 0 {
        return;
    }
    let h = r.height as usize;
    let thumb = (h * page / len).clamp(1, h);
    let max_scroll = len.saturating_sub(page).max(1);
    let top = (h - thumb) * scroll.min(max_scroll) / max_scroll;
    for i in 0..h {
        let (symbol, style) = if i >= top && i < top + thumb {
            ("┃", theme::accent())
        } else {
            ("│", theme::dim())
        };
        frame.cell(r.x, r.y + i as u16, symbol, style);
    }
}

fn handle_events_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    let view = &mut state.events_view;
    let list = view.visible(&state.events);
    let last = list.len().saturating_sub(1);
    let page = view.page.max(1) as isize;
    match key.key {
        KeyCode::Up => view.move_selection(&list, -1),
        KeyCode::Down => view.move_selection(&list, 1),
        KeyCode::PageUp => view.move_selection(&list, -page),
        KeyCode::PageDown => view.move_selection(&list, page),
        KeyCode::Home => view.follow(),
        KeyCode::End => view.select_index(&list, last),
        KeyCode::Tab => view.cycle_chip(if key.modifiers.shift { -1 } else { 1 }),
        KeyCode::Left => view.cycle_chip(-1),
        KeyCode::Right => view.cycle_chip(1),
        KeyCode::Enter => {
            let target = view
                .selected_index(&list)
                .and_then(|i| list.get(i))
                .map(|(_, e)| (e.key, e.name.clone()));
            if let Some((key, name)) = target {
                open_event_process(state, key, &name);
            }
        }
        _ => match typed_char(key) {
            Some('k') => view.move_selection(&list, -1),
            Some('j') => view.move_selection(&list, 1),
            _ => return Outcome::Ignored,
        },
    }
    Outcome::Consumed
}

fn open_event_process(state: &mut ExplorerState, key: Option<ProcessKey>, name: &str) {
    match key {
        Some(key) => {
            jump_to_process(state, key);
        }
        None => state.flash(format!("“{name}” is not tied to a process")),
    }
}

/// Switch to the Processes screen with `key` selected and its details open.
/// Returns false (and explains) when the process is gone.
pub fn jump_to_process(state: &mut ExplorerState, key: ProcessKey) -> bool {
    if !state.store.get(&key).is_some_and(|r| !r.is_ghost()) {
        let name = state.name_of(&key);
        state.flash(format!("{name} (PID {}) is no longer running", key.pid));
        return false;
    }
    if hidden_by_search(state, &key) {
        state.search.clear();
        state.table.mark_dirty();
    }
    state.screen = Screen::Processes;
    state.table.select(key);
    state.detail.open(key, false);
    state.request(UiRequest::LoadDetail(key));
    true
}

/// Whether the active search filter would hide the process `key`.
fn hidden_by_search(state: &ExplorerState, key: &ProcessKey) -> bool {
    let (Some(filter), Some(row)) = (state.search.filter(), state.store.get(key)) else {
        return false;
    };
    let ctx = state.table.column_ctx(&state.store, state.traffic);
    filter.matches(row, &ctx).is_none()
}

fn handle_events_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let wheel = click.wheel();
    if wheel != 0 {
        let len = state.events_view.visible(&state.events).len();
        state.events_view.scroll_by(len, wheel);
        return Outcome::Consumed;
    }
    match *click.hit {
        Hit::WatchControl {
            index: EVENTS_CTRL,
            control,
        } if click.is_left() => {
            if control == FOLLOW {
                state.events_view.follow();
            } else if let Some(chip) = EventChip::ALL.get(usize::from(control)) {
                state.events_view.toggle_chip(*chip);
            }
            Outcome::Consumed
        }
        Hit::EventRow { index } if click.is_left() || click.is_right() => {
            let seq = index as u64;
            let list = state.events_view.visible(&state.events);
            let Some(i) = list.iter().position(|(s, _)| *s == seq) else {
                return Outcome::Consumed;
            };
            let (key, name) = (list[i].1.key, list[i].1.name.clone());
            state.events_view.select_index(&list, i);
            if click.is_right() {
                if let Some(key) = key
                    && state.store.get(&key).is_some_and(|r| !r.is_ghost())
                {
                    let at = Point::new(click.mouse.position.x, click.mouse.position.y);
                    let menu = overlays::process_menu(state, key, at);
                    state.overlays.push(menu);
                }
            } else if click.double {
                open_event_process(state, key, &name);
            }
            Outcome::Consumed
        }
        _ => Outcome::Ignored,
    }
}

// ---- watches ---------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct WatchesView {
    /// Selected rule index in `state.watch.rules`.
    pub selected: usize,
    pub scroll: usize,
    /// Rule id armed for deletion by a first Delete press.
    pub confirm_delete: Option<u64>,
    /// Keyboard focus is on the "Currently alerting" list.
    pub focus_alerts: bool,
    pub alert_selected: usize,
    /// Alerting processes in display order (from the last frame).
    pub alert_keys: Vec<ProcessKey>,
    /// Visible rule rows in the last frame.
    pub page: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchButton {
    New,
    Edit,
    Delete,
    Toggle,
}

impl WatchButton {
    const ALL: [WatchButton; 4] = [Self::New, Self::Edit, Self::Delete, Self::Toggle];
}

/// Whether `target` covers the process `key` (with its store row, if any).
/// Delegates to [`WatchTarget::matches`] so the Watches screen agrees with
/// the alert evaluator; only a pinned process can match without a row.
pub fn target_matches(target: &WatchTarget, key: &ProcessKey, row: Option<&Row>) -> bool {
    match target {
        WatchTarget::Process { key: k, .. } => k == key,
        WatchTarget::Any => true,
        _ => row.is_some_and(|r| target.matches(&r.entry.record)),
    }
}

pub fn rule_alerting(state: &ExplorerState, rule: &WatchRule) -> bool {
    rule.enabled
        && state
            .alerting
            .iter()
            .any(|k| target_matches(&rule.target, k, state.store.get(k)))
}

/// Editor for a new rule; "This process" is offered for the selected process.
pub fn new_rule_editor(state: &ExplorerState) -> Overlay {
    let process = state.table.selected().map(|key| (key, state.name_of(&key)));
    Overlay::WatchEditor(EditorForm::blank(process).into())
}

/// Editor for an existing rule, or `None` when `id` is unknown.
pub fn edit_rule_editor(state: &ExplorerState, id: u64) -> Option<Overlay> {
    state
        .watch
        .rule(id)
        .map(|r| Overlay::WatchEditor(EditorForm::from_rule(r).into()))
}

pub fn draw_watches(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &mut ExplorerState) {
    frame.hit(area, Hit::ScreenBody);
    let mut alert_keys: Vec<ProcessKey> = state.alerting.iter().copied().collect();
    alert_keys.sort_by_key(|k| (k.pid, k.start_time));
    state.watches_view.alert_keys = alert_keys;
    let n_rules = state.watch.rules.len();
    let view = &mut state.watches_view;
    view.selected = view.selected.min(n_rules.saturating_sub(1));
    view.alert_selected = view
        .alert_selected
        .min(view.alert_keys.len().saturating_sub(1));
    if view.alert_keys.is_empty() {
        view.focus_alerts = false;
    }
    if area.width == 0 || area.height == 0 {
        return;
    }
    draw_watch_buttons(frame, Rect::new(area.x, area.y, area.width, 1), state);
    let avail = area.height.saturating_sub(1);
    let alerts_h = if avail >= 8 {
        (state.watches_view.alert_keys.len().max(1) as u16 + 2).clamp(3, avail / 2)
    } else {
        0
    };
    let rules = Rect::new(area.x, area.y + 1, area.width, avail - alerts_h);
    draw_rules(frame, rules, state);
    if alerts_h > 0 {
        draw_alerts(
            frame,
            Rect::new(area.x, rules.bottom(), area.width, alerts_h),
            state,
        );
    }
}

fn draw_watch_buttons(frame: &mut Frame<'_, '_, '_>, r: Rect, state: &ExplorerState) {
    let view = &state.watches_view;
    let rule = state.watch.rules.get(view.selected);
    let mut x = r.x;
    for button in WatchButton::ALL {
        let (label, style, enabled) = match button {
            WatchButton::New => (" + New rule ".to_string(), theme::button_primary(), true),
            WatchButton::Edit => (" Edit ".into(), theme::button(), rule.is_some()),
            WatchButton::Delete => {
                let armed = rule.is_some_and(|r| view.confirm_delete == Some(r.id));
                if armed {
                    (" Confirm delete ".into(), theme::button_danger(), true)
                } else {
                    (" Delete ".into(), theme::button(), rule.is_some())
                }
            }
            WatchButton::Toggle => (
                if rule.is_some_and(|r| r.enabled) {
                    " Disable ".into()
                } else {
                    " Enable ".into()
                },
                theme::button(),
                rule.is_some(),
            ),
        };
        let w = (display_width(&label) as u16).min(r.right().saturating_sub(x));
        if w == 0 {
            break;
        }
        let rect = Rect::new(x, r.y, w, 1);
        let style = if enabled { style } else { theme::chip_off() };
        frame.text(rect, &truncate(&label, w as usize), style);
        if enabled {
            frame.hit(
                rect,
                Hit::WatchControl {
                    index: WATCH_BUTTONS,
                    control: button as u16,
                },
            );
        }
        x = x.saturating_add(w + 1);
    }
    let hint = "n new · Enter edit · Space on/off · d delete · Tab alerts";
    let w = r.right().saturating_sub(x + 1);
    if w as usize >= display_width(hint) {
        let hw = display_width(hint) as u16;
        frame.text(Rect::new(r.right() - hw, r.y, hw, 1), hint, theme::dim());
    }
}

fn draw_rules(frame: &mut Frame<'_, '_, '_>, r: Rect, state: &mut ExplorerState) {
    if r.height == 0 {
        return;
    }
    let name_w: usize = if r.width >= 80 { 22 } else { 14 };
    let status_w: usize = 12;
    let header = format!(
        " {} {} {}",
        fit("ON", 4, false),
        fit("NAME", name_w, false),
        fit(
            "RULE",
            (r.width as usize).saturating_sub(name_w + status_w + 8),
            false
        ),
    );
    let header = format!("{}{}", header, fit("STATUS", status_w, false));
    frame.line_bg(
        Rect::new(r.x, r.y, r.width, 1),
        &Line::from_spans(vec![Span::styled(header, theme::header())]),
        theme::header(),
    );
    let body_h = r.height.saturating_sub(1) as usize;
    let rules_len = state.watch.rules.len();
    let view = &mut state.watches_view;
    view.page = body_h.max(1);
    if view.selected < view.scroll {
        view.scroll = view.selected;
    } else if view.selected >= view.scroll + body_h.max(1) {
        view.scroll = view.selected + 1 - body_h.max(1);
    }
    view.scroll = view.scroll.min(rules_len.saturating_sub(body_h));
    let body = Rect::new(r.x, r.y + 1, r.width, body_h as u16);
    if rules_len == 0 {
        let lines = [
            "No watch rules yet.",
            "Click [+ New rule] or press n to create one.",
            "Tip: select a process and press W to watch it.",
        ];
        let top = body.y + body.height.saturating_sub(lines.len() as u16) / 2;
        for (i, text) in lines.iter().enumerate() {
            let y = top + i as u16;
            if y >= body.bottom() {
                break;
            }
            let w = (display_width(text) as u16).min(body.width);
            let x = body.x + (body.width - w) / 2;
            let style = if i == 0 { theme::bold() } else { theme::dim() };
            frame.text(Rect::new(x, y, w, 1), &truncate(text, w as usize), style);
        }
        return;
    }
    let view = &state.watches_view;
    let focused = !view.focus_alerts;
    for (row, rule) in state
        .watch
        .rules
        .iter()
        .enumerate()
        .skip(view.scroll)
        .take(body_h)
    {
        let y = body.y + (row - view.scroll) as u16;
        let rect = Rect::new(body.x, y, body.width, 1);
        let selected = row == view.selected;
        let hovered = state
            .pointer
            .position
            .is_some_and(|p| super::hits::contains(rect, p));
        let base = match (selected, focused) {
            (true, true) => theme::selected(),
            (true, false) => theme::selected_unfocused(),
            _ if hovered => theme::hovered(),
            _ => theme::base(),
        };
        let alerting = rule_alerting(state, rule);
        let (status, status_style) = if !rule.enabled {
            ("off", theme::dim())
        } else if alerting {
            ("● alerting", theme::alert())
        } else {
            ("idle", theme::dim())
        };
        let keep = |s: Style| if selected && focused { Style::new() } else { s };
        let check = if rule.enabled { "[✓]" } else { "[ ]" };
        let rule_w = (body.width as usize).saturating_sub(name_w + status_w + 8);
        let line = Line::from_spans(vec![
            Span::styled(" ", Style::new()),
            Span::styled(
                check,
                keep(if rule.enabled {
                    theme::ok()
                } else {
                    theme::dim()
                }),
            ),
            Span::styled("  ", Style::new()),
            Span::styled(
                format!("{} ", fit(&rule.name, name_w, false)),
                keep(theme::bold()),
            ),
            Span::styled(
                format!("{} ", fit(&rule.describe(), rule_w, false)),
                Style::new(),
            ),
            Span::styled(fit(status, status_w, false), keep(status_style)),
        ]);
        frame.line_bg(rect, &line, base);
        frame.hit(rect, Hit::WatchRow { index: row });
        frame.hit(
            Rect::new(body.x + 1, y, 3.min(body.width.saturating_sub(1)), 1),
            Hit::WatchControl {
                index: row,
                control: CHECKBOX,
            },
        );
    }
}

fn draw_alerts(frame: &mut Frame<'_, '_, '_>, r: Rect, state: &ExplorerState) {
    let view = &state.watches_view;
    let n = view.alert_keys.len();
    let title = format!(" Currently alerting ({n}) ");
    let title_style = if n > 0 { theme::alert() } else { theme::bold() };
    let rule_y = r.y;
    for x in r.x..r.right() {
        frame.cell(x, rule_y, "─", theme::dim());
    }
    let tw = (display_width(&title) as u16).min(r.width.saturating_sub(2));
    frame.text(
        Rect::new(r.x + 1.min(r.width), rule_y, tw, 1),
        &truncate(&title, tw as usize),
        title_style,
    );
    let body = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(1));
    if n == 0 {
        frame.text(
            Rect::new(
                body.x + 2.min(body.width),
                body.y,
                body.width.saturating_sub(2),
                1,
            ),
            "Nothing is alerting right now.",
            theme::dim(),
        );
        return;
    }
    let scroll = (view.alert_selected + 1).saturating_sub(body.height as usize);
    for (i, key) in view
        .alert_keys
        .iter()
        .enumerate()
        .skip(scroll)
        .take(body.height as usize)
    {
        let y = body.y + (i - scroll) as u16;
        let rect = Rect::new(body.x, y, body.width, 1);
        let selected = view.focus_alerts && i == view.alert_selected;
        let hovered = state
            .pointer
            .position
            .is_some_and(|p| super::hits::contains(rect, p));
        let base = if selected {
            theme::selected()
        } else if hovered {
            theme::hovered()
        } else {
            theme::base()
        };
        let row = state.store.get(key);
        let name = state.name_of(key);
        let (cpu, mem) = row
            .map(|r| (r.entry.sample.cpu_percent, r.entry.sample.mem_percent))
            .unwrap_or_default();
        let rules: Vec<&str> = state
            .watch
            .rules
            .iter()
            .filter(|rule| rule.enabled && target_matches(&rule.target, key, row))
            .map(|rule| rule.name.as_str())
            .collect();
        let keep = |s: Style| if selected { Style::new() } else { s };
        let line = Line::from_spans(vec![
            Span::styled(" ⚠ ", keep(theme::alert())),
            Span::styled(format!("{:>7} ", key.pid), keep(theme::accent())),
            Span::styled(format!("{} ", fit(&name, 20, false)), keep(theme::bold())),
            Span::styled(
                format!("CPU {:>6}%  ", fmt::percent(cpu)),
                keep(Style::new().fg(theme::heat(f64::from(cpu)))),
            ),
            Span::styled(
                format!("MEM {:>5}%  ", fmt::percent(mem)),
                keep(Style::new().fg(theme::heat(f64::from(mem)))),
            ),
            Span::styled(rules.join(", "), keep(theme::dim())),
        ]);
        frame.line_bg(rect, &line, base);
        frame.hit(
            rect,
            Hit::WatchControl {
                index: ALERT_ROWS,
                control: u16::try_from(i).unwrap_or(u16::MAX),
            },
        );
    }
}

fn selected_rule_id(state: &ExplorerState) -> Option<u64> {
    state
        .watch
        .rules
        .get(state.watches_view.selected)
        .map(|r| r.id)
}

fn toggle_rule(state: &mut ExplorerState, index: usize) {
    let Some(rule) = state.watch.rules.get(index) else {
        return;
    };
    let (id, name) = (rule.id, rule.name.clone());
    // Through the engine so the rule's alert state restarts.
    let Some(enabled) = state.watch.toggle(id) else {
        return;
    };
    let message = format!("{} “{name}”", if enabled { "Enabled" } else { "Disabled" });
    state.request(UiRequest::SaveSettings);
    state.flash(message);
}

/// First press arms, second press deletes.
fn delete_selected(state: &mut ExplorerState) {
    let Some(id) = selected_rule_id(state) else {
        return;
    };
    if state.watches_view.confirm_delete != Some(id) {
        state.watches_view.confirm_delete = Some(id);
        let name = state.watch.rules[state.watches_view.selected].name.clone();
        state.flash(format!("Press d again to delete “{name}”"));
        return;
    }
    let index = state.watches_view.selected;
    let Some(rule) = state.watch.remove(id) else {
        return;
    };
    state.watches_view.confirm_delete = None;
    state.watches_view.selected = index.min(state.watch.rules.len().saturating_sub(1));
    state.request(UiRequest::SaveSettings);
    state.flash(format!("Deleted “{}”", rule.name));
}

fn edit_selected(state: &mut ExplorerState) {
    if let Some(editor) = selected_rule_id(state).and_then(|id| edit_rule_editor(state, id)) {
        state.overlays.push(editor);
    }
}

fn run_button(state: &mut ExplorerState, button: WatchButton) {
    if button != WatchButton::Delete {
        state.watches_view.confirm_delete = None;
    }
    match button {
        WatchButton::New => {
            let editor = new_rule_editor(state);
            state.overlays.push(editor);
        }
        WatchButton::Edit => edit_selected(state),
        WatchButton::Delete => delete_selected(state),
        WatchButton::Toggle => toggle_rule(state, state.watches_view.selected),
    }
}

fn handle_watches_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    let delete = is(key, KeyCode::Delete) || typed_char(key) == Some('d');
    if !delete {
        state.watches_view.confirm_delete = None;
    }
    if is(key, KeyCode::Tab) {
        let view = &mut state.watches_view;
        view.focus_alerts = !view.focus_alerts && !view.alert_keys.is_empty();
        return Outcome::Consumed;
    }
    if state.watches_view.focus_alerts {
        let view = &mut state.watches_view;
        let n = view.alert_keys.len();
        match key.key {
            KeyCode::Up => view.alert_selected = view.alert_selected.saturating_sub(1),
            KeyCode::Down => {
                view.alert_selected = (view.alert_selected + 1).min(n.saturating_sub(1))
            }
            KeyCode::Home => view.alert_selected = 0,
            KeyCode::End => view.alert_selected = n.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(key) = view.alert_keys.get(view.alert_selected).copied() {
                    jump_to_process(state, key);
                }
            }
            KeyCode::Escape => view.focus_alerts = false,
            _ => return Outcome::Ignored,
        }
        return Outcome::Consumed;
    }
    let n = state.watch.rules.len();
    let page = state.watches_view.page.max(1);
    let view = &mut state.watches_view;
    match key.key {
        KeyCode::Up => view.selected = view.selected.saturating_sub(1),
        KeyCode::Down => view.selected = (view.selected + 1).min(n.saturating_sub(1)),
        KeyCode::PageUp => view.selected = view.selected.saturating_sub(page),
        KeyCode::PageDown => view.selected = (view.selected + page).min(n.saturating_sub(1)),
        KeyCode::Home => view.selected = 0,
        KeyCode::End => view.selected = n.saturating_sub(1),
        KeyCode::Enter => edit_selected(state),
        KeyCode::Delete => delete_selected(state),
        _ if is_space(key) => toggle_rule(state, state.watches_view.selected),
        _ => match typed_char(key) {
            Some('n') => run_button(state, WatchButton::New),
            Some('d') => delete_selected(state),
            Some('k') => view.selected = view.selected.saturating_sub(1),
            Some('j') => view.selected = (view.selected + 1).min(n.saturating_sub(1)),
            _ => return Outcome::Ignored,
        },
    }
    Outcome::Consumed
}

fn handle_watches_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let wheel = click.wheel();
    if wheel != 0 {
        let n = state.watch.rules.len();
        let view = &mut state.watches_view;
        let max = n.saturating_sub(view.page.max(1));
        view.scroll = (view.scroll as i64 + i64::from(wheel)).clamp(0, max as i64) as usize;
        // Keep the selection inside the viewport so drawing doesn't snap back.
        let page = view.page.max(1);
        view.selected = view
            .selected
            .clamp(view.scroll, view.scroll + page - 1)
            .min(n.saturating_sub(1));
        return Outcome::Consumed;
    }
    if !click.is_left() && !click.is_right() {
        return Outcome::Ignored;
    }
    let armed = state.watches_view.confirm_delete;
    state.watches_view.confirm_delete = None;
    match *click.hit {
        Hit::WatchControl {
            index: WATCH_BUTTONS,
            control,
        } if click.is_left() => {
            if let Some(button) = WatchButton::ALL.get(usize::from(control)).copied() {
                if button == WatchButton::Delete {
                    state.watches_view.confirm_delete = armed;
                }
                run_button(state, button);
            }
        }
        Hit::WatchControl {
            index: ALERT_ROWS,
            control,
        } => {
            let i = usize::from(control);
            if let Some(key) = state.watches_view.alert_keys.get(i).copied() {
                state.watches_view.focus_alerts = true;
                state.watches_view.alert_selected = i;
                if click.is_left() {
                    jump_to_process(state, key);
                } else {
                    let at = click.mouse.position;
                    let menu = overlays::process_menu(state, key, at);
                    state.overlays.push(menu);
                }
            }
        }
        Hit::WatchControl {
            index,
            control: CHECKBOX,
        } if click.is_left() && index < state.watch.rules.len() => {
            state.watches_view.selected = index;
            state.watches_view.focus_alerts = false;
            toggle_rule(state, index);
        }
        Hit::WatchRow { index } if index < state.watch.rules.len() => {
            state.watches_view.selected = index;
            state.watches_view.focus_alerts = false;
            if click.is_left() && click.double {
                edit_selected(state);
            }
        }
        _ => return Outcome::Ignored,
    }
    Outcome::Consumed
}

// ---- dispatch --------------------------------------------------------------------

pub fn handle_key(state: &mut ExplorerState, key: &KeyStroke) -> Outcome {
    match state.screen {
        Screen::Events => handle_events_key(state, key),
        Screen::Watches => handle_watches_key(state, key),
        Screen::Processes => Outcome::Ignored,
    }
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    match state.screen {
        Screen::Events => handle_events_mouse(state, click),
        Screen::Watches => handle_watches_mouse(state, click),
        Screen::Processes => Outcome::Ignored,
    }
}

/// Editor pre-filled to watch `key`.
pub fn watch_editor_for(state: &ExplorerState, key: ProcessKey) -> Overlay {
    Overlay::WatchEditor(EditorForm::for_process(key, &state.name_of(&key)).into())
}

pub fn draw_editor(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    index: usize,
    editor: &WatchEditor,
    state: &ExplorerState,
) {
    editor::draw(frame, area, index, editor, state);
}

pub fn handle_editor(
    editor: &mut WatchEditor,
    index: usize,
    event: &Event,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    editor::handle(editor, index, event, state)
}

#[cfg(test)]
pub mod test_support;
#[cfg(test)]
mod tests;
