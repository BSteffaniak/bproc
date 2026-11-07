//! Column registry: the single source of truth for table headers, widths,
//! sorting, filtering keys, settings and export.
use super::model::{ProcessKey, fmt};
use super::store::Row;
use crate::process::TrafficType;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ColumnId {
    Pid,
    Ppid,
    User,
    Name,
    Command,
    State,
    Cpu,
    Mem,
    Rss,
    Virt,
    Threads,
    Nice,
    Priority,
    CpuTime,
    Started,
    DiskRead,
    DiskWrite,
    DiskReadTotal,
    DiskWriteTotal,
    NetDown,
    NetUp,
    NetDownTotal,
    NetUpTotal,
    Connections,
    Fds,
    CtxSwitches,
    MajorFaults,
    Tty,
    Group,
    Exe,
    Throttle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// Values a column needs beyond the row itself.
#[derive(Debug, Clone, Copy)]
pub struct ColumnCtx {
    pub traffic: TrafficType,
    pub now_ms: u64,
    pub mem_total: u64,
    pub cpu_count: usize,
}

impl Default for ColumnCtx {
    fn default() -> Self {
        Self {
            traffic: TrafficType::All,
            now_ms: 0,
            mem_total: 0,
            cpu_count: 1,
        }
    }
}

pub const ALL_COLUMNS: &[ColumnId] = &[
    ColumnId::Pid,
    ColumnId::Ppid,
    ColumnId::User,
    ColumnId::Name,
    ColumnId::Command,
    ColumnId::State,
    ColumnId::Cpu,
    ColumnId::Mem,
    ColumnId::Rss,
    ColumnId::Virt,
    ColumnId::Threads,
    ColumnId::Nice,
    ColumnId::Priority,
    ColumnId::CpuTime,
    ColumnId::Started,
    ColumnId::DiskRead,
    ColumnId::DiskWrite,
    ColumnId::DiskReadTotal,
    ColumnId::DiskWriteTotal,
    ColumnId::NetDown,
    ColumnId::NetUp,
    ColumnId::NetDownTotal,
    ColumnId::NetUpTotal,
    ColumnId::Connections,
    ColumnId::Fds,
    ColumnId::CtxSwitches,
    ColumnId::MajorFaults,
    ColumnId::Tty,
    ColumnId::Group,
    ColumnId::Exe,
    ColumnId::Throttle,
];

/// Built-in column presets shown as tabs above the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Preset {
    Overview,
    Cpu,
    Memory,
    Disk,
    Network,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 6] = [
        Preset::Overview,
        Preset::Cpu,
        Preset::Memory,
        Preset::Disk,
        Preset::Network,
        Preset::Custom,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Disk => "Disk",
            Self::Network => "Network",
            Self::Custom => "Custom",
        }
    }

    pub fn default_columns(self) -> Vec<ColumnId> {
        use ColumnId::*;
        match self {
            Self::Overview => vec![
                Pid, User, Cpu, Mem, Rss, State, CpuTime, NetDown, NetUp, Command,
            ],
            Self::Cpu => vec![
                Pid,
                User,
                Cpu,
                CpuTime,
                Threads,
                Nice,
                Priority,
                State,
                CtxSwitches,
                Command,
            ],
            Self::Memory => vec![Pid, User, Mem, Rss, Virt, MajorFaults, Threads, Command],
            Self::Disk => vec![
                Pid,
                User,
                DiskRead,
                DiskWrite,
                DiskReadTotal,
                DiskWriteTotal,
                Fds,
                Command,
            ],
            Self::Network => vec![
                Pid,
                User,
                NetDown,
                NetUp,
                NetDownTotal,
                NetUpTotal,
                Connections,
                Throttle,
                Command,
            ],
            Self::Custom => vec![Pid, Ppid, User, State, Cpu, Mem, Started, Group, Command],
        }
    }

    /// The column a preset sorts by when first opened.
    pub const fn default_sort(self) -> ColumnId {
        match self {
            Self::Overview | Self::Cpu | Self::Custom => ColumnId::Cpu,
            Self::Memory => ColumnId::Rss,
            Self::Disk => ColumnId::DiskRead,
            Self::Network => ColumnId::NetDown,
        }
    }
}

fn net(row: &Row, ctx: &ColumnCtx) -> Option<(u64, u64, u64, u64)> {
    let n = row.net.as_ref()?;
    Some(match ctx.traffic {
        TrafficType::All => (
            n.download_rate,
            n.upload_rate,
            n.total_download,
            n.total_upload,
        ),
        TrafficType::Internet => (
            n.internet_download_rate,
            n.internet_upload_rate,
            n.internet_total_download,
            n.internet_total_upload,
        ),
        TrafficType::Local => (
            n.local_download_rate,
            n.local_upload_rate,
            n.local_total_download,
            n.local_total_upload,
        ),
    })
}

fn opt<T: ToString>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "–".into())
}

impl ColumnId {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Pid => "PID",
            Self::Ppid => "PPID",
            Self::User => "USER",
            Self::Name => "NAME",
            Self::Command => "COMMAND",
            Self::State => "S",
            Self::Cpu => "CPU%",
            Self::Mem => "MEM%",
            Self::Rss => "RES",
            Self::Virt => "VIRT",
            Self::Threads => "THR",
            Self::Nice => "NI",
            Self::Priority => "PRI",
            Self::CpuTime => "TIME+",
            Self::Started => "AGE",
            Self::DiskRead => "DISK R",
            Self::DiskWrite => "DISK W",
            Self::DiskReadTotal => "READ",
            Self::DiskWriteTotal => "WRITTEN",
            Self::NetDown => "NET ↓",
            Self::NetUp => "NET ↑",
            Self::NetDownTotal => "RECV",
            Self::NetUpTotal => "SENT",
            Self::Connections => "CONN",
            Self::Fds => "FDS",
            Self::CtxSwitches => "CSW",
            Self::MajorFaults => "MAJFLT",
            Self::Tty => "TTY",
            Self::Group => "GROUP",
            Self::Exe => "EXE",
            Self::Throttle => "LIMIT",
        }
    }

    /// One-line description for the column chooser and header tooltips.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Pid => "Process ID",
            Self::Ppid => "Parent process ID",
            Self::User => "Owning user",
            Self::Name => "Process name",
            Self::Command => "Full command line (tree mode draws the hierarchy here)",
            Self::State => "Scheduler state (R running, S sleeping, D disk, T stopped, Z zombie)",
            Self::Cpu => "CPU usage, percent of one core (can exceed 100 on multicore)",
            Self::Mem => "Resident memory as percent of physical memory",
            Self::Rss => "Resident memory",
            Self::Virt => "Virtual memory size",
            Self::Threads => "Thread count",
            Self::Nice => "Nice value (-20 highest priority, 19 lowest)",
            Self::Priority => "Kernel scheduling priority",
            Self::CpuTime => "Accumulated CPU time",
            Self::Started => "Time since the process started",
            Self::DiskRead => "Disk read rate",
            Self::DiskWrite => "Disk write rate",
            Self::DiskReadTotal => "Total bytes read from disk",
            Self::DiskWriteTotal => "Total bytes written to disk",
            Self::NetDown => "Network download rate (needs capture privileges)",
            Self::NetUp => "Network upload rate (needs capture privileges)",
            Self::NetDownTotal => "Total bytes received",
            Self::NetUpTotal => "Total bytes sent",
            Self::Connections => "Open network connections",
            Self::Fds => "Open file descriptors",
            Self::CtxSwitches => "Context switches",
            Self::MajorFaults => "Major page faults",
            Self::Tty => "Controlling terminal",
            Self::Group => "Container, cgroup or launchd group",
            Self::Exe => "Executable path",
            Self::Throttle => "Active bandwidth limit",
        }
    }

    /// Key used by the filter language (`cpu>10`, `user:root`).
    pub const fn filter_key(self) -> &'static str {
        match self {
            Self::Pid => "pid",
            Self::Ppid => "ppid",
            Self::User => "user",
            Self::Name => "name",
            Self::Command => "cmd",
            Self::State => "state",
            Self::Cpu => "cpu",
            Self::Mem => "mem",
            Self::Rss => "rss",
            Self::Virt => "virt",
            Self::Threads => "threads",
            Self::Nice => "nice",
            Self::Priority => "pri",
            Self::CpuTime => "time",
            Self::Started => "age",
            Self::DiskRead => "read",
            Self::DiskWrite => "write",
            Self::DiskReadTotal => "readtotal",
            Self::DiskWriteTotal => "writetotal",
            Self::NetDown => "down",
            Self::NetUp => "up",
            Self::NetDownTotal => "recv",
            Self::NetUpTotal => "sent",
            Self::Connections => "conn",
            Self::Fds => "fds",
            Self::CtxSwitches => "csw",
            Self::MajorFaults => "majflt",
            Self::Tty => "tty",
            Self::Group => "group",
            Self::Exe => "exe",
            Self::Throttle => "limit",
        }
    }

    pub fn from_filter_key(key: &str) -> Option<Self> {
        ALL_COLUMNS
            .iter()
            .copied()
            .find(|c| c.filter_key().eq_ignore_ascii_case(key))
    }

    pub const fn align(self) -> Align {
        match self {
            Self::User
            | Self::Name
            | Self::Command
            | Self::State
            | Self::Tty
            | Self::Group
            | Self::Exe => Align::Left,
            _ => Align::Right,
        }
    }

    /// Preferred width in cells. `Command`/`Exe` are flexible and absorb slack.
    pub const fn width(self) -> u16 {
        match self {
            Self::Pid | Self::Ppid => 7,
            Self::User => 9,
            Self::Name => 16,
            Self::Command | Self::Exe => 0,
            Self::State => 1,
            Self::Cpu | Self::Mem => 5,
            Self::Rss | Self::Virt => 6,
            Self::Threads | Self::Nice | Self::Priority => 4,
            Self::CpuTime => 9,
            Self::Started => 9,
            Self::DiskRead | Self::DiskWrite | Self::NetDown | Self::NetUp => 8,
            Self::DiskReadTotal | Self::DiskWriteTotal | Self::NetDownTotal | Self::NetUpTotal => 7,
            Self::Connections | Self::Fds => 5,
            Self::CtxSwitches | Self::MajorFaults => 7,
            Self::Tty => 7,
            Self::Group => 14,
            Self::Throttle => 9,
        }
    }

    pub const fn is_flexible(self) -> bool {
        matches!(self, Self::Command | Self::Exe)
    }

    /// Lower values survive longer when the terminal is too narrow.
    pub const fn priority(self) -> u8 {
        match self {
            Self::Command | Self::Name => 0,
            Self::Pid | Self::Cpu => 1,
            Self::Mem | Self::NetDown | Self::DiskRead | Self::Rss => 2,
            Self::User | Self::NetUp | Self::DiskWrite => 3,
            Self::State | Self::CpuTime | Self::Throttle => 4,
            _ => 5,
        }
    }

    /// Numeric columns sort descending by default; text columns ascending.
    pub const fn default_descending(self) -> bool {
        !matches!(self.align(), Align::Left)
            && !matches!(self, Self::Pid | Self::Ppid | Self::Nice | Self::Priority)
    }

    /// Numeric value for heat colouring, filters and sorting.
    pub fn numeric(self, row: &Row, ctx: &ColumnCtx) -> Option<f64> {
        let s = &row.entry.sample;
        let r = &row.entry.record;
        Some(match self {
            Self::Pid => r.key.pid as f64,
            Self::Ppid => r.ppid? as f64,
            Self::Cpu => s.cpu_percent as f64,
            Self::Mem => s.mem_percent as f64,
            Self::Rss => s.mem_rss as f64,
            Self::Virt => s.mem_virtual as f64,
            Self::Threads => s.threads? as f64,
            Self::Nice => s.nice? as f64,
            Self::Priority => s.priority? as f64,
            Self::CpuTime => s.cpu_time_ms as f64,
            Self::Started => {
                if r.key.start_time == 0 {
                    return None;
                }
                (ctx.now_ms / 1000).saturating_sub(r.key.start_time) as f64
            }
            Self::DiskRead => s.disk_read_rate as f64,
            Self::DiskWrite => s.disk_write_rate as f64,
            Self::DiskReadTotal => s.disk_read_total as f64,
            Self::DiskWriteTotal => s.disk_write_total as f64,
            Self::NetDown => net(row, ctx)?.0 as f64,
            Self::NetUp => net(row, ctx)?.1 as f64,
            Self::NetDownTotal => net(row, ctx)?.2 as f64,
            Self::NetUpTotal => net(row, ctx)?.3 as f64,
            Self::Connections => row.net.as_ref()?.connections.len() as f64,
            Self::Fds => s.open_fds? as f64,
            Self::CtxSwitches => s.ctx_switches? as f64,
            Self::MajorFaults => s.major_faults? as f64,
            Self::Throttle => {
                let limit = row.net.as_ref()?.throttle_limit.as_ref()?;
                limit
                    .download_limit
                    .unwrap_or(0)
                    .max(limit.upload_limit.unwrap_or(0)) as f64
            }
            Self::User
            | Self::Name
            | Self::Command
            | Self::State
            | Self::Tty
            | Self::Group
            | Self::Exe => {
                return None;
            }
        })
    }

    /// Plain cell text (tree prefixes and highlighting are added by the view).
    pub fn text(self, row: &Row, ctx: &ColumnCtx) -> String {
        let s = &row.entry.sample;
        let r = &row.entry.record;
        match self {
            Self::Pid => r.key.pid.to_string(),
            Self::Ppid => opt(r.ppid),
            Self::User => r
                .user
                .clone()
                .or_else(|| r.uid.map(|u| u.to_string()))
                .unwrap_or_else(|| "–".into()),
            Self::Name => r.name.clone(),
            Self::Command => r.command(),
            Self::State => s.state.code().to_string(),
            Self::Cpu => fmt::percent(s.cpu_percent),
            Self::Mem => fmt::percent(s.mem_percent),
            Self::Rss => fmt::bytes(s.mem_rss),
            Self::Virt => fmt::bytes(s.mem_virtual),
            Self::Threads => opt(s.threads),
            Self::Nice => opt(s.nice),
            Self::Priority => opt(s.priority),
            Self::CpuTime => fmt::cpu_time(s.cpu_time_ms),
            Self::Started => {
                if r.key.start_time == 0 {
                    "–".into()
                } else {
                    fmt::duration_secs((ctx.now_ms / 1000).saturating_sub(r.key.start_time))
                }
            }
            Self::DiskRead => fmt::rate(s.disk_read_rate),
            Self::DiskWrite => fmt::rate(s.disk_write_rate),
            Self::DiskReadTotal => fmt::bytes(s.disk_read_total),
            Self::DiskWriteTotal => fmt::bytes(s.disk_write_total),
            Self::NetDown => net(row, ctx)
                .map(|n| fmt::rate(n.0))
                .unwrap_or_else(|| "–".into()),
            Self::NetUp => net(row, ctx)
                .map(|n| fmt::rate(n.1))
                .unwrap_or_else(|| "–".into()),
            Self::NetDownTotal => net(row, ctx)
                .map(|n| fmt::bytes(n.2))
                .unwrap_or_else(|| "–".into()),
            Self::NetUpTotal => net(row, ctx)
                .map(|n| fmt::bytes(n.3))
                .unwrap_or_else(|| "–".into()),
            Self::Connections => opt(row.net.as_ref().map(|n| n.connections.len())),
            Self::Fds => opt(s.open_fds),
            Self::CtxSwitches => opt(s.ctx_switches),
            Self::MajorFaults => opt(s.major_faults),
            Self::Tty => opt(r.tty.clone()),
            Self::Group => opt(r.group.clone()),
            Self::Exe => opt(r.exe.clone()),
            Self::Throttle => match row.net.as_ref().and_then(|n| n.throttle_limit.as_ref()) {
                Some(limit) => {
                    let d = limit
                        .download_limit
                        .map(fmt::bytes)
                        .unwrap_or_else(|| "∞".into());
                    let u = limit
                        .upload_limit
                        .map(fmt::bytes)
                        .unwrap_or_else(|| "∞".into());
                    format!("{d}/{u}")
                }
                None => String::new(),
            },
        }
    }

    /// True for columns whose values are text (sorted case-insensitively).
    pub const fn is_text(self) -> bool {
        matches!(self.align(), Align::Left)
    }

    /// Precomputed sort key so large views compute each value once per sort.
    pub fn sort_key(self, row: &Row, ctx: &ColumnCtx) -> SortKey {
        if !self.is_text() {
            return SortKey::Num(self.numeric(row, ctx).filter(|v| !v.is_nan()));
        }
        let r = &row.entry.record;
        let text = match self {
            Self::User => r.user.clone().or_else(|| r.uid.map(|u| u.to_string())),
            Self::Name => Some(r.name.clone()),
            Self::Command => Some(r.command()),
            Self::Tty => r.tty.clone(),
            Self::Group => r.group.clone(),
            Self::Exe => r.exe.clone(),
            _ => Some(self.text(row, ctx)),
        };
        SortKey::Text(text.filter(|t| !t.is_empty()).map(|t| t.to_lowercase()))
    }

    /// Total order for sorting (ascending sense); ties are broken by the
    /// caller (PID). Missing values order before present ones.
    pub fn compare(self, a: &Row, b: &Row, ctx: &ColumnCtx) -> Ordering {
        if self.is_text() {
            let x = self.text(a, ctx).to_lowercase();
            let y = self.text(b, ctx).to_lowercase();
            return x.cmp(&y);
        }
        match (self.numeric(a, ctx), self.numeric(b, ctx)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        }
    }
}

/// A column value prepared for sorting: numbers compare numerically, text
/// lowercased. `None` marks a missing value.
#[derive(Debug, Clone, PartialEq)]
pub enum SortKey {
    Num(Option<f64>),
    Text(Option<String>),
}

impl SortKey {
    /// Compare two keys in the requested direction. Missing values always sort
    /// last, whatever the direction, so blanks never crowd the top.
    pub fn cmp_directed(&self, other: &Self, descending: bool) -> Ordering {
        fn directed<T>(
            a: Option<T>,
            b: Option<T>,
            ord: impl Fn(T, T) -> Ordering,
            desc: bool,
        ) -> Ordering {
            match (a, b) {
                (Some(x), Some(y)) => {
                    let o = ord(x, y);
                    if desc { o.reverse() } else { o }
                }
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
        match (self, other) {
            (Self::Num(a), Self::Num(b)) => {
                directed(a.as_ref(), b.as_ref(), |x, y| x.total_cmp(y), descending)
            }
            (Self::Text(a), Self::Text(b)) => {
                directed(a.as_deref(), b.as_deref(), |x, y| x.cmp(y), descending)
            }
            (Self::Num(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Num(_)) => Ordering::Greater,
        }
    }
}

/// Stable tiebreak shared by all sorts.
pub fn tiebreak(a: &ProcessKey, b: &ProcessKey) -> Ordering {
    a.pid.cmp(&b.pid).then(a.start_time.cmp(&b.start_time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{ProcessInfo, ThrottleLimit};
    use crate::procs::model::{ProcState, ProcessEntry, ProcessRecord, ProcessSample};
    use std::collections::HashSet;
    use std::sync::Arc;

    fn default_row() -> Row {
        Row {
            entry: ProcessEntry {
                record: Arc::new(ProcessRecord::default()),
                sample: ProcessSample::default(),
            },
            net: None,
            exited_at_ms: None,
        }
    }

    fn rich_row(pid: i32) -> Row {
        let mut info = ProcessInfo::new(pid, "x".into());
        info.download_rate = pid as u64 * 10;
        info.throttle_limit = Some(ThrottleLimit {
            download_limit: Some(1024),
            upload_limit: None,
            traffic_type: TrafficType::All,
        });
        Row {
            entry: ProcessEntry {
                record: Arc::new(ProcessRecord {
                    key: ProcessKey::new(pid, 100),
                    ppid: Some(1),
                    name: format!("proc{pid}"),
                    cmdline: vec!["/bin/x".into(), "ünïcode".into()],
                    exe: Some("/bin/x".into()),
                    user: Some("root".into()),
                    uid: Some(0),
                    tty: Some("pts/0".into()),
                    ..Default::default()
                }),
                sample: ProcessSample {
                    state: ProcState::Running,
                    cpu_percent: pid as f32,
                    threads: Some(pid as u32),
                    nice: Some(-(pid % 20)),
                    open_fds: Some(3),
                    ..Default::default()
                },
            },
            net: Some(Arc::new(info)),
            exited_at_ms: None,
        }
    }

    #[test]
    fn filter_keys_are_unique_and_round_trip() {
        let mut seen = HashSet::new();
        for &c in ALL_COLUMNS {
            assert!(
                seen.insert(c.filter_key()),
                "duplicate key {}",
                c.filter_key()
            );
            assert_eq!(ColumnId::from_filter_key(c.filter_key()), Some(c));
            assert_eq!(
                ColumnId::from_filter_key(&c.filter_key().to_uppercase()),
                Some(c)
            );
            assert!(!c.title().is_empty());
            assert!(!c.description().is_empty());
        }
        assert_eq!(ColumnId::from_filter_key("nope"), None);
        assert_eq!(seen.len(), ALL_COLUMNS.len());
    }

    #[test]
    fn registry_is_complete_and_consistent() {
        let unique: HashSet<_> = ALL_COLUMNS.iter().collect();
        assert_eq!(unique.len(), ALL_COLUMNS.len());
        assert!(ALL_COLUMNS.contains(&ColumnId::Throttle));
        for &c in ALL_COLUMNS {
            if c.is_flexible() {
                assert_eq!(c.width(), 0);
            } else {
                assert!(c.width() as usize >= c.title().chars().count(), "{c:?}");
            }
            if c.is_text() {
                assert!(c.numeric(&rich_row(5), &ColumnCtx::default()).is_none());
                assert_eq!(c.align(), Align::Left);
            }
        }
        for preset in Preset::ALL {
            let cols = preset.default_columns();
            assert!(cols.contains(&ColumnId::Pid));
            assert!(cols.contains(&preset.default_sort()), "{preset:?}");
        }
    }

    #[test]
    fn text_never_panics_on_sparse_rows() {
        let ctx = ColumnCtx {
            now_ms: u64::MAX,
            ..ColumnCtx::default()
        };
        for &c in ALL_COLUMNS {
            let _ = c.text(&default_row(), &ColumnCtx::default());
            let _ = c.text(&default_row(), &ctx);
            let _ = c.text(&rich_row(7), &ctx);
            let _ = c.numeric(&default_row(), &ctx);
            let _ = c.sort_key(&default_row(), &ctx);
        }
        assert_eq!(ColumnId::Started.text(&default_row(), &ctx), "–");
        assert_eq!(ColumnId::Started.numeric(&default_row(), &ctx), None);
        assert_eq!(ColumnId::Throttle.text(&rich_row(1), &ctx), "1.0K/∞");
    }

    #[test]
    fn compare_is_a_total_order() {
        let ctx = ColumnCtx::default();
        let rows = [default_row(), rich_row(3), rich_row(12), rich_row(3)];
        for &c in ALL_COLUMNS {
            for a in &rows {
                assert_eq!(c.compare(a, a, &ctx), Ordering::Equal, "{c:?} reflexive");
                for b in &rows {
                    assert_eq!(
                        c.compare(a, b, &ctx),
                        c.compare(b, a, &ctx).reverse(),
                        "{c:?}"
                    );
                    for d in &rows {
                        if c.compare(a, b, &ctx) != Ordering::Greater
                            && c.compare(b, d, &ctx) != Ordering::Greater
                        {
                            assert_ne!(
                                c.compare(a, d, &ctx),
                                Ordering::Greater,
                                "{c:?} transitive"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn sort_keys_put_missing_last() {
        let present = SortKey::Num(Some(1.0));
        let missing = SortKey::Num(None);
        for desc in [false, true] {
            assert_eq!(present.cmp_directed(&missing, desc), Ordering::Less);
            assert_eq!(missing.cmp_directed(&present, desc), Ordering::Greater);
            assert_eq!(missing.cmp_directed(&missing, desc), Ordering::Equal);
        }
        let a = SortKey::Text(Some("alpha".into()));
        let b = SortKey::Text(Some("beta".into()));
        assert_eq!(a.cmp_directed(&b, false), Ordering::Less);
        assert_eq!(a.cmp_directed(&b, true), Ordering::Greater);
        assert_eq!(
            ColumnId::Name.sort_key(&rich_row(4), &ColumnCtx::default()),
            SortKey::Text(Some("proc4".into()))
        );
        assert_eq!(
            ColumnId::Tty.sort_key(&default_row(), &ColumnCtx::default()),
            SortKey::Text(None)
        );
        let mut nan = default_row();
        nan.entry.sample.cpu_percent = f32::NAN;
        assert_eq!(
            ColumnId::Cpu.sort_key(&nan, &ColumnCtx::default()),
            SortKey::Num(None)
        );
    }

    #[test]
    fn default_directions() {
        assert!(ColumnId::Cpu.default_descending());
        assert!(ColumnId::Rss.default_descending());
        assert!(!ColumnId::Pid.default_descending());
        assert!(!ColumnId::Name.default_descending());
        assert!(!ColumnId::Nice.default_descending());
    }
}
