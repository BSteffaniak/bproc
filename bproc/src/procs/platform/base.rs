//! Portable sysinfo-backed sampler. Keeps one persistent `System` so CPU
//! deltas are meaningful, builds each `ProcessRecord` once per key and hands
//! every process to an optional platform `Enricher`.
use super::{Enricher, RawSample, RecordRef, Sampler};
use crate::procs::model::{
    ProcState, ProcessEntry, ProcessKey, ProcessRecord, ProcessSample, SystemSnapshot, TaskCounts,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::{
    Pid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, ThreadKind,
    UpdateKind, Users,
};

/// Warm-up gap so the first published CPU% is a real measurement.
const PRIMING_DELAY: Duration = Duration::from_millis(250);
/// Minimum gap between user database reloads triggered by unknown uids.
const USERS_RELOAD: Duration = Duration::from_secs(30);
/// On Linux, command line / exe / cwd are fetched in a second pass for new
/// processes only, so unreadable ones (kernel threads, other users) are not
/// retried every tick. Other platforms read them in the main pass because a
/// partial refresh would disturb sysinfo's CPU clock there.
const SPLIT_STATIC_REFRESH: bool = cfg!(target_os = "linux");

pub struct SysinfoSampler {
    system: System,
    users: Users,
    users_loaded: Instant,
    records: HashMap<ProcessKey, Arc<ProcessRecord>>,
    spare: HashMap<ProcessKey, Arc<ProcessRecord>>,
    new_pids: Vec<Pid>,
    enricher: Option<Box<dyn Enricher>>,
    host: Option<HostInfo>,
    last: Option<Instant>,
    boot_shift: i64,
}

/// System attributes that do not change while bproc runs.
struct HostInfo {
    hostname: String,
    os: String,
    boot_time: u64,
}

impl SysinfoSampler {
    pub fn new() -> Self {
        Self::with_enricher(None)
    }

    pub fn with_enricher(enricher: Option<Box<dyn Enricher>>) -> Self {
        // Created first so `boot_shift` reads btime right after sysinfo did.
        let system = System::new();
        Self {
            system,
            boot_shift: boot_shift(),
            users: Users::new_with_refreshed_list(),
            users_loaded: Instant::now(),
            records: HashMap::new(),
            spare: HashMap::new(),
            new_pids: Vec::new(),
            enricher,
            host: None,
            last: None,
        }
    }

    fn refresh_kind() -> ProcessRefreshKind {
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_disk_usage()
            .with_user(UpdateKind::OnlyIfNotSet);
        if SPLIT_STATIC_REFRESH {
            kind
        } else {
            kind.with_cmd(UpdateKind::OnlyIfNotSet)
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cwd(UpdateKind::OnlyIfNotSet)
        }
    }

    /// Second pass over processes seen for the first time (Linux only).
    fn refresh_new_statics(&mut self) {
        self.new_pids.clear();
        for (pid, process) in self.system.processes() {
            if process.thread_kind() != Some(ThreadKind::Userland)
                && !self
                    .records
                    .contains_key(&identity(*pid, process, self.boot_shift).key)
            {
                self.new_pids.push(*pid);
            }
        }
        if self.new_pids.is_empty() {
            return;
        }
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&self.new_pids),
            false,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_exe(UpdateKind::Always)
                .with_cwd(UpdateKind::Always),
        );
    }

    fn host(&mut self) -> &HostInfo {
        self.host.get_or_insert_with(|| HostInfo {
            hostname: System::host_name().unwrap_or_default(),
            os: System::long_os_version().unwrap_or_default(),
            boot_time: System::boot_time(),
        })
    }
}

impl Default for SysinfoSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// Name for `uid`, reloading the user database (rate limited) on a miss so
/// accounts created after startup still resolve.
fn resolve_user(users: &mut Users, loaded: &mut Instant, uid: Option<u32>) -> Option<String> {
    let uid = uid?;
    let find = |users: &Users| {
        users
            .iter()
            .find(|u| **u.id() == uid)
            .map(|u| u.name().to_string())
    };
    if let Some(name) = find(users) {
        return Some(name);
    }
    if loaded.elapsed() < USERS_RELOAD {
        return None;
    }
    users.refresh();
    *loaded = Instant::now();
    find(users)
}

/// Seconds to add to sysinfo start times so they are expressed against the
/// process-wide boot reference that `actions::current_start_time` also uses.
/// sysinfo bakes the `/proc/stat` btime it read in `System::new()` into every
/// start time; read right after construction this is normally 0.
#[cfg(target_os = "linux")]
fn boot_shift() -> i64 {
    super::linux::boot_reference() as i64 - System::boot_time() as i64
}

#[cfg(not(target_os = "linux"))]
fn boot_shift() -> i64 {
    0
}

/// Who a process is: its key plus the parent and owner, completed from an
/// unprivileged source where sysinfo could not read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    key: ProcessKey,
    ppid: Option<i32>,
    uid: Option<u32>,
    /// The start time did not come from sysinfo, so sysinfo may still hold
    /// the static fields (name, command line) of an earlier PID owner.
    fallback_start: bool,
}

fn identity(pid: Pid, process: &Process, boot_shift: i64) -> Identity {
    let raw_pid = pid.as_u32() as i32;
    let mut start = process.start_time();
    if start != 0 {
        start = start.saturating_add_signed(boot_shift);
    }
    #[allow(unused_mut)]
    let mut ppid = process.parent().map(|p| p.as_u32() as i32);
    #[allow(unused_mut)]
    let mut uid = process.user_id().map(|u| **u);
    #[allow(unused_mut)]
    let mut fallback_start = false;
    // Without root, PROC_PIDTBSDINFO is denied for other users' processes and
    // sysinfo reports start 0, no parent and no owner (~20% of processes).
    #[cfg(target_os = "macos")]
    {
        if start == 0
            && let Some(sysctl) = super::macos::start_time_sysctl(raw_pid)
        {
            start = sysctl;
            fallback_start = true;
        }
        if (ppid.is_none() || uid.is_none())
            && let Some(info) = super::macos::short_info(raw_pid)
        {
            ppid = ppid.or(super::macos::short_ppid(&info));
            uid = uid.or(Some(info.pbsi_ruid));
        }
    }
    Identity {
        key: ProcessKey::new(raw_pid, start),
        ppid,
        uid,
        fallback_start,
    }
}

pub(crate) fn map_status(status: ProcessStatus) -> ProcState {
    match status {
        ProcessStatus::Run => ProcState::Running,
        ProcessStatus::Sleep
        | ProcessStatus::Waking
        | ProcessStatus::Wakekill
        | ProcessStatus::Parked => ProcState::Sleeping,
        ProcessStatus::UninterruptibleDiskSleep => ProcState::DiskSleep,
        ProcessStatus::Stop => ProcState::Stopped,
        ProcessStatus::Tracing => ProcState::Tracing,
        ProcessStatus::Zombie => ProcState::Zombie,
        ProcessStatus::Idle => ProcState::Idle,
        ProcessStatus::Dead => ProcState::Dead,
        _ => ProcState::Unknown,
    }
}

/// Fold one process into the task summary (threads default to 1).
pub(crate) fn tally(tasks: &mut TaskCounts, sample: &ProcessSample) {
    tasks.total += 1;
    match sample.state {
        ProcState::Running => tasks.running += 1,
        ProcState::Stopped | ProcState::Tracing => tasks.stopped += 1,
        ProcState::Zombie => tasks.zombie += 1,
        _ => tasks.sleeping += 1,
    }
    tasks.threads += sample.threads.unwrap_or(1).max(1) as usize;
}

fn percent_of(part: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (part as f64 * 100.0 / total as f64) as f32
    }
}

fn base_sample(process: &Process, mem_total: u64, elapsed: f64) -> ProcessSample {
    let disk = process.disk_usage();
    ProcessSample {
        state: map_status(process.status()),
        cpu_percent: process.cpu_usage(),
        cpu_time_ms: process.accumulated_cpu_time(),
        mem_rss: process.memory(),
        mem_virtual: process.virtual_memory(),
        mem_percent: percent_of(process.memory(), mem_total),
        threads: process.tasks().map(|t| t.len() as u32),
        disk_read_rate: (disk.read_bytes as f64 / elapsed) as u64,
        disk_write_rate: (disk.written_bytes as f64 / elapsed) as u64,
        disk_read_total: disk.total_read_bytes,
        disk_write_total: disk.total_written_bytes,
        ..Default::default()
    }
}

fn base_record(id: &Identity, process: &Process) -> ProcessRecord {
    ProcessRecord {
        key: id.key,
        ppid: id.ppid,
        name: process.name().to_string_lossy().into_owned(),
        cmdline: process
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect(),
        exe: process.exe().map(|p| p.display().to_string()),
        cwd: process.cwd().map(|p| p.display().to_string()),
        uid: id.uid,
        kernel_thread: process.thread_kind() == Some(ThreadKind::Kernel),
        ..Default::default()
    }
}

/// sysinfo cannot tell two unreadable owners of one PID apart, so after a
/// reuse it still reports the previous owner's name and command line. Keep
/// only what is known to belong to the new process.
fn forget_stale_statics(record: &mut ProcessRecord) {
    #[cfg(target_os = "macos")]
    if let Some(info) = super::macos::short_info(record.key.pid) {
        record.name = super::macos::short_comm(&info);
    }
    record.cmdline.clear();
    record.exe = None;
    record.cwd = None;
}

impl Sampler for SysinfoSampler {
    fn sample(&mut self) -> Result<RawSample> {
        let now = Instant::now();
        let elapsed = self
            .last
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(1.0)
            .max(0.001);
        self.last = Some(now);
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, Self::refresh_kind());
        if SPLIT_STATIC_REFRESH {
            self.refresh_new_statics();
        }

        let mem_total = self.system.total_memory();
        let mut tasks = TaskCounts::default();
        let mut permission_limited = 0;
        let mut processes = Vec::with_capacity(self.system.processes().len());
        let mut next = std::mem::take(&mut self.spare);
        next.reserve(self.system.processes().len());
        for (pid, process) in self.system.processes() {
            if process.thread_kind() == Some(ThreadKind::Userland) {
                continue; // Threads are reported inside their process.
            }
            let id = identity(*pid, process, self.boot_shift);
            let (key, ppid) = (id.key, id.ppid);
            let mut sample = base_sample(process, mem_total, elapsed);
            let record = match self.records.remove(&key) {
                Some(record) => {
                    if let Some(enricher) = self.enricher.as_mut() {
                        permission_limited +=
                            enricher.enrich(RecordRef::Known(&record), &mut sample) as usize;
                    }
                    if record.ppid == ppid {
                        record
                    } else {
                        // Re-parented (e.g. orphan adopted by init/launchd).
                        Arc::new(ProcessRecord {
                            ppid,
                            ..(*record).clone()
                        })
                    }
                }
                None => {
                    let mut record = base_record(&id, process);
                    if id.fallback_start && self.records.keys().any(|k| k.pid == key.pid) {
                        forget_stale_statics(&mut record);
                    }
                    if let Some(enricher) = self.enricher.as_mut() {
                        permission_limited +=
                            enricher.enrich(RecordRef::New(&mut record), &mut sample) as usize;
                    }
                    record.user = resolve_user(&mut self.users, &mut self.users_loaded, record.uid);
                    Arc::new(record)
                }
            };
            tally(&mut tasks, &sample);
            next.insert(key, record.clone());
            processes.push(ProcessEntry { record, sample });
        }
        self.records.clear();
        self.spare = std::mem::replace(&mut self.records, next);

        if let Some(enricher) = self.enricher.as_mut() {
            let records = &self.records;
            enricher.retain(&|key| records.contains_key(key));
        }

        let cpus: Vec<f32> = self.system.cpus().iter().map(|c| c.cpu_usage()).collect();
        let cpu_total = self.system.global_cpu_usage();
        let mem_used = self.system.used_memory();
        let mem_available = self.system.available_memory();
        let swap_total = self.system.total_swap();
        let swap_used = self.system.used_swap();
        let load = System::load_average();
        let host = self.host();
        Ok(RawSample {
            system: SystemSnapshot {
                cpu_total,
                cpus,
                mem_total,
                mem_used,
                mem_available,
                swap_total,
                swap_used,
                load_average: [load.one, load.five, load.fifteen],
                uptime_secs: System::uptime(),
                boot_time: host.boot_time,
                tasks,
                hostname: host.hostname.clone(),
                os: host.os.clone(),
            },
            processes,
            permission_limited,
        })
    }

    fn priming_delay(&self) -> Option<Duration> {
        Some(PRIMING_DELAY)
    }
}
