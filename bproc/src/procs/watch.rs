//! Watch rules, alert evaluation and the process event log.
use super::actions::Signal;
use super::model::{ProcSnapshot, ProcessKey, ProcessRecord, fmt};
use super::store::{ProcessStore, Row};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WatchTarget {
    /// One specific process instance.
    Process {
        key: ProcessKey,
        name: String,
    },
    /// Process name; `*` and `?` globs allowed, case-insensitive.
    Name(String),
    /// Substring of the full command line, case-insensitive.
    Command(String),
    User(String),
    Any,
}

/// Anything a watch target can be tested against: a store [`Row`] (what the
/// UI and the engine hold) or a bare [`ProcessRecord`] (exited processes).
pub trait WatchSubject {
    fn watch_record(&self) -> &ProcessRecord;
}

impl WatchSubject for ProcessRecord {
    fn watch_record(&self) -> &ProcessRecord {
        self
    }
}

impl WatchSubject for std::sync::Arc<ProcessRecord> {
    fn watch_record(&self) -> &ProcessRecord {
        self
    }
}

impl WatchSubject for Row {
    fn watch_record(&self) -> &ProcessRecord {
        &self.entry.record
    }
}

impl WatchTarget {
    /// Whether this target covers `subject` (a [`Row`] or a record):
    /// `Process` compares the exact key (PID + start time, so a reused PID
    /// never matches); `Name` is a case-insensitive `*`/`?` glob on the
    /// process name; `Command` a case-insensitive substring of the command
    /// line (empty matches all); `User` the exact user name, or the numeric
    /// uid when the name is unknown.
    pub fn matches<S: WatchSubject + ?Sized>(&self, subject: &S) -> bool {
        let record = subject.watch_record();
        match self {
            Self::Process { key, .. } => record.key == *key,
            Self::Name(pattern) => glob_match(pattern, &record.name),
            Self::Command(needle) => {
                needle.is_empty()
                    || record
                        .command()
                        .to_lowercase()
                        .contains(&needle.to_lowercase())
            }
            Self::User(user) => match &record.user {
                Some(name) => name == user,
                None => record.uid.is_some_and(|uid| user.parse::<u32>() == Ok(uid)),
            },
            Self::Any => true,
        }
    }

    /// e.g. "name node*", "process node (4242)".
    pub fn describe(&self) -> String {
        match self {
            Self::Process { key, name } => format!("process {name} ({})", key.pid),
            Self::Name(pattern) => format!("name {pattern}"),
            Self::Command(needle) => format!("command contains \"{needle}\""),
            Self::User(user) => format!("user {user}"),
            Self::Any => "any process".into(),
        }
    }
}

/// Case-insensitive glob supporting `*` (any run) and `?` (one char).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                backtrack = Some((pi, ti));
                pi += 1;
            }
            Some(&c) if c == '?' || c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match backtrack {
                Some((star, matched)) => {
                    pi = star + 1;
                    ti = matched + 1;
                    backtrack = Some((star, matched + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchMetric {
    CpuPercent,
    MemPercent,
    MemRss,
    DiskRead,
    DiskWrite,
    NetDown,
    NetUp,
    Threads,
    Fds,
    /// Fires when a matching process exits (threshold ignored).
    Exits,
    /// Fires when a matching process starts (threshold ignored).
    Starts,
}

impl WatchMetric {
    pub const ALL: [WatchMetric; 11] = [
        Self::CpuPercent,
        Self::MemPercent,
        Self::MemRss,
        Self::DiskRead,
        Self::DiskWrite,
        Self::NetDown,
        Self::NetUp,
        Self::Threads,
        Self::Fds,
        Self::Exits,
        Self::Starts,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::CpuPercent => "CPU %",
            Self::MemPercent => "Memory %",
            Self::MemRss => "Resident memory (bytes)",
            Self::DiskRead => "Disk read (bytes/s)",
            Self::DiskWrite => "Disk write (bytes/s)",
            Self::NetDown => "Network down (bytes/s)",
            Self::NetUp => "Network up (bytes/s)",
            Self::Threads => "Threads",
            Self::Fds => "Open files",
            Self::Exits => "Process exits",
            Self::Starts => "Process starts",
        }
    }
    pub const fn is_event(self) -> bool {
        matches!(self, Self::Exits | Self::Starts)
    }

    /// Compact name used in rule descriptions and alert messages.
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::CpuPercent => "CPU %",
            Self::MemPercent => "memory %",
            Self::MemRss => "RSS",
            Self::DiskRead => "disk read",
            Self::DiskWrite => "disk write",
            Self::NetDown => "download",
            Self::NetUp => "upload",
            Self::Threads => "threads",
            Self::Fds => "open files",
            Self::Exits => "process exits",
            Self::Starts => "process starts",
        }
    }

    /// Key accepted on the command line and in config (`cpu`, `rss`, ...).
    pub const fn key(self) -> &'static str {
        match self {
            Self::CpuPercent => "cpu",
            Self::MemPercent => "mem",
            Self::MemRss => "rss",
            Self::DiskRead => "read",
            Self::DiskWrite => "write",
            Self::NetDown => "down",
            Self::NetUp => "up",
            Self::Threads => "threads",
            Self::Fds => "fds",
            Self::Exits => "exits",
            Self::Starts => "starts",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        let key = key.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|m| m.key() == key)
            .or(match key.as_str() {
                "memory" | "mem%" => Some(Self::MemPercent),
                "res" | "resident" => Some(Self::MemRss),
                "net-down" | "download" | "rx" => Some(Self::NetDown),
                "net-up" | "upload" | "tx" => Some(Self::NetUp),
                "thr" => Some(Self::Threads),
                "files" => Some(Self::Fds),
                "exit" => Some(Self::Exits),
                "start" => Some(Self::Starts),
                _ => None,
            })
    }

    /// Thresholds for these metrics are byte counts (or bytes per second).
    pub const fn is_bytes(self) -> bool {
        matches!(
            self,
            Self::MemRss | Self::DiskRead | Self::DiskWrite | Self::NetDown | Self::NetUp
        )
    }

    pub const fn is_rate(self) -> bool {
        matches!(
            self,
            Self::DiskRead | Self::DiskWrite | Self::NetDown | Self::NetUp
        )
    }

    /// Current value for a live row; `None` when unknown or for event metrics.
    /// `network_known` treats a missing network attribution as zero traffic.
    pub fn value(self, row: &Row, network_known: bool) -> Option<f64> {
        let s = &row.entry.sample;
        let net = |pick: fn(&crate::process::ProcessInfo) -> u64| match &row.net {
            Some(n) => Some(pick(n) as f64),
            None => network_known.then_some(0.0),
        };
        match self {
            Self::CpuPercent => Some(s.cpu_percent as f64),
            Self::MemPercent => Some(s.mem_percent as f64),
            Self::MemRss => Some(s.mem_rss as f64),
            Self::DiskRead => Some(s.disk_read_rate as f64),
            Self::DiskWrite => Some(s.disk_write_rate as f64),
            Self::NetDown => net(|n| n.download_rate),
            Self::NetUp => net(|n| n.upload_rate),
            Self::Threads => s.threads.map(f64::from),
            Self::Fds => s.open_fds.map(f64::from),
            Self::Exits | Self::Starts => None,
        }
    }

    /// Human value with units: "93%", "1.2G", "10M/s", "300".
    pub fn format_value(self, value: f64) -> String {
        match self {
            Self::CpuPercent | Self::MemPercent => format!("{}%", number(value)),
            _ if self.is_rate() => format!("{}/s", fmt::bytes(value.max(0.0) as u64)),
            _ if self.is_bytes() => fmt::bytes(value.max(0.0) as u64),
            _ => number(value),
        }
    }
}

/// Trim needless decimals: 80 → "80", 2.5 → "2.5", 93.4 → "93".
fn number(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    if value.abs() >= 10.0 || value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        let s = format!("{value:.1}");
        s.trim_end_matches(".0").to_string()
    }
}

/// Compact span: "45s", "3m12s", "5m", "1h30m", "2d3h".
pub fn format_span(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        (secs % 86_400) / 3600,
        (secs % 3600) / 60,
        secs % 60,
    );
    let pair = |a: u64, ua: &str, b: u64, ub: &str| {
        if b == 0 {
            format!("{a}{ua}")
        } else {
            format!("{a}{ua}{b}{ub}")
        }
    };
    if d > 0 {
        pair(d, "d", h, "h")
    } else if h > 0 {
        pair(h, "h", m, "m")
    } else if m > 0 {
        pair(m, "m", s, "s")
    } else {
        format!("{s}s")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    Above,
    Below,
}

impl Comparison {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Above => "above",
            Self::Below => "below",
        }
    }
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Above => ">",
            Self::Below => "<",
        }
    }
    pub fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::Above => value > threshold,
            Self::Below => value < threshold,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchAction {
    /// Toast + event log entry.
    Notify,
    /// Event log only.
    Log,
    /// Notify, then send this signal to the offending process.
    Signal(Signal),
}

impl WatchAction {
    pub fn label(self) -> String {
        match self {
            Self::Notify => "notify".into(),
            Self::Log => "log only".into(),
            Self::Signal(signal) => format!("send {}", signal.name()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchRule {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub target: WatchTarget,
    pub metric: WatchMetric,
    pub comparison: Comparison,
    pub threshold: f64,
    /// Condition must hold continuously this long before firing.
    pub sustain_secs: u64,
    /// Minimum time between firings for the same process. 0 fires once per
    /// episode (again only after the condition has cleared).
    pub cooldown_secs: u64,
    pub action: WatchAction,
}

pub const DEFAULT_SUSTAIN_SECS: u64 = 10;
pub const DEFAULT_COOLDOWN_SECS: u64 = 60;

impl WatchRule {
    /// A rule with editor defaults: enabled, 10s sustain, 60s cooldown, notify.
    pub fn new(
        id: u64,
        name: impl Into<String>,
        target: WatchTarget,
        metric: WatchMetric,
        comparison: Comparison,
        threshold: f64,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            enabled: true,
            target,
            metric,
            comparison,
            threshold,
            sustain_secs: if metric.is_event() {
                0
            } else {
                DEFAULT_SUSTAIN_SECS
            },
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
            action: WatchAction::Notify,
        }
    }

    pub fn with_sustain(mut self, secs: u64) -> Self {
        self.sustain_secs = secs;
        self
    }
    pub fn with_cooldown(mut self, secs: u64) -> Self {
        self.cooldown_secs = secs;
        self
    }
    pub fn with_action(mut self, action: WatchAction) -> Self {
        self.action = action;
        self
    }

    /// e.g. "CPU % above 80 for 30s · name node* → notify"
    pub fn describe(&self) -> String {
        let condition = if self.metric.is_event() {
            self.metric.short_label().to_string()
        } else {
            let threshold = if self.metric.is_bytes() {
                self.metric.format_value(self.threshold)
            } else {
                number(self.threshold)
            };
            let mut s = format!(
                "{} {} {threshold}",
                self.metric.short_label(),
                self.comparison.label()
            );
            if self.sustain_secs > 0 {
                s.push_str(&format!(" for {}", format_span(self.sustain_secs)));
            }
            s
        };
        format!(
            "{condition} · {} → {}",
            self.target.describe(),
            self.action.label()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    Started,
    Exited,
    Alert,
    Action,
    Info,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcEvent {
    pub at_ms: u64,
    pub kind: EventKind,
    pub key: Option<ProcessKey>,
    pub name: String,
    pub message: String,
}

pub const EVENT_LOG_LEN: usize = 5_000;

#[derive(Debug, Default)]
pub struct EventLog {
    events: VecDeque<ProcEvent>,
    /// Total events ever pushed (lets views detect new entries).
    pushed: u64,
}

impl EventLog {
    pub fn push(&mut self, event: ProcEvent) {
        if self.events.len() == EVENT_LOG_LEN {
            self.events.pop_front();
        }
        self.events.push_back(event);
        self.pushed += 1;
    }
    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub fn pushed(&self) -> u64 {
        self.pushed
    }
    /// Oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ProcEvent> + ExactSizeIterator {
        self.events.iter()
    }
    /// Record spawn/exit events from a snapshot already applied to `store`.
    /// Spawns are skipped on the first snapshot (everything is "new" then).
    pub fn record_snapshot(&mut self, snapshot: &ProcSnapshot, store: &ProcessStore) {
        let now = snapshot.acquired_at_ms;
        let mut exited: Vec<_> = snapshot.exited.iter().collect();
        exited.sort_by_key(|e| e.record.key);
        for gone in exited {
            self.push(ProcEvent {
                at_ms: now,
                kind: EventKind::Exited,
                key: Some(gone.record.key),
                name: gone.record.name.clone(),
                message: exit_message(&gone.record, gone.last_sample.cpu_time_ms, now),
            });
        }
        if snapshot.sequence <= 1 {
            return;
        }
        let mut spawned = snapshot.spawned.clone();
        spawned.sort();
        for key in spawned {
            let record = store.get(&key).map(|r| &*r.entry.record).or_else(|| {
                snapshot
                    .processes
                    .iter()
                    .find(|p| p.key() == key)
                    .map(|p| &*p.record)
            });
            let (name, message) = match record {
                Some(r) => (r.name.clone(), format!("started · {}", r.command())),
                None => (format!("PID {}", key.pid), "started".into()),
            };
            self.push(ProcEvent {
                at_ms: now,
                kind: EventKind::Started,
                key: Some(key),
                name,
                message,
            });
        }
    }
}

fn lifetime_secs(record: &ProcessRecord, now_ms: u64) -> Option<u64> {
    (record.key.start_time != 0).then(|| (now_ms / 1000).saturating_sub(record.key.start_time))
}

fn exit_message(record: &ProcessRecord, cpu_time_ms: u64, now_ms: u64) -> String {
    let cpu = fmt::cpu_time(cpu_time_ms);
    match lifetime_secs(record, now_ms) {
        Some(secs) => format!("exited after {} · cpu {cpu}", format_span(secs)),
        None => format!("exited · cpu {cpu}"),
    }
}

/// A rule fired for a process this tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Firing {
    pub rule_id: u64,
    pub key: ProcessKey,
    pub name: String,
    pub message: String,
    pub action: WatchAction,
}

/// Per (rule, process) condition tracking.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Tracker {
    /// When the condition became continuously true.
    since_ms: Option<u64>,
    last_fired_ms: Option<u64>,
    /// Fired (or would have) during the current episode.
    fired_this_episode: bool,
    /// Condition true and held for at least the sustain time.
    sustained: bool,
}

#[derive(Debug, Default)]
pub struct WatchEngine {
    pub rules: Vec<WatchRule>,
    trackers: HashMap<(u64, ProcessKey), Tracker>,
}

impl WatchEngine {
    pub fn new(rules: Vec<WatchRule>) -> Self {
        Self {
            rules,
            trackers: HashMap::new(),
        }
    }
    pub fn next_id(&self) -> u64 {
        self.rules.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }
    /// Evaluate all enabled rules after a snapshot has been applied to `store`.
    pub fn evaluate(&mut self, snapshot: &ProcSnapshot, store: &ProcessStore) -> Vec<Firing> {
        let now = snapshot.acquired_at_ms;
        let network_known = store.has_network();
        let mut firings = Vec::new();
        let mut live_pairs = HashSet::new();
        for rule in self.rules.iter().filter(|r| r.enabled) {
            if rule.metric.is_event() {
                firings.extend(event_firings(rule, snapshot, store));
                continue;
            }
            for row in store.rows().iter().filter(|r| !r.is_ghost()) {
                if !rule.target.matches(row) {
                    continue;
                }
                let record = &row.entry.record;
                let pair = (rule.id, record.key);
                live_pairs.insert(pair);
                let value = rule.metric.value(row, network_known);
                let holds = value.is_some_and(|v| rule.comparison.holds(v, rule.threshold));
                let tracker = self.trackers.entry(pair).or_default();
                if !holds {
                    tracker.since_ms = None;
                    tracker.sustained = false;
                    tracker.fired_this_episode = false;
                    continue;
                }
                let since = *tracker.since_ms.get_or_insert(now);
                let held_ms = now.saturating_sub(since);
                tracker.sustained = held_ms >= rule.sustain_secs.saturating_mul(1000);
                if !tracker.sustained {
                    continue;
                }
                let cooled = match tracker.last_fired_ms {
                    None => true,
                    Some(_) if rule.cooldown_secs == 0 => !tracker.fired_this_episode,
                    Some(last) => {
                        now.saturating_sub(last) >= rule.cooldown_secs.saturating_mul(1000)
                    }
                };
                if !cooled {
                    continue;
                }
                tracker.last_fired_ms = Some(now);
                tracker.fired_this_episode = true;
                let value = value.unwrap_or_default();
                let mut message = format!(
                    "{} ({}) {} {} {} {}",
                    record.name,
                    record.key.pid,
                    alert_metric_name(rule.metric),
                    rule.metric.format_value(value),
                    rule.comparison.symbol(),
                    rule.metric.format_value(rule.threshold),
                );
                let held = held_ms / 1000;
                if held > 0 {
                    message.push_str(&format!(" for {}", format_span(held)));
                }
                firings.push(Firing {
                    rule_id: rule.id,
                    key: record.key,
                    name: record.name.clone(),
                    message,
                    action: rule.action,
                });
            }
        }
        self.trackers.retain(|pair, _| live_pairs.contains(pair));
        firings
    }
    /// Keys currently violating any rule (shown highlighted in the table).
    pub fn alerting(&self) -> Vec<ProcessKey> {
        let mut keys: Vec<ProcessKey> = self
            .trackers
            .iter()
            .filter(|(_, t)| t.sustained)
            .map(|((_, key), _)| *key)
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }
    /// Rules a process currently violates (for detail views and tooltips).
    pub fn alerting_rules(&self, key: &ProcessKey) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .trackers
            .iter()
            .filter(|((_, k), t)| k == key && t.sustained)
            .map(|((id, _), _)| *id)
            .collect();
        ids.sort();
        ids
    }
    pub fn rule(&self, id: u64) -> Option<&WatchRule> {
        self.rules.iter().find(|r| r.id == id)
    }
    pub fn rule_mut(&mut self, id: u64) -> Option<&mut WatchRule> {
        self.rules.iter_mut().find(|r| r.id == id)
    }
    /// Insert or replace (by id) a rule; its tracking state restarts.
    pub fn upsert(&mut self, rule: WatchRule) {
        self.forget(rule.id);
        match self.rule_mut(rule.id) {
            Some(existing) => *existing = rule,
            None => self.rules.push(rule),
        }
    }
    pub fn remove(&mut self, id: u64) -> Option<WatchRule> {
        self.forget(id);
        let index = self.rules.iter().position(|r| r.id == id)?;
        Some(self.rules.remove(index))
    }
    /// Flip a rule on/off; returns the new state.
    pub fn toggle(&mut self, id: u64) -> Option<bool> {
        self.forget(id);
        let rule = self.rule_mut(id)?;
        rule.enabled = !rule.enabled;
        Some(rule.enabled)
    }
    fn forget(&mut self, id: u64) {
        self.trackers.retain(|(rule, _), _| *rule != id);
    }
}

fn alert_metric_name(metric: WatchMetric) -> &'static str {
    match metric {
        WatchMetric::CpuPercent => "CPU",
        WatchMetric::MemPercent => "memory",
        other => other.short_label(),
    }
}

fn event_firings(rule: &WatchRule, snapshot: &ProcSnapshot, store: &ProcessStore) -> Vec<Firing> {
    let now = snapshot.acquired_at_ms;
    let fire = |record: &ProcessRecord, message: String, action: WatchAction| Firing {
        rule_id: rule.id,
        key: record.key,
        name: record.name.clone(),
        message,
        action,
    };
    let mut out = Vec::new();
    match rule.metric {
        WatchMetric::Exits => {
            // Signalling a process that is already gone is meaningless.
            let action = match rule.action {
                WatchAction::Signal(_) => WatchAction::Notify,
                other => other,
            };
            for gone in &snapshot.exited {
                let r = &gone.record;
                if rule.target.matches(r) {
                    let after = lifetime_secs(r, now)
                        .map(|s| format!(" after {}", format_span(s)))
                        .unwrap_or_default();
                    out.push(fire(
                        r,
                        format!("{} ({}) exited{after}", r.name, r.key.pid),
                        action,
                    ));
                }
            }
        }
        WatchMetric::Starts if snapshot.sequence > 1 => {
            for key in &snapshot.spawned {
                let Some(row) = store.get(key).filter(|r| !r.is_ghost()) else {
                    continue;
                };
                if rule.target.matches(row) {
                    let r = &*row.entry.record;
                    out.push(fire(
                        r,
                        format!("{} ({}) started", r.name, r.key.pid),
                        rule.action,
                    ));
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::model::{ExitedProcess, ProcessEntry, ProcessSample, SystemSnapshot};
    use std::sync::Arc;

    const T0: u64 = 1_700_000_000_000;
    const MIB: f64 = 1024.0 * 1024.0;

    fn record(pid: i32, name: &str, cmd: &str, user: &str) -> Arc<ProcessRecord> {
        Arc::new(ProcessRecord {
            key: ProcessKey::new(pid, 1_699_999_000 + pid as u64),
            ppid: Some(1),
            name: name.into(),
            cmdline: cmd.split_whitespace().map(String::from).collect(),
            user: Some(user.into()),
            uid: Some(501),
            ..Default::default()
        })
    }

    fn entry(record: &Arc<ProcessRecord>, cpu: f32) -> ProcessEntry {
        ProcessEntry {
            record: record.clone(),
            sample: ProcessSample {
                cpu_percent: cpu,
                mem_rss: 100 * 1024 * 1024,
                threads: Some(8),
                ..Default::default()
            },
        }
    }

    struct Sim {
        store: ProcessStore,
        seq: u64,
    }

    impl Sim {
        fn new() -> Self {
            Self {
                store: ProcessStore::new(),
                seq: 0,
            }
        }
        /// Apply a tick `secs` after T0 and return the snapshot used.
        fn tick(&mut self, secs: u64, processes: Vec<ProcessEntry>) -> ProcSnapshot {
            self.tick_with(secs, processes, Vec::new(), Vec::new())
        }
        fn tick_with(
            &mut self,
            secs: u64,
            processes: Vec<ProcessEntry>,
            spawned: Vec<ProcessKey>,
            exited: Vec<ExitedProcess>,
        ) -> ProcSnapshot {
            self.seq += 1;
            let snapshot = ProcSnapshot {
                sequence: self.seq,
                acquired_at_ms: T0 + secs * 1000,
                interval_secs: 1.0,
                system: SystemSnapshot::default(),
                processes,
                spawned,
                exited,
                permission_limited: 0,
            };
            self.store.apply_snapshot(snapshot.clone());
            snapshot
        }
    }

    fn cpu_rule(id: u64, target: WatchTarget, sustain: u64, cooldown: u64) -> WatchRule {
        WatchRule::new(
            id,
            "cpu",
            target,
            WatchMetric::CpuPercent,
            Comparison::Above,
            80.0,
        )
        .with_sustain(sustain)
        .with_cooldown(cooldown)
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("node*", "node"));
        assert!(glob_match("node*", "NodeJS"));
        assert!(glob_match("*sh", "zsh"));
        assert!(glob_match("?sh", "zsh"));
        assert!(!glob_match("?sh", "bash"));
        assert!(glob_match("*a*b*", "xxaYYbzz"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("", "x"));
        assert!(glob_match("chrome", "Chrome"));
        assert!(!glob_match("chrome", "chromedriver"));
        assert!(glob_match("a*a*a", "aaaaaaaaa"));
        assert!(!glob_match("a*b", "aaaaaaaac"));
    }

    #[test]
    fn targets_match_records() {
        let r = record(42, "node", "/usr/bin/node server.js --port 80", "alice");
        assert!(WatchTarget::Any.matches(&r));
        assert!(WatchTarget::Name("NODE".into()).matches(&r));
        assert!(!WatchTarget::Name("nod".into()).matches(&r));
        assert!(WatchTarget::Command("SERVER.js".into()).matches(&r));
        assert!(!WatchTarget::Command("python".into()).matches(&r));
        assert!(WatchTarget::User("alice".into()).matches(&r));
        assert!(!WatchTarget::User("Alice".into()).matches(&r));
        let exact = WatchTarget::Process {
            key: r.key,
            name: "node".into(),
        };
        assert!(exact.matches(&r));
        let reused = WatchTarget::Process {
            key: ProcessKey::new(42, 1),
            name: "node".into(),
        };
        assert!(!reused.matches(&r));
        let anonymous = ProcessRecord {
            uid: Some(0),
            ..Default::default()
        };
        assert!(WatchTarget::User("0".into()).matches(&anonymous));
    }

    #[test]
    fn targets_match_store_rows() {
        let r = record(42, "node", "/usr/bin/node server.js", "alice");
        let store = ProcessStore::from_entries(vec![entry(&r, 1.0)], SystemSnapshot::default());
        let row = store.get(&r.key).expect("row");
        for target in [
            WatchTarget::Any,
            WatchTarget::Name("no*".into()),
            WatchTarget::Command("server".into()),
            WatchTarget::User("alice".into()),
            WatchTarget::Process {
                key: r.key,
                name: "node".into(),
            },
        ] {
            assert!(target.matches(row), "{target:?}");
            assert_eq!(target.matches(row), target.matches(&*r), "{target:?}");
        }
        assert!(!WatchTarget::Name("python".into()).matches(row));
        assert!(!WatchTarget::User("bob".into()).matches(row));
    }

    #[test]
    fn describe_is_human() {
        let rule = cpu_rule(1, WatchTarget::Name("node*".into()), 30, 60);
        assert_eq!(
            rule.describe(),
            "CPU % above 80 for 30s · name node* → notify"
        );
        let rss = WatchRule::new(
            2,
            "mem",
            WatchTarget::User("root".into()),
            WatchMetric::MemRss,
            Comparison::Above,
            500.0 * MIB,
        )
        .with_sustain(0)
        .with_action(WatchAction::Signal(Signal::Term));
        assert_eq!(rss.describe(), "RSS above 500M · user root → send SIGTERM");
        let exits = WatchRule::new(
            3,
            "x",
            WatchTarget::Command("worker".into()),
            WatchMetric::Exits,
            Comparison::Above,
            0.0,
        );
        assert_eq!(
            exits.describe(),
            "process exits · command contains \"worker\" → notify"
        );
        let fds = WatchRule::new(
            4,
            "f",
            WatchTarget::Any,
            WatchMetric::DiskWrite,
            Comparison::Below,
            1024.0,
        )
        .with_sustain(300)
        .with_action(WatchAction::Log);
        assert_eq!(
            fds.describe(),
            "disk write below 1.0K/s for 5m · any process → log only"
        );
    }

    #[test]
    fn constructor_defaults() {
        let rule = cpu_rule(7, WatchTarget::Any, 10, 60);
        let fresh = WatchRule::new(
            7,
            "cpu",
            WatchTarget::Any,
            WatchMetric::CpuPercent,
            Comparison::Above,
            80.0,
        );
        assert_eq!(fresh, rule);
        assert!(fresh.enabled);
        assert_eq!(fresh.action, WatchAction::Notify);
    }

    #[test]
    fn metric_keys_roundtrip() {
        for metric in WatchMetric::ALL {
            assert_eq!(WatchMetric::from_key(metric.key()), Some(metric));
        }
        assert_eq!(
            WatchMetric::from_key("Memory"),
            Some(WatchMetric::MemPercent)
        );
        assert_eq!(WatchMetric::from_key("nope"), None);
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(format_span(0), "0s");
        assert_eq!(format_span(45), "45s");
        assert_eq!(format_span(192), "3m12s");
        assert_eq!(format_span(300), "5m");
        assert_eq!(format_span(5400), "1h30m");
        assert_eq!(format_span(3 * 86_400 + 7200), "3d2h");
        assert_eq!(number(80.0), "80");
        assert_eq!(number(2.5), "2.5");
        assert_eq!(number(93.4), "93");
        assert_eq!(number(0.04), "0");
        assert_eq!(WatchMetric::CpuPercent.format_value(93.2), "93%");
        assert_eq!(WatchMetric::DiskRead.format_value(10.0 * MIB), "10.0M/s");
        assert_eq!(WatchMetric::Threads.format_value(300.0), "300");
    }

    #[test]
    fn fires_immediately_without_sustain() {
        let node = record(4242, "node", "node app.js", "alice");
        let mut engine = WatchEngine::new(vec![cpu_rule(1, WatchTarget::Any, 0, 60)]);
        let mut sim = Sim::new();
        let snap = sim.tick(0, vec![entry(&node, 93.0)]);
        let firings = engine.evaluate(&snap, &sim.store);
        assert_eq!(firings.len(), 1);
        assert_eq!(firings[0].message, "node (4242) CPU 93% > 80%");
        assert_eq!(firings[0].key, node.key);
        assert_eq!(firings[0].rule_id, 1);
        assert_eq!(engine.alerting(), vec![node.key]);
    }

    #[test]
    fn sustain_then_cooldown() {
        let node = record(4242, "node", "node app.js", "alice");
        let mut engine =
            WatchEngine::new(vec![cpu_rule(1, WatchTarget::Name("node".into()), 30, 60)]);
        let mut sim = Sim::new();
        let mut fired_at = Vec::new();
        for secs in (0..=200).step_by(10) {
            let snap = sim.tick(secs, vec![entry(&node, 93.0)]);
            let firings = engine.evaluate(&snap, &sim.store);
            if secs < 30 {
                assert!(engine.alerting().is_empty(), "not sustained at {secs}");
            }
            if let Some(f) = firings.first() {
                fired_at.push((secs, f.message.clone()));
            }
        }
        assert_eq!(
            fired_at.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![30, 90, 150]
        );
        assert_eq!(fired_at[0].1, "node (4242) CPU 93% > 80% for 30s");
        assert_eq!(fired_at[1].1, "node (4242) CPU 93% > 80% for 1m30s");
    }

    #[test]
    fn dip_resets_sustain_but_cooldown_persists() {
        let node = record(1, "node", "node", "alice");
        let mut engine = WatchEngine::new(vec![cpu_rule(1, WatchTarget::Any, 20, 60)]);
        let mut sim = Sim::new();
        let mut run = |secs, cpu| {
            let snap = sim.tick(secs, vec![entry(&node, cpu)]);
            engine.evaluate(&snap, &sim.store).len()
        };
        assert_eq!(run(0, 90.0), 0);
        assert_eq!(run(10, 90.0), 0);
        assert_eq!(run(15, 10.0), 0);
        assert_eq!(run(20, 90.0), 0);
        assert_eq!(run(35, 90.0), 0);
        assert_eq!(run(40, 90.0), 1);
        assert_eq!(run(45, 10.0), 0);
        assert_eq!(run(50, 90.0), 0);
        assert_eq!(run(70, 90.0), 0, "sustained again but still cooling down");
        assert_eq!(run(100, 90.0), 1);
    }

    #[test]
    fn zero_cooldown_fires_once_per_episode() {
        let node = record(1, "node", "node", "alice");
        let mut engine = WatchEngine::new(vec![cpu_rule(1, WatchTarget::Any, 0, 0)]);
        let mut sim = Sim::new();
        let mut run = |secs, cpu| {
            let snap = sim.tick(secs, vec![entry(&node, cpu)]);
            engine.evaluate(&snap, &sim.store).len()
        };
        assert_eq!(run(0, 90.0), 1);
        assert_eq!(run(1, 90.0), 0);
        assert_eq!(run(2, 90.0), 0);
        assert_eq!(run(3, 10.0), 0);
        assert_eq!(run(4, 90.0), 1);
    }

    #[test]
    fn below_comparison_and_unknown_values() {
        let a = record(1, "a", "a", "u");
        let mut sample = entry(&a, 0.0);
        sample.sample.threads = None;
        let mut engine = WatchEngine::new(vec![
            WatchRule::new(
                1,
                "t",
                WatchTarget::Any,
                WatchMetric::Threads,
                Comparison::Below,
                4.0,
            )
            .with_sustain(0),
            WatchRule::new(
                2,
                "c",
                WatchTarget::Any,
                WatchMetric::CpuPercent,
                Comparison::Below,
                1.0,
            )
            .with_sustain(0),
            WatchRule::new(
                3,
                "n",
                WatchTarget::Any,
                WatchMetric::NetDown,
                Comparison::Below,
                1.0,
            )
            .with_sustain(0),
        ]);
        let mut sim = Sim::new();
        let snap = sim.tick(0, vec![sample]);
        let firings = engine.evaluate(&snap, &sim.store);
        let ids: Vec<u64> = firings.iter().map(|f| f.rule_id).collect();
        assert_eq!(ids, vec![2], "unknown threads/network never match");
    }

    #[test]
    fn disabled_and_deleted_rules_clean_up() {
        let node = record(1, "node", "node", "alice");
        let mut engine = WatchEngine::new(vec![
            cpu_rule(1, WatchTarget::Any, 0, 60),
            cpu_rule(2, WatchTarget::Any, 0, 60),
        ]);
        let mut sim = Sim::new();
        let snap = sim.tick(0, vec![entry(&node, 99.0)]);
        assert_eq!(engine.evaluate(&snap, &sim.store).len(), 2);
        assert_eq!(engine.alerting_rules(&node.key), vec![1, 2]);
        engine.rules[0].enabled = false;
        engine.rules.retain(|r| r.id != 2);
        let snap = sim.tick(1, vec![entry(&node, 99.0)]);
        assert!(engine.evaluate(&snap, &sim.store).is_empty());
        assert!(engine.alerting().is_empty());
        assert!(engine.trackers.is_empty());
        engine.rules[0].enabled = true;
        let snap = sim.tick(2, vec![entry(&node, 99.0)]);
        assert_eq!(
            engine.evaluate(&snap, &sim.store).len(),
            1,
            "fresh state after re-enable"
        );
    }

    #[test]
    fn vanished_processes_are_forgotten() {
        let a = record(1, "a", "a", "u");
        let b = record(2, "b", "b", "u");
        let mut engine = WatchEngine::new(vec![cpu_rule(1, WatchTarget::Any, 0, 600)]);
        let mut sim = Sim::new();
        let snap = sim.tick(0, vec![entry(&a, 90.0), entry(&b, 90.0)]);
        assert_eq!(engine.evaluate(&snap, &sim.store).len(), 2);
        let exited = vec![ExitedProcess {
            record: b.clone(),
            last_sample: entry(&b, 90.0).sample,
        }];
        let snap = sim.tick_with(1, vec![entry(&a, 90.0)], vec![], exited);
        assert!(engine.evaluate(&snap, &sim.store).is_empty());
        assert_eq!(engine.alerting(), vec![a.key], "ghost rows do not alert");
        assert_eq!(engine.trackers.len(), 1);
    }

    #[test]
    fn upsert_toggle_remove() {
        let mut engine = WatchEngine::default();
        assert_eq!(engine.next_id(), 1);
        engine.upsert(cpu_rule(engine.next_id(), WatchTarget::Any, 0, 0));
        engine.upsert(cpu_rule(5, WatchTarget::Any, 0, 0));
        assert_eq!(engine.next_id(), 6);
        let mut edited = cpu_rule(1, WatchTarget::Any, 0, 0);
        edited.threshold = 50.0;
        engine.upsert(edited);
        assert_eq!(engine.rules.len(), 2);
        assert_eq!(engine.rule(1).map(|r| r.threshold), Some(50.0));
        assert_eq!(engine.toggle(1), Some(false));
        assert_eq!(engine.toggle(99), None);
        assert!(engine.remove(5).is_some());
        assert!(engine.remove(5).is_none());
    }

    #[test]
    fn exit_and_start_rules() {
        let shell = record(10, "zsh", "-zsh", "alice");
        let worker = record(11, "worker", "worker --queue a", "alice");
        let other = record(12, "cron", "cron", "root");
        let mut engine = WatchEngine::new(vec![
            WatchRule::new(
                1,
                "exit",
                WatchTarget::Name("work*".into()),
                WatchMetric::Exits,
                Comparison::Above,
                0.0,
            )
            .with_action(WatchAction::Signal(Signal::Kill)),
            WatchRule::new(
                2,
                "start",
                WatchTarget::User("alice".into()),
                WatchMetric::Starts,
                Comparison::Above,
                0.0,
            ),
        ]);
        let mut sim = Sim::new();
        let snap = sim.tick(0, vec![entry(&shell, 0.0)]);
        assert!(engine.evaluate(&snap, &sim.store).is_empty());
        let snap = sim.tick_with(
            1,
            vec![entry(&shell, 0.0), entry(&worker, 0.0), entry(&other, 0.0)],
            vec![worker.key, other.key],
            vec![],
        );
        let firings = engine.evaluate(&snap, &sim.store);
        assert_eq!(firings.len(), 1);
        assert_eq!(firings[0].message, "worker (11) started");
        let exited = vec![ExitedProcess {
            record: worker.clone(),
            last_sample: ProcessSample::default(),
        }];
        let snap = sim.tick_with(
            2,
            vec![entry(&shell, 0.0), entry(&other, 0.0)],
            vec![],
            exited,
        );
        let firings = engine.evaluate(&snap, &sim.store);
        assert_eq!(firings.len(), 1);
        assert_eq!(
            firings[0].action,
            WatchAction::Notify,
            "no signals to dead processes"
        );
        assert!(firings[0].message.starts_with("worker (11) exited after "));
        assert!(engine.alerting().is_empty());
    }

    #[test]
    fn start_rules_skip_first_snapshot() {
        let a = record(1, "a", "a", "u");
        let mut engine = WatchEngine::new(vec![WatchRule::new(
            1,
            "s",
            WatchTarget::Any,
            WatchMetric::Starts,
            Comparison::Above,
            0.0,
        )]);
        let mut sim = Sim::new();
        let snap = sim.tick_with(0, vec![entry(&a, 0.0)], vec![a.key], vec![]);
        assert!(engine.evaluate(&snap, &sim.store).is_empty());
    }

    #[test]
    fn network_metrics_use_all_traffic() {
        let a = record(1, "curl", "curl", "u");
        let mut map = crate::process::ProcessMap::new();
        let mut info = crate::process::ProcessInfo::new(1, "curl".into());
        info.download_rate = 5 * 1024 * 1024;
        info.internet_download_rate = 1;
        map.insert(1, info);
        let mut sim = Sim::new();
        sim.store.merge_network(&map);
        let snap = sim.tick(0, vec![entry(&a, 0.0)]);
        let mut engine = WatchEngine::new(vec![
            WatchRule::new(
                1,
                "n",
                WatchTarget::Any,
                WatchMetric::NetDown,
                Comparison::Above,
                MIB,
            )
            .with_sustain(0),
        ]);
        let firings = engine.evaluate(&snap, &sim.store);
        assert_eq!(firings.len(), 1);
        assert_eq!(firings[0].message, "curl (1) download 5.0M/s > 1.0M/s");
    }

    #[test]
    fn event_log_records_spawns_and_exits() {
        let a = record(1, "init", "/sbin/init", "root");
        let b = record(2, "foo", "/usr/bin/foo --bar", "alice");
        let mut log = EventLog::default();
        let mut sim = Sim::new();
        let snap = sim.tick_with(0, vec![entry(&a, 0.0)], vec![a.key], vec![]);
        log.record_snapshot(&snap, &sim.store);
        assert!(log.is_empty(), "first snapshot spawns are skipped");
        let snap = sim.tick_with(1, vec![entry(&a, 0.0), entry(&b, 0.0)], vec![b.key], vec![]);
        log.record_snapshot(&snap, &sim.store);
        assert_eq!(log.len(), 1);
        let started = log.iter().last().unwrap();
        assert_eq!(started.kind, EventKind::Started);
        assert_eq!(started.name, "foo");
        assert_eq!(started.message, "started · /usr/bin/foo --bar");
        assert_eq!(started.at_ms, T0 + 1000);

        let gone = ProcessRecord {
            key: ProcessKey::new(2, (T0 + 193_000) / 1000 - 192),
            ..(*b).clone()
        };
        let exited = vec![ExitedProcess {
            record: Arc::new(gone),
            last_sample: ProcessSample {
                cpu_time_ms: 62_000,
                ..Default::default()
            },
        }];
        let snap = sim.tick_with(193, vec![entry(&a, 0.0)], vec![], exited);
        log.record_snapshot(&snap, &sim.store);
        let exit = log.iter().last().unwrap();
        assert_eq!(exit.kind, EventKind::Exited);
        assert_eq!(exit.message, "exited after 3m12s · cpu 1:02.00");
        assert_eq!(log.pushed(), 2);
    }

    #[test]
    fn event_log_handles_unknown_start_and_missing_rows() {
        let mut log = EventLog::default();
        let store = ProcessStore::new();
        let snapshot = ProcSnapshot {
            sequence: 5,
            acquired_at_ms: T0,
            spawned: vec![ProcessKey::new(77, 0)],
            exited: vec![ExitedProcess {
                record: Arc::new(ProcessRecord {
                    key: ProcessKey::new(78, 0),
                    name: "x".into(),
                    ..Default::default()
                }),
                last_sample: ProcessSample::default(),
            }],
            ..Default::default()
        };
        log.record_snapshot(&snapshot, &store);
        let messages: Vec<&str> = log.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, vec!["exited · cpu 0:00.00", "started"]);
        assert_eq!(log.iter().last().unwrap().name, "PID 77");
    }

    #[test]
    fn event_log_is_bounded() {
        let mut log = EventLog::default();
        for i in 0..EVENT_LOG_LEN + 10 {
            log.push(ProcEvent {
                at_ms: i as u64,
                kind: EventKind::Info,
                key: None,
                name: String::new(),
                message: String::new(),
            });
        }
        assert_eq!(log.len(), EVENT_LOG_LEN);
        assert_eq!(log.pushed(), (EVENT_LOG_LEN + 10) as u64);
        assert_eq!(log.iter().next().unwrap().at_ms, 10);
    }
}
