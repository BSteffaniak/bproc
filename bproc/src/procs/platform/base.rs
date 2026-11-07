//! Portable sysinfo-backed sampler. Keeps one persistent `System` so CPU
//! deltas are meaningful and static attributes are read only on first sight.
use super::{RawSample, Sampler};
use crate::procs::model::{
    ProcState, ProcessEntry, ProcessKey, ProcessRecord, ProcessSample, SystemSnapshot, TaskCounts,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind, Users};

pub struct SysinfoSampler {
    system: System,
    users: Users,
    records: HashMap<ProcessKey, Arc<ProcessRecord>>,
    last: Option<std::time::Instant>,
}

impl SysinfoSampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            users: Users::new_with_refreshed_list(),
            records: HashMap::new(),
            last: None,
        }
    }
}

impl Default for SysinfoSampler {
    fn default() -> Self {
        Self::new()
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

impl Sampler for SysinfoSampler {
    fn sample(&mut self) -> Result<RawSample> {
        let now = std::time::Instant::now();
        let elapsed = self
            .last
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(1.0)
            .max(0.001);
        self.last = Some(now);
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_disk_usage()
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cwd(UpdateKind::OnlyIfNotSet)
                .with_tasks(),
        );
        let mem_total = self.system.total_memory();
        let mut tasks = TaskCounts::default();
        let mut processes = Vec::with_capacity(self.system.processes().len());
        let mut seen = HashMap::with_capacity(self.records.len());
        for (pid, process) in self.system.processes() {
            if process.thread_kind().is_some() {
                continue; // Linux userland threads are reported inside their process.
            }
            let key = ProcessKey::new(pid.as_u32() as i32, process.start_time());
            let record = self.records.get(&key).cloned().unwrap_or_else(|| {
                let uid = process.user_id().map(|u| **u);
                Arc::new(ProcessRecord {
                    key,
                    ppid: process.parent().map(|p| p.as_u32() as i32),
                    name: process.name().to_string_lossy().into_owned(),
                    cmdline: process
                        .cmd()
                        .iter()
                        .map(|s| s.to_string_lossy().into_owned())
                        .collect(),
                    exe: process.exe().map(|p| p.display().to_string()),
                    cwd: process.cwd().map(|p| p.display().to_string()),
                    uid,
                    user: process
                        .user_id()
                        .and_then(|id| self.users.get_user_by_id(id))
                        .map(|u| u.name().to_string()),
                    ..Default::default()
                })
            });
            seen.insert(key, record.clone());
            let disk = process.disk_usage();
            let state = map_status(process.status());
            tasks.total += 1;
            match state {
                ProcState::Running => tasks.running += 1,
                ProcState::Stopped | ProcState::Tracing => tasks.stopped += 1,
                ProcState::Zombie => tasks.zombie += 1,
                _ => tasks.sleeping += 1,
            }
            let threads = process.tasks().map(|t| t.len() as u32);
            tasks.threads += threads.unwrap_or(1) as usize;
            processes.push(ProcessEntry {
                record,
                sample: ProcessSample {
                    state,
                    cpu_percent: process.cpu_usage(),
                    cpu_time_ms: process.accumulated_cpu_time(),
                    mem_rss: process.memory(),
                    mem_virtual: process.virtual_memory(),
                    mem_percent: if mem_total > 0 {
                        (process.memory() as f64 * 100.0 / mem_total as f64) as f32
                    } else {
                        0.0
                    },
                    threads,
                    disk_read_rate: (disk.read_bytes as f64 / elapsed) as u64,
                    disk_write_rate: (disk.written_bytes as f64 / elapsed) as u64,
                    disk_read_total: disk.total_read_bytes,
                    disk_write_total: disk.total_written_bytes,
                    ..Default::default()
                },
            });
        }
        self.records = seen;
        let cpus: Vec<f32> = self.system.cpus().iter().map(|c| c.cpu_usage()).collect();
        let load = System::load_average();
        Ok(RawSample {
            system: SystemSnapshot {
                cpu_total: self.system.global_cpu_usage(),
                cpus,
                mem_total,
                mem_used: self.system.used_memory(),
                mem_available: self.system.available_memory(),
                swap_total: self.system.total_swap(),
                swap_used: self.system.used_swap(),
                load_average: [load.one, load.five, load.fifteen],
                uptime_secs: System::uptime(),
                boot_time: System::boot_time(),
                tasks,
                hostname: System::host_name().unwrap_or_default(),
                os: System::long_os_version().unwrap_or_default(),
            },
            processes,
            permission_limited: 0,
        })
    }
}
