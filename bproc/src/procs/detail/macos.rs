//! macOS collection through `proc_pidinfo` / `proc_pidfdinfo` and
//! `KERN_PROCARGS2`.
use super::parse::darwin::{
    ProcArgs, c_buf_to_string, darwin_tcp_state, mach_perms, mach_region_label, mach_thread_state,
    parse_procargs2,
};
use super::parse::format_inet;
use super::{
    DeepDetail, EXITED, FileKind, LIMITED_ACCESS, MAX_ENTRIES, MemoryMap, OpenFile,
    PERMISSION_DENIED, ResourceLimit, Section, ThreadInfo, errno_reason, group_name, id_label,
    push_fact, push_started, sorted_env, user_name,
};
use crate::procs::model::{ProcessKey, now_ms};
use libc::{c_char, c_int, c_void};
use libproc::bsd_info::BSDInfo;
use libproc::net_info::{InSockInfo, SocketFDInfo};
use libproc::thread_info::ThreadInfo as MachThreadInfo;
use std::mem::{size_of, zeroed};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

unsafe extern "C" {
    fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int)
    -> c_int;
    fn proc_pidfdinfo(
        pid: c_int,
        fd: c_int,
        flavor: c_int,
        buffer: *mut c_void,
        size: c_int,
    ) -> c_int;
}

const PROC_PIDLISTFDS: c_int = 1;
const PROC_PIDTBSDINFO: c_int = 3;
const PROC_PIDTHREADINFO: c_int = 5;
const PROC_PIDLISTTHREADS: c_int = 6;
const PROC_PIDREGIONPATHINFO: c_int = 8;
const PROC_PIDVNODEPATHINFO: c_int = 9;
const PROC_PIDT_SHORTBSDINFO: c_int = 13;
const PROC_PIDTHREADID64INFO: c_int = 15;
const PROC_PIDLISTTHREADIDS: c_int = 28;

const PROC_PIDFDVNODEPATHINFO: c_int = 2;
const PROC_PIDFDSOCKETINFO: c_int = 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcFdInfo {
    fd: i32,
    fdtype: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ShortBsdInfo {
    pid: u32,
    ppid: u32,
    pgid: u32,
    status: u32,
    comm: [c_char; 16],
    flags: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    rfu: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcFileInfo {
    openflags: u32,
    status: u32,
    offset: i64,
    kind: i32,
    guardflags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VinfoStat {
    dev: u32,
    mode: u16,
    nlink: u16,
    ino: u64,
    uid: u32,
    gid: u32,
    times: [i64; 8],
    size: i64,
    blocks: i64,
    blksize: i32,
    flags: u32,
    gen_: u32,
    rdev: u32,
    qspare: [i64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeInfo {
    stat: VinfoStat,
    vtype: c_int,
    pad: c_int,
    fsid: [i32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeInfoPath {
    vi: VnodeInfo,
    path: [c_char; 1024],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: VnodeInfoPath,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VnodePathInfo {
    cdir: VnodeInfoPath,
    rdir: VnodeInfoPath,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RegionInfo {
    protection: u32,
    max_protection: u32,
    inheritance: u32,
    flags: u32,
    offset: u64,
    behavior: u32,
    user_wired_count: u32,
    user_tag: u32,
    pages_resident: u32,
    pages_shared_now_private: u32,
    pages_swapped_out: u32,
    pages_dirtied: u32,
    ref_count: u32,
    shadow_depth: u32,
    share_mode: u32,
    private_pages_resident: u32,
    shared_pages_resident: u32,
    obj_id: u32,
    depth: u32,
    address: u64,
    size: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RegionWithPathInfo {
    info: RegionInfo,
    vip: VnodeInfoPath,
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// One fixed-size `proc_pidinfo` record.
fn pidinfo<T: Copy>(pid: i32, flavor: c_int, arg: u64) -> Result<T, i32> {
    let mut out: T = unsafe { zeroed() };
    let size = size_of::<T>() as c_int;
    let ret = unsafe { proc_pidinfo(pid, flavor, arg, (&mut out as *mut T).cast(), size) };
    if ret <= 0 {
        Err(match errno() {
            0 => libc::ESRCH,
            e => e,
        })
    } else if ret < size {
        Err(libc::EINVAL)
    } else {
        Ok(out)
    }
}

/// A `proc_pidinfo` list flavor, growing the buffer until everything fits
/// (bounded by `MAX_ENTRIES + 1` so callers can detect truncation).
fn pidlist<T: Copy>(pid: i32, flavor: c_int) -> Result<Vec<T>, i32> {
    let mut capacity = 64usize;
    loop {
        let mut buf: Vec<T> = Vec::with_capacity(capacity);
        let bytes = (capacity * size_of::<T>()) as c_int;
        let ret = unsafe { proc_pidinfo(pid, flavor, 0, buf.as_mut_ptr().cast(), bytes) };
        if ret <= 0 {
            return match errno() {
                0 => Ok(Vec::new()),
                e => Err(e),
            };
        }
        let len = (ret as usize / size_of::<T>()).min(capacity);
        if len < capacity || capacity > MAX_ENTRIES {
            unsafe { buf.set_len(len) };
            return Ok(buf);
        }
        capacity = capacity.saturating_mul(4).min(MAX_ENTRIES + 1);
    }
}

/// `T` must be a plain `repr(C)` record (valid when zeroed).
fn pidfdinfo<T>(pid: i32, fd: i32, flavor: c_int) -> Result<T, i32> {
    let mut out: T = unsafe { zeroed() };
    let size = size_of::<T>() as c_int;
    let ret = unsafe { proc_pidfdinfo(pid, fd, flavor, (&mut out as *mut T).cast(), size) };
    if ret < size {
        Err(match errno() {
            0 => libc::EINVAL,
            e => e,
        })
    } else {
        Ok(out)
    }
}

/// Ownership and session facts. Full BSD info is refused for other users'
/// processes, so the short form (always readable) is the fallback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Identity {
    uid: Option<u32>,
    gid: Option<u32>,
    ppid: Option<u32>,
    pgid: Option<u32>,
    tdev: Option<u32>,
    start_secs: Option<u64>,
    nice: Option<i32>,
}

impl Identity {
    fn read(pid: i32) -> Self {
        if let Ok(b) = pidinfo::<BSDInfo>(pid, PROC_PIDTBSDINFO, 0) {
            return Self {
                uid: Some(b.pbi_uid),
                gid: Some(b.pbi_gid),
                ppid: Some(b.pbi_ppid),
                pgid: Some(b.pbi_pgid),
                tdev: Some(b.e_tdev),
                start_secs: Some(b.pbi_start_tvsec),
                nice: Some(b.pbi_nice),
            };
        }
        match pidinfo::<ShortBsdInfo>(pid, PROC_PIDT_SHORTBSDINFO, 0) {
            Ok(s) => Self {
                uid: Some(s.uid),
                gid: Some(s.gid),
                ppid: Some(s.ppid),
                pgid: Some(s.pgid),
                ..Self::default()
            },
            Err(_) => Self::default(),
        }
    }
}

pub fn collect(key: ProcessKey) -> DeepDetail {
    let pid = key.pid;
    let identity = Identity::read(pid);
    let own_uid = unsafe { libc::geteuid() };
    let foreign = own_uid != 0 && identity.uid != Some(own_uid);
    let deny = |errno: i32| -> String {
        match errno {
            // KERN_PROCARGS2 and friends report EINVAL when access is refused.
            libc::EINVAL if foreign => PERMISSION_DENIED.into(),
            other => errno_reason(other),
        }
    };

    let args = procargs(pid).map_err(deny);
    let collected_at_ms = now_ms();
    let threads = threads(pid).map_err(deny);
    let files = files(pid).map_err(deny);
    let memory_maps = memory_maps(pid);
    let limits = if pid == std::process::id() as i32 {
        Ok(own_limits())
    } else {
        Err("Only available for bproc itself on macOS".into())
    };

    let mut facts = Vec::new();
    facts_for(key, &identity, args.as_ref().ok(), &mut facts);
    if let Ok(threads) = &threads {
        push_fact(&mut facts, "Threads", threads.len());
    }
    let limited = [
        threads.as_ref().err(),
        files.as_ref().err(),
        args.as_ref().err(),
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
        environment: args.map(|a| sorted_env(a.env)),
        memory_maps,
        limits,
        cgroups: Err("Not applicable on macOS".into()),
    }
}

fn facts_for(
    key: ProcessKey,
    id: &Identity,
    args: Option<&ProcArgs>,
    facts: &mut Vec<(String, String)>,
) {
    let pid = key.pid;
    let exe = libproc::proc_pid::pidpath(pid)
        .ok()
        .filter(|p| !p.is_empty())
        .or_else(|| args.map(|a| a.exec_path.clone()).filter(|p| !p.is_empty()));
    if let Some(exe) = exe {
        push_fact(facts, "Executable", exe);
    }
    if let Some(args) = args.filter(|a| !a.args.is_empty()) {
        push_fact(facts, "Command", args.args.join(" "));
    }
    if let Ok(paths) = pidinfo::<VnodePathInfo>(pid, PROC_PIDVNODEPATHINFO, 0) {
        let cwd = c_buf_to_string(&paths.cdir.path);
        if !cwd.is_empty() {
            push_fact(facts, "Working dir", cwd);
        }
        let root = c_buf_to_string(&paths.rdir.path);
        push_fact(
            facts,
            "Root",
            if root.is_empty() { "/".into() } else { root },
        );
    }
    if let Some(uid) = id.uid {
        push_fact(facts, "User", id_label(user_name(uid), uid));
    }
    if let Some(gid) = id.gid {
        push_fact(facts, "Group", id_label(group_name(gid), gid));
    }
    if let Some(ppid) = id.ppid.filter(|_| pid != 0) {
        let ppid = ppid as i32;
        let parent = match process_name(ppid) {
            Some(name) => format!("{ppid} ({name})"),
            None => ppid.to_string(),
        };
        push_fact(facts, "Parent", parent);
    }
    if let Some(pgid) = id.pgid {
        push_fact(facts, "Process group", pgid);
    }
    let sid = unsafe { libc::getsid(pid) };
    if sid >= 0 {
        push_fact(facts, "Session", sid);
    }
    if let Some(tdev) = id.tdev {
        push_fact(
            facts,
            "Terminal",
            tty_name(tdev).unwrap_or_else(|| "none".into()),
        );
    }
    push_started(facts, id.start_secs.unwrap_or(key.start_time));
    if let Some(nice) = id.nice {
        push_fact(facts, "Nice", nice);
    }
}

/// Short process name; falls back to data readable for any user's process.
fn process_name(pid: i32) -> Option<String> {
    if pid == 0 {
        return Some("kernel_task".into());
    }
    libproc::proc_pid::name(pid)
        .ok()
        .filter(|n| !n.is_empty())
        .or_else(|| {
            let short = pidinfo::<ShortBsdInfo>(pid, PROC_PIDT_SHORTBSDINFO, 0).ok()?;
            Some(c_buf_to_string(&short.comm)).filter(|n| !n.is_empty())
        })
}

fn tty_name(dev: u32) -> Option<String> {
    static DEVNAME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if dev == u32::MAX {
        return None;
    }
    let _guard = DEVNAME.lock().ok()?;
    let ptr = unsafe { libc::devname(dev as libc::dev_t, libc::S_IFCHR) };
    if ptr.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    (!name.is_empty() && name != "??").then_some(name)
}

fn procargs(pid: i32) -> Result<ProcArgs, i32> {
    let mut argmax: c_int = 0;
    let mut size = size_of::<c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&mut argmax as *mut c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    let capacity = if rc == 0 && argmax > 0 {
        argmax as usize
    } else {
        1 << 20
    };
    let mut buf = vec![0u8; capacity];
    let mut size = capacity;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(errno());
    }
    buf.truncate(size.min(capacity));
    parse_procargs2(&buf).ok_or(libc::EINVAL)
}

fn thread_info(raw: &MachThreadInfo, tid: i64) -> ThreadInfo {
    let name = c_buf_to_string(&raw.pth_name);
    let ns = raw.pth_user_time.saturating_add(raw.pth_system_time);
    ThreadInfo {
        tid,
        name: (!name.is_empty()).then_some(name),
        state: mach_thread_state(raw.pth_run_state, raw.pth_flags),
        // Scheduler estimate (TH_USAGE_SCALE = 1000); replaced by deltas later.
        cpu_percent: Some(raw.pth_cpu_usage.max(0) as f32 / 10.0),
        cpu_time_ms: Some(ns / 1_000_000),
        priority: Some(raw.pth_curpri),
    }
}

fn threads(pid: i32) -> Result<Vec<ThreadInfo>, i32> {
    // Prefer real thread ids; older kernels only expose opaque handles.
    if let Ok(ids) = pidlist::<u64>(pid, PROC_PIDLISTTHREADIDS) {
        let threads: Vec<ThreadInfo> = ids
            .iter()
            .filter_map(|&id| {
                let raw = pidinfo::<MachThreadInfo>(pid, PROC_PIDTHREADID64INFO, id).ok()?;
                Some(thread_info(&raw, id as i64))
            })
            .collect();
        if !threads.is_empty() {
            return Ok(threads);
        }
    }
    let handles = pidlist::<u64>(pid, PROC_PIDLISTTHREADS)?;
    Ok(handles
        .iter()
        .filter_map(|&handle| {
            let raw = pidinfo::<MachThreadInfo>(pid, PROC_PIDTHREADINFO, handle).ok()?;
            Some(thread_info(&raw, handle as i64))
        })
        .collect())
}

fn files(pid: i32) -> Result<Vec<OpenFile>, i32> {
    let fds = pidlist::<ProcFdInfo>(pid, PROC_PIDLISTFDS)?;
    Ok(fds
        .iter()
        .filter_map(|fd| {
            let (kind, target) = describe_fd(pid, fd)?;
            Some(OpenFile {
                fd: Some(fd.fd),
                kind,
                target,
            })
        })
        .collect())
}

/// `None` when the descriptor was closed between listing and inspection.
fn describe_fd(pid: i32, fd: &ProcFdInfo) -> Option<(FileKind, String)> {
    let gone = |e: i32| e == libc::EBADF || e == libc::ESRCH;
    Some(match fd.fdtype {
        1 => match pidfdinfo::<VnodeFdInfoWithPath>(pid, fd.fd, PROC_PIDFDVNODEPATHINFO) {
            Ok(info) => {
                let kind = match info.pvip.vi.vtype {
                    2 => FileKind::Directory,
                    3 | 4 => FileKind::Device,
                    6 => FileKind::Socket,
                    7 => FileKind::Pipe,
                    _ => FileKind::File,
                };
                let path = c_buf_to_string(&info.pvip.path);
                (
                    kind,
                    if path.is_empty() {
                        "(unnamed vnode)".into()
                    } else {
                        path
                    },
                )
            }
            Err(e) if gone(e) => return None,
            Err(_) => (FileKind::File, "(unreadable)".into()),
        },
        2 => match pidfdinfo::<SocketFDInfo>(pid, fd.fd, PROC_PIDFDSOCKETINFO) {
            Ok(info) => (FileKind::Socket, describe_socket(&info)),
            Err(e) if gone(e) => return None,
            Err(_) => (FileKind::Socket, "socket".into()),
        },
        3 => (FileKind::Other, "POSIX shared memory".into()),
        4 => (FileKind::Other, "POSIX semaphore".into()),
        5 => (FileKind::Event, "kqueue".into()),
        6 => (FileKind::Pipe, "pipe".into()),
        7 => (FileKind::Event, "fsevents".into()),
        9 => (FileKind::Other, "network policy".into()),
        other => (FileKind::Other, format!("descriptor type {other}")),
    })
}

fn in_endpoint(ini: &InSockInfo, local: bool) -> SocketAddr {
    const INI_IPV6: u8 = 0x2;
    let (addr, port) = if local {
        (&ini.insi_laddr, ini.insi_lport)
    } else {
        (&ini.insi_faddr, ini.insi_fport)
    };
    let port = u16::from_be(port as u16);
    let ip = if ini.insi_vflag & INI_IPV6 != 0 {
        IpAddr::V6(Ipv6Addr::from(unsafe { addr.ina_6.s6_addr }))
    } else {
        let raw = unsafe { addr.ina_46.i46a_addr4.s_addr };
        IpAddr::V4(Ipv4Addr::from(u32::from_be(raw)))
    };
    SocketAddr::new(ip, port)
}

fn describe_socket(info: &SocketFDInfo) -> String {
    const KIND_IN: c_int = 1;
    const KIND_TCP: c_int = 2;
    const KIND_UN: c_int = 3;
    const KIND_NDRV: c_int = 4;
    const KIND_KERN_EVENT: c_int = 5;
    const KIND_KERN_CTL: c_int = 6;
    let psi = &info.psi;
    match psi.soi_kind {
        KIND_TCP => {
            let tcp = unsafe { psi.soi_proto.pri_tcp };
            let ini = tcp.tcpsi_ini;
            format_inet(
                "TCP",
                in_endpoint(&ini, true),
                Some(in_endpoint(&ini, false)),
                Some(darwin_tcp_state(tcp.tcpsi_state)),
            )
        }
        KIND_IN => {
            let ini = unsafe { psi.soi_proto.pri_in };
            let proto = match (psi.soi_protocol, psi.soi_type) {
                (libc::IPPROTO_UDP, _) | (_, libc::SOCK_DGRAM) => "UDP",
                (libc::IPPROTO_ICMP, _) | (libc::IPPROTO_ICMPV6, _) => "ICMP",
                _ => "IP",
            };
            format_inet(
                proto,
                in_endpoint(&ini, true),
                Some(in_endpoint(&ini, false)),
                None,
            )
        }
        KIND_UN => {
            let un = unsafe { psi.soi_proto.pri_un };
            let own = c_buf_to_string(unsafe { &un.unsi_addr.ua_sun.sun_path });
            let peer = c_buf_to_string(unsafe { &un.unsi_caddr.ua_sun.sun_path });
            match (own.is_empty(), peer.is_empty()) {
                (false, _) => format!("UNIX {own}"),
                (true, false) => format!("UNIX -> {peer}"),
                (true, true) => "UNIX (unnamed)".into(),
            }
        }
        KIND_KERN_CTL => {
            let ctl = unsafe { psi.soi_proto.pri_kern_ctl };
            format!("kernel control {}", c_buf_to_string(&ctl.kcsi_name))
        }
        KIND_KERN_EVENT => "kernel event socket".into(),
        KIND_NDRV => "network driver socket".into(),
        _ => format!("socket (family {})", psi.soi_family),
    }
}

fn memory_maps(pid: i32) -> Section<Vec<MemoryMap>> {
    let page = match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
        n if n > 0 => n as u64,
        _ => 4096,
    };
    let mut maps = Vec::new();
    let mut addr = 0u64;
    while maps.len() <= MAX_ENTRIES {
        let region = match pidinfo::<RegionWithPathInfo>(pid, PROC_PIDREGIONPATHINFO, addr) {
            Ok(region) => region,
            Err(e) if maps.is_empty() => {
                return Err(match e {
                    libc::EPERM | libc::EACCES => PERMISSION_DENIED.into(),
                    libc::ESRCH => EXITED.into(),
                    _ => "Memory maps require vmmap on macOS".into(),
                });
            }
            Err(_) => break,
        };
        let info = region.info;
        let path = c_buf_to_string(&region.vip.path);
        maps.push(MemoryMap {
            start: info.address,
            end: info.address.saturating_add(info.size),
            perms: mach_perms(info.protection, info.share_mode),
            rss: Some(u64::from(info.pages_resident).saturating_mul(page)),
            path: if path.is_empty() {
                mach_region_label(info.user_tag).into()
            } else {
                path
            },
        });
        let next = info.address.saturating_add(info.size);
        if next <= addr {
            break;
        }
        addr = next;
    }
    if maps.is_empty() {
        return Err("Memory maps require vmmap on macOS".into());
    }
    Ok(maps)
}

fn own_limits() -> Vec<ResourceLimit> {
    const LIMITS: [(c_int, &str, &str); 9] = [
        (libc::RLIMIT_CPU, "Max cpu time", "seconds"),
        (libc::RLIMIT_FSIZE, "Max file size", "bytes"),
        (libc::RLIMIT_DATA, "Max data size", "bytes"),
        (libc::RLIMIT_STACK, "Max stack size", "bytes"),
        (libc::RLIMIT_CORE, "Max core file size", "bytes"),
        (libc::RLIMIT_AS, "Max address space", "bytes"),
        (libc::RLIMIT_MEMLOCK, "Max locked memory", "bytes"),
        (libc::RLIMIT_NPROC, "Max processes", "processes"),
        (libc::RLIMIT_NOFILE, "Max open files", "files"),
    ];
    let value = |v: libc::rlim_t| (v != libc::RLIM_INFINITY).then_some(v);
    LIMITS
        .iter()
        .filter_map(|&(resource, name, unit)| {
            let mut lim: libc::rlimit = unsafe { zeroed() };
            (unsafe { libc::getrlimit(resource, &mut lim) } == 0).then(|| ResourceLimit {
                name: name.into(),
                soft: value(lim.rlim_cur),
                hard: value(lim.rlim_max),
                unit: unit.into(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_layouts_match_sdk() {
        assert_eq!(size_of::<VinfoStat>(), 136);
        assert_eq!(size_of::<VnodeInfo>(), 152);
        assert_eq!(size_of::<VnodeFdInfoWithPath>(), 1200);
        assert_eq!(size_of::<VnodePathInfo>(), 2352);
        assert_eq!(size_of::<RegionInfo>(), 96);
        assert_eq!(size_of::<ProcFdInfo>(), 8);
    }

    #[test]
    fn thread_ids_include_current_thread() {
        let mut tid = 0u64;
        unsafe { libc::pthread_threadid_np(0, &mut tid) };
        let pid = std::process::id() as i32;
        let threads = threads(pid).unwrap();
        assert!(
            threads.iter().any(|t| t.tid == tid as i64),
            "{tid} not in {threads:?}"
        );
    }

    #[test]
    fn names_and_identity_for_foreign_processes() {
        assert_eq!(process_name(0).as_deref(), Some("kernel_task"));
        assert_eq!(process_name(1).as_deref(), Some("launchd"));
        let launchd = Identity::read(1);
        assert_eq!(launchd.uid, Some(0));
        assert_eq!(launchd.ppid, Some(0));
        assert_eq!(Identity::read(-5), Identity::default());
    }

    #[test]
    fn own_limits_cover_open_files() {
        let limits = own_limits();
        let nofile = limits.iter().find(|l| l.name == "Max open files").unwrap();
        assert!(nofile.soft.is_some());
    }

    #[test]
    fn other_pid_limits_explained() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let key = ProcessKey::new(child.id() as i32, 0);
        let d = collect(key);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            d.limits,
            Err("Only available for bproc itself on macOS".into())
        );
        assert_eq!(d.cgroups, Err("Not applicable on macOS".into()));
    }
}
