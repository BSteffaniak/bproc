//! Linux enrichment from /proc (state, nice, priority, threads, fds, ctx
//! switches, major faults, tty, kernel threads, cgroup/container). Parsing
//! lives in `proc_text` so it is unit-tested on every host.
use super::proc_text;
use super::{Enricher, RecordRef};
use crate::procs::model::{ProcessKey, ProcessSample};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{ErrorKind, Read};
use std::sync::OnceLock;

pub struct ProcfsEnricher {
    path: String,
    stat: String,
    text: String,
    /// Keys whose fd directory is unreadable; not retried for their lifetime.
    fd_denied: HashSet<ProcessKey>,
}

impl ProcfsEnricher {
    pub fn new() -> Self {
        Self {
            path: String::with_capacity(32),
            stat: String::with_capacity(512),
            text: String::with_capacity(2048),
            fd_denied: HashSet::new(),
        }
    }
}

impl Default for ProcfsEnricher {
    fn default() -> Self {
        Self::new()
    }
}

/// Boot time (epoch seconds) every process start time in bproc is expressed
/// against. Captured once: `/proc/stat` `btime` is realtime minus boot time,
/// so it moves whenever the wall clock is stepped, and mixing two readings
/// would make every live process look like a reused PID.
static BOOT_REFERENCE: OnceLock<u64> = OnceLock::new();

pub fn boot_reference() -> u64 {
    *BOOT_REFERENCE.get_or_init(sysinfo::System::boot_time)
}

/// Kernel clock ticks per second (`USER_HZ`), the unit of stat field 22.
fn clock_ticks() -> u64 {
    static TICKS: OnceLock<u64> = OnceLock::new();
    // SAFETY: sysconf has no preconditions.
    *TICKS.get_or_init(|| (unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).max(1) as u64)
}

/// Start time from boot-relative clock ticks, on the same formula as sysinfo
/// (`ticks / HZ + btime`) but against [`boot_reference`].
pub fn start_time_from_ticks(ticks: u64) -> u64 {
    boot_reference().saturating_add(ticks / clock_ticks())
}

/// Start time of `pid` from `/proc/<pid>/stat`, or `None` if unreadable.
pub fn start_time_procfs(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let stat = proc_text::parse_stat(&text)?;
    Some(start_time_from_ticks(stat.start_ticks))
}

fn set_path(path: &mut String, pid: i32, file: &str) {
    path.clear();
    let _ = write!(path, "/proc/{pid}/{file}");
}

/// Read a whole file into `buf`, reusing its allocation.
fn read_into(path: &str, buf: &mut String) -> bool {
    buf.clear();
    std::fs::File::open(path)
        .and_then(|mut f| f.read_to_string(buf))
        .is_ok()
}

impl Enricher for ProcfsEnricher {
    fn enrich(&mut self, mut record: RecordRef<'_>, sample: &mut ProcessSample) -> bool {
        let key = record.get().key;
        let pid = key.pid;
        set_path(&mut self.path, pid, "stat");
        if read_into(&self.path, &mut self.stat)
            && let Some(stat) = proc_text::parse_stat(&self.stat)
        {
            sample.state = proc_text::map_state(stat.state);
            sample.priority = Some(stat.priority as i32);
            sample.nice = Some(stat.nice as i32);
            sample.threads = Some(stat.num_threads.max(1));
            sample.major_faults = Some(stat.major_faults);
            if let Some(new) = record.new_mut() {
                new.kernel_thread |= stat.is_kernel_thread(pid);
                new.tty = proc_text::tty_name(stat.tty_nr);
                set_path(&mut self.path, pid, "cgroup");
                if read_into(&self.path, &mut self.text) {
                    new.group =
                        proc_text::cgroup_path(&self.text).and_then(proc_text::cgroup_label);
                }
            }
        }
        set_path(&mut self.path, pid, "status");
        if read_into(&self.path, &mut self.text) {
            sample.ctx_switches = proc_text::parse_ctx_switches(&self.text);
        }
        if record.get().kernel_thread {
            sample.open_fds = Some(0);
            return false;
        }
        if self.fd_denied.contains(&key) {
            return true;
        }
        set_path(&mut self.path, pid, "fd");
        match std::fs::read_dir(&self.path) {
            Ok(dir) => {
                sample.open_fds = Some(dir.count().min(u32::MAX as usize) as u32);
                false
            }
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                self.fd_denied.insert(key);
                true
            }
            Err(_) => false,
        }
    }

    fn retain(&mut self, live: &dyn Fn(&ProcessKey) -> bool) {
        self.fd_denied.retain(|key| live(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procfs_start_time_matches_sysinfo() {
        let me = sysinfo::get_current_pid().expect("pid");
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[me]), true);
        let expected = system.process(me).expect("self").start_time();
        let start = start_time_procfs(me.as_u32() as i32).expect("own stat");
        // Equal unless the wall clock was stepped while the test ran.
        assert!(start.abs_diff(expected) <= 1, "{start} vs {expected}");
        assert_eq!(start_time_procfs(i32::MAX - 7), None);
        assert_eq!(start_time_procfs(0), None);
    }

    #[test]
    fn start_time_uses_fixed_boot_reference() {
        let boot = boot_reference();
        assert_eq!(start_time_from_ticks(0), boot);
        assert_eq!(start_time_from_ticks(clock_ticks() * 10), boot + 10);
        assert_eq!(boot_reference(), boot);
    }
}
