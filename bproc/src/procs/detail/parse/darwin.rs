//! Parsers for Darwin `KERN_PROCARGS2` buffers and Mach records.
use super::split_env;
use crate::procs::model::ProcState;

/// Mach `pth_run_state` / `pth_flags` to `ProcState`.
pub fn mach_thread_state(run_state: i32, flags: i32) -> ProcState {
    const TH_FLAGS_IDLE: i32 = 0x2;
    match run_state {
        1 => ProcState::Running,
        2 => ProcState::Stopped,
        3 if flags & TH_FLAGS_IDLE != 0 => ProcState::Idle,
        3 => ProcState::Sleeping,
        4 => ProcState::DiskSleep,
        5 => ProcState::Dead,
        _ => ProcState::Unknown,
    }
}

pub fn darwin_tcp_state(code: i32) -> &'static str {
    match code {
        0 => "CLOSED",
        1 => "LISTEN",
        2 => "SYN_SENT",
        3 => "SYN_RECEIVED",
        4 => "ESTABLISHED",
        5 => "CLOSE_WAIT",
        6 => "FIN_WAIT_1",
        7 => "CLOSING",
        8 => "LAST_ACK",
        9 => "FIN_WAIT_2",
        10 => "TIME_WAIT",
        _ => "",
    }
}

/// Decoded `KERN_PROCARGS2` buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcArgs {
    pub exec_path: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Layout: `int argc`, exec path, NUL padding, `argc` NUL-terminated args,
/// then NUL-terminated `KEY=value` env strings up to an empty string (Apple
/// strings follow and are skipped). Truncated buffers yield what was read.
pub fn parse_procargs2(buf: &[u8]) -> Option<ProcArgs> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let mut rest = &buf[4..];
    let take = |rest: &mut &[u8]| -> Option<String> {
        if rest.is_empty() {
            return None;
        }
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let s = String::from_utf8_lossy(&rest[..end]).into_owned();
        *rest = rest.get(end + 1..).unwrap_or(&[]);
        Some(s)
    };
    let exec_path = take(&mut rest)?;
    let skip = rest.iter().position(|&b| b != 0).unwrap_or(rest.len());
    rest = &rest[skip..];
    let mut args = Vec::with_capacity(argc.min(256));
    while args.len() < argc {
        match take(&mut rest) {
            Some(arg) => args.push(arg),
            None => break,
        }
    }
    let mut env = Vec::new();
    while let Some(entry) = take(&mut rest) {
        if entry.is_empty() {
            break;
        }
        if let Some(pair) = split_env(entry.as_bytes()) {
            env.push(pair);
        }
    }
    Some(ProcArgs {
        exec_path,
        args,
        env,
    })
}

/// `rwxp`-style permissions from Mach VM protection and share mode.
pub fn mach_perms(protection: u32, share_mode: u32) -> String {
    const SM_SHARED: u32 = 4;
    const SM_TRUESHARED: u32 = 5;
    const SM_SHARED_ALIASED: u32 = 7;
    let flag = |bit: u32, c: char| if protection & bit != 0 { c } else { '-' };
    let shared = matches!(share_mode, SM_SHARED | SM_TRUESHARED | SM_SHARED_ALIASED);
    format!(
        "{}{}{}{}",
        flag(1, 'r'),
        flag(2, 'w'),
        flag(4, 'x'),
        if shared { 's' } else { 'p' }
    )
}

/// Label for an anonymous Mach region from its `user_tag`.
pub fn mach_region_label(user_tag: u32) -> &'static str {
    match user_tag {
        1..=11 => "[malloc]",
        30 => "[stack]",
        31 | 32 => "[guard]",
        33 => "[shared pmap]",
        35 => "[dylib]",
        60 => "[dyld]",
        _ => "[anon]",
    }
}

/// C string from a fixed-size `c_char` buffer (stops at the first NUL).
pub fn c_buf_to_string(buf: &[std::os::raw::c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn procargs(argc: i32, body: &[u8]) -> Vec<u8> {
        let mut buf = argc.to_ne_bytes().to_vec();
        buf.extend_from_slice(body);
        buf
    }

    #[test]
    fn procargs2_parses() {
        let buf = procargs(
            3,
            b"/bin/sleep\0\0\0\0sleep\0-x\x0030\0PATH=/usr/bin\0HOME=/Users/a\0\0ptr_munge=\0executable_path=/bin/sleep\0\0\0",
        );
        let p = parse_procargs2(&buf).unwrap();
        assert_eq!(p.exec_path, "/bin/sleep");
        assert_eq!(p.args, vec!["sleep", "-x", "30"]);
        assert_eq!(
            p.env,
            vec![
                ("PATH".into(), "/usr/bin".into()),
                ("HOME".into(), "/Users/a".into())
            ]
        );
    }

    #[test]
    fn procargs2_is_robust_to_truncation() {
        assert!(parse_procargs2(&[]).is_none());
        assert!(parse_procargs2(&[1, 0]).is_none());
        assert!(parse_procargs2(&procargs(-1, b"/x\0")).is_none());
        let p = parse_procargs2(&procargs(5, b"/bin/x\0\0x\0a")).unwrap();
        assert_eq!(p.args, vec!["x", "a"]);
        assert!(p.env.is_empty());
        let p = parse_procargs2(&procargs(1, b"/bin/x\0x\0PATH=/u")).unwrap();
        assert_eq!(p.env, vec![("PATH".into(), "/u".into())]);
        let p = parse_procargs2(&procargs(0, b"/bin/x")).unwrap();
        assert_eq!(p.exec_path, "/bin/x");
        assert!(p.args.is_empty());
    }

    #[test]
    fn mach_helpers() {
        assert_eq!(mach_perms(1 | 4, 1), "r-xp");
        assert_eq!(mach_perms(3, 4), "rw-s");
        assert_eq!(mach_perms(0, 2), "---p");
        assert_eq!(mach_region_label(30), "[stack]");
        assert_eq!(mach_region_label(1), "[malloc]");
        assert_eq!(mach_region_label(0), "[anon]");
        let raw: Vec<std::os::raw::c_char> = b"main\0junk".iter().map(|&b| b as _).collect();
        assert_eq!(c_buf_to_string(&raw), "main");
        assert_eq!(c_buf_to_string(&[]), "");
    }

    #[test]
    fn states_and_tcp_codes() {
        assert_eq!(mach_thread_state(1, 0), ProcState::Running);
        assert_eq!(mach_thread_state(3, 0), ProcState::Sleeping);
        assert_eq!(mach_thread_state(3, 2), ProcState::Idle);
        assert_eq!(mach_thread_state(2, 0), ProcState::Stopped);
        assert_eq!(mach_thread_state(4, 0), ProcState::DiskSleep);
        assert_eq!(mach_thread_state(99, 0), ProcState::Unknown);
        assert_eq!(darwin_tcp_state(4), "ESTABLISHED");
        assert_eq!(darwin_tcp_state(1), "LISTEN");
        assert_eq!(darwin_tcp_state(-3), "");
    }
}
