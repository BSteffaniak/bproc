//! Linux collection from `/proc/<pid>`. All parsing lives in `parse`; this
//! module only performs the reads.
use super::parse::procfs::{
    Stat, classify_fd_link, linux_state, linux_tty_name, parse_btime, parse_cgroups, parse_environ,
    parse_limits, parse_maps, parse_net_inet, parse_net_unix, parse_smaps, parse_stat,
    parse_status_ids,
};
use super::{
    DeepDetail, FileKind, LIMITED_ACCESS, MAX_ENTRIES, MemoryMap, OpenFile, PERMISSION_DENIED,
    ResourceLimit, Section, ThreadInfo, group_name, id_label, io_reason, push_fact, push_started,
    sorted_env, user_name,
};
use crate::procs::model::{ProcessKey, now_ms};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

/// Read at most `cap` bytes (procfs files report size 0, so `fs::read` alone
/// cannot be bounded up front).
fn read_capped(path: impl AsRef<Path>, cap: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    fs::File::open(path)?.take(cap).read_to_end(&mut out)?;
    Ok(out)
}

fn read_text(path: impl AsRef<Path>, cap: u64) -> std::io::Result<String> {
    read_capped(path, cap).map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn section<T>(result: std::io::Result<T>) -> Section<T> {
    result.map_err(|e| io_reason(&e))
}

fn clock_ticks() -> u64 {
    match unsafe { libc::sysconf(libc::_SC_CLK_TCK) } {
        n if n > 0 => n as u64,
        _ => 100,
    }
}

pub fn collect(key: ProcessKey) -> DeepDetail {
    let base = format!("/proc/{}", key.pid);
    let stat = read_text(format!("{base}/stat"), 4096)
        .ok()
        .and_then(|t| parse_stat(&t));
    let collected_at_ms = now_ms();
    let threads = section(threads(&base));
    let files = section(files(&base));
    let environment = section(
        read_capped(format!("{base}/environ"), 1 << 20).map(|b| sorted_env(parse_environ(&b))),
    );
    let memory_maps = section(memory_maps(&base));
    let limits: Section<Vec<ResourceLimit>> =
        section(read_text(format!("{base}/limits"), 64 << 10).map(|t| parse_limits(&t)));
    let cgroups = section(read_text(format!("{base}/cgroup"), 64 << 10).map(|t| parse_cgroups(&t)));

    let mut facts = Vec::new();
    let mut limited = facts_for(key, &base, stat.as_ref(), &mut facts);
    limited |= [
        files.as_ref().err(),
        environment.as_ref().err(),
        memory_maps.as_ref().err(),
    ]
    .into_iter()
    .flatten()
    .any(|e| e == PERMISSION_DENIED);
    if limited {
        push_fact(&mut facts, "Access", LIMITED_ACCESS);
    }

    DeepDetail {
        key,
        collected_at_ms,
        facts,
        threads,
        files,
        environment,
        memory_maps,
        limits,
        cgroups,
    }
}

/// Overview facts. Returns true when some links were hidden by permissions.
fn facts_for(
    key: ProcessKey,
    base: &str,
    stat: Option<&Stat>,
    facts: &mut Vec<(String, String)>,
) -> bool {
    let mut limited = false;
    let mut link = |name: &str| match fs::read_link(format!("{base}/{name}")) {
        Ok(path) => Some(path.display().to_string()),
        Err(e) => {
            limited |= e.kind() == std::io::ErrorKind::PermissionDenied;
            None
        }
    };
    let exe = link("exe");
    let cwd = link("cwd");
    let root = link("root");
    if let Some(exe) = exe {
        push_fact(facts, "Executable", exe);
    }
    let cmdline = read_capped(format!("{base}/cmdline"), 64 << 10).unwrap_or_default();
    let args: Vec<String> = cmdline
        .split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    if !args.is_empty() {
        push_fact(facts, "Command", args.join(" "));
    } else if let Some(stat) = stat {
        push_fact(facts, "Command", format!("[{}]", stat.comm));
    }
    if let Some(cwd) = cwd {
        push_fact(facts, "Working dir", cwd);
    }
    if let Some(root) = root {
        push_fact(facts, "Root", root);
    }
    let (uid, gid) = read_text(format!("{base}/status"), 64 << 10)
        .map(|t| parse_status_ids(&t))
        .unwrap_or((None, None));
    if let Some(uid) = uid {
        push_fact(facts, "User", id_label(user_name(uid), uid));
    }
    if let Some(gid) = gid {
        push_fact(facts, "Group", id_label(group_name(gid), gid));
    }
    if let Some(stat) = stat {
        push_fact(facts, "State", linux_state(stat.state).label());
        if stat.ppid > 0 {
            let parent = read_text(format!("/proc/{}/comm", stat.ppid), 256)
                .ok()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty());
            let label = match parent {
                Some(name) => format!("{} ({name})", stat.ppid),
                None => stat.ppid.to_string(),
            };
            push_fact(facts, "Parent", label);
        }
        push_fact(facts, "Process group", stat.pgrp);
        push_fact(facts, "Session", stat.session);
        push_fact(
            facts,
            "Terminal",
            linux_tty_name(stat.tty_nr).unwrap_or_else(|| "none".into()),
        );
    }
    let started = match (key.start_time, stat) {
        (0, Some(stat)) => read_text("/proc/stat", 1 << 20)
            .ok()
            .and_then(|t| parse_btime(&t))
            .map(|boot| boot.saturating_add(stat.starttime / clock_ticks()))
            .unwrap_or(0),
        (start, _) => start,
    };
    push_started(facts, started);
    if let Some(stat) = stat {
        push_fact(facts, "Nice", stat.nice);
        push_fact(facts, "Priority", stat.priority);
        push_fact(facts, "Threads", stat.num_threads);
    }
    limited
}

fn threads(base: &str) -> std::io::Result<Vec<ThreadInfo>> {
    let ticks = clock_ticks();
    let mut threads: Vec<ThreadInfo> = fs::read_dir(format!("{base}/task"))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let tid: i64 = entry.file_name().to_str()?.parse().ok()?;
            // A thread may exit between listing and reading; skip it.
            let stat = parse_stat(&read_text(entry.path().join("stat"), 4096).ok()?)?;
            let ticks_used = stat.utime.saturating_add(stat.stime);
            Some(ThreadInfo {
                tid,
                name: (!stat.comm.is_empty()).then(|| stat.comm.clone()),
                state: linux_state(stat.state),
                cpu_percent: None,
                cpu_time_ms: Some(ticks_used.saturating_mul(1000) / ticks),
                priority: i32::try_from(stat.priority).ok(),
            })
        })
        .take(MAX_ENTRIES + 1)
        .collect();
    threads.sort_by_key(|t| t.tid);
    Ok(threads)
}

fn file_type_kind(meta: &fs::Metadata) -> FileKind {
    use std::os::unix::fs::FileTypeExt;
    let ft = meta.file_type();
    if ft.is_dir() {
        FileKind::Directory
    } else if ft.is_char_device() || ft.is_block_device() {
        FileKind::Device
    } else if ft.is_fifo() {
        FileKind::Pipe
    } else if ft.is_socket() {
        FileKind::Socket
    } else if ft.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

fn files(base: &str) -> std::io::Result<Vec<OpenFile>> {
    let mut files = Vec::new();
    let mut sockets: Vec<(usize, u64)> = Vec::new();
    for entry in fs::read_dir(format!("{base}/fd"))?.take(MAX_ENTRIES + 1) {
        let Ok(entry) = entry else { continue };
        let Some(fd) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        let target = target.to_string_lossy();
        let hint = if target.starts_with('/') {
            fs::metadata(entry.path()).ok().map(|m| file_type_kind(&m))
        } else {
            None
        };
        let link = classify_fd_link(&target, hint);
        if let Some(inode) = link.socket_inode {
            sockets.push((files.len(), inode));
        }
        files.push(OpenFile {
            fd: Some(fd),
            kind: link.kind,
            target: link.target,
        });
    }
    if !sockets.is_empty() {
        let table = socket_table(base);
        for (index, inode) in sockets {
            if let Some(desc) = table.get(&inode) {
                files[index].target = desc.clone();
            }
        }
    }
    files.sort_by_key(|f| f.fd);
    Ok(files)
}

/// Socket inode descriptions from the process's network namespace. Each
/// table is read with a size cap so huge servers stay responsive.
fn socket_table(base: &str) -> HashMap<u64, String> {
    const CAP: u64 = 4 << 20;
    let mut table = HashMap::new();
    for (file, proto) in [
        ("tcp", "TCP"),
        ("tcp6", "TCP"),
        ("udp", "UDP"),
        ("udp6", "UDP"),
    ] {
        if let Ok(text) = read_text(format!("{base}/net/{file}"), CAP) {
            table.extend(parse_net_inet(&text, proto));
        }
    }
    if let Ok(text) = read_text(format!("{base}/net/unix"), CAP) {
        table.extend(parse_net_unix(&text));
    }
    table
}

fn memory_maps(base: &str) -> std::io::Result<Vec<MemoryMap>> {
    /// smaps makes the kernel walk page tables; only use it for modest maps.
    const SMAPS_MAX_REGIONS: usize = 1_500;
    let text = read_text(format!("{base}/maps"), 32 << 20)?;
    let maps = parse_maps(&text, MAX_ENTRIES + 1);
    if maps.len() <= SMAPS_MAX_REGIONS
        && let Ok(smaps) = read_text(format!("{base}/smaps"), 64 << 20)
    {
        let detailed = parse_smaps(&smaps, MAX_ENTRIES + 1);
        if !detailed.is_empty() {
            return Ok(detailed);
        }
    }
    Ok(maps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_own_proc_sections() {
        let base = format!("/proc/{}", std::process::id());
        let threads = threads(&base).unwrap();
        assert!(!threads.is_empty());
        assert!(threads.iter().all(|t| t.cpu_time_ms.is_some()));
        let files = files(&base).unwrap();
        assert!(files.iter().any(|f| f.fd == Some(0)));
        let maps = memory_maps(&base).unwrap();
        assert!(maps.iter().any(|m| m.rss.is_some()));
        assert!(maps.iter().any(|m| m.path == "[stack]"));
    }

    #[test]
    fn pipes_and_unix_sockets_resolve() {
        let (_a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let dir = std::env::temp_dir().join(format!("bproc-detail-{}", std::process::id()));
        let _ = fs::remove_file(&dir);
        let _listener = std::os::unix::net::UnixListener::bind(&dir).unwrap();
        let base = format!("/proc/{}", std::process::id());
        let files = files(&base).unwrap();
        let path = dir.display().to_string();
        assert!(
            files
                .iter()
                .any(|f| f.kind == FileKind::Socket && f.target == format!("UNIX {path}")),
            "{files:?}"
        );
        let _ = fs::remove_file(&dir);
    }
}
