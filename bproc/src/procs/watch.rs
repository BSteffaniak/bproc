//! Watch rules, alert evaluation and the process event log.
use super::actions::Signal;
use super::model::{ProcSnapshot, ProcessKey};
use super::store::ProcessStore;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    Above,
    Below,
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
    /// Minimum time between firings for the same process.
    pub cooldown_secs: u64,
    pub action: WatchAction,
}

impl WatchRule {
    /// e.g. "node CPU % > 80 for 30s → notify"
    pub fn describe(&self) -> String {
        format!(
            "{} ({:?} {:?} {})",
            self.name, self.metric, self.comparison, self.threshold
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
    /// Record spawn/exit events from a snapshot.
    pub fn record_snapshot(&mut self, snapshot: &ProcSnapshot, store: &ProcessStore) {
        let _ = (snapshot, store);
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

#[derive(Debug, Default)]
pub struct WatchEngine {
    pub rules: Vec<WatchRule>,
}

impl WatchEngine {
    pub fn new(rules: Vec<WatchRule>) -> Self {
        Self { rules }
    }
    pub fn next_id(&self) -> u64 {
        self.rules.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }
    /// Evaluate all enabled rules after a snapshot has been applied to `store`.
    pub fn evaluate(&mut self, snapshot: &ProcSnapshot, store: &ProcessStore) -> Vec<Firing> {
        let _ = (snapshot, store);
        Vec::new()
    }
    /// Keys currently violating any rule (shown highlighted in the table).
    pub fn alerting(&self) -> Vec<ProcessKey> {
        Vec::new()
    }
}
