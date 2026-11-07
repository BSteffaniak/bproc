//! Pure parsers for Linux /proc text. Kept free of I/O so they are unit-tested
//! on every host.
use crate::procs::model::ProcState;

/// `PF_KTHREAD` from `include/linux/sched.h`.
pub const PF_KTHREAD: u64 = 0x0020_0000;
/// PID of `kthreadd`, the parent of every kernel thread.
pub const KTHREADD_PID: i32 = 2;

/// Fields of `/proc/[pid]/stat` the explorer uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStat<'a> {
    pub comm: &'a str,
    pub state: char,
    pub ppid: i32,
    pub tty_nr: i64,
    pub flags: u64,
    pub major_faults: u64,
    pub priority: i64,
    pub nice: i64,
    pub num_threads: u32,
    pub start_ticks: u64,
}

impl ProcStat<'_> {
    pub fn is_kernel_thread(&self, pid: i32) -> bool {
        self.flags & PF_KTHREAD != 0 || pid == KTHREADD_PID || self.ppid == KTHREADD_PID
    }
}

/// Parse `/proc/[pid]/stat`. The command name is delimited by the first `(`
/// and the *last* `)` because it may itself contain spaces and parentheses.
pub fn parse_stat(text: &str) -> Option<ProcStat<'_>> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let comm = &text[open + 1..close];
    // Field numbers below follow proc(5); `rest` starts at field 3 (state).
    let mut rest = text[close + 1..].split_ascii_whitespace();
    let mut field = |n: usize| -> Option<&str> { rest.nth(n) };
    let state = field(0)?.chars().next()?; // 3
    let ppid = field(0)?.parse().ok()?; // 4
    let tty_nr = field(2)?.parse().ok()?; // 7
    let flags = field(1)?.parse().ok()?; // 9
    let major_faults = field(2)?.parse().ok()?; // 12
    let priority = field(5)?.parse().ok()?; // 18
    let nice = field(0)?.parse().ok()?; // 19
    let num_threads = field(0)?.parse().ok()?; // 20
    let start_ticks = field(1)?.parse().ok()?; // 22
    Some(ProcStat {
        comm,
        state,
        ppid,
        tty_nr,
        flags,
        major_faults,
        priority,
        nice,
        num_threads,
        start_ticks,
    })
}

/// Map the single-letter state from `/proc/[pid]/stat`.
pub fn map_state(code: char) -> ProcState {
    match code {
        'R' => ProcState::Running,
        'S' | 'W' | 'K' | 'P' => ProcState::Sleeping,
        'D' => ProcState::DiskSleep,
        'T' => ProcState::Stopped,
        't' => ProcState::Tracing,
        'Z' => ProcState::Zombie,
        'I' => ProcState::Idle,
        'X' | 'x' => ProcState::Dead,
        _ => ProcState::Unknown,
    }
}

/// Voluntary plus involuntary context switches from `/proc/[pid]/status`.
pub fn parse_ctx_switches(status: &str) -> Option<u64> {
    let mut total = None::<u64>;
    for line in status.lines() {
        let value = line
            .strip_prefix("voluntary_ctxt_switches:")
            .or_else(|| line.strip_prefix("nonvoluntary_ctxt_switches:"));
        if let Some(n) = value.and_then(|v| v.trim().parse::<u64>().ok()) {
            total = Some(total.unwrap_or(0).saturating_add(n));
        }
    }
    total
}

/// Decode `tty_nr` (major in bits 8..19, minor in bits 0..7 and 20..31) into
/// a device name such as `pts/3`, `tty1` or `ttyS0`.
pub fn tty_name(tty_nr: i64) -> Option<String> {
    if tty_nr <= 0 {
        return None;
    }
    let nr = tty_nr as u64;
    let major = (nr >> 8) & 0xfff;
    let minor = (nr & 0xff) | ((nr >> 12) & 0xfff00);
    Some(match major {
        136..=143 => format!("pts/{}", (major - 136) * 256 + minor),
        4 if minor < 64 => format!("tty{minor}"),
        4 => format!("ttyS{}", minor - 64),
        5 if minor == 0 => "tty".into(),
        5 if minor == 1 => "console".into(),
        166 => format!("ttyACM{minor}"),
        188 => format!("ttyUSB{minor}"),
        204 if minor >= 64 => format!("ttyAMA{}", minor - 64),
        _ => format!("{major}:{minor}"),
    })
}

/// The most specific cgroup path in `/proc/[pid]/cgroup`: the v2 unified
/// entry when present, else the systemd (or first non-root) v1 hierarchy.
pub fn cgroup_path(text: &str) -> Option<&str> {
    let mut v1 = None;
    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if id == "0" && controllers.is_empty() {
            return Some(path);
        }
        if controllers == "name=systemd" || (v1.is_none() && path != "/") {
            v1 = Some(path);
        }
    }
    v1
}

/// Human label for a cgroup path: container runtime + short id, Kubernetes
/// pod, LXC container or systemd unit. `None` for the root cgroup.
pub fn cgroup_label(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path == "/" {
        return None;
    }
    if let Some(pod) = kube_pod(path) {
        return Some(format!("kubepods/{pod}"));
    }
    for segment in path.rsplit('/') {
        let unit = segment.strip_suffix(".scope").unwrap_or(segment);
        let runtime = [
            ("docker-", "docker"),
            ("cri-containerd-", "containerd"),
            ("crio-", "cri-o"),
            ("libpod-conmon-", "podman"),
            ("libpod-", "podman"),
        ];
        for (prefix, label) in runtime {
            if let Some(id) = unit.strip_prefix(prefix).and_then(container_id) {
                return Some(format!("{label}/{id}"));
            }
        }
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for pair in segments.windows(2) {
        let label = match pair[0] {
            "docker" => "docker",
            "containerd" => "containerd",
            "libpod_parent" | "machine.slice" => "podman",
            "lxc" => {
                return Some(format!("lxc/{}", pair[1]));
            }
            _ => continue,
        };
        if let Some(id) = container_id(pair[1].strip_prefix("libpod-").unwrap_or(pair[1])) {
            return Some(format!("{label}/{id}"));
        }
    }
    if let Some(name) = segments.iter().find_map(|s| s.strip_prefix("lxc.payload.")) {
        return Some(format!("lxc/{name}"));
    }
    segments
        .iter()
        .rev()
        .find(|s| s.ends_with(".service") || s.ends_with(".scope"))
        .or(segments.last())
        .map(|s| (*s).to_string())
}

/// First 12 characters of a hex container id (at least 12 long).
fn container_id(text: &str) -> Option<String> {
    let id = text.strip_suffix(".scope").unwrap_or(text);
    (id.len() >= 12 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| id[..12].to_string())
}

/// Pod UID (first 8 characters) from cgroupfs (`/kubepods/burstable/pod<uid>`)
/// or systemd (`kubepods-burstable-pod<uid_with_underscores>.slice`) paths.
fn kube_pod(path: &str) -> Option<String> {
    if !path.contains("kubepods") {
        return None;
    }
    path.split('/').find_map(|segment| {
        let segment = segment.strip_suffix(".slice").unwrap_or(segment);
        let uid = segment
            .strip_prefix("pod")
            .or_else(|| segment.rsplit_once("-pod").map(|(_, uid)| uid))?;
        let uid: String = uid
            .chars()
            .map(|c| if c == '_' { '-' } else { c })
            .take(8)
            .collect();
        (!uid.is_empty()).then_some(uid)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str = "4242 (Web Content) S 4100 4100 4100 34817 4100 4194560 92833 0 512 0 \
        8123 1290 0 0 20 0 27 0 1234567 3010502656 61234 18446744073709551615 1 1 0 0 0 0 0 \
        16781312 1082195196 0 0 0 17 3 0 0 0 0 0";

    #[test]
    fn parses_stat_with_spaces_in_comm() {
        let stat = parse_stat(FIREFOX).expect("parse");
        assert_eq!(stat.comm, "Web Content");
        assert_eq!(stat.state, 'S');
        assert_eq!(stat.ppid, 4100);
        assert_eq!(stat.tty_nr, 34817);
        assert_eq!(stat.flags, 4194560);
        assert_eq!(stat.major_faults, 512);
        assert_eq!(stat.priority, 20);
        assert_eq!(stat.nice, 0);
        assert_eq!(stat.num_threads, 27);
        assert_eq!(stat.start_ticks, 1234567);
        assert!(!stat.is_kernel_thread(4242));
        assert_eq!(tty_name(stat.tty_nr).as_deref(), Some("pts/1"));
    }

    #[test]
    fn parses_stat_with_parens_in_comm() {
        let text = "77 (a) (b)) R 1 77 77 0 -1 4194304 10 0 3 0 5 6 0 0 39 19 1 0 999 0 0";
        let stat = parse_stat(text).expect("parse");
        assert_eq!(stat.comm, "a) (b)");
        assert_eq!(stat.state, 'R');
        assert_eq!(stat.tty_nr, 0);
        assert_eq!(stat.major_faults, 3);
        assert_eq!(stat.priority, 39);
        assert_eq!(stat.nice, 19);
        assert_eq!(stat.start_ticks, 999);
    }

    #[test]
    fn detects_kernel_threads() {
        let kworker = "123 (kworker/0:1-events) I 2 0 0 0 -1 69238880 0 0 0 0 0 1 0 0 20 0 1 0 \
            30 0 0 18446744073709551615 0 0 0 0 0 0 0 2147483647 0 0 0 0 17 0 0 0 0 0 0";
        let stat = parse_stat(kworker).expect("parse");
        assert!(stat.flags & PF_KTHREAD != 0);
        assert!(stat.is_kernel_thread(123));
        assert_eq!(map_state(stat.state), ProcState::Idle);
        let kthreadd = "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 2 0 0";
        assert!(parse_stat(kthreadd).expect("parse").is_kernel_thread(2));
    }

    #[test]
    fn rejects_malformed_stat() {
        assert!(parse_stat("").is_none());
        assert!(parse_stat("1 (init").is_none());
        assert!(parse_stat("1 ) init ( S").is_none());
        assert!(parse_stat("1 (init) S 0 1").is_none());
        assert!(parse_stat("1 (init) S x 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5").is_none());
    }

    #[test]
    fn maps_every_state_letter() {
        let cases = [
            ('R', ProcState::Running),
            ('S', ProcState::Sleeping),
            ('D', ProcState::DiskSleep),
            ('T', ProcState::Stopped),
            ('t', ProcState::Tracing),
            ('Z', ProcState::Zombie),
            ('X', ProcState::Dead),
            ('I', ProcState::Idle),
            ('P', ProcState::Sleeping),
            ('?', ProcState::Unknown),
        ];
        for (code, state) in cases {
            assert_eq!(map_state(code), state, "{code}");
        }
    }

    #[test]
    fn sums_context_switches() {
        let status = "Name:\tbash\nState:\tS (sleeping)\nThreads:\t1\n\
            voluntary_ctxt_switches:\t150\nnonvoluntary_ctxt_switches:\t7\n";
        assert_eq!(parse_ctx_switches(status), Some(157));
        assert_eq!(parse_ctx_switches("Name:\tbash\n"), None);
    }

    #[test]
    fn names_ttys() {
        assert_eq!(tty_name(0), None);
        assert_eq!(tty_name(-1), None);
        assert_eq!(tty_name(34816).as_deref(), Some("pts/0"));
        // pts/300: major 137, minor 44.
        assert_eq!(tty_name((137 << 8) | 44).as_deref(), Some("pts/300"));
        // Minor above 255 lives in bits 20..31.
        assert_eq!(tty_name((136 << 8) | (1 << 20)).as_deref(), Some("pts/256"));
        assert_eq!(tty_name(1025).as_deref(), Some("tty1"));
        assert_eq!(tty_name(1088).as_deref(), Some("ttyS0"));
        assert_eq!(tty_name(1281).as_deref(), Some("console"));
        assert_eq!(tty_name((188 << 8) | 2).as_deref(), Some("ttyUSB2"));
        assert_eq!(tty_name((200 << 8) | 3).as_deref(), Some("200:3"));
    }

    #[test]
    fn picks_cgroup_path() {
        assert_eq!(
            cgroup_path("0::/user.slice/user-1000.slice/session-2.scope\n"),
            Some("/user.slice/user-1000.slice/session-2.scope")
        );
        let v1 = "12:pids:/system.slice/ssh.service\n11:cpu,cpuacct:/\n\
            1:name=systemd:/system.slice/ssh.service\n";
        assert_eq!(cgroup_path(v1), Some("/system.slice/ssh.service"));
        let hybrid = "1:name=systemd:/init.scope\n0::/init.scope\n";
        assert_eq!(cgroup_path(hybrid), Some("/init.scope"));
        assert_eq!(cgroup_path(""), None);
        assert_eq!(cgroup_path("garbage"), None);
    }

    #[test]
    fn labels_containers_and_units() {
        let id = "4f3c2a1b0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3b";
        let cases: Vec<(String, Option<&str>)> = vec![
            ("/".into(), None),
            (format!("/docker/{id}"), Some("docker/4f3c2a1b0e9d")),
            (
                format!("/system.slice/docker-{id}.scope"),
                Some("docker/4f3c2a1b0e9d"),
            ),
            (
                format!("/system.slice/containerd.service/cri-containerd-{id}.scope"),
                Some("containerd/4f3c2a1b0e9d"),
            ),
            (
                format!("/machine.slice/libpod-{id}.scope/container"),
                Some("podman/4f3c2a1b0e9d"),
            ),
            (
                format!(
                    "/user.slice/user-1000.slice/user@1000.service/user.slice/libpod-{id}.scope"
                ),
                Some("podman/4f3c2a1b0e9d"),
            ),
            (
                format!("/kubepods/burstable/pod8f1e2d3c-aaaa-bbbb-cccc-123456789abc/{id}"),
                Some("kubepods/8f1e2d3c"),
            ),
            (
                format!(
                    "/kubepods.slice/kubepods-besteffort.slice/\
                     kubepods-besteffort-pod1a2b3c4d_eeee_ffff.slice/cri-containerd-{id}.scope"
                ),
                Some("kubepods/1a2b3c4d"),
            ),
            ("/lxc/web01".into(), Some("lxc/web01")),
            ("/lxc.payload.db/init.scope".into(), Some("lxc/db")),
            ("/system.slice/nginx.service".into(), Some("nginx.service")),
            ("/init.scope".into(), Some("init.scope")),
            (
                "/user.slice/user-1000.slice/user@1000.service/app.slice/app-firefox-12.scope"
                    .into(),
                Some("app-firefox-12.scope"),
            ),
            (
                "/user.slice/user-1000.slice".into(),
                Some("user-1000.slice"),
            ),
            ("/docker/not-hex".into(), Some("not-hex")),
        ];
        for (path, expected) in cases {
            assert_eq!(cgroup_label(&path).as_deref(), expected, "{path}");
        }
    }
}
