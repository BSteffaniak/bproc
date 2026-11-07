//! Watch-rule editor overlay: a small form with text inputs, cycling
//! selectors, inline validation and Cancel / Save buttons.
use super::super::hits::Hit;
use super::super::overlays::OverlayOutcome;
use super::super::paint::{Frame, centered, fit, theme, truncate};
use super::super::{ExplorerState, UiRequest};
use super::input::{TextInput, is, is_space};
use crate::procs::ProcessKey;
use crate::procs::actions::Signal;
use crate::procs::watch::{Comparison, WatchAction, WatchMetric, WatchRule, WatchTarget};
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui::geometry::Rect;
use bmux_tui::style::{Color, Style};
use bmux_tui::text::{Line, Span};
use bmux_tui::text_width::display_width;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Target,
    Value,
    Metric,
    Comparison,
    Threshold,
    Sustain,
    Cooldown,
    Action,
    Enabled,
    Cancel,
    Save,
}

impl Field {
    pub const ALL: [Field; 12] = [
        Field::Name,
        Field::Target,
        Field::Value,
        Field::Metric,
        Field::Comparison,
        Field::Threshold,
        Field::Sustain,
        Field::Cooldown,
        Field::Action,
        Field::Enabled,
        Field::Cancel,
        Field::Save,
    ];
    /// Fields painted as form rows (buttons are painted separately).
    const ROWS: usize = 10;

    fn index(self) -> usize {
        Self::ALL.iter().position(|f| *f == self).unwrap_or(0)
    }
    fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }
    const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Target => "Watch",
            Self::Value => "Match",
            Self::Metric => "Metric",
            Self::Comparison => "When",
            Self::Threshold => "Threshold",
            Self::Sustain => "Sustain",
            Self::Cooldown => "Cooldown",
            Self::Action => "Action",
            Self::Enabled => "Enabled",
            Self::Cancel => "Cancel",
            Self::Save => "Save",
        }
    }
    const fn is_text(self) -> bool {
        matches!(
            self,
            Self::Name | Self::Value | Self::Threshold | Self::Sustain | Self::Cooldown
        )
    }
    const fn is_selector(self) -> bool {
        matches!(
            self,
            Self::Target | Self::Metric | Self::Comparison | Self::Action
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Process,
    Name,
    Command,
    User,
    Any,
}

impl TargetKind {
    pub const ALL: [TargetKind; 5] = [
        Self::Process,
        Self::Name,
        Self::Command,
        Self::User,
        Self::Any,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Process => "This process",
            Self::Name => "Name pattern",
            Self::Command => "Command contains",
            Self::User => "User",
            Self::Any => "Any process",
        }
    }
    const fn placeholder(self) -> &'static str {
        match self {
            Self::Name => "e.g. node or *chrome*",
            Self::Command => "e.g. --inspect",
            Self::User => "e.g. root",
            Self::Process | Self::Any => "",
        }
    }
    const fn has_value(self) -> bool {
        matches!(self, Self::Name | Self::Command | Self::User)
    }
}

/// Actions offered by the selector (others survive editing unchanged).
pub const ACTIONS: [WatchAction; 5] = [
    WatchAction::Notify,
    WatchAction::Log,
    WatchAction::Signal(Signal::Term),
    WatchAction::Signal(Signal::Kill),
    WatchAction::Signal(Signal::Stop),
];

pub fn action_label(action: WatchAction) -> String {
    match action {
        WatchAction::Notify => "Notify".into(),
        WatchAction::Log => "Log only".into(),
        WatchAction::Signal(s) => format!("Signal {}", s.name().trim_start_matches("SIG")),
    }
}

/// How a metric's threshold is entered and displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Percent,
    Bytes,
    Rate,
    Count,
    Event,
}

pub const fn unit(metric: WatchMetric) -> Unit {
    match metric {
        WatchMetric::CpuPercent | WatchMetric::MemPercent => Unit::Percent,
        WatchMetric::MemRss => Unit::Bytes,
        WatchMetric::DiskRead
        | WatchMetric::DiskWrite
        | WatchMetric::NetDown
        | WatchMetric::NetUp => Unit::Rate,
        WatchMetric::Threads | WatchMetric::Fds => Unit::Count,
        WatchMetric::Exits | WatchMetric::Starts => Unit::Event,
    }
}

const fn unit_hint(metric: WatchMetric) -> &'static str {
    match metric {
        WatchMetric::CpuPercent => "% of one core (80 or 80%)",
        WatchMetric::MemPercent => "% of RAM (0–100)",
        WatchMetric::MemRss => "bytes, e.g. 500M or 2G",
        WatchMetric::DiskRead
        | WatchMetric::DiskWrite
        | WatchMetric::NetDown
        | WatchMetric::NetUp => "bytes/s, e.g. 10M",
        WatchMetric::Threads => "threads",
        WatchMetric::Fds => "open files",
        WatchMetric::Exits | WatchMetric::Starts => "",
    }
}

/// Short metric name for generated rule names and summaries.
pub const fn metric_short(metric: WatchMetric) -> &'static str {
    match metric {
        WatchMetric::CpuPercent => "CPU",
        WatchMetric::MemPercent => "memory %",
        WatchMetric::MemRss => "RSS",
        WatchMetric::DiskRead => "disk read",
        WatchMetric::DiskWrite => "disk write",
        WatchMetric::NetDown => "net down",
        WatchMetric::NetUp => "net up",
        WatchMetric::Threads => "threads",
        WatchMetric::Fds => "open files",
        WatchMetric::Exits => "exit",
        WatchMetric::Starts => "start",
    }
}

// ---- parsing ---------------------------------------------------------------------

/// Parse a threshold for `metric`: `80`/`80%` for percentages, `500M`/`1.5G`
/// (binary units, optional `B` and `/s`) for byte metrics, plain counts.
pub fn parse_threshold(metric: WatchMetric, text: &str) -> Result<f64, String> {
    let s = text.trim();
    let kind = unit(metric);
    if kind == Unit::Event {
        return Ok(0.0);
    }
    if s.is_empty() {
        return Err("Enter a threshold".into());
    }
    let value = match kind {
        Unit::Percent => {
            let v = parse_number(s.trim_end_matches('%').trim())
                .ok_or_else(|| format!("“{s}” is not a percentage (try 80 or 80%)"))?;
            if metric == WatchMetric::MemPercent && v > 100.0 {
                return Err("Memory % must be between 0 and 100".into());
            }
            v
        }
        Unit::Bytes | Unit::Rate => {
            parse_bytes(s).ok_or_else(|| format!("“{s}” is not a size (try 500M or 1G)"))?
        }
        Unit::Count => parse_number(s)
            .map(f64::round)
            .ok_or_else(|| format!("“{s}” is not a whole number"))?,
        Unit::Event => 0.0,
    };
    Ok(value)
}

fn parse_number(s: &str) -> Option<f64> {
    let v: f64 = s.parse().ok()?;
    (v.is_finite() && v >= 0.0).then_some(v)
}

/// `1024`, `500K`, `500M`, `1.5G`, `2T`, `10MB`, `10MiB`, `10M/s` (binary units).
pub fn parse_bytes(text: &str) -> Option<f64> {
    let mut s = text.trim().to_ascii_lowercase();
    if let Some(stripped) = s.strip_suffix("/s") {
        s = stripped.trim_end().to_string();
    }
    if let Some(stripped) = s.strip_suffix("ib") {
        s = stripped.to_string();
    } else if s.len() > 1
        && let Some(stripped) = s.strip_suffix('b')
    {
        s = stripped.to_string();
    }
    let s = s.trim_end();
    let (number, mult) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1u64 << 10),
        'm' => (&s[..s.len() - 1], 1 << 20),
        'g' => (&s[..s.len() - 1], 1 << 30),
        't' => (&s[..s.len() - 1], 1 << 40),
        _ => (s, 1),
    };
    parse_number(number.trim()).map(|v| (v * mult as f64).round())
}

/// `30`, `30s`, `5m`, `1h`, `1m30s`, `2 min` → seconds.
pub fn parse_duration_secs(text: &str) -> Result<u64, String> {
    let s = text.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err("Enter a duration (e.g. 30s or 5m)".into());
    }
    let bad = || format!("“{}” is not a duration (try 30s or 5m)", text.trim());
    if let Ok(v) = s.parse::<u64>() {
        return Ok(v);
    }
    let mut total: u64 = 0;
    let mut chars = s.chars().peekable();
    let mut any = false;
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut number = String::new();
        while let Some(c) = chars
            .peek()
            .copied()
            .filter(|c| c.is_ascii_digit() || *c == '.')
        {
            number.push(c);
            chars.next();
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut word = String::new();
        while let Some(c) = chars.peek().copied().filter(|c| c.is_ascii_alphabetic()) {
            word.push(c);
            chars.next();
        }
        if number.is_empty() {
            if word.is_empty() && chars.peek().is_none() {
                break;
            }
            return Err(bad());
        }
        let value = parse_number(&number).ok_or_else(bad)?;
        let mult = match word.as_str() {
            "" | "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86_400.0,
            _ => return Err(bad()),
        };
        total = total.saturating_add((value * mult).round() as u64);
        any = true;
    }
    if any { Ok(total) } else { Err(bad()) }
}

pub fn format_duration(secs: u64) -> String {
    if secs == 0 {
        "0s".into()
    } else if secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else if secs > 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn format_number(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Threshold text for an input box, inverse of [`parse_threshold`].
pub fn format_threshold(metric: WatchMetric, value: f64) -> String {
    match unit(metric) {
        Unit::Bytes | Unit::Rate => {
            for (suffix, mult) in [
                ("T", 1u64 << 40),
                ("G", 1 << 30),
                ("M", 1 << 20),
                ("K", 1 << 10),
            ] {
                let scaled = value / mult as f64;
                if scaled >= 1.0 && (scaled * 10.0).fract() == 0.0 {
                    return format!("{}{suffix}", format_number(scaled));
                }
            }
            format_number(value)
        }
        Unit::Event => String::new(),
        Unit::Percent | Unit::Count => format_number(value),
    }
}

/// Threshold with its unit for summaries (`80%`, `500M`, `10M/s`).
fn threshold_display(metric: WatchMetric, value: f64) -> String {
    match unit(metric) {
        Unit::Percent => format!("{}%", format_number(value)),
        Unit::Rate => format!("{}/s", format_threshold(metric, value)),
        _ => format_threshold(metric, value),
    }
}

// ---- editor ----------------------------------------------------------------------

/// The overlay payload; boxed so the overlay enum stays small.
#[derive(Debug)]
pub struct WatchEditor(pub Box<EditorForm>);

impl From<EditorForm> for WatchEditor {
    fn from(form: EditorForm) -> Self {
        Self(Box::new(form))
    }
}

impl std::ops::Deref for WatchEditor {
    type Target = EditorForm;
    fn deref(&self) -> &EditorForm {
        &self.0
    }
}

impl std::ops::DerefMut for WatchEditor {
    fn deref_mut(&mut self) -> &mut EditorForm {
        &mut self.0
    }
}

#[derive(Debug)]
pub struct EditorForm {
    /// Rule being edited; `None` creates a new rule.
    pub id: Option<u64>,
    pub focus: Field,
    pub name: TextInput,
    pub target: TargetKind,
    pub value: TextInput,
    /// The process offered by "This process".
    pub process: Option<(ProcessKey, String)>,
    pub metric: WatchMetric,
    pub comparison: Comparison,
    pub threshold: TextInput,
    pub sustain: TextInput,
    pub cooldown: TextInput,
    pub action: WatchAction,
    pub enabled: bool,
    pub error: Option<(Field, String)>,
    /// Form row scroll for short terminals.
    pub scroll: usize,
}

impl EditorForm {
    /// Blank editor with sensible defaults (CPU above 80% for 10s, notify).
    pub fn blank(process: Option<(ProcessKey, String)>) -> Self {
        Self {
            id: None,
            focus: Field::Name,
            name: TextInput::default(),
            target: TargetKind::Name,
            value: TextInput::default(),
            process,
            metric: WatchMetric::CpuPercent,
            comparison: Comparison::Above,
            threshold: TextInput::new("80"),
            sustain: TextInput::new("10s"),
            cooldown: TextInput::new("1m"),
            action: WatchAction::Notify,
            enabled: true,
            error: None,
            scroll: 0,
        }
    }

    pub fn for_process(key: ProcessKey, name: &str) -> Self {
        let mut editor = Self::blank(Some((key, name.to_string())));
        editor.name = TextInput::new(format!("{name} CPU"));
        editor.target = TargetKind::Process;
        editor
    }

    pub fn from_rule(rule: &WatchRule) -> Self {
        let mut editor = Self::blank(None);
        editor.id = Some(rule.id);
        editor.name = TextInput::new(rule.name.clone());
        match &rule.target {
            WatchTarget::Process { key, name } => {
                editor.target = TargetKind::Process;
                editor.process = Some((*key, name.clone()));
            }
            WatchTarget::Name(v) => {
                editor.target = TargetKind::Name;
                editor.value = TextInput::new(v.clone());
            }
            WatchTarget::Command(v) => {
                editor.target = TargetKind::Command;
                editor.value = TextInput::new(v.clone());
            }
            WatchTarget::User(v) => {
                editor.target = TargetKind::User;
                editor.value = TextInput::new(v.clone());
            }
            WatchTarget::Any => editor.target = TargetKind::Any,
        }
        editor.metric = rule.metric;
        editor.comparison = rule.comparison;
        if !rule.metric.is_event() {
            editor.threshold = TextInput::new(format_threshold(rule.metric, rule.threshold));
        }
        editor.sustain = TextInput::new(format_duration(rule.sustain_secs));
        editor.cooldown = TextInput::new(format_duration(rule.cooldown_secs));
        editor.action = rule.action;
        editor.enabled = rule.enabled;
        editor
    }

    /// Whether `field` currently takes input (some depend on target/metric).
    pub fn is_active(&self, field: Field) -> bool {
        match field {
            Field::Value => self.target.has_value(),
            Field::Comparison | Field::Threshold | Field::Sustain => !self.metric.is_event(),
            _ => true,
        }
    }

    fn input_mut(&mut self, field: Field) -> Option<&mut TextInput> {
        match field {
            Field::Name => Some(&mut self.name),
            Field::Value => Some(&mut self.value),
            Field::Threshold => Some(&mut self.threshold),
            Field::Sustain => Some(&mut self.sustain),
            Field::Cooldown => Some(&mut self.cooldown),
            _ => None,
        }
    }

    fn input(&self, field: Field) -> Option<&TextInput> {
        match field {
            Field::Name => Some(&self.name),
            Field::Value => Some(&self.value),
            Field::Threshold => Some(&self.threshold),
            Field::Sustain => Some(&self.sustain),
            Field::Cooldown => Some(&self.cooldown),
            _ => None,
        }
    }

    pub fn move_focus(&mut self, delta: isize) {
        let n = Field::ALL.len() as isize;
        let mut i = self.focus.index() as isize;
        for _ in 0..n {
            i = (i + delta).rem_euclid(n);
            let field = Field::ALL[i as usize];
            if self.is_active(field) {
                self.focus = field;
                return;
            }
        }
    }

    /// Cycle a selector field; toggles `Enabled`.
    pub fn cycle(&mut self, field: Field, delta: isize) {
        fn step<T: Copy + PartialEq>(all: &[T], current: T, delta: isize) -> T {
            let n = all.len() as isize;
            let i = all
                .iter()
                .position(|v| *v == current)
                .map_or(if delta >= 0 { -1 } else { 0 }, |i| i as isize);
            all[(i + delta).rem_euclid(n) as usize]
        }
        match field {
            Field::Target => {
                let kinds: Vec<TargetKind> = TargetKind::ALL
                    .into_iter()
                    .filter(|k| *k != TargetKind::Process || self.process.is_some())
                    .collect();
                let old = self.target;
                self.target = step(&kinds, self.target, delta);
                self.rename_if_generated(old, self.metric);
            }
            Field::Metric => {
                let old = self.metric;
                let next = step(&WatchMetric::ALL, self.metric, delta);
                if unit(next) != unit(old) && !next.is_event() {
                    self.threshold = TextInput::new(default_threshold(next));
                }
                self.metric = next;
                self.rename_if_generated(self.target, old);
            }
            Field::Comparison => {
                self.comparison = match self.comparison {
                    Comparison::Above => Comparison::Below,
                    Comparison::Below => Comparison::Above,
                }
            }
            Field::Action => self.action = step(&ACTIONS, self.action, delta),
            Field::Enabled => self.enabled = !self.enabled,
            _ => {}
        }
        if self.error.as_ref().is_some_and(|(f, _)| *f == field) {
            self.error = None;
        }
    }

    /// Keep an auto-generated name in sync when the target or metric changes.
    fn rename_if_generated(&mut self, old_target: TargetKind, old_metric: WatchMetric) {
        let old = self.generated_name(old_target, old_metric);
        if self.name.text.trim().is_empty() || self.name.text == old {
            let new = self.generated_name(self.target, self.metric);
            if !self.name.text.trim().is_empty() {
                self.name = TextInput::new(new);
            }
        }
    }

    fn generated_name(&self, target: TargetKind, metric: WatchMetric) -> String {
        let who = match target {
            TargetKind::Process => self
                .process
                .as_ref()
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
            TargetKind::Any => "Any process".into(),
            _ => {
                let v = self.value.text.trim();
                if v.is_empty() {
                    target.label().to_string()
                } else {
                    v.to_string()
                }
            }
        };
        let what = match metric {
            WatchMetric::CpuPercent => "CPU".to_string(),
            WatchMetric::MemPercent | WatchMetric::MemRss => "memory".into(),
            other => metric_short(other).to_string(),
        };
        format!("{who} {what}")
    }

    /// Validate the form into a rule with `id`.
    pub fn build(&self, id: u64) -> Result<WatchRule, (Field, String)> {
        let target = match self.target {
            TargetKind::Process => match &self.process {
                Some((key, name)) => WatchTarget::Process {
                    key: *key,
                    name: name.clone(),
                },
                None => return Err((Field::Target, "No process selected".into())),
            },
            kind => {
                let v = self.value.text.trim().to_string();
                if kind.has_value() && v.is_empty() {
                    let what = match kind {
                        TargetKind::Name => "Enter a process name or pattern",
                        TargetKind::Command => "Enter text to look for in the command line",
                        _ => "Enter a user name",
                    };
                    return Err((Field::Value, what.into()));
                }
                match kind {
                    TargetKind::Name => WatchTarget::Name(v),
                    TargetKind::Command => WatchTarget::Command(v),
                    TargetKind::User => WatchTarget::User(v),
                    _ => WatchTarget::Any,
                }
            }
        };
        let event = self.metric.is_event();
        let threshold = parse_threshold(self.metric, &self.threshold.text)
            .map_err(|e| (Field::Threshold, e))?;
        let sustain_secs = if event {
            0
        } else {
            parse_duration_secs(&self.sustain.text).map_err(|e| (Field::Sustain, e))?
        };
        let cooldown_secs =
            parse_duration_secs(&self.cooldown.text).map_err(|e| (Field::Cooldown, e))?;
        let name = match self.name.text.trim() {
            "" => self.generated_name(self.target, self.metric),
            n => n.to_string(),
        };
        Ok(WatchRule {
            id,
            name,
            enabled: self.enabled,
            target,
            metric: self.metric,
            comparison: self.comparison,
            threshold,
            sustain_secs,
            cooldown_secs,
            action: self.action,
        })
    }

    /// Validate and store the rule. Returns true when saved.
    pub fn save(&mut self, state: &mut ExplorerState) -> bool {
        let id = self.id.unwrap_or_else(|| state.watch.next_id());
        match self.build(id) {
            Ok(rule) => {
                let name = rule.name.clone();
                let rules = &mut state.watch.rules;
                let index = match rules.iter().position(|r| r.id == id) {
                    Some(i) => {
                        rules[i] = rule;
                        i
                    }
                    None => {
                        rules.push(rule);
                        rules.len() - 1
                    }
                };
                state.watches_view.selected = index;
                state.request(UiRequest::SaveSettings);
                state.flash(if self.id.is_some() {
                    format!("Updated watch “{name}”")
                } else {
                    format!("Watching: {name}")
                });
                true
            }
            Err((field, message)) => {
                self.focus = field;
                self.error = Some((field, message));
                false
            }
        }
    }

    /// One-line plain-language summary of the rule as currently entered.
    pub fn summary(&self) -> String {
        let who = match self.target {
            TargetKind::Process => self
                .process
                .as_ref()
                .map(|(k, n)| format!("{n} (PID {})", k.pid))
                .unwrap_or_else(|| "this process".into()),
            TargetKind::Name => format!("processes named “{}”", self.value.text.trim()),
            TargetKind::Command => format!("commands containing “{}”", self.value.text.trim()),
            TargetKind::User => format!("processes of user “{}”", self.value.text.trim()),
            TargetKind::Any => "any process".into(),
        };
        let action = match self.action {
            WatchAction::Notify => "notify".to_string(),
            WatchAction::Log => "log".to_string(),
            WatchAction::Signal(s) => format!("send {}", s.name()),
        };
        if self.metric.is_event() {
            return format!("When {who} {}s → {action}", metric_short(self.metric));
        }
        let threshold = parse_threshold(self.metric, &self.threshold.text)
            .map(|v| threshold_display(self.metric, v))
            .unwrap_or_else(|_| "?".into());
        let op = match self.comparison {
            Comparison::Above => ">",
            Comparison::Below => "<",
        };
        let sustain = parse_duration_secs(&self.sustain.text)
            .map(|s| {
                if s == 0 {
                    String::new()
                } else {
                    format!(" for {}", format_duration(s))
                }
            })
            .unwrap_or_default();
        format!(
            "When {who} {} {op} {threshold}{sustain} → {action}",
            metric_short(self.metric)
        )
    }

    fn selector_text(&self, field: Field) -> String {
        match field {
            Field::Target => self.target.label().to_string(),
            Field::Metric => self.metric.label().to_string(),
            Field::Comparison => match self.comparison {
                Comparison::Above => "Above".into(),
                Comparison::Below => "Below".into(),
            },
            Field::Action => action_label(self.action),
            _ => String::new(),
        }
    }
}

fn default_threshold(metric: WatchMetric) -> String {
    match metric {
        WatchMetric::CpuPercent => "80".into(),
        WatchMetric::MemPercent => "50".into(),
        WatchMetric::MemRss => "1G".into(),
        WatchMetric::DiskRead | WatchMetric::DiskWrite => "50M".into(),
        WatchMetric::NetDown | WatchMetric::NetUp => "10M".into(),
        WatchMetric::Threads => "500".into(),
        WatchMetric::Fds => "1000".into(),
        WatchMetric::Exits | WatchMetric::Starts => String::new(),
    }
}

// ---- controls & layout ---------------------------------------------------------

/// `OverlayControl` encoding: field index × 4 + part.
const PART_BODY: u16 = 0;
const PART_PREV: u16 = 1;
const PART_NEXT: u16 = 2;
/// The editable box of a text field (click positions the cursor).
const PART_INPUT: u16 = 3;

pub fn control(field: Field, part: u16) -> u16 {
    field.index() as u16 * 4 + part
}

fn decode(control: u16) -> Option<(Field, u16)> {
    Field::from_index(usize::from(control / 4)).map(|f| (f, control % 4))
}

const LABEL_W: u16 = 11;

/// Dialog rectangle within `area`.
pub fn dialog_rect(area: Rect) -> Rect {
    centered(area, 72, Field::ROWS as u16 + 5)
}

fn input_style() -> Style {
    Style::new()
        .fg(Color::BrightWhite)
        .bg(Color::Rgb(48, 52, 62))
}

pub fn draw(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    index: usize,
    editor: &WatchEditor,
    _state: &ExplorerState,
) {
    frame.hit(area, Hit::OverlayBackdrop { overlay: index });
    let rect = dialog_rect(area);
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let title = if editor.id.is_some() {
        "Edit watch rule"
    } else {
        "New watch rule"
    };
    let bg = theme::panel_bg();
    let inner = frame.panel(rect, Some(title), theme::panel_border(), bg);
    frame.hit(rect, Hit::OverlayBody { overlay: index });
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // Bottom rows: message, buttons. Form rows fill the rest.
    let buttons_y = inner.bottom() - 1;
    let message_y = (inner.height >= 2).then(|| buttons_y - 1);
    let form_h = inner.height.saturating_sub(2) as usize;
    let focus_row = editor.focus.index().min(Field::ROWS - 1);
    let scroll = if form_h == 0 {
        0
    } else {
        editor
            .scroll
            .min(focus_row)
            .max((focus_row + 1).saturating_sub(form_h))
            .min(Field::ROWS.saturating_sub(form_h))
    };
    for (row, field) in Field::ALL[..Field::ROWS]
        .iter()
        .enumerate()
        .skip(scroll)
        .take(form_h)
    {
        let y = inner.y + (row - scroll) as u16;
        draw_row(
            frame,
            Rect::new(inner.x, y, inner.width, 1),
            index,
            editor,
            *field,
        );
    }
    if let Some(y) = message_y {
        let r = Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1);
        let (text, style) = match &editor.error {
            Some((_, e)) => (format!("⚠ {e}"), bg.patch(theme::error())),
            None => (editor.summary(), bg.patch(theme::dim())),
        };
        frame.text(r, &truncate(&text, r.width as usize), style);
    }
    draw_buttons(
        frame,
        Rect::new(inner.x, buttons_y, inner.width, 1),
        index,
        editor,
    );
}

fn draw_row(
    frame: &mut Frame<'_, '_, '_>,
    r: Rect,
    index: usize,
    editor: &WatchEditor,
    field: Field,
) {
    let bg = theme::panel_bg();
    let focused = editor.focus == field;
    let active = editor.is_active(field);
    let errored = editor.error.as_ref().is_some_and(|(f, _)| *f == field);
    let label_style = if errored {
        bg.patch(theme::error())
    } else if focused {
        bg.patch(theme::accent()).patch(theme::bold())
    } else if active {
        bg
    } else {
        bg.patch(theme::dim())
    };
    let marker = if focused { "▸" } else { " " };
    let label = match field {
        Field::Value => match editor.target {
            TargetKind::Name => "Pattern",
            TargetKind::Command => "Contains",
            TargetKind::User => "User",
            _ => field.label(),
        },
        _ => field.label(),
    };
    frame.line_bg(
        r,
        &Line::from_spans(vec![
            Span::styled(marker, label_style),
            Span::styled(fit(label, LABEL_W as usize, false), label_style),
        ]),
        bg,
    );
    let label_w = (LABEL_W + 1).min(r.width);
    frame.hit(
        Rect::new(r.x, r.y, label_w, 1),
        Hit::OverlayControl {
            overlay: index,
            control: control(field, PART_BODY),
        },
    );
    let x = r.x + label_w;
    let avail = r.right().saturating_sub(x).saturating_sub(1);
    if avail == 0 {
        return;
    }
    if !active {
        let note = match field {
            Field::Value if editor.target == TargetKind::Process => editor
                .process
                .as_ref()
                .map(|(k, n)| format!("{n} · PID {}", k.pid))
                .unwrap_or_default(),
            Field::Value => "every process".into(),
            _ => "not used for start/exit events".into(),
        };
        frame.text(
            Rect::new(x, r.y, avail, 1),
            &truncate(&note, avail as usize),
            bg.patch(theme::dim()),
        );
        return;
    }
    let body = |frame: &mut Frame<'_, '_, '_>, rect: Rect| {
        frame.hit(
            rect,
            Hit::OverlayControl {
                overlay: index,
                control: control(field, PART_BODY),
            },
        );
    };
    if let Some(input) = editor.input(field) {
        let hint = match field {
            Field::Threshold => unit_hint(editor.metric),
            Field::Sustain => "condition must hold this long",
            Field::Cooldown => "quiet time between alerts",
            Field::Name => "blank = auto",
            _ => "",
        };
        let w = if hint.is_empty() || avail < 30 {
            avail
        } else {
            avail.min(22)
        };
        let placeholder = match field {
            Field::Value => editor.target.placeholder(),
            Field::Name => "auto",
            _ => "",
        };
        let rect = Rect::new(x, r.y, w, 1);
        let style = if errored {
            input_style().patch(Style::new().fg(Color::BrightRed))
        } else {
            input_style()
        };
        input.draw(frame, rect, style, focused, placeholder);
        frame.hit(
            rect,
            Hit::OverlayControl {
                overlay: index,
                control: control(field, PART_INPUT),
            },
        );
        let hint_w = avail.saturating_sub(w + 1);
        if !hint.is_empty() && hint_w > 3 {
            frame.text(
                Rect::new(x + w + 1, r.y, hint_w, 1),
                &truncate(hint, hint_w as usize),
                bg.patch(theme::dim()),
            );
        }
        return;
    }
    if field == Field::Enabled {
        let text = if editor.enabled {
            "[✓] Enabled"
        } else {
            "[ ] Disabled"
        };
        let w = (display_width(text) as u16).min(avail);
        let style = if focused {
            theme::selected()
        } else if editor.enabled {
            bg.patch(theme::ok())
        } else {
            bg.patch(theme::dim())
        };
        frame.text(Rect::new(x, r.y, w, 1), text, style);
        body(frame, Rect::new(x, r.y, w, 1));
        return;
    }
    if field.is_selector() {
        let value = editor.selector_text(field);
        let inner_w = (display_width(&value) as u16).min(avail.saturating_sub(4));
        let total = (inner_w + 4).min(avail);
        let style = if focused {
            theme::selected()
        } else {
            input_style()
        };
        let arrow = if focused {
            style
        } else {
            bg.patch(theme::accent())
        };
        frame.text(Rect::new(x, r.y, 1, 1), "‹", arrow);
        frame.text(
            Rect::new(x + 1, r.y, total.saturating_sub(2), 1),
            &format!(" {} ", fit(&value, inner_w as usize, false)),
            style,
        );
        if total >= 2 {
            frame.text(Rect::new(x + total - 1, r.y, 1, 1), "›", arrow);
        }
        body(frame, Rect::new(x + 1, r.y, total.saturating_sub(2), 1));
        frame.hit(
            Rect::new(x, r.y, 1, 1),
            Hit::OverlayControl {
                overlay: index,
                control: control(field, PART_PREV),
            },
        );
        if total >= 2 {
            frame.hit(
                Rect::new(x + total - 1, r.y, 1, 1),
                Hit::OverlayControl {
                    overlay: index,
                    control: control(field, PART_NEXT),
                },
            );
        }
        let hint = match field {
            Field::Metric | Field::Target | Field::Action => "←/→ or click",
            _ => "",
        };
        let hint_w = avail.saturating_sub(total + 1);
        if focused && !hint.is_empty() && hint_w > 3 {
            frame.text(
                Rect::new(x + total + 1, r.y, hint_w, 1),
                &truncate(hint, hint_w as usize),
                bg.patch(theme::dim()),
            );
        }
    }
}

fn draw_buttons(frame: &mut Frame<'_, '_, '_>, r: Rect, index: usize, editor: &WatchEditor) {
    let save = " Save ";
    let cancel = " Cancel ";
    let save_w = display_width(save) as u16;
    let cancel_w = display_width(cancel) as u16;
    let total = save_w + cancel_w + 2;
    let start = r.x + r.width.saturating_sub(total + 1);
    let mut x = start.max(r.x);
    for (field, text, w, base) in [
        (Field::Cancel, cancel, cancel_w, theme::button()),
        (Field::Save, save, save_w, theme::button_primary()),
    ] {
        let w = w.min(r.right().saturating_sub(x));
        if w == 0 {
            break;
        }
        let style = if editor.focus == field {
            base.add_modifier(bmux_tui::style::Modifier::UNDERLINE)
                .add_modifier(bmux_tui::style::Modifier::BOLD)
        } else {
            base
        };
        let rect = Rect::new(x, r.y, w, 1);
        frame.text(rect, text, style);
        frame.hit(
            rect,
            Hit::OverlayControl {
                overlay: index,
                control: control(field, PART_BODY),
            },
        );
        x = x.saturating_add(w + 2);
    }
    let hint_w = start.saturating_sub(r.x + 2);
    if hint_w > 6 {
        frame.text(
            Rect::new(r.x + 1, r.y, hint_w, 1),
            &truncate("Tab next · Enter save · Esc cancel", hint_w as usize),
            theme::panel_bg().patch(theme::dim()),
        );
    }
}

// ---- input -----------------------------------------------------------------------

pub fn handle(
    editor: &mut WatchEditor,
    index: usize,
    event: &Event,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    match event {
        Event::Key(key) => handle_key(editor, key, state),
        Event::Paste(text) => {
            let focus = editor.focus;
            if let Some(input) = editor.input_mut(focus) {
                input.insert_str(text);
            }
            OverlayOutcome::Consumed
        }
        Event::Mouse(mouse) => {
            let left = matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left));
            let right = matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right));
            if !left && !right {
                return OverlayOutcome::Consumed;
            }
            let Some(Hit::OverlayControl { overlay, control }) =
                state.hits.at(mouse.position).cloned()
            else {
                return OverlayOutcome::Consumed;
            };
            if overlay != index {
                return OverlayOutcome::Consumed;
            }
            let Some((field, part)) = decode(control) else {
                return OverlayOutcome::Consumed;
            };
            if !editor.is_active(field) {
                return OverlayOutcome::Consumed;
            }
            editor.focus = field;
            match field {
                Field::Cancel => return OverlayOutcome::Close,
                Field::Save => {
                    return if editor.save(state) {
                        OverlayOutcome::Close
                    } else {
                        OverlayOutcome::Consumed
                    };
                }
                Field::Enabled => editor.cycle(field, 1),
                f if f.is_selector() => {
                    let delta = match part {
                        PART_PREV => -1,
                        PART_NEXT => 1,
                        _ if right => -1,
                        _ => 1,
                    };
                    editor.cycle(field, delta);
                }
                f if f.is_text() && part == PART_INPUT => {
                    let rect = state.hits.rect_of(|h| {
                        *h == Hit::OverlayControl {
                            overlay: index,
                            control,
                        }
                    });
                    if let (Some(rect), Some(input)) = (rect, editor.input_mut(field)) {
                        let len = input.text.chars().count();
                        let ascii = input.text.is_ascii();
                        input.cursor = if ascii && len < rect.width as usize {
                            usize::from(mouse.position.x.saturating_sub(rect.x)).min(len)
                        } else {
                            len
                        };
                    }
                }
                _ => {}
            }
            OverlayOutcome::Consumed
        }
        _ => OverlayOutcome::Consumed,
    }
}

fn handle_key(
    editor: &mut WatchEditor,
    key: &bmux_keyboard::KeyStroke,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    let m = key.modifiers;
    if is(key, KeyCode::Escape) {
        return OverlayOutcome::Close;
    }
    if m.ctrl && key.key == KeyCode::Char('s') {
        return if editor.save(state) {
            OverlayOutcome::Close
        } else {
            OverlayOutcome::Consumed
        };
    }
    match key.key {
        KeyCode::Tab => {
            editor.move_focus(if m.shift { -1 } else { 1 });
            return OverlayOutcome::Consumed;
        }
        KeyCode::Down => {
            editor.move_focus(1);
            return OverlayOutcome::Consumed;
        }
        KeyCode::Up => {
            editor.move_focus(-1);
            return OverlayOutcome::Consumed;
        }
        KeyCode::Enter => {
            if editor.focus == Field::Cancel {
                return OverlayOutcome::Close;
            }
            return if editor.save(state) {
                OverlayOutcome::Close
            } else {
                OverlayOutcome::Consumed
            };
        }
        _ => {}
    }
    let focus = editor.focus;
    if let Some(input) = editor.input_mut(focus) {
        if input.handle_key(key) && editor.error.as_ref().is_some_and(|(f, _)| *f == focus) {
            editor.error = None;
        }
        return OverlayOutcome::Consumed;
    }
    match focus {
        Field::Cancel | Field::Save => match key.key {
            KeyCode::Left | KeyCode::Right => {
                editor.focus = if focus == Field::Cancel {
                    Field::Save
                } else {
                    Field::Cancel
                };
            }
            _ if is_space(key) => {
                if focus == Field::Cancel {
                    return OverlayOutcome::Close;
                }
                if editor.save(state) {
                    return OverlayOutcome::Close;
                }
            }
            _ => {}
        },
        _ => match key.key {
            KeyCode::Left => editor.cycle(focus, -1),
            KeyCode::Right => editor.cycle(focus, 1),
            _ if is_space(key) => editor.cycle(focus, 1),
            _ => {}
        },
    }
    OverlayOutcome::Consumed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_parse_by_unit() {
        use WatchMetric::*;
        assert_eq!(parse_threshold(CpuPercent, "80"), Ok(80.0));
        assert_eq!(parse_threshold(CpuPercent, " 80% "), Ok(80.0));
        assert_eq!(parse_threshold(CpuPercent, "250"), Ok(250.0));
        assert!(parse_threshold(MemPercent, "150").is_err());
        assert!(parse_threshold(CpuPercent, "-5").is_err());
        assert!(parse_threshold(CpuPercent, "abc").is_err());
        assert!(parse_threshold(CpuPercent, "").is_err());
        assert_eq!(parse_threshold(MemRss, "500M"), Ok(500.0 * 1048576.0));
        assert_eq!(parse_threshold(MemRss, "1G"), Ok(1073741824.0));
        assert_eq!(parse_threshold(MemRss, "1.5g"), Ok(1610612736.0));
        assert_eq!(parse_threshold(NetDown, "10MB/s"), Ok(10.0 * 1048576.0));
        assert_eq!(parse_threshold(DiskRead, "10 MiB"), Ok(10.0 * 1048576.0));
        assert_eq!(parse_threshold(DiskRead, "2048"), Ok(2048.0));
        assert!(parse_threshold(MemRss, "lots").is_err());
        assert_eq!(parse_threshold(Threads, "12.4"), Ok(12.0));
        assert_eq!(parse_threshold(Exits, "whatever"), Ok(0.0));
    }

    #[test]
    fn durations_parse_with_units() {
        assert_eq!(parse_duration_secs("30"), Ok(30));
        assert_eq!(parse_duration_secs("30s"), Ok(30));
        assert_eq!(parse_duration_secs("5m"), Ok(300));
        assert_eq!(parse_duration_secs("1m30s"), Ok(90));
        assert_eq!(parse_duration_secs("2 min"), Ok(120));
        assert_eq!(parse_duration_secs("1h"), Ok(3600));
        assert_eq!(parse_duration_secs("0.5m"), Ok(30));
        assert!(parse_duration_secs("").is_err());
        assert!(parse_duration_secs("soon").is_err());
        assert!(parse_duration_secs("5x").is_err());
        assert!(parse_duration_secs("m").is_err());
    }

    #[test]
    fn formatting_round_trips() {
        use WatchMetric::*;
        for (metric, text) in [
            (MemRss, "500M"),
            (MemRss, "1.5G"),
            (NetUp, "10K"),
            (CpuPercent, "80"),
            (CpuPercent, "12.5"),
            (Fds, "1000"),
        ] {
            let v = parse_threshold(metric, text).unwrap();
            assert_eq!(format_threshold(metric, v), text);
        }
        assert_eq!(format_threshold(MemRss, 1000.0), "1000");
        for secs in [0, 5, 60, 90, 300, 3600, 5400] {
            assert_eq!(parse_duration_secs(&format_duration(secs)), Ok(secs));
        }
        assert_eq!(format_duration(90), "1m30s");
        assert_eq!(threshold_display(NetDown, 10.0 * 1048576.0), "10M/s");
        assert_eq!(threshold_display(CpuPercent, 80.0), "80%");
    }

    #[test]
    fn focus_skips_inactive_fields() {
        let mut e = EditorForm::for_process(ProcessKey::new(5, 1), "node");
        assert_eq!(e.focus, Field::Name);
        e.move_focus(1);
        assert_eq!(e.focus, Field::Target);
        e.move_focus(1);
        assert_eq!(e.focus, Field::Metric, "process target has no value field");
        e.metric = WatchMetric::Exits;
        e.move_focus(1);
        assert_eq!(
            e.focus,
            Field::Cooldown,
            "event metrics skip threshold fields"
        );
        e.focus = Field::Name;
        e.move_focus(-1);
        assert_eq!(e.focus, Field::Save);
    }

    #[test]
    fn cycling_targets_and_metrics() {
        let mut e = EditorForm::blank(None);
        assert_eq!(e.target, TargetKind::Name);
        e.cycle(Field::Target, -1);
        assert_eq!(e.target, TargetKind::Any, "This process needs a process");
        e.cycle(Field::Target, 1);
        assert_eq!(e.target, TargetKind::Name);
        e.cycle(Field::Metric, 1);
        assert_eq!(e.metric, WatchMetric::MemPercent);
        e.cycle(Field::Metric, 1);
        assert_eq!(e.metric, WatchMetric::MemRss);
        assert_eq!(e.threshold.text, "1G", "unit change resets the threshold");
        e.cycle(Field::Metric, -2);
        assert_eq!(e.metric, WatchMetric::CpuPercent);
        e.cycle(Field::Metric, -1);
        assert_eq!(e.metric, WatchMetric::Starts);
        e.cycle(Field::Comparison, 1);
        assert_eq!(e.comparison, Comparison::Below);
        e.cycle(Field::Action, 1);
        assert_eq!(e.action, WatchAction::Log);
        e.cycle(Field::Action, -2);
        assert_eq!(e.action, WatchAction::Signal(Signal::Stop));
        e.cycle(Field::Enabled, 1);
        assert!(!e.enabled);
    }

    #[test]
    fn generated_names_follow_target_and_metric() {
        let mut e = EditorForm::for_process(ProcessKey::new(5, 1), "node");
        assert_eq!(e.name.text, "node CPU");
        e.cycle(Field::Metric, 1);
        assert_eq!(e.name.text, "node memory");
        e.name = TextInput::new("custom");
        e.cycle(Field::Metric, 1);
        assert_eq!(e.name.text, "custom", "user-entered names are kept");
    }

    #[test]
    fn build_validates_each_field() {
        let mut e = EditorForm::blank(None);
        assert_eq!(e.build(1).unwrap_err().0, Field::Value);
        e.value = TextInput::new("node*");
        e.threshold = TextInput::new("lots");
        assert_eq!(e.build(1).unwrap_err().0, Field::Threshold);
        e.threshold = TextInput::new("90%");
        e.sustain = TextInput::new("later");
        assert_eq!(e.build(1).unwrap_err().0, Field::Sustain);
        e.sustain = TextInput::new("30s");
        e.cooldown = TextInput::new("");
        assert_eq!(e.build(1).unwrap_err().0, Field::Cooldown);
        e.cooldown = TextInput::new("5m");
        let rule = e.build(7).unwrap();
        assert_eq!(rule.id, 7);
        assert_eq!(rule.name, "node* CPU");
        assert_eq!(rule.target, WatchTarget::Name("node*".into()));
        assert_eq!(rule.threshold, 90.0);
        assert_eq!(rule.sustain_secs, 30);
        assert_eq!(rule.cooldown_secs, 300);
        assert!(e.summary().contains("> 90% for 30s → notify"));
    }

    #[test]
    fn from_rule_round_trips() {
        let rule = WatchRule {
            id: 3,
            name: "big".into(),
            enabled: false,
            target: WatchTarget::User("root".into()),
            metric: WatchMetric::MemRss,
            comparison: Comparison::Below,
            threshold: 512.0 * 1048576.0,
            sustain_secs: 90,
            cooldown_secs: 3600,
            action: WatchAction::Signal(Signal::Hup),
        };
        let e = EditorForm::from_rule(&rule);
        assert_eq!(e.threshold.text, "512M");
        assert_eq!(e.build(3).unwrap(), rule);
        assert_eq!(action_label(rule.action), "Signal HUP");
    }

    #[test]
    fn controls_encode_and_decode() {
        for field in Field::ALL {
            for part in [PART_BODY, PART_PREV, PART_NEXT, PART_INPUT] {
                assert_eq!(decode(control(field, part)), Some((field, part)));
            }
        }
        assert_eq!(decode(999), None);
    }
}
