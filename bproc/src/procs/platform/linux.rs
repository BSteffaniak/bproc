//! Linux enrichment from /proc (state, nice, priority, threads, fds, ctx
//! switches, major faults, tty, kernel threads, cgroup/container). Parsing
//! lives in `proc_text` so it is unit-tested on every host.
use super::proc_text;
use super::{Enricher, RecordRef};
use crate::procs::model::{ProcessKey, ProcessSample};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{ErrorKind, Read};

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
