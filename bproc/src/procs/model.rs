//! Platform-neutral process observation model shared by collectors, the store and the UI.
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Stable process identity. PIDs are reused by the kernel; the start time
/// disambiguates them, so every signal, pin, watch and selection uses this key.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct ProcessKey {
    pub pid: i32,
    /// Process start time in seconds since the Unix epoch (0 when unknown).
    pub start_time: u64,
}

impl ProcessKey {
    pub const fn new(pid: i32, start_time: u64) -> Self {
        Self { pid, start_time }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ProcState {
    Running,
    Sleeping,
    DiskSleep,
    Stopped,
    Tracing,
    Zombie,
    Idle,
    Dead,
    #[default]
    Unknown,
}

impl ProcState {
    /// htop-style single character code.
    pub const fn code(self) -> char {
        match self {
            Self::Running => 'R',
            Self::Sleeping => 'S',
            Self::DiskSleep => 'D',
            Self::Stopped => 'T',
            Self::Tracing => 't',
            Self::Zombie => 'Z',
            Self::Idle => 'I',
            Self::Dead => 'X',
            Self::Unknown => '?',
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Sleeping => "Sleeping",
            Self::DiskSleep => "Disk sleep",
            Self::Stopped => "Stopped",
            Self::Tracing => "Tracing stop",
            Self::Zombie => "Zombie",
            Self::Idle => "Idle",
            Self::Dead => "Dead",
            Self::Unknown => "Unknown",
        }
    }
}

/// Attributes that do not change for the life of a process (or change rarely
/// enough that collectors refresh them only on first sight).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessRecord {
    pub key: ProcessKey,
    pub ppid: Option<i32>,
    pub name: String,
    pub cmdline: Vec<String>,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub uid: Option<u32>,
    pub user: Option<String>,
    /// Linux kernel threads (kthreadd children); always false elsewhere.
    pub kernel_thread: bool,
    pub tty: Option<String>,
    /// Container / cgroup / launchd label when known.
    pub group: Option<String>,
}

impl ProcessRecord {
    /// Full command line, falling back to the name when arguments are unreadable.
    pub fn command(&self) -> String {
        if self.cmdline.is_empty() {
            self.name.clone()
        } else {
            self.cmdline.join(" ")
        }
    }
}

/// Per-tick measurements. Optional fields are `None` when the platform or the
/// current privileges do not expose them; the UI renders those as `–`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessSample {
    pub state: ProcState,
    /// Percent of one core (htop semantics: may exceed 100 on multicore).
    pub cpu_percent: f32,
    /// Accumulated user+system CPU time in milliseconds.
    pub cpu_time_ms: u64,
    pub mem_rss: u64,
    pub mem_virtual: u64,
    /// RSS as a percent of physical memory.
    pub mem_percent: f32,
    pub threads: Option<u32>,
    pub nice: Option<i32>,
    pub priority: Option<i32>,
    pub disk_read_rate: u64,
    pub disk_write_rate: u64,
    pub disk_read_total: u64,
    pub disk_write_total: u64,
    pub open_fds: Option<u32>,
    pub ctx_switches: Option<u64>,
    pub major_faults: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessEntry {
    pub record: Arc<ProcessRecord>,
    pub sample: ProcessSample,
}

impl ProcessEntry {
    pub fn key(&self) -> ProcessKey {
        self.record.key
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskCounts {
    pub total: usize,
    pub running: usize,
    pub sleeping: usize,
    pub stopped: usize,
    pub zombie: usize,
    pub threads: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemSnapshot {
    /// Per-logical-core utilisation, 0..=100.
    pub cpus: Vec<f32>,
    pub cpu_total: f32,
    pub mem_total: u64,
    pub mem_used: u64,
    pub mem_available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub load_average: [f64; 3],
    pub uptime_secs: u64,
    pub boot_time: u64,
    pub tasks: TaskCounts,
    pub hostname: String,
    pub os: String,
}

/// A process that disappeared between two samples.
#[derive(Debug, Clone, PartialEq)]
pub struct ExitedProcess {
    pub record: Arc<ProcessRecord>,
    pub last_sample: ProcessSample,
}

/// One complete collector tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcSnapshot {
    pub sequence: u64,
    /// Wall clock milliseconds since the Unix epoch at acquisition.
    pub acquired_at_ms: u64,
    /// Seconds covered by the rate fields (time since the previous sample).
    pub interval_secs: f32,
    pub system: SystemSnapshot,
    pub processes: Vec<ProcessEntry>,
    pub spawned: Vec<ProcessKey>,
    pub exited: Vec<ExitedProcess>,
    /// Processes whose details were partially hidden by permissions.
    pub permission_limited: usize,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Human formatting shared by every view so units stay consistent.
pub mod fmt {
    pub fn bytes(value: u64) -> String {
        const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
        let mut v = value as f64;
        let mut unit = 0;
        while v >= 1024.0 && unit < UNITS.len() - 1 {
            v /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            format!("{value}B")
        } else if v >= 100.0 {
            format!("{v:.0}{}", UNITS[unit])
        } else {
            format!("{v:.1}{}", UNITS[unit])
        }
    }

    pub fn rate(value: u64) -> String {
        if value == 0 {
            "0".into()
        } else {
            format!("{}/s", bytes(value))
        }
    }

    /// `h:mm:ss` or `Nd hh:mm` for CPU time / uptime.
    pub fn duration_secs(secs: u64) -> String {
        let days = secs / 86_400;
        let h = (secs % 86_400) / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        if days > 0 {
            format!("{days}d {h:02}:{m:02}")
        } else {
            format!("{h}:{m:02}:{s:02}")
        }
    }

    /// htop-style TIME+ column (`m:ss.cc`, or `hh:mm:ss` past an hour).
    pub fn cpu_time(ms: u64) -> String {
        let total_secs = ms / 1000;
        if total_secs >= 3600 {
            duration_secs(total_secs)
        } else {
            format!(
                "{}:{:02}.{:02}",
                total_secs / 60,
                total_secs % 60,
                (ms % 1000) / 10
            )
        }
    }

    pub fn percent(value: f32) -> String {
        if value >= 100.0 {
            format!("{value:.0}")
        } else {
            format!("{value:.1}")
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn formats_are_compact() {
            assert_eq!(bytes(512), "512B");
            assert_eq!(bytes(1536), "1.5K");
            assert_eq!(bytes(200 * 1024 * 1024), "200M");
            assert_eq!(rate(0), "0");
            assert_eq!(cpu_time(61_230), "1:01.23");
            assert_eq!(duration_secs(90_061), "1d 01:01");
            assert_eq!(percent(5.26), "5.3");
        }
    }
}
