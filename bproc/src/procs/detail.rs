//! On-demand deep inspection of one process (threads, files, environment,
//! memory maps, limits, cgroups). Collected off the UI thread only for the
//! process shown in the detail panel.
use super::model::{ProcState, ProcessKey};
use std::collections::HashMap;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
mod parse;

/// `Err` carries a human reason the section is unavailable (permissions,
/// platform support, process exited) so the UI can explain instead of hiding.
pub type Section<T> = Result<T, String>;

/// Upper bound for every list section; longer lists are truncated and the
/// truncation is noted in `facts`.
pub const MAX_ENTRIES: usize = 5_000;

/// Reason shown when the kernel refuses to expose a section.
pub const PERMISSION_DENIED: &str =
    "Permission denied: run bproc with sudo to inspect this process";

/// Reason shown when the target is gone (or its PID was reused).
pub const EXITED: &str = "process has exited";

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

    /// Value of the fact labelled `label`.
    pub fn fact(&self, label: &str) -> Option<&str> {
        self.facts
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.as_str())
    }
}

/// Collect deep detail (blocking). `previous` lets per-thread CPU% be derived
/// from time deltas between refreshes.
pub fn collect(key: ProcessKey, previous: Option<&DeepDetail>) -> DeepDetail {
    if super::actions::verify_identity(key).is_err() {
        return DeepDetail::unavailable(key, EXITED);
    }
    let mut detail = collect_platform(key);
    // The process may have exited (or its PID been reused) mid-collection.
    if super::actions::verify_identity(key).is_err() {
        return DeepDetail::unavailable(key, EXITED);
    }
    finalize(&mut detail, previous);
    detail
}

#[cfg(target_os = "linux")]
fn collect_platform(key: ProcessKey) -> DeepDetail {
    linux::collect(key)
}

#[cfg(target_os = "macos")]
fn collect_platform(key: ProcessKey) -> DeepDetail {
    macos::collect(key)
}

/// Portable fallback built on sysinfo (exe, cwd, command, environment).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn collect_platform(key: ProcessKey) -> DeepDetail {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut detail = DeepDetail::unavailable(key, "Not available on this platform");
    let mut system = System::new();
    let pid = Pid::from_u32(key.pid.max(0) as u32);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_cmd(UpdateKind::Always)
            .with_environ(UpdateKind::Always),
    );
    let Some(process) = system.process(pid) else {
        return DeepDetail::unavailable(key, EXITED);
    };
    if let Some(exe) = process.exe() {
        push_fact(&mut detail.facts, "Executable", exe.display());
    }
    let cmd: Vec<String> = process
        .cmd()
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    if !cmd.is_empty() {
        push_fact(&mut detail.facts, "Command", cmd.join(" "));
    }
    if let Some(cwd) = process.cwd() {
        push_fact(&mut detail.facts, "Working dir", cwd.display());
    }
    if let Some(parent) = process.parent() {
        push_fact(&mut detail.facts, "Parent", parent);
    }
    push_started(&mut detail.facts, key.start_time);
    let env: Vec<(String, String)> = process
        .environ()
        .iter()
        .filter_map(|e| {
            let e = e.to_string_lossy();
            let (k, v) = e.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();
    detail.environment = Ok(sorted_env(env));
    detail
}

/// Apply caps (noting truncation) and derive per-thread CPU%.
fn finalize(detail: &mut DeepDetail, previous: Option<&DeepDetail>) {
    let mut notes = Vec::new();
    cap(&mut detail.threads, "threads", &mut notes);
    cap(&mut detail.files, "open files", &mut notes);
    cap(&mut detail.environment, "environment variables", &mut notes);
    cap(&mut detail.memory_maps, "memory maps", &mut notes);
    cap(&mut detail.cgroups, "cgroups", &mut notes);
    detail.facts.extend(notes);
    derive_thread_cpu(detail, previous);
}

fn cap<T>(section: &mut Section<Vec<T>>, what: &str, notes: &mut Vec<(String, String)>) {
    if let Ok(items) = section
        && items.len() > MAX_ENTRIES
    {
        items.truncate(MAX_ENTRIES);
        notes.push((
            "Truncated".into(),
            format!("showing the first {MAX_ENTRIES} {what}"),
        ));
    }
}

/// CPU% per thread from the CPU-time delta against `previous` (same process
/// only). Threads that are new since then keep any platform estimate.
fn derive_thread_cpu(detail: &mut DeepDetail, previous: Option<&DeepDetail>) {
    let Some(prev) = previous.filter(|p| p.key == detail.key) else {
        return;
    };
    let (Ok(threads), Ok(before)) = (&mut detail.threads, &prev.threads) else {
        return;
    };
    let wall_ms = detail.collected_at_ms.saturating_sub(prev.collected_at_ms);
    if wall_ms == 0 {
        return;
    }
    let before: HashMap<i64, u64> = before
        .iter()
        .filter_map(|t| Some((t.tid, t.cpu_time_ms?)))
        .collect();
    for thread in threads.iter_mut() {
        if let (Some(now), Some(&then)) = (thread.cpu_time_ms, before.get(&thread.tid)) {
            let pct = now.saturating_sub(then) as f64 / wall_ms as f64 * 100.0;
            thread.cpu_percent = Some(pct as f32);
        }
    }
}

fn push_fact(facts: &mut Vec<(String, String)>, label: &str, value: impl std::fmt::Display) {
    facts.push((label.to_string(), value.to_string()));
}

fn push_started(facts: &mut Vec<(String, String)>, start_secs: u64) {
    use chrono::{Local, TimeZone};
    if start_secs == 0 {
        return;
    }
    if let Some(when) = i64::try_from(start_secs)
        .ok()
        .and_then(|s| Local.timestamp_opt(s, 0).single())
    {
        push_fact(facts, "Started", when.format("%Y-%m-%d %H:%M:%S"));
    }
}

fn sorted_env(mut env: Vec<(String, String)>) -> Vec<(String, String)> {
    env.sort_by(|a, b| a.0.cmp(&b.0));
    env
}

/// Note shown in facts when some sections were hidden by permissions.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const LIMITED_ACCESS: &str = "limited: run bproc with sudo for full detail";

/// `"name (id)"`, or just the id when the name is unknown.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn id_label(name: Option<String>, id: u32) -> String {
    match name {
        Some(name) => format!("{name} ({id})"),
        None => id.to_string(),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t,
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe {
        libc::getgrgid_r(
            gid as libc::gid_t,
            &mut grp,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || grp.gr_name.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(grp.gr_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// Human reason for an errno from a per-process query.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn errno_reason(errno: i32) -> String {
    match errno {
        libc::EPERM | libc::EACCES => PERMISSION_DENIED.into(),
        libc::ESRCH | libc::ENOENT => EXITED.into(),
        other => std::io::Error::from_raw_os_error(other).to_string(),
    }
}

#[cfg(any(target_os = "linux", all(target_os = "macos", test)))]
fn io_reason(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::PermissionDenied => PERMISSION_DENIED.into(),
        std::io::ErrorKind::NotFound => EXITED.into(),
        _ => err
            .raw_os_error()
            .map(errno_reason)
            .unwrap_or_else(|| err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(tid: i64, cpu_ms: u64) -> ThreadInfo {
        ThreadInfo {
            tid,
            name: None,
            state: ProcState::Sleeping,
            cpu_percent: None,
            cpu_time_ms: Some(cpu_ms),
            priority: None,
        }
    }

    fn detail(key: ProcessKey, at: u64, threads: Vec<ThreadInfo>) -> DeepDetail {
        DeepDetail {
            collected_at_ms: at,
            threads: Ok(threads),
            ..DeepDetail::unavailable(key, "x")
        }
    }

    #[test]
    fn thread_cpu_from_deltas() {
        let key = ProcessKey::new(10, 5);
        let prev = detail(key, 1_000, vec![thread(1, 100), thread(2, 50)]);
        let mut now = detail(
            key,
            3_000,
            vec![thread(1, 1_100), thread(2, 50), thread(3, 999)],
        );
        derive_thread_cpu(&mut now, Some(&prev));
        let threads = now.threads.unwrap();
        assert_eq!(threads[0].cpu_percent, Some(50.0));
        assert_eq!(threads[1].cpu_percent, Some(0.0));
        assert_eq!(threads[2].cpu_percent, None, "new thread has no baseline");
    }

    #[test]
    fn thread_cpu_ignores_other_process_and_zero_wall() {
        let key = ProcessKey::new(10, 5);
        let other = detail(ProcessKey::new(11, 5), 1_000, vec![thread(1, 0)]);
        let mut now = detail(key, 2_000, vec![thread(1, 500)]);
        derive_thread_cpu(&mut now, Some(&other));
        assert_eq!(now.threads.as_ref().unwrap()[0].cpu_percent, None);
        let same_time = detail(key, 2_000, vec![thread(1, 0)]);
        derive_thread_cpu(&mut now, Some(&same_time));
        assert_eq!(now.threads.as_ref().unwrap()[0].cpu_percent, None);
        // Counter going backwards (tid reuse) saturates to zero.
        let ahead = detail(key, 1_000, vec![thread(1, 9_000)]);
        derive_thread_cpu(&mut now, Some(&ahead));
        assert_eq!(now.threads.as_ref().unwrap()[0].cpu_percent, Some(0.0));
        derive_thread_cpu(&mut now, None);
    }

    #[test]
    fn caps_long_sections_and_notes_it() {
        let key = ProcessKey::new(1, 1);
        let mut d = DeepDetail::unavailable(key, "x");
        d.files = Ok((0..MAX_ENTRIES + 10)
            .map(|i| OpenFile {
                fd: Some(i as i32),
                kind: FileKind::File,
                target: String::new(),
            })
            .collect());
        d.cgroups = Ok(vec!["/".into()]);
        finalize(&mut d, None);
        assert_eq!(d.files.as_ref().unwrap().len(), MAX_ENTRIES);
        assert_eq!(d.cgroups.as_ref().unwrap().len(), 1);
        assert_eq!(
            d.fact("Truncated"),
            Some("showing the first 5000 open files")
        );
    }

    #[test]
    fn started_and_env_helpers() {
        let mut facts = Vec::new();
        push_started(&mut facts, 0);
        assert!(facts.is_empty());
        push_started(&mut facts, 1_700_000_000);
        assert_eq!(facts[0].0, "Started");
        assert_eq!(facts[0].1.len(), "2023-11-14 22:13:20".len());
        let env = sorted_env(vec![("b".into(), "1".into()), ("A".into(), "2".into())]);
        assert_eq!(env[0].0, "A");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn reasons_and_names() {
        assert_eq!(errno_reason(libc::EPERM), PERMISSION_DENIED);
        assert_eq!(errno_reason(libc::ESRCH), EXITED);
        let denied = std::io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(io_reason(&denied), PERMISSION_DENIED);
        assert_eq!(user_name(0).as_deref(), Some("root"));
        assert!(group_name(0).is_some());
        assert_eq!(id_label(None, 7), "7");
        assert_eq!(id_label(Some("x".into()), 7), "x (7)");
    }

    #[test]
    fn exited_process_is_unavailable() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn");
        let pid = child.id() as i32;
        let start = super::super::actions::current_start_time(pid).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let d = collect(ProcessKey::new(pid, start), None);
        assert_eq!(d.threads, Err(EXITED.to_string()));
        assert_eq!(d.files, Err(EXITED.to_string()));
        assert!(d.facts.is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod live {
        use super::super::*;
        use std::process::{Child, Command};

        fn own_key() -> ProcessKey {
            let pid = std::process::id() as i32;
            let start = crate::procs::actions::current_start_time(pid).unwrap();
            ProcessKey::new(pid, start)
        }

        struct Sleeper(Child);
        impl Drop for Sleeper {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        fn is_root() -> bool {
            unsafe { libc::geteuid() == 0 }
        }

        fn thread_cpu_ms() -> u64 {
            let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
            ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
        }

        #[test]
        fn own_process() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (spun_tx, spun_rx) = std::sync::mpsc::channel();
            let spin = std::thread::Builder::new()
                .name("bproc-spin".into())
                .spawn(move || {
                    let until = std::time::Instant::now() + std::time::Duration::from_millis(300);
                    let mut x = 0u64;
                    while std::time::Instant::now() < until {
                        x = std::hint::black_box(x.wrapping_add(1));
                    }
                    spun_tx.send(thread_cpu_ms()).unwrap();
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    x
                })
                .unwrap();
            let measured = spun_rx.recv().unwrap();
            let key = own_key();
            let first = collect(key, None);
            let threads = first.threads.as_ref().expect("threads");
            assert!(threads.len() >= 2, "{threads:?}");
            let me = threads
                .iter()
                .find(|t| t.name.as_deref() == Some("bproc-spin"))
                .unwrap_or_else(|| panic!("spin thread missing: {threads:?}"));
            // Units check: the reported time must match the thread's own clock.
            let spent = me.cpu_time_ms.unwrap();
            let slack = (measured / 4).max(30);
            assert!(
                spent.abs_diff(measured) <= slack,
                "reported {spent}ms vs measured {measured}ms"
            );

            let files = first.files.as_ref().expect("files");
            assert!(!files.is_empty());
            let needle = format!("127.0.0.1:{port}");
            assert!(
                files.iter().any(|f| f.kind == FileKind::Socket
                    && f.target.contains(&needle)
                    && f.target.contains("LISTEN")),
                "listener {needle} missing: {files:?}"
            );
            let env = first.environment.as_ref().expect("env");
            assert!(env.iter().any(|(k, _)| k == "PATH"));
            assert!(first.fact("Executable").is_some(), "{:?}", first.facts);
            assert!(first.fact("User").is_some());
            assert!(first.fact("Started").is_some());
            let maps = first.memory_maps.as_ref().expect("maps");
            assert!(!maps.is_empty());
            let limits = first.limits.as_ref().expect("limits");
            assert!(limits.iter().any(|l| l.name == "Max open files"));

            std::thread::sleep(std::time::Duration::from_millis(50));
            let second = collect(key, Some(&first));
            assert!(second.collected_at_ms >= first.collected_at_ms);
            let threads = second.threads.as_ref().unwrap();
            assert!(threads.iter().any(|t| t.cpu_percent.is_some()));
            drop(listener);
            spin.join().unwrap();
        }

        #[test]
        fn sleep_child() {
            let child = Command::new("sleep").arg("30").spawn().unwrap();
            let child = Sleeper(child);
            let pid = child.0.id() as i32;
            let key = ProcessKey::new(pid, crate::procs::actions::current_start_time(pid).unwrap());
            // Give exec a moment so we inspect `sleep`, not the forked test.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let d = collect(key, None);
            assert!(!d.threads.as_ref().expect("threads").is_empty());
            assert!(!d.files.as_ref().expect("files").is_empty());
            let env = d.environment.as_ref().expect("env");
            assert!(env.iter().any(|(k, _)| k == "PATH"));
            let command = d.fact("Command").expect("command");
            assert!(
                command.contains("sleep") && command.contains("30"),
                "{command}"
            );
            assert!(
                d.fact("Parent")
                    .unwrap()
                    .starts_with(&std::process::id().to_string())
            );
        }

        #[test]
        fn foreign_process_permission() {
            if is_root() {
                return;
            }
            let d = collect(ProcessKey::new(1, 0), None);
            assert_eq!(d.files, Err(PERMISSION_DENIED.to_string()));
            assert_eq!(d.environment, Err(PERMISSION_DENIED.to_string()));
            assert!(d.fact("Access").is_some());
            assert_eq!(d.fact("User"), Some("root (0)"));
            assert!(d.fact("Executable").is_some(), "{:?}", d.facts);
        }
    }
}
