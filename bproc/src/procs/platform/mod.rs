//! Platform samplers. `SysinfoSampler` is the portable base; platform modules
//! enrich samples with fields sysinfo does not expose (nice, priority, threads,
//! fds, context switches, kernel threads, tty, cgroup/container).
use super::model::{ProcessEntry, ProcessKey, ProcessRecord, ProcessSample, SystemSnapshot};
use anyhow::Result;
use std::time::Duration;

mod base;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(any(target_os = "linux", test))]
pub mod proc_text;

pub use base::SysinfoSampler;

/// Raw measurements for one tick. Rates are computed by the sampler from the
/// previous tick it observed; the collector only orchestrates timing.
pub struct RawSample {
    pub system: SystemSnapshot,
    pub processes: Vec<ProcessEntry>,
    pub permission_limited: usize,
}

pub trait Sampler: Send {
    fn sample(&mut self) -> Result<RawSample>;

    /// Delay between a discarded warm-up sample and the first published one,
    /// for samplers whose rates need two observations (CPU%, disk rates).
    fn priming_delay(&self) -> Option<Duration> {
        None
    }
}

/// The record of the process being enriched.
pub enum RecordRef<'a> {
    /// First sighting: per-process attributes (tty, group, kernel thread) are
    /// filled in once, before the record is shared.
    New(&'a mut ProcessRecord),
    Known(&'a ProcessRecord),
}

impl RecordRef<'_> {
    pub fn get(&self) -> &ProcessRecord {
        match self {
            Self::New(record) => record,
            Self::Known(record) => record,
        }
    }

    pub fn new_mut(&mut self) -> Option<&mut ProcessRecord> {
        match self {
            Self::New(record) => Some(record),
            Self::Known(_) => None,
        }
    }
}

/// Per-platform enrichment applied to every process of a base sample.
pub trait Enricher: Send {
    /// Fill platform-only fields of `sample` (and of a new record). Returns
    /// true when permissions hid some of the details.
    fn enrich(&mut self, record: RecordRef<'_>, sample: &mut ProcessSample) -> bool;

    /// Drop per-process state for keys that are no longer alive.
    fn retain(&mut self, _live: &dyn Fn(&ProcessKey) -> bool) {}
}

/// Pick the best sampler for this host: sysinfo plus native enrichment where
/// available (libproc on macOS, /proc on Linux).
pub fn create_sampler() -> Box<dyn Sampler> {
    Box::new(SysinfoSampler::with_enricher(create_enricher()))
}

#[cfg(target_os = "macos")]
fn create_enricher() -> Option<Box<dyn Enricher>> {
    Some(Box::new(macos::LibprocEnricher::new()))
}

#[cfg(target_os = "linux")]
fn create_enricher() -> Option<Box<dyn Enricher>> {
    Some(Box::new(linux::ProcfsEnricher::new()))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn create_enricher() -> Option<Box<dyn Enricher>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::model::ProcState;
    use std::time::Instant;

    #[test]
    fn real_sampler_reports_this_process() {
        let mut sampler = create_sampler();
        let raw = sampler.sample().expect("sample");
        let me = std::process::id() as i32;
        let entry = raw
            .processes
            .iter()
            .find(|e| e.key().pid == me)
            .expect("own process listed");
        assert!(entry.key().start_time > 0);
        assert!(entry.sample.mem_rss > 0);
        assert!(!entry.record.name.is_empty());
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(entry.sample.threads.is_some_and(|t| t >= 1));
            assert!(entry.sample.open_fds.is_some_and(|n| n >= 1));
            assert!(entry.sample.nice.is_some());
            assert!(entry.sample.priority.is_some());
            assert!(entry.sample.ctx_switches.is_some());
            assert_ne!(entry.sample.state, ProcState::Unknown);
        }
        let tasks = &raw.system.tasks;
        assert_eq!(tasks.total, raw.processes.len());
        assert!(tasks.threads >= tasks.total);
        assert!(raw.system.mem_total > 0);
        assert!(raw.system.boot_time > 0);
        assert!(!raw.system.cpus.is_empty());
    }

    #[test]
    fn keys_match_sysinfo_start_time() {
        let mut sampler = create_sampler();
        let raw = sampler.sample().expect("sample");
        let me = sysinfo::get_current_pid().expect("pid");
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[me]), true);
        let start = system.process(me).expect("self").start_time();
        let entry = raw
            .processes
            .iter()
            .find(|e| e.key().pid == me.as_u32() as i32)
            .expect("own process listed");
        assert_eq!(entry.key().start_time, start);
    }

    #[test]
    fn keys_match_action_verification() {
        let mut sampler = create_sampler();
        let raw = sampler.sample().expect("sample");
        let me = std::process::id() as i32;
        let entry = raw
            .processes
            .iter()
            .find(|e| e.key().pid == me)
            .expect("own process listed");
        assert_eq!(crate::procs::actions::verify_identity(entry.key()), Ok(()));
        if cfg!(target_os = "macos") {
            let launchd = raw
                .processes
                .iter()
                .find(|e| e.key().pid == 1)
                .expect("launchd listed");
            assert_eq!(
                crate::procs::actions::verify_identity(launchd.key()),
                Ok(())
            );
        }
    }

    /// Without root, sysinfo cannot read other users' processes on macOS;
    /// the sampler must still give them a start time, parent and owner.
    #[cfg(target_os = "macos")]
    #[test]
    fn other_users_processes_have_identity() {
        let mut sampler = create_sampler();
        let raw = sampler.sample().expect("sample");
        let launchd = raw
            .processes
            .iter()
            .find(|e| e.key().pid == 1)
            .expect("launchd listed");
        assert!(launchd.key().start_time > 0);
        assert_eq!(launchd.record.uid, Some(0));
        assert_eq!(launchd.record.user.as_deref(), Some("root"));
        assert_eq!(launchd.record.ppid, None);
        // Every root daemon launched by launchd has a parent and an owner.
        let daemons: Vec<_> = raw
            .processes
            .iter()
            .filter(|e| e.key().pid > 1 && e.record.uid == Some(0))
            .collect();
        assert!(!daemons.is_empty());
        assert!(daemons.iter().all(|e| e.key().start_time > 0));
        assert!(daemons.iter().any(|e| e.record.ppid == Some(1)));
        let orphans = raw
            .processes
            .iter()
            .filter(|e| e.key().pid > 1 && e.record.ppid.is_none())
            .count();
        // Only launchd/kernel_task are roots (allow a process exiting mid-tick).
        assert!(orphans <= 2, "{orphans} processes without a parent");

        // A second tick reuses the records instead of rebuilding them.
        let again = sampler.sample().expect("sample");
        let launchd2 = again
            .processes
            .iter()
            .find(|e| e.key().pid == 1)
            .expect("launchd listed");
        assert!(std::sync::Arc::ptr_eq(&launchd.record, &launchd2.record));
    }

    #[test]
    #[ignore = "measures the real system; run with --ignored --nocapture"]
    fn sample_timings() {
        let started = Instant::now();
        let mut sampler = create_sampler();
        let created = started.elapsed();
        let report = |label: &str, sampler: &mut Box<dyn Sampler>| {
            let t = Instant::now();
            let raw = sampler.sample().expect("sample");
            let took = t.elapsed();
            let busy = raw
                .processes
                .iter()
                .filter(|e| e.sample.cpu_percent > 0.0)
                .count();
            let with_fds = raw
                .processes
                .iter()
                .filter(|e| e.sample.open_fds.is_some())
                .count();
            let with_tty = raw
                .processes
                .iter()
                .filter(|e| e.record.tty.is_some())
                .count();
            let with_group = raw
                .processes
                .iter()
                .filter(|e| e.record.group.is_some())
                .count();
            let kthreads = raw
                .processes
                .iter()
                .filter(|e| e.record.kernel_thread)
                .count();
            println!(
                "{label}: {took:?} procs={} threads={} running={} cpu>0={busy} fds={with_fds} \
                 tty={with_tty} group={with_group} kthreads={kthreads} limited={}",
                raw.processes.len(),
                raw.system.tasks.threads,
                raw.system.tasks.running,
                raw.permission_limited,
            );
            took
        };
        println!("create: {created:?}");
        report("cold", &mut sampler);
        std::thread::sleep(Duration::from_millis(250));
        let mut warm = Vec::new();
        for i in 0..5 {
            warm.push(report(&format!("warm{i}"), &mut sampler));
            std::thread::sleep(Duration::from_millis(100));
        }
        warm.sort();
        println!("warm median: {:?}", warm[warm.len() / 2]);

        let mut base: Box<dyn Sampler> = Box::new(SysinfoSampler::new());
        report("base cold", &mut base);
        std::thread::sleep(Duration::from_millis(250));
        let mut warm: Vec<_> = (0..5)
            .map(|i| report(&format!("base warm{i}"), &mut base))
            .collect();
        warm.sort();
        println!("base warm median: {:?}", warm[warm.len() / 2]);
    }
}
