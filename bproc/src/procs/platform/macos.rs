//! macOS enrichment via libproc's `proc_pidinfo` (state, nice, priority,
//! threads, ctx switches, page-ins, fds, tty). Called through `libc` directly
//! so failures expose `errno` (EPERM vs. exited) without allocating.
//!
//! `major_faults` reports `pti_pageins` (faults that had to read from disk);
//! `pti_faults` also counts soft faults and would not match Linux semantics.
use super::{Enricher, RecordRef};
use crate::procs::model::{ProcState, ProcessSample};
use std::collections::HashMap;
use std::ffi::CStr;
use std::mem::{MaybeUninit, size_of, size_of_val};

/// `NODEV` from `<sys/param.h>`: process has no controlling terminal.
const NODEV: u32 = u32::MAX;

pub struct LibprocEnricher {
    fds: Vec<libc::proc_fdinfo>,
    ttys: HashMap<u32, Option<String>>,
}

impl LibprocEnricher {
    pub fn new() -> Self {
        Self {
            fds: Vec::new(),
            ttys: HashMap::new(),
        }
    }

    fn tty(&mut self, dev: u32) -> Option<String> {
        if dev == 0 || dev == NODEV {
            return None;
        }
        self.ttys.entry(dev).or_insert_with(|| devname(dev)).clone()
    }

    /// Open descriptor count; `nfiles` (the fd table size) bounds the buffer.
    fn count_fds(&mut self, pid: i32, nfiles: u32) -> Option<u32> {
        let slots = (nfiles as usize).clamp(16, 1 << 20);
        if self.fds.len() < slots {
            self.fds.resize(
                slots,
                libc::proc_fdinfo {
                    proc_fd: 0,
                    proc_fdtype: 0,
                },
            );
        }
        let bytes = (slots * size_of::<libc::proc_fdinfo>()).min(i32::MAX as usize) as i32;
        // SAFETY: the buffer holds `slots` initialised entries of the type the
        // PROC_PIDLISTFDS flavour writes, and `bytes` never exceeds its size.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                self.fds.as_mut_ptr().cast(),
                bytes,
            )
        };
        (written > 0).then(|| (written as usize / size_of::<libc::proc_fdinfo>()) as u32)
    }
}

impl Default for LibprocEnricher {
    fn default() -> Self {
        Self::new()
    }
}

/// Fetch one fixed-size `proc_pidinfo` flavour. `Err` carries `errno`.
fn pidinfo<T>(pid: i32, flavor: i32) -> Result<T, i32> {
    let mut out = MaybeUninit::<T>::zeroed();
    let size = size_of::<T>() as i32;
    // SAFETY: `out` is a writable buffer of exactly `size` bytes; the kernel
    // fills it with the C struct `T` mirrors for this flavour.
    let ret = unsafe { libc::proc_pidinfo(pid, flavor, 0, out.as_mut_ptr().cast(), size) };
    if ret == size {
        // SAFETY: zero-initialised plain-old-data fully written by the kernel.
        Ok(unsafe { out.assume_init() })
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::ESRCH))
    }
}

/// Start time (epoch seconds) of `pid` from `sysctl(KERN_PROC_PID)`, which,
/// unlike `PROC_PIDTBSDINFO`, also answers for other users' processes without
/// root. Same clock as sysinfo's `pbi_start_tvsec`. `None` when no such
/// process exists or the call fails.
pub fn start_time_sysctl(pid: i32) -> Option<u64> {
    if pid < 0 {
        return None;
    }
    // `struct kinfo_proc` (648 bytes on 64-bit macOS) is not in `libc`; it
    // starts with `kp_proc.p_un`, a union whose `p_starttime` is a
    // `struct timeval`, so `tv_sec` is the first 8 bytes. The buffer is
    // oversized and 8-byte aligned so any kinfo_proc revision fits.
    let mut buf = [0u64; 128];
    let mut len = size_of_val(&buf);
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    // SAFETY: `mib` names a read-only sysctl; `buf` is writable for `len`
    // bytes and the kernel never writes more than `len`.
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 || len < size_of::<libc::timeval>() {
        return None; // len 0: no such process.
    }
    let sec = buf[0] as i64;
    (sec >= 0).then_some(sec as u64)
}

/// `PROC_PIDT_SHORTBSDINFO` for `pid`: parent, owner and short command name.
/// Readable for every process, including other users', without root.
pub fn short_info(pid: i32) -> Option<libc::proc_bsdshortinfo> {
    pidinfo::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO).ok()
}

/// Parent PID from short info (`None` for launchd / kernel_task, like sysinfo).
pub fn short_ppid(info: &libc::proc_bsdshortinfo) -> Option<i32> {
    match info.pbsi_ppid as i32 {
        0 => None,
        ppid => Some(ppid),
    }
}

/// Short (`MAXCOMLEN`) command name from short info.
pub fn short_comm(info: &libc::proc_bsdshortinfo) -> String {
    let bytes: Vec<u8> = info
        .pbsi_comm
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn devname(dev: u32) -> Option<String> {
    // SAFETY: `devname` returns a pointer to a static NUL-terminated buffer
    // (or NULL); it is copied before any other call can overwrite it.
    let name = unsafe {
        let ptr = libc::devname(dev as libc::dev_t, libc::S_IFCHR);
        if ptr.is_null() {
            return None;
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    };
    (!name.is_empty() && name != "??").then_some(name)
}

/// Process state from the BSD status plus, when the task is inspectable, its
/// number of running threads. Without task access an `SRUN` process is most
/// likely idle, so it is reported as sleeping rather than running.
pub(crate) fn refine_state(status: u32, running_threads: Option<i32>) -> ProcState {
    match status {
        libc::SZOMB => ProcState::Zombie,
        libc::SSTOP => ProcState::Stopped,
        libc::SIDL => ProcState::Idle,
        libc::SSLEEP => ProcState::Sleeping,
        libc::SRUN => match running_threads {
            Some(n) if n > 0 => ProcState::Running,
            _ => ProcState::Sleeping,
        },
        _ => ProcState::Unknown,
    }
}

fn to_u64(value: i32) -> u64 {
    value.max(0) as u64
}

impl Enricher for LibprocEnricher {
    fn enrich(&mut self, mut record: RecordRef<'_>, sample: &mut ProcessSample) -> bool {
        let pid = record.get().key.pid;
        let (bsd, limited) = match pidinfo::<libc::proc_taskallinfo>(pid, libc::PROC_PIDTASKALLINFO)
        {
            Ok(all) => {
                let task = &all.ptinfo;
                sample.state = refine_state(all.pbsd.pbi_status, Some(task.pti_numrunning));
                sample.threads = Some(task.pti_threadnum.max(1) as u32);
                sample.priority = Some(task.pti_priority);
                sample.ctx_switches = Some(to_u64(task.pti_csw));
                sample.major_faults = Some(to_u64(task.pti_pageins));
                sample.open_fds = self.count_fds(pid, all.pbsd.pbi_nfiles);
                (Some(all.pbsd), false)
            }
            Err(errno) => {
                // Zombies have no task but full BSD info; other users'
                // processes only expose the short form (state only).
                let bsd = pidinfo::<libc::proc_bsdinfo>(pid, libc::PROC_PIDTBSDINFO).ok();
                if let Some(bsd) = &bsd {
                    sample.state = refine_state(bsd.pbi_status, None);
                } else if let Ok(short) =
                    pidinfo::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO)
                {
                    sample.state = refine_state(short.pbsi_status, None);
                }
                (bsd, errno == libc::EPERM)
            }
        };
        if let Some(bsd) = bsd {
            sample.nice = Some(bsd.pbi_nice);
            if let Some(new) = record.new_mut() {
                new.tty = self.tty(bsd.e_tdev);
            }
        }
        limited
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::model::{ProcessKey, ProcessRecord};

    #[test]
    fn refines_bsd_states() {
        assert_eq!(refine_state(libc::SRUN, Some(2)), ProcState::Running);
        assert_eq!(refine_state(libc::SRUN, Some(0)), ProcState::Sleeping);
        assert_eq!(refine_state(libc::SRUN, None), ProcState::Sleeping);
        assert_eq!(refine_state(libc::SSTOP, Some(1)), ProcState::Stopped);
        assert_eq!(refine_state(libc::SZOMB, None), ProcState::Zombie);
        assert_eq!(refine_state(libc::SSLEEP, None), ProcState::Sleeping);
        assert_eq!(refine_state(libc::SIDL, None), ProcState::Idle);
        assert_eq!(refine_state(99, None), ProcState::Unknown);
    }

    #[test]
    fn no_tty_for_nodev() {
        let mut enricher = LibprocEnricher::new();
        assert_eq!(enricher.tty(0), None);
        assert_eq!(enricher.tty(NODEV), None);
    }

    #[test]
    fn enriches_own_process() {
        let mut enricher = LibprocEnricher::new();
        let pid = std::process::id() as i32;
        let mut record = ProcessRecord {
            key: ProcessKey::new(pid, 1),
            ..Default::default()
        };
        let mut sample = ProcessSample::default();
        let _files: Vec<std::fs::File> = (0..5)
            .filter_map(|_| std::fs::File::open("/dev/null").ok())
            .collect();
        let limited = enricher.enrich(RecordRef::New(&mut record), &mut sample);
        assert!(!limited);
        assert!(sample.threads.is_some_and(|t| t >= 1));
        assert!(sample.open_fds.is_some_and(|n| n >= 5));
        assert!(sample.ctx_switches.is_some());
        assert!(sample.priority.is_some());
        assert_eq!(sample.nice, Some(0));
        assert!(matches!(
            sample.state,
            ProcState::Running | ProcState::Sleeping
        ));
    }

    #[test]
    fn exited_process_is_not_permission_limited() {
        let mut enricher = LibprocEnricher::new();
        let record = ProcessRecord {
            key: ProcessKey::new(i32::MAX - 7, 1),
            ..Default::default()
        };
        let mut sample = ProcessSample::default();
        assert!(!enricher.enrich(RecordRef::Known(&record), &mut sample));
        assert_eq!(sample, ProcessSample::default());
    }

    fn sysinfo_start(pid: i32) -> u64 {
        let pid = sysinfo::Pid::from_u32(pid as u32);
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        system.process(pid).expect("visible").start_time()
    }

    #[test]
    fn sysctl_start_time_matches_sysinfo() {
        let me = std::process::id() as i32;
        let start = start_time_sysctl(me).expect("own process");
        assert!(start > 0);
        assert_eq!(start, sysinfo_start(me));
        assert_eq!(start_time_sysctl(i32::MAX - 7), None);
        assert_eq!(start_time_sysctl(-1), None);
    }

    #[test]
    fn unprivileged_identity_for_launchd() {
        // Works without root, where sysinfo reports start 0 and no uid.
        assert!(start_time_sysctl(1).is_some_and(|s| s > 0));
        let info = short_info(1).expect("launchd short info");
        assert_eq!(info.pbsi_ruid, 0);
        assert_eq!(short_ppid(&info), None);
        assert_eq!(short_comm(&info), "launchd");
        let me = short_info(std::process::id() as i32).expect("own short info");
        // SAFETY: getuid/getppid have no preconditions.
        assert_eq!(me.pbsi_ruid, unsafe { libc::getuid() });
        assert_eq!(short_ppid(&me), Some(unsafe { libc::getppid() }));
    }

    #[test]
    fn launchd_is_limited_without_root() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let mut enricher = LibprocEnricher::new();
        let mut record = ProcessRecord {
            key: ProcessKey::new(1, 1),
            ..Default::default()
        };
        let mut sample = ProcessSample::default();
        assert!(enricher.enrich(RecordRef::New(&mut record), &mut sample));
        assert_eq!(sample.state, ProcState::Sleeping);
        assert_eq!(sample.threads, None);
        assert_eq!(sample.open_fds, None);
        assert_eq!(record.tty, None);
    }
}
