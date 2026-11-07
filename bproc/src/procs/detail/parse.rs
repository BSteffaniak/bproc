//! Pure parsers for platform detail sources. Compiled on every host under
//! test so Linux `/proc` parsing is verified on macOS and vice versa.
use std::net::{IpAddr, SocketAddr};

#[cfg(any(target_os = "macos", test))]
pub mod darwin;
#[cfg(any(target_os = "linux", test))]
pub mod procfs;

/// `"TCP 127.0.0.1:5432 -> 10.0.0.2:51122 ESTABLISHED"`. The remote is
/// omitted when unset; an unspecified local address renders as `*`.
pub fn format_inet(
    proto: &str,
    local: SocketAddr,
    remote: Option<SocketAddr>,
    state: Option<&str>,
) -> String {
    let v6 = local.is_ipv6();
    let mut out = format!("{proto}{} {}", if v6 { "6" } else { "" }, endpoint(local));
    if let Some(remote) = remote.filter(|r| !r.ip().is_unspecified() || r.port() != 0) {
        out.push_str(" -> ");
        out.push_str(&endpoint(remote));
    }
    if let Some(state) = state.filter(|s| !s.is_empty()) {
        out.push(' ');
        out.push_str(state);
    }
    out
}

fn endpoint(addr: SocketAddr) -> String {
    let port = match addr.port() {
        0 => "*".to_string(),
        p => p.to_string(),
    };
    match addr.ip() {
        ip if ip.is_unspecified() => format!("*:{port}"),
        IpAddr::V4(ip) => format!("{ip}:{port}"),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => format!("{v4}:{port}"),
            None => format!("[{ip}]:{port}"),
        },
    }
}

fn split_env(raw: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8_lossy(raw);
    // A leading '=' is legal on some platforms (e.g. "=C:"); split after it.
    let at = text.get(1..)?.find('=')? + 1;
    Some((text[..at].to_string(), text[at + 1..].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inet_formatting() {
        let l: SocketAddr = "127.0.0.1:5432".parse().unwrap();
        let r: SocketAddr = "10.0.0.2:51122".parse().unwrap();
        assert_eq!(
            format_inet("TCP", l, Some(r), Some("ESTABLISHED")),
            "TCP 127.0.0.1:5432 -> 10.0.0.2:51122 ESTABLISHED"
        );
        let any: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let listen: SocketAddr = "0.0.0.0:80".parse().unwrap();
        assert_eq!(
            format_inet("TCP", listen, Some(any), Some("LISTEN")),
            "TCP *:80 LISTEN"
        );
        let v6: SocketAddr = "[::1]:8080".parse().unwrap();
        assert_eq!(format_inet("UDP", v6, None, None), "UDP6 [::1]:8080");
        let mapped: SocketAddr = "[::ffff:10.1.2.3]:22".parse().unwrap();
        assert_eq!(
            format_inet("TCP", mapped, None, Some("")),
            "TCP6 10.1.2.3:22"
        );
    }
}
