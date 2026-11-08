//! Process management actions. Every action re-verifies the target identity
//! (PID + start time) immediately before acting so a reused PID is never hit.
use super::model::ProcessKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Signal {
    Term,
    Kill,
    Hup,
    Int,
    Quit,
    Stop,
    Cont,
    Usr1,
    Usr2,
}

impl Signal {
    pub const ALL: [Signal; 9] = [
        Signal::Term,
        Signal::Kill,
        Signal::Hup,
        Signal::Int,
        Signal::Quit,
        Signal::Stop,
        Signal::Cont,
        Signal::Usr1,
        Signal::Usr2,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
            Self::Hup => "SIGHUP",
            Self::Int => "SIGINT",
            Self::Quit => "SIGQUIT",
            Self::Stop => "SIGSTOP",
            Self::Cont => "SIGCONT",
            Self::Usr1 => "SIGUSR1",
            Self::Usr2 => "SIGUSR2",
        }
    }

    /// Friendly verb for buttons and menus.
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Term => "Terminate",
            Self::Kill => "Force kill",
            Self::Hup => "Hang up / reload",
            Self::Int => "Interrupt",
            Self::Quit => "Quit (core dump)",
            Self::Stop => "Suspend",
            Self::Cont => "Resume",
            Self::Usr1 => "User signal 1",
            Self::Usr2 => "User signal 2",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Term => "Ask the process to exit cleanly (default)",
            Self::Kill => "Stop immediately; cannot be caught or ignored",
            Self::Hup => "Terminal hang-up; many daemons reload config",
            Self::Int => "Same as pressing Ctrl+C in its terminal",
            Self::Quit => "Exit and write a core dump",
            Self::Stop => "Pause execution; resume with SIGCONT",
            Self::Cont => "Resume a stopped process",
            Self::Usr1 => "Application-defined",
            Self::Usr2 => "Application-defined",
        }
    }

    #[cfg(unix)]
    pub const fn number(self) -> i32 {
        match self {
            Self::Term => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
            Self::Hup => libc::SIGHUP,
            Self::Int => libc::SIGINT,
            Self::Quit => libc::SIGQUIT,
            Self::Stop => libc::SIGSTOP,
            Self::Cont => libc::SIGCONT,
            Self::Usr1 => libc::SIGUSR1,
            Self::Usr2 => libc::SIGUSR2,
        }
    }

    /// Signals that end the process (used for escalation prompts).
    pub const fn is_terminating(self) -> bool {
        matches!(
            self,
            Self::Term | Self::Kill | Self::Int | Self::Quit | Self::Hup
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionRequest {
    Signal {
        targets: Vec<ProcessKey>,
        signal: Signal,
    },
    Renice {
        targets: Vec<ProcessKey>,
        nice: i32,
    },
    /// Linux only: restrict to these logical CPUs.
    Affinity {
        targets: Vec<ProcessKey>,
        cpus: Vec<usize>,
    },
}

impl ActionRequest {
    pub fn targets(&self) -> &[ProcessKey] {
        match self {
            Self::Signal { targets, .. }
            | Self::Renice { targets, .. }
            | Self::Affinity { targets, .. } => targets,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Signal { signal, .. } => signal.name().to_string(),
            Self::Renice { nice, .. } => format!("renice {nice}"),
            Self::Affinity { cpus, .. } => format!("affinity {cpus:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    NotFound,
    PidReused,
    PermissionDenied,
    Unsupported(String),
    Failed(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "process no longer exists"),
            Self::PidReused => write!(f, "PID now belongs to a different process; skipped"),
            Self::PermissionDenied => write!(f, "permission denied (try running bproc with sudo)"),
            Self::Unsupported(what) => write!(f, "not supported on this platform: {what}"),
            Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionOutcome {
    pub key: ProcessKey,
    pub name: String,
    pub result: Result<(), ActionError>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionReport {
    pub request: ActionRequest,
    pub outcomes: Vec<ActionOutcome>,
}

impl ActionReport {
    pub fn succeeded(&self) -> usize {
        self.outcomes.iter().filter(|o| o.result.is_ok()).count()
    }
    pub fn failed(&self) -> impl Iterator<Item = &ActionOutcome> {
        self.outcomes.iter().filter(|o| o.result.is_err())
    }
    /// One-line human summary for toasts and the event log.
    pub fn summary(&self) -> String {
        let ok = self.succeeded();
        let total = self.outcomes.len();
        match (total, self.outcomes.first()) {
            (1, Some(o)) => match &o.result {
                Ok(()) => format!("{} → {} ({})", self.request.label(), o.name, o.key.pid),
                Err(e) => format!(
                    "{} {} ({}) failed: {e}",
                    self.request.label(),
                    o.name,
                    o.key.pid
                ),
            },
            _ => format!("{} sent to {ok}/{total} processes", self.request.label()),
        }
    }
}

/// Current start time for `pid` using the same clock as the collector, or
/// `None` if no such process exists.
///
/// macOS reads `sysctl(KERN_PROC_PID)`, which works for other users'
/// processes without root (sysinfo reports 0 for those). Linux reads
/// `/proc/<pid>/stat` against the collector's fixed boot reference so a wall
/// clock step does not make every process look reused. Both fall back to
/// sysinfo when the native source fails.
pub fn current_start_time(pid: i32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    if let Some(start) = super::platform::macos::start_time_sysctl(pid) {
        return Some(start);
    }
    #[cfg(target_os = "linux")]
    if let Some(start) = super::platform::linux::start_time_procfs(pid) {
        return Some(start);
    }
    sysinfo_start_time(pid)
}

fn sysinfo_start_time(pid: i32) -> Option<u64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    if pid < 0 {
        return None;
    }
    let mut system = System::new();
    let pid = Pid::from_u32(pid as u32);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).map(|p| p.start_time())
}

/// Executable path of a live process, if readable.
pub fn exe_path(pid: i32) -> Option<String> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    let pid = Pid::from_u32(pid as u32);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    system.process(pid)?.exe().map(|p| p.display().to_string())
}

/// Check that `key.pid` still belongs to the process `key` identifies. A
/// start time of 0 (unknown) is not a wildcard: it matches only a process
/// whose start time still reads as unknown, never a readable newcomer.
pub fn verify_identity(key: ProcessKey) -> Result<(), ActionError> {
    check_identity(key, current_start_time(key.pid))
}

fn check_identity(key: ProcessKey, current: Option<u64>) -> Result<(), ActionError> {
    match current {
        None => Err(ActionError::NotFound),
        Some(start) if start != key.start_time => Err(ActionError::PidReused),
        Some(_) => Ok(()),
    }
}

/// Whether `pid` is this bproc process. bproc never signals itself on
/// purpose: doing so mid-list stops the remaining targets and can leave the
/// terminal in raw mode.
pub fn is_self(pid: i32) -> bool {
    i64::from(pid) == i64::from(std::process::id())
}

/// Processes whose tree is (almost) everything: the kernel swapper (PID 0),
/// init / launchd (PID 1) and, on Linux, kthreadd (PID 2). Tree-wide
/// actions refuse them.
pub fn is_system_root(pid: i32) -> bool {
    pid <= 1 || (cfg!(target_os = "linux") && pid == 2)
}

/// `keys` without bproc itself; the flag says whether it was present.
pub fn exclude_self(keys: Vec<ProcessKey>) -> (Vec<ProcessKey>, bool) {
    let before = keys.len();
    let kept: Vec<ProcessKey> = keys.into_iter().filter(|k| !is_self(k.pid)).collect();
    let removed = kept.len() != before;
    (kept, removed)
}

/// Execute a request (blocking). `names` supplies display names for reports.
///
/// A signal reaches bproc itself (if it is a target at all) only after
/// every other target, so stopping or terminating bproc can never leave the
/// rest of the list unsignalled.
pub fn execute(request: &ActionRequest, names: &dyn Fn(&ProcessKey) -> String) -> ActionReport {
    let mut order: Vec<&ProcessKey> = request.targets().iter().collect();
    if matches!(request, ActionRequest::Signal { .. }) {
        // Stable: the other targets keep their order.
        order.sort_by_key(|k| is_self(k.pid));
    }
    let outcomes = order
        .into_iter()
        .map(|key| ActionOutcome {
            key: *key,
            name: names(key),
            result: verify_identity(*key).and_then(|()| apply(request, *key)),
        })
        .collect();
    ActionReport {
        request: request.clone(),
        outcomes,
    }
}

/// Valid nice values (lower = higher priority).
pub const NICE_RANGE: std::ops::RangeInclusive<i32> = -20..=19;

/// Whether `ActionRequest::Affinity` can succeed on this platform.
pub const fn affinity_supported() -> bool {
    cfg!(target_os = "linux")
}

/// Logical CPUs `pid` may currently run on (Linux only).
pub fn current_affinity(pid: i32) -> Result<Vec<usize>, ActionError> {
    #[cfg(target_os = "linux")]
    {
        linux::get_affinity(pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Err(ActionError::Unsupported("CPU affinity".into()))
    }
}

#[cfg(unix)]
fn errno_error() -> ActionError {
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ESRCH) => ActionError::NotFound,
        Some(libc::EPERM) | Some(libc::EACCES) => ActionError::PermissionDenied,
        _ => ActionError::Failed(err.to_string()),
    }
}

fn check_nice(nice: i32) -> Result<(), ActionError> {
    if NICE_RANGE.contains(&nice) {
        Ok(())
    } else {
        Err(ActionError::Failed(format!(
            "nice {nice} is outside {}..={}",
            NICE_RANGE.start(),
            NICE_RANGE.end()
        )))
    }
}

#[cfg(unix)]
fn renice_one(id: i32, nice: i32) -> Result<(), ActionError> {
    // setpriority returns 0 or -1 (unlike getpriority); errno is only read on -1.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, id as libc::id_t, nice) } == 0 {
        Ok(())
    } else {
        Err(errno_error())
    }
}

#[cfg(unix)]
fn apply(request: &ActionRequest, key: ProcessKey) -> Result<(), ActionError> {
    if key.pid <= 0 {
        return Err(ActionError::Failed(format!(
            "refusing to target PID {}",
            key.pid
        )));
    }
    match request {
        ActionRequest::Signal { signal, .. } => {
            if unsafe { libc::kill(key.pid, signal.number()) } == 0 {
                Ok(())
            } else {
                Err(errno_error())
            }
        }
        ActionRequest::Renice { nice, .. } => {
            check_nice(*nice)?;
            renice_one(key.pid, *nice)?;
            // Linux applies nice per thread; follow htop and renice every task.
            #[cfg(target_os = "linux")]
            for tid in linux::tasks(key.pid) {
                if tid != key.pid {
                    let _ = renice_one(tid, *nice);
                }
            }
            Ok(())
        }
        ActionRequest::Affinity { cpus, .. } => {
            #[cfg(target_os = "linux")]
            {
                linux::set_affinity(key.pid, cpus)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = cpus;
                Err(ActionError::Unsupported("CPU affinity".into()))
            }
        }
    }
}

#[cfg(not(unix))]
fn apply(request: &ActionRequest, key: ProcessKey) -> Result<(), ActionError> {
    if let ActionRequest::Renice { nice, .. } = request {
        check_nice(*nice)?;
    }
    apply_sysinfo(request, key)
}

/// Portable fallback (Windows): terminate through sysinfo; everything else is
/// unsupported.
#[cfg(any(not(unix), test))]
fn apply_sysinfo(request: &ActionRequest, key: ProcessKey) -> Result<(), ActionError> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let signal = match request {
        ActionRequest::Signal { signal, .. } => *signal,
        ActionRequest::Renice { .. } => return Err(ActionError::Unsupported("renice".into())),
        ActionRequest::Affinity { .. } => {
            return Err(ActionError::Unsupported("CPU affinity".into()));
        }
    };
    if !matches!(signal, Signal::Term | Signal::Kill) {
        return Err(ActionError::Unsupported(signal.name().into()));
    }
    let mut system = System::new();
    let pid = Pid::from_u32(key.pid.max(0) as u32);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let process = system.process(pid).ok_or(ActionError::NotFound)?;
    let sent = match signal {
        Signal::Kill => Some(process.kill()),
        _ => process.kill_with(sysinfo::Signal::Term),
    };
    match sent {
        Some(true) => Ok(()),
        Some(false) => Err(ActionError::Failed(format!(
            "{} could not be delivered (access denied or already exiting)",
            signal.name()
        ))),
        None => Err(ActionError::Unsupported(format!(
            "{} (use {} instead)",
            signal.name(),
            Signal::Kill.verb()
        ))),
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{ActionError, errno_error};

    /// Thread ids of `pid` (empty when unreadable).
    pub fn tasks(pid: i32) -> Vec<i32> {
        std::fs::read_dir(format!("/proc/{pid}/task"))
            .map(|dir| {
                dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    const MAX_CPUS: usize = libc::CPU_SETSIZE as usize;

    fn set_one(tid: i32, set: &libc::cpu_set_t) -> Result<(), ActionError> {
        let size = std::mem::size_of::<libc::cpu_set_t>();
        if unsafe { libc::sched_setaffinity(tid, size, set) } == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINVAL) {
            Err(ActionError::Failed(
                "none of the selected CPUs are online or permitted".into(),
            ))
        } else {
            Err(errno_error_from(err))
        }
    }

    fn errno_error_from(err: std::io::Error) -> ActionError {
        match err.raw_os_error() {
            Some(libc::ESRCH) => ActionError::NotFound,
            Some(libc::EPERM) | Some(libc::EACCES) => ActionError::PermissionDenied,
            _ => ActionError::Failed(err.to_string()),
        }
    }

    pub fn set_affinity(pid: i32, cpus: &[usize]) -> Result<(), ActionError> {
        if cpus.is_empty() {
            return Err(ActionError::Failed("select at least one CPU".into()));
        }
        if let Some(bad) = cpus.iter().find(|&&c| c >= MAX_CPUS) {
            return Err(ActionError::Failed(format!("CPU {bad} is out of range")));
        }
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for &cpu in cpus {
            unsafe { libc::CPU_SET(cpu, &mut set) };
        }
        set_one(pid, &set)?;
        for tid in tasks(pid) {
            if tid != pid {
                let _ = set_one(tid, &set);
            }
        }
        Ok(())
    }

    pub fn get_affinity(pid: i32) -> Result<Vec<usize>, ActionError> {
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::cpu_set_t>();
        if unsafe { libc::sched_getaffinity(pid, size, &mut set) } != 0 {
            return Err(errno_error());
        }
        Ok((0..MAX_CPUS)
            .filter(|&c| unsafe { libc::CPU_ISSET(c, &set) })
            .collect())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    struct Sleeper(Child);

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn sleeper() -> (Sleeper, ProcessKey) {
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        let start = current_start_time(pid).expect("child visible");
        (Sleeper(child), ProcessKey::new(pid, start))
    }

    fn send(key: ProcessKey, signal: Signal) -> ActionReport {
        execute(
            &ActionRequest::Signal {
                targets: vec![key],
                signal,
            },
            &|_| "sleep".into(),
        )
    }

    fn ps_state(pid: i32) -> String {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn is_root() -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    #[test]
    fn stop_cont_term_child() {
        let (mut child, key) = sleeper();
        let report = send(key, Signal::Stop);
        assert_eq!(report.succeeded(), 1, "{}", report.summary());
        assert!(wait_for(|| ps_state(key.pid).starts_with('T')));
        assert!(child.0.try_wait().unwrap().is_none());

        let report = send(key, Signal::Cont);
        assert_eq!(report.succeeded(), 1);
        assert!(wait_for(|| !ps_state(key.pid).starts_with('T')));
        assert!(child.0.try_wait().unwrap().is_none());

        let report = send(key, Signal::Term);
        assert_eq!(report.succeeded(), 1);
        assert!(report.summary().starts_with("SIGTERM → sleep"));
        let mut status = None;
        assert!(wait_for(|| {
            status = child.0.try_wait().unwrap();
            status.is_some()
        }));
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.unwrap().signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn wrong_start_time_is_pid_reused() {
        let (mut child, key) = sleeper();
        let stale = ProcessKey::new(key.pid, key.start_time.saturating_sub(1000));
        let report = send(stale, Signal::Kill);
        assert_eq!(report.outcomes[0].result, Err(ActionError::PidReused));
        assert!(report.summary().contains("different process"));
        assert!(child.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn unknown_start_time_is_not_a_wildcard() {
        // A row keyed with start 0 (unreadable owner) must not match a live
        // process whose start time is readable: that is a reused PID.
        let (mut child, key) = sleeper();
        assert!(key.start_time > 0);
        let unknown = ProcessKey::new(key.pid, 0);
        assert_eq!(verify_identity(unknown), Err(ActionError::PidReused));
        let report = send(unknown, Signal::Kill);
        assert_eq!(report.outcomes[0].result, Err(ActionError::PidReused));
        assert!(child.0.try_wait().unwrap().is_none());
        assert_eq!(verify_identity(key), Ok(()));
    }

    #[test]
    fn check_identity_compares_exactly() {
        let key = |start| ProcessKey::new(42, start);
        assert_eq!(check_identity(key(0), Some(0)), Ok(()));
        assert_eq!(check_identity(key(0), Some(7)), Err(ActionError::PidReused));
        assert_eq!(check_identity(key(7), Some(0)), Err(ActionError::PidReused));
        assert_eq!(check_identity(key(7), Some(7)), Ok(()));
        assert_eq!(check_identity(key(7), None), Err(ActionError::NotFound));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn other_users_processes_have_a_start_time() {
        // launchd is unreadable through libproc without root; the sysctl
        // clock still identifies it, so it never needs a 0 wildcard key.
        let start = current_start_time(1).expect("launchd");
        assert!(start > 0);
        assert_eq!(verify_identity(ProcessKey::new(1, start)), Ok(()));
        assert_eq!(
            verify_identity(ProcessKey::new(1, start - 1)),
            Err(ActionError::PidReused)
        );
    }

    #[test]
    fn exited_process_is_not_found() {
        let (mut child, key) = sleeper();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert_eq!(verify_identity(key), Err(ActionError::NotFound));
        let report = send(key, Signal::Term);
        assert_eq!(report.outcomes[0].result, Err(ActionError::NotFound));
    }

    #[test]
    fn renice_child() {
        let (_child, key) = sleeper();
        let renice = |nice| {
            execute(
                &ActionRequest::Renice {
                    targets: vec![key],
                    nice,
                },
                &|_| "sleep".into(),
            )
        };
        let report = renice(5);
        assert_eq!(report.succeeded(), 1, "{}", report.summary());
        assert_eq!(
            unsafe { libc::getpriority(libc::PRIO_PROCESS, key.pid as libc::id_t) },
            5
        );
        let report = renice(-5);
        if !is_root() {
            assert_eq!(
                report.outcomes[0].result,
                Err(ActionError::PermissionDenied)
            );
        }
        let report = renice(40);
        assert!(matches!(
            report.outcomes[0].result,
            Err(ActionError::Failed(_))
        ));
    }

    #[test]
    fn affinity_matches_platform() {
        let (_child, key) = sleeper();
        let report = execute(
            &ActionRequest::Affinity {
                targets: vec![key],
                cpus: vec![0],
            },
            &|_| "sleep".into(),
        );
        if affinity_supported() {
            assert_eq!(report.succeeded(), 1, "{}", report.summary());
            assert_eq!(current_affinity(key.pid), Ok(vec![0]));
            let empty = ActionRequest::Affinity {
                targets: vec![key],
                cpus: vec![],
            };
            assert!(
                execute(&empty, &|_| String::new()).outcomes[0]
                    .result
                    .is_err()
            );
        } else {
            assert!(matches!(
                report.outcomes[0].result,
                Err(ActionError::Unsupported(_))
            ));
            assert!(current_affinity(key.pid).is_err());
        }
    }

    #[test]
    fn signals_reach_bproc_itself_last() {
        // A wrong start time: identity checks fail, so nothing is sent.
        let me = ProcessKey::new(std::process::id() as i32, 1);
        let (a, b) = (
            ProcessKey::new(i32::MAX - 5, 1),
            ProcessKey::new(i32::MAX - 6, 1),
        );
        let order = |request: &ActionRequest| -> Vec<ProcessKey> {
            execute(request, &|_| String::new())
                .outcomes
                .iter()
                .map(|o| o.key)
                .collect()
        };
        let signal = ActionRequest::Signal {
            targets: vec![me, a, b],
            signal: Signal::Cont,
        };
        assert_eq!(order(&signal), vec![a, b, me]);
        let renice = ActionRequest::Renice {
            targets: vec![me, a, b],
            nice: 0,
        };
        assert_eq!(order(&renice), vec![me, a, b], "only signals reorder");
    }

    #[test]
    fn self_and_system_root_helpers() {
        let me = std::process::id() as i32;
        assert!(is_self(me) && !is_self(me + 1));
        assert!(is_system_root(0) && is_system_root(1));
        assert_eq!(is_system_root(2), cfg!(target_os = "linux"));
        assert!(!is_system_root(100));
        let keys = vec![ProcessKey::new(10, 1), ProcessKey::new(me, 1)];
        assert_eq!(exclude_self(keys.clone()), (vec![keys[0]], true));
        assert_eq!(exclude_self(vec![keys[0]]), (vec![keys[0]], false));
    }

    #[test]
    fn multi_target_summary_counts() {
        let (_a, ka) = sleeper();
        let (_b, kb) = sleeper();
        let gone = ProcessKey::new(i32::MAX - 7, 1);
        let report = execute(
            &ActionRequest::Signal {
                targets: vec![ka, kb, gone],
                signal: Signal::Cont,
            },
            &|k| format!("p{}", k.pid),
        );
        assert_eq!(report.succeeded(), 2);
        assert_eq!(report.failed().count(), 1);
        assert_eq!(report.summary(), "SIGCONT sent to 2/3 processes");
    }

    #[test]
    fn sysinfo_fallback_kills_and_rejects_others() {
        let (mut child, key) = sleeper();
        let renice = ActionRequest::Renice {
            targets: vec![key],
            nice: 1,
        };
        assert!(matches!(
            apply_sysinfo(&renice, key),
            Err(ActionError::Unsupported(_))
        ));
        let hup = ActionRequest::Signal {
            targets: vec![key],
            signal: Signal::Hup,
        };
        assert!(matches!(
            apply_sysinfo(&hup, key),
            Err(ActionError::Unsupported(_))
        ));
        let kill = ActionRequest::Signal {
            targets: vec![key],
            signal: Signal::Kill,
        };
        assert_eq!(apply_sysinfo(&kill, key), Ok(()));
        assert!(wait_for(|| child.0.try_wait().unwrap().is_some()));
    }

    #[test]
    fn refuses_non_positive_pids() {
        let req = ActionRequest::Signal {
            targets: vec![],
            signal: Signal::Term,
        };
        for pid in [0, -1] {
            assert!(matches!(
                apply(&req, ProcessKey::new(pid, 0)),
                Err(ActionError::Failed(_))
            ));
        }
    }
}
