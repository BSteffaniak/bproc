//! Parsers for Linux `/proc` text.
use super::super::{FileKind, MemoryMap, ResourceLimit};
use super::{format_inet, split_env};
use crate::procs::model::ProcState;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Fields of `/proc/<pid>/stat` (or `/proc/<pid>/task/<tid>/stat`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stat {
    pub comm: String,
    pub state: char,
    pub ppid: i32,
    pub pgrp: i32,
    pub session: i32,
    pub tty_nr: i64,
    /// Clock ticks.
    pub utime: u64,
    pub stime: u64,
    pub priority: i64,
    pub nice: i64,
    pub num_threads: i64,
    pub starttime: u64,
}

/// Parse a stat line. `comm` may contain spaces and parentheses, so fields
/// are split after the last `)`.
pub fn parse_stat(text: &str) -> Option<Stat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let comm = text[open + 1..close].to_string();
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // rest[0] is field 3 (state); field N lives at rest[N - 3].
    let field = |n: usize| rest.get(n - 3).copied();
    let num = |n: usize| field(n).and_then(|v| v.parse::<i64>().ok());
    let unum = |n: usize| field(n).and_then(|v| v.parse::<u64>().ok());
    Some(Stat {
        comm,
        state: field(3)?.chars().next()?,
        ppid: num(4)? as i32,
        pgrp: num(5).unwrap_or(0) as i32,
        session: num(6).unwrap_or(0) as i32,
        tty_nr: num(7).unwrap_or(0),
        utime: unum(14).unwrap_or(0),
        stime: unum(15).unwrap_or(0),
        priority: num(18).unwrap_or(0),
        nice: num(19).unwrap_or(0),
        num_threads: num(20).unwrap_or(0),
        starttime: unum(22).unwrap_or(0),
    })
}

/// Linux state letter to `ProcState`.
pub fn linux_state(code: char) -> ProcState {
    match code {
        'R' => ProcState::Running,
        'S' => ProcState::Sleeping,
        'D' => ProcState::DiskSleep,
        'T' => ProcState::Stopped,
        't' => ProcState::Tracing,
        'Z' => ProcState::Zombie,
        'I' => ProcState::Idle,
        'X' | 'x' => ProcState::Dead,
        _ => ProcState::Unknown,
    }
}

/// Decode a Linux `tty_nr` (encoded dev_t) to a device name.
pub fn linux_tty_name(tty_nr: i64) -> Option<String> {
    if tty_nr <= 0 {
        return None;
    }
    let dev = tty_nr as u64;
    let major = (dev >> 8) & 0xfff;
    let minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
    Some(match major {
        4 if minor < 64 => format!("tty{minor}"),
        4 => format!("ttyS{}", minor - 64),
        5 if minor == 0 => "tty".into(),
        5 if minor == 1 => "console".into(),
        136..=143 => format!("pts/{}", (major - 136) * 256 + minor),
        _ => format!("dev {major}:{minor}"),
    })
}

/// Classification of a `/proc/<pid>/fd/<n>` link target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdLink {
    pub kind: FileKind,
    pub target: String,
    /// Socket inode to resolve through `/proc/net/*`.
    pub socket_inode: Option<u64>,
}

/// Classify a readlink result. `hint` is the file type from `stat` on the
/// fd (used to tell directories, devices and FIFOs apart from files).
pub fn classify_fd_link(link: &str, hint: Option<FileKind>) -> FdLink {
    let plain = |kind: FileKind, target: String| FdLink {
        kind,
        target,
        socket_inode: None,
    };
    if let Some(inode) = bracketed(link, "socket:") {
        return FdLink {
            kind: FileKind::Socket,
            target: link.to_string(),
            socket_inode: inode.parse().ok(),
        };
    }
    if bracketed(link, "pipe:").is_some() {
        return plain(FileKind::Pipe, link.to_string());
    }
    if let Some(name) = link.strip_prefix("anon_inode:") {
        let name = name.trim_start_matches('[').trim_end_matches(']');
        let kind = match name {
            "eventfd" | "eventpoll" | "inotify" | "timerfd" | "signalfd" | "fanotify"
            | "io_uring" | "pidfd" | "userfaultfd" => FileKind::Event,
            _ => FileKind::Other,
        };
        let label = match name {
            "eventpoll" => "epoll",
            other => other,
        };
        return plain(kind, label.to_string());
    }
    if link.starts_with('/') {
        let kind = match hint {
            Some(kind) => kind,
            None if link.starts_with("/dev/") && !link.starts_with("/dev/shm/") => FileKind::Device,
            None => FileKind::File,
        };
        return plain(kind, link.to_string());
    }
    plain(hint.unwrap_or(FileKind::Other), link.to_string())
}

fn bracketed<'a>(link: &'a str, prefix: &str) -> Option<&'a str> {
    link.strip_prefix(prefix)?
        .strip_prefix('[')?
        .strip_suffix(']')
}

/// TCP state names as used by `/proc/net/tcp` (hex code) and Darwin
/// `tcpsi_state` (0-based).
pub fn linux_tcp_state(code: u8) -> &'static str {
    match code {
        0x01 => "ESTABLISHED",
        0x02 => "SYN_SENT",
        0x03 => "SYN_RECV",
        0x04 => "FIN_WAIT1",
        0x05 => "FIN_WAIT2",
        0x06 => "TIME_WAIT",
        0x07 => "CLOSE",
        0x08 => "CLOSE_WAIT",
        0x09 => "LAST_ACK",
        0x0A => "LISTEN",
        0x0B => "CLOSING",
        0x0C => "NEW_SYN_RECV",
        _ => "",
    }
}

/// Parse a `/proc/net` hex endpoint such as `0100007F:1538`. Address words
/// are printed in host byte order, so they are decoded with native endianness.
pub fn parse_proc_endpoint(text: &str) -> Option<SocketAddr> {
    let (addr, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let word = |s: &str| u32::from_str_radix(s, 16).ok().map(u32::to_ne_bytes);
    let ip = match addr.len() {
        8 => IpAddr::V4(Ipv4Addr::from(word(addr)?)),
        32 => {
            let mut bytes = [0u8; 16];
            for (i, chunk) in bytes.chunks_mut(4).enumerate() {
                chunk.copy_from_slice(&word(addr.get(i * 8..i * 8 + 8)?)?);
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Map socket inode to description from `/proc/net/{tcp,tcp6,udp,udp6}`.
/// `proto` is `"TCP"` or `"UDP"`.
pub fn parse_net_inet(text: &str, proto: &str) -> HashMap<u64, String> {
    let tcp = proto == "TCP";
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let local = parse_proc_endpoint(cols.get(1)?)?;
            let remote = parse_proc_endpoint(cols.get(2)?)?;
            let state = u8::from_str_radix(cols.get(3)?, 16).ok()?;
            let inode: u64 = cols.get(9)?.parse().ok()?;
            if inode == 0 {
                return None;
            }
            let state = if tcp {
                linux_tcp_state(state)
            } else if state == 0x01 {
                "CONNECTED"
            } else {
                ""
            };
            Some((inode, format_inet(proto, local, Some(remote), Some(state))))
        })
        .collect()
}

/// Map socket inode to description from `/proc/net/unix`.
pub fn parse_net_unix(text: &str) -> HashMap<u64, String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let inode: u64 = cols.get(6)?.parse().ok()?;
            let kind = match cols.get(4).copied() {
                Some("0002") => " dgram",
                Some("0005") => " seqpacket",
                _ => "",
            };
            let path = cols.get(7..).map(|p| p.join(" ")).unwrap_or_default();
            let label = if path.is_empty() {
                format!("UNIX{kind} (unnamed)")
            } else {
                format!("UNIX{kind} {path}")
            };
            Some((inode, label))
        })
        .collect()
}

/// NUL-separated `KEY=value` list (`/proc/<pid>/environ`).
pub fn parse_environ(bytes: &[u8]) -> Vec<(String, String)> {
    bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(split_env)
        .collect()
}

/// One `/proc/<pid>/maps` line.
pub fn parse_maps_line(line: &str) -> Option<MemoryMap> {
    let mut cols = line.split_whitespace();
    let (start, end) = cols.next()?.split_once('-')?;
    let perms = cols.next()?.to_string();
    let _offset = cols.next()?;
    let _dev = cols.next()?;
    let _inode = cols.next()?;
    let path = cols.collect::<Vec<_>>().join(" ");
    Some(MemoryMap {
        start: u64::from_str_radix(start, 16).ok()?,
        end: u64::from_str_radix(end, 16).ok()?,
        perms,
        rss: None,
        path: if path.is_empty() {
            "[anon]".into()
        } else {
            path
        },
    })
}

pub fn parse_maps(text: &str, limit: usize) -> Vec<MemoryMap> {
    text.lines()
        .filter_map(parse_maps_line)
        .take(limit)
        .collect()
}

/// `/proc/<pid>/smaps`: maps headers followed by `Key: value kB` lines; the
/// `Rss` line fills `rss` in bytes.
pub fn parse_smaps(text: &str, limit: usize) -> Vec<MemoryMap> {
    let mut maps: Vec<MemoryMap> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Rss:") {
            if let Some(last) = maps.last_mut() {
                last.rss = parse_kb(rest);
            }
        } else if line
            .split_whitespace()
            .next()
            .is_some_and(|c| c.contains('-'))
        {
            if maps.len() >= limit {
                break;
            }
            if let Some(map) = parse_maps_line(line) {
                maps.push(map);
            }
        }
    }
    maps
}

fn parse_kb(text: &str) -> Option<u64> {
    let mut parts = text.split_whitespace();
    let value: u64 = parts.next()?.parse().ok()?;
    match parts.next() {
        Some("kB") | None => Some(value.saturating_mul(1024)),
        Some("mB") => Some(value.saturating_mul(1024 * 1024)),
        _ => Some(value),
    }
}

/// `/proc/<pid>/limits`. Columns are located from the header so names with
/// spaces ("Max open files") and blank units parse correctly.
pub fn parse_limits(text: &str) -> Vec<ResourceLimit> {
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let (Some(soft_at), Some(hard_at), Some(units_at)) = (
        header.find("Soft Limit"),
        header.find("Hard Limit"),
        header.find("Units"),
    ) else {
        return Vec::new();
    };
    let column = |line: &str, from: usize, to: usize| -> String {
        let to = to.min(line.len());
        line.get(from.min(to)..to).unwrap_or("").trim().to_string()
    };
    lines
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let name = column(line, 0, soft_at);
            if name.is_empty() {
                return None;
            }
            let soft = column(line, soft_at, hard_at);
            let hard = column(line, hard_at, units_at);
            let unit = column(line, units_at, line.len());
            Some(ResourceLimit {
                name,
                soft: parse_limit_value(&soft),
                hard: parse_limit_value(&hard),
                unit,
            })
        })
        .collect()
}

fn parse_limit_value(text: &str) -> Option<u64> {
    if text == "unlimited" {
        None
    } else {
        text.parse().ok()
    }
}

/// `/proc/<pid>/cgroup` lines: v2 entries render as the path, v1 entries as
/// `controllers: path`.
pub fn parse_cgroups(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let _id = parts.next()?;
            let controllers = parts.next()?;
            let path = parts.next()?;
            Some(if controllers.is_empty() {
                path.to_string()
            } else {
                format!("{controllers}: {path}")
            })
        })
        .collect()
}

/// Real uid and gid from `/proc/<pid>/status`.
/// Boot time (seconds since the epoch) from the `btime` line of `/proc/stat`.
pub fn parse_btime(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
}

pub fn parse_status_ids(text: &str) -> (Option<u32>, Option<u32>) {
    let first = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
    };
    (first("Uid:"), first("Gid:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "4242 (tmux: server (x)) S 1 4242 4242 34816 4242 4194560 1056 0 0 0 \
        150 75 0 0 20 0 3 0 123456 15302656 812 18446744073709551615 1 1 0 0 0 0 0 \
        3670016 1266777851 0 0 0 17 2 0 0 0 0 0";

    #[test]
    fn stat_handles_parens_in_comm() {
        let s = parse_stat(STAT).unwrap();
        assert_eq!(s.comm, "tmux: server (x)");
        assert_eq!(s.state, 'S');
        assert_eq!((s.ppid, s.pgrp, s.session), (1, 4242, 4242));
        assert_eq!(s.tty_nr, 34816);
        assert_eq!((s.utime, s.stime), (150, 75));
        assert_eq!((s.priority, s.nice, s.num_threads), (20, 0, 3));
        assert_eq!(s.starttime, 123456);
        assert!(parse_stat("garbage").is_none());
        assert!(parse_stat("1 (x) ").is_none());
        assert!(parse_stat(") broken (").is_none());
    }

    #[test]
    fn tty_names() {
        assert_eq!(linux_tty_name(34816).as_deref(), Some("pts/0"));
        assert_eq!(linux_tty_name(34817).as_deref(), Some("pts/1"));
        assert_eq!(linux_tty_name(1025).as_deref(), Some("tty1"));
        assert_eq!(linux_tty_name(1088).as_deref(), Some("ttyS0"));
        assert_eq!(linux_tty_name(0), None);
        assert_eq!(linux_tty_name(-1), None);
        // pts/300: major 137, minor 44 -> (137-136)*256 + 44
        let dev = (137 << 8) | 44;
        assert_eq!(linux_tty_name(dev).as_deref(), Some("pts/300"));
    }

    #[test]
    fn fd_links_classify() {
        let s = classify_fd_link("socket:[98765]", None);
        assert_eq!(s.kind, FileKind::Socket);
        assert_eq!(s.socket_inode, Some(98765));
        let p = classify_fd_link("pipe:[1234]", Some(FileKind::Pipe));
        assert_eq!((p.kind, p.target.as_str()), (FileKind::Pipe, "pipe:[1234]"));
        assert_eq!(s.target, "socket:[98765]");
        let e = classify_fd_link("anon_inode:[eventfd]", None);
        assert_eq!((e.kind, e.target.as_str()), (FileKind::Event, "eventfd"));
        let ep = classify_fd_link("anon_inode:[eventpoll]", None);
        assert_eq!((ep.kind, ep.target.as_str()), (FileKind::Event, "epoll"));
        let i = classify_fd_link("anon_inode:inotify", None);
        assert_eq!(i.kind, FileKind::Event);
        let o = classify_fd_link("anon_inode:[perf_event]", None);
        assert_eq!(o.kind, FileKind::Other);
        let d = classify_fd_link("/dev/pts/3", None);
        assert_eq!(d.kind, FileKind::Device);
        let f = classify_fd_link("/home/u/log.txt (deleted)", None);
        assert_eq!(
            (f.kind, f.target.as_str()),
            (FileKind::File, "/home/u/log.txt (deleted)")
        );
        let dir = classify_fd_link("/home/u", Some(FileKind::Directory));
        assert_eq!(dir.kind, FileKind::Directory);
        let shm = classify_fd_link("/dev/shm/x", None);
        assert_eq!(shm.kind, FileKind::File);
        let odd = classify_fd_link("net:[4026531992]", None);
        assert_eq!(odd.kind, FileKind::Other);
    }

    // Realistic little-endian samples captured from a Linux host.
    const NET_TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000   112        0 23456 1 0000000000000000 100 0 0 10 0
   1: 0F02000A:0016 0202000A:C7B2 01 00000000:00000000 02:0009B5A1 00000000     0        0 34567 4 0000000000000000 20 4 30 10 -1
   2: 0F02000A:0017 0202000A:C7B3 06 00000000:00000000 03:00000000 00000000     0        0 0 3 0000000000000000
";

    const NET_TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 45678 1 0000000000000000 100 0 0 10 0
   1: 0000000000000000FFFF00000100007F:1F90 0000000000000000FFFF00000100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 56789 1 0000000000000000 20 4 0 10 -1
";

    const NET_UDP: &str = "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  128: 3500007F:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000   101        0 13579 2 0000000000000000 0
  200: 0F02000A:D80B 08080808:0035 01 00000000:00000000 00:00000000 00000000  1000        0 24680 2 0000000000000000 0
";

    const NET_UNIX: &str = "Num       RefCount Protocol Flags    Type St Inode Path
0000000000000000: 00000002 00000000 00010000 0001 01 11111 /run/systemd/notify
0000000000000000: 00000003 00000000 00000000 0001 03 22222
0000000000000000: 00000002 00000000 00000000 0002 01 33333 @/tmp/.X11-unix/X0
0000000000000000: 00000002 00000000 00000000 0001 01 44444 /tmp/dir with space/s.sock
";

    #[test]
    fn proc_net_tables() {
        let tcp = parse_net_inet(NET_TCP, "TCP");
        assert_eq!(tcp[&23456], "TCP 127.0.0.1:5432 LISTEN");
        assert_eq!(
            tcp[&34567],
            "TCP 10.0.2.15:22 -> 10.0.2.2:51122 ESTABLISHED"
        );
        assert_eq!(tcp.len(), 2, "inode 0 (TIME_WAIT) skipped");
        let tcp6 = parse_net_inet(NET_TCP6, "TCP");
        assert_eq!(tcp6[&45678], "TCP6 [::1]:80 LISTEN");
        assert_eq!(
            tcp6[&56789],
            "TCP6 127.0.0.1:8080 -> 127.0.0.1:54321 ESTABLISHED"
        );
        let udp = parse_net_inet(NET_UDP, "UDP");
        assert_eq!(udp[&13579], "UDP 127.0.0.53:53");
        assert_eq!(udp[&24680], "UDP 10.0.2.15:55307 -> 8.8.8.8:53 CONNECTED");
        let unix = parse_net_unix(NET_UNIX);
        assert_eq!(unix[&11111], "UNIX /run/systemd/notify");
        assert_eq!(unix[&22222], "UNIX (unnamed)");
        assert_eq!(unix[&33333], "UNIX dgram @/tmp/.X11-unix/X0");
        assert_eq!(unix[&44444], "UNIX /tmp/dir with space/s.sock");
        assert!(parse_net_inet("", "TCP").is_empty());
        assert!(parse_net_inet("header\n  0: zz:zz", "TCP").is_empty());
        assert!(parse_proc_endpoint("0100007F").is_none());
        assert!(parse_proc_endpoint("01007F:0016").is_none());
    }

    #[test]
    fn environ_parses() {
        let env = parse_environ(b"PATH=/usr/bin:/bin\0HOME=/root\0EMPTY=\0EQ=a=b\0junk\0\0");
        assert_eq!(
            env,
            vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), "/root".into()),
                ("EMPTY".into(), "".into()),
                ("EQ".into(), "a=b".into()),
            ]
        );
        assert!(parse_environ(b"").is_empty());
        assert_eq!(parse_environ(b"=C:=C:\\x")[0].0, "=C:");
    }

    const MAPS: &str = "\
55d0c2a00000-55d0c2a28000 r--p 00000000 08:01 1048602                    /usr/bin/bash
55d0c2a28000-55d0c2ad9000 r-xp 00028000 08:01 1048602                    /usr/bin/bash
55d0c3b5e000-55d0c3cc5000 rw-p 00000000 00:00 0                          [heap]
7f1a2c000000-7f1a2c021000 rw-p 00000000 00:00 0
7ffd6f3a1000-7ffd6f3c2000 rw-p 00000000 00:00 0                          [stack]
7f1a2d000000-7f1a2d001000 r--s 00000000 08:01 42                         /tmp/my file (deleted)
";

    #[test]
    fn maps_parse() {
        let maps = parse_maps(MAPS, 100);
        assert_eq!(maps.len(), 6);
        assert_eq!(maps[0].start, 0x55d0c2a00000);
        assert_eq!(maps[0].end, 0x55d0c2a28000);
        assert_eq!(maps[1].perms, "r-xp");
        assert_eq!(maps[2].path, "[heap]");
        assert_eq!(maps[3].path, "[anon]");
        assert_eq!(maps[5].path, "/tmp/my file (deleted)");
        assert_eq!(parse_maps(MAPS, 2).len(), 2);
        assert!(parse_maps("nonsense\n", 10).is_empty());
    }

    #[test]
    fn smaps_parse_rss() {
        let text = "\
55d0c2a00000-55d0c2a28000 r--p 00000000 08:01 1048602                    /usr/bin/bash
Size:                160 kB
KernelPageSize:        4 kB
Rss:                 152 kB
Pss:                  40 kB
VmFlags: rd mr mw me dw sd
7ffd6f3a1000-7ffd6f3c2000 rw-p 00000000 00:00 0                          [stack]
Size:                132 kB
Rss:                  16 kB
VmFlags: rd wr mr mw me gd ac
";
        let maps = parse_smaps(text, 100);
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[0].rss, Some(152 * 1024));
        assert_eq!(maps[1].rss, Some(16 * 1024));
        assert_eq!(maps[1].path, "[stack]");
        assert_eq!(parse_smaps(text, 1).len(), 1);
    }

    const LIMITS: &str = "\
Limit                     Soft Limit           Hard Limit           Units
Max cpu time              unlimited            unlimited            seconds
Max file size             unlimited            unlimited            bytes
Max stack size            8388608              unlimited            bytes
Max open files            1024                 1048576              files
Max nice priority         0                    0
Max realtime timeout      unlimited            unlimited            us
";

    #[test]
    fn limits_parse() {
        let limits = parse_limits(LIMITS);
        assert_eq!(limits.len(), 6);
        assert_eq!(limits[0].name, "Max cpu time");
        assert_eq!((limits[0].soft, limits[0].hard), (None, None));
        assert_eq!(limits[0].unit, "seconds");
        assert_eq!(limits[2].soft, Some(8_388_608));
        assert_eq!(limits[3].name, "Max open files");
        assert_eq!(
            (limits[3].soft, limits[3].hard),
            (Some(1024), Some(1_048_576))
        );
        assert_eq!(limits[4].unit, "");
        assert_eq!(limits[5].unit, "us");
        assert!(parse_limits("").is_empty());
        assert!(parse_limits("no header here\nMax x 1 2 y").is_empty());
    }

    #[test]
    fn cgroups_and_status() {
        let stat = "cpu  1 2 3\nintr 5 6\nctxt 99\nbtime 1700000000\nprocesses 42\n";
        assert_eq!(parse_btime(stat), Some(1_700_000_000));
        assert_eq!(parse_btime("cpu 1\n"), None);
        let cg = parse_cgroups(
            "0::/user.slice/user-1000.slice/session-2.scope\n12:memory:/docker/abc\n1:name=systemd:/init.scope\n",
        );
        assert_eq!(
            cg,
            vec![
                "/user.slice/user-1000.slice/session-2.scope",
                "memory: /docker/abc",
                "name=systemd: /init.scope"
            ]
        );
        let status = "Name:\tbash\nUid:\t1000\t1000\t1000\t1000\nGid:\t100\t100\t100\t100\n";
        assert_eq!(parse_status_ids(status), (Some(1000), Some(100)));
        assert_eq!(parse_status_ids(""), (None, None));
    }

    #[test]
    fn states_and_tcp_codes() {
        assert_eq!(linux_state('R'), ProcState::Running);
        assert_eq!(linux_state('D'), ProcState::DiskSleep);
        assert_eq!(linux_state('t'), ProcState::Tracing);
        assert_eq!(linux_state('?'), ProcState::Unknown);
        assert_eq!(linux_tcp_state(0x0A), "LISTEN");
        assert_eq!(linux_tcp_state(0xFF), "");
    }
}
