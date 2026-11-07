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
pub fn current_start_time(pid: i32) -> Option<u64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
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

pub fn verify_identity(key: ProcessKey) -> Result<(), ActionError> {
    match current_start_time(key.pid) {
        None => Err(ActionError::NotFound),
        Some(start) if key.start_time != 0 && start != key.start_time => {
            Err(ActionError::PidReused)
        }
        Some(_) => Ok(()),
    }
}

pub fn is_alive(key: ProcessKey) -> bool {
    verify_identity(key).is_ok()
}

/// Execute a request (blocking). `names` supplies display names for reports.
pub fn execute(request: &ActionRequest, names: &dyn Fn(&ProcessKey) -> String) -> ActionReport {
    let outcomes = request
        .targets()
        .iter()
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

#[cfg(unix)]
fn errno_error() -> ActionError {
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ESRCH) => ActionError::NotFound,
        Some(libc::EPERM) | Some(libc::EACCES) => ActionError::PermissionDenied,
        _ => ActionError::Failed(err.to_string()),
    }
}

#[cfg(unix)]
fn apply(request: &ActionRequest, key: ProcessKey) -> Result<(), ActionError> {
    match request {
        ActionRequest::Signal { signal, .. } => {
            if unsafe { libc::kill(key.pid, signal.number()) } == 0 {
                Ok(())
            } else {
                Err(errno_error())
            }
        }
        ActionRequest::Renice { nice, .. } => {
            if unsafe { libc::setpriority(libc::PRIO_PROCESS, key.pid as libc::id_t, *nice) } == 0 {
                Ok(())
            } else {
                Err(errno_error())
            }
        }
        ActionRequest::Affinity { .. } => Err(ActionError::Unsupported("CPU affinity".into())),
    }
}

#[cfg(not(unix))]
fn apply(request: &ActionRequest, _key: ProcessKey) -> Result<(), ActionError> {
    Err(ActionError::Unsupported(request.label()))
}
