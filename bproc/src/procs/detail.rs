//! On-demand deep inspection of one process (threads, files, environment,
//! memory maps, limits, cgroups). Collected off the UI thread only for the
//! process shown in the detail panel.
use super::model::{ProcState, ProcessKey};

/// `Err` carries a human reason the section is unavailable (permissions,
/// platform support, process exited) so the UI can explain instead of hiding.
pub type Section<T> = Result<T, String>;

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadInfo {
    pub tid: i64,
    pub name: Option<String>,
    pub state: ProcState,
    pub cpu_percent: Option<f32>,
    pub cpu_time_ms: Option<u64>,
    pub priority: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Socket,
    Pipe,
    Device,
    Event,
    Other,
}

impl FileKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "dir",
            Self::Socket => "socket",
            Self::Pipe => "pipe",
            Self::Device => "device",
            Self::Event => "event",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenFile {
    pub fd: Option<i32>,
    pub kind: FileKind,
    /// Path, socket description, or pipe identifier.
    pub target: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryMap {
    pub start: u64,
    pub end: u64,
    pub perms: String,
    pub rss: Option<u64>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResourceLimit {
    pub name: String,
    /// `None` = unlimited.
    pub soft: Option<u64>,
    pub hard: Option<u64>,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeepDetail {
    pub key: ProcessKey,
    pub collected_at_ms: u64,
    /// Labelled facts for the overview (exe, cwd, root, session, tty, …).
    pub facts: Vec<(String, String)>,
    pub threads: Section<Vec<ThreadInfo>>,
    pub files: Section<Vec<OpenFile>>,
    pub environment: Section<Vec<(String, String)>>,
    pub memory_maps: Section<Vec<MemoryMap>>,
    pub limits: Section<Vec<ResourceLimit>>,
    pub cgroups: Section<Vec<String>>,
}

impl DeepDetail {
    pub fn unavailable(key: ProcessKey, why: &str) -> Self {
        Self {
            key,
            collected_at_ms: super::model::now_ms(),
            facts: Vec::new(),
            threads: Err(why.into()),
            files: Err(why.into()),
            environment: Err(why.into()),
            memory_maps: Err(why.into()),
            limits: Err(why.into()),
            cgroups: Err(why.into()),
        }
    }
}

/// Collect deep detail (blocking). `previous` lets per-thread CPU% be derived
/// from time deltas between refreshes.
pub fn collect(key: ProcessKey, previous: Option<&DeepDetail>) -> DeepDetail {
    let _ = previous;
    if super::actions::verify_identity(key).is_err() {
        return DeepDetail::unavailable(key, "process has exited");
    }
    DeepDetail::unavailable(key, "not yet implemented on this platform")
}
