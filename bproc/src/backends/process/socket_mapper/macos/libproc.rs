// macOS libproc-based socket mapper
//
// This backend uses the native macOS libproc API via the libproc crate.
// This provides direct kernel access to socket information without spawning
// external processes like lsof.

use super::super::SocketMapperBackend;
use crate::backends::process::{ConnectionEntry, ConnectionMap};
use crate::backends::{BackendCapabilities, BackendPriority};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

// Constants from sys/proc_info.h (not exposed by libproc crate)
const INI_IPV4: u8 = 0x1;

// TCP state constants from netinet/tcp_fsm.h
const TCPS_CLOSED: i32 = 0;
const TCPS_LISTEN: i32 = 1;
const TCPS_SYN_SENT: i32 = 2;
const TCPS_SYN_RECEIVED: i32 = 3;
const TCPS_ESTABLISHED: i32 = 4;
const TCPS_CLOSE_WAIT: i32 = 5;
const TCPS_FIN_WAIT_1: i32 = 6;
const TCPS_CLOSING: i32 = 7;
const TCPS_LAST_ACK: i32 = 8;
const TCPS_FIN_WAIT_2: i32 = 9;
const TCPS_TIME_WAIT: i32 = 10;

/// Socket mapper using native macOS libproc API
///
/// This backend uses the libproc crate which wraps macOS's native
/// libproc API. This provides direct kernel access to socket information
/// without spawning external processes like lsof.
///
/// Performance: ~2-5ms per scan (vs ~10-20ms for lsof)
pub struct LibprocSocketMapper;

impl LibprocSocketMapper {
    pub fn new() -> Result<Self> {
        if !Self::is_available() {
            anyhow::bail!("libproc not available");
        }
        Ok(Self)
    }

    /// Enumerate all network connections using libproc API
    fn enumerate_connections() -> Result<ConnectionMap> {
        use libproc::libproc::bsd_info::BSDInfo;
        use libproc::libproc::file_info::{ListFDs, ProcFDType, pidfdinfo};
        use libproc::libproc::net_info::{SocketFDInfo, SocketInfoKind};
        use libproc::libproc::proc_pid::{listpidinfo, pidinfo};
        use libproc::processes;

        let mut socket_to_pid = HashMap::new();
        let mut tcp_connections = Vec::new();
        let mut tcp6_connections = Vec::new();
        let mut udp_connections = Vec::new();
        let mut udp6_connections = Vec::new();

        // Get all PIDs
        let pids = processes::pids_by_type(processes::ProcFilter::All)
            .context("Failed to list processes")?;

        for pid in pids {
            let pid = pid as i32;

            // Get process name
            let process_name =
                libproc::libproc::proc_pid::name(pid).unwrap_or_else(|_| format!("PID {}", pid));

            // Get BSD info to know how many FDs
            let info = match pidinfo::<BSDInfo>(pid, 0) {
                Ok(info) => info,
                Err(_) => continue, // Process may have exited
            };

            // List all file descriptors for this PID
            let fds = match listpidinfo::<ListFDs>(pid, info.pbi_nfiles as usize) {
                Ok(fds) => fds,
                Err(_) => continue, // Process may have exited
            };

            // Filter for socket FDs and get socket info
            for fd in fds {
                if let ProcFDType::Socket = fd.proc_fdtype.into() {
                    // Get socket info for this FD
                    let sock_info: Result<SocketFDInfo, _> = pidfdinfo(pid, fd.proc_fd);

                    if let Ok(sock_info) = sock_info {
                        // Check socket kind (TCP vs UDP vs Unix, etc.)
                        let kind = SocketInfoKind::from(sock_info.psi.soi_kind);

                        match kind {
                            SocketInfoKind::Tcp => {
                                // TCP socket - extract from pri_tcp
                                if let Some(entry) =
                                    Self::parse_tcp_socket(pid, &process_name, &sock_info)
                                {
                                    let inode = entry.inode;
                                    socket_to_pid.insert(inode, (pid, process_name.clone()));

                                    if entry.local_addr.is_ipv6() {
                                        tcp6_connections.push(entry);
                                    } else {
                                        tcp_connections.push(entry);
                                    }
                                }
                            }
                            SocketInfoKind::In => {
                                // Generic IP socket (typically UDP)
                                if let Some(entry) =
                                    Self::parse_in_socket(pid, &process_name, &sock_info)
                                {
                                    let inode = entry.inode;
                                    socket_to_pid.insert(inode, (pid, process_name.clone()));

                                    if entry.local_addr.is_ipv6() {
                                        udp6_connections.push(entry);
                                    } else {
                                        udp_connections.push(entry);
                                    }
                                }
                            }
                            _ => {
                                // Ignore other socket types (Unix domain, etc.)
                            }
                        }
                    }
                }
            }
        }

        Ok(ConnectionMap {
            socket_to_pid,
            tcp_connections,
            tcp6_connections,
            udp_connections,
            udp6_connections,
        })
    }

    /// Parse TCP socket info into ConnectionEntry
    fn parse_tcp_socket(
        _pid: i32,
        _name: &str,
        sock_info: &libproc::libproc::net_info::SocketFDInfo,
    ) -> Option<ConnectionEntry> {
        unsafe {
            let tcp_info = &sock_info.psi.soi_proto.pri_tcp;
            let in_info = &tcp_info.tcpsi_ini;

            // Convert addresses and ports
            let (local_addr, local_port) = Self::parse_in_sockaddr(in_info, true)?;
            let (remote_addr, remote_port) = Self::parse_in_sockaddr(in_info, false)?;

            // Generate pseudo-inode (same as lsof implementation)
            let inode =
                Self::generate_pseudo_inode(&local_addr, local_port, &remote_addr, remote_port);

            // Get TCP state
            let state = Self::tcp_state_to_string(tcp_info.tcpsi_state);

            Some(ConnectionEntry {
                local_addr,
                local_port,
                remote_addr,
                remote_port,
                inode,
                state,
            })
        }
    }

    /// Parse generic IP socket info (UDP) into ConnectionEntry
    fn parse_in_socket(
        _pid: i32,
        _name: &str,
        sock_info: &libproc::libproc::net_info::SocketFDInfo,
    ) -> Option<ConnectionEntry> {
        unsafe {
            let in_info = &sock_info.psi.soi_proto.pri_in;

            let (local_addr, local_port) = Self::parse_in_sockaddr(in_info, true)?;
            let (remote_addr, remote_port) = Self::parse_in_sockaddr(in_info, false)?;

            let inode =
                Self::generate_pseudo_inode(&local_addr, local_port, &remote_addr, remote_port);

            Some(ConnectionEntry {
                local_addr,
                local_port,
                remote_addr,
                remote_port,
                inode,
                state: "UDP".to_string(),
            })
        }
    }

    /// Parse InSockInfo structure to extract IP address and port
    ///
    /// This handles both IPv4 and IPv6 addresses, as well as IPv4-mapped-IPv6 addresses.
    /// Ports are converted from network byte order to host byte order.
    fn parse_in_sockaddr(
        in_info: &libproc::libproc::net_info::InSockInfo,
        is_local: bool,
    ) -> Option<(IpAddr, u16)> {
        // Get port (convert from network byte order to host byte order)
        let port = if is_local {
            u16::from_be(in_info.insi_lport as u16)
        } else {
            u16::from_be(in_info.insi_fport as u16)
        };

        // Determine if IPv4 or IPv6
        let is_ipv4 = (in_info.insi_vflag & INI_IPV4) != 0;

        let addr = if is_ipv4 {
            // IPv4 - extract from in4in6_addr
            let addr_union = if is_local {
                &in_info.insi_laddr
            } else {
                &in_info.insi_faddr
            };

            unsafe {
                let ipv4_bytes = addr_union.ina_46.i46a_addr4;
                // s_addr is already in network byte order, convert to host byte order
                let addr_u32 = u32::from_be(ipv4_bytes.s_addr);
                IpAddr::V4(Ipv4Addr::from(addr_u32))
            }
        } else {
            // IPv6
            let addr_union = if is_local {
                &in_info.insi_laddr
            } else {
                &in_info.insi_faddr
            };

            unsafe {
                let ipv6_addr = addr_union.ina_6;
                IpAddr::V6(Ipv6Addr::from(ipv6_addr.s6_addr))
            }
        };

        Some((addr, port))
    }

    /// Generate pseudo-inode from connection tuple
    ///
    /// Since macOS doesn't expose socket inodes like Linux,
    /// we generate a deterministic hash from the connection parameters.
    /// This matches the implementation in lsof.rs for consistency.
    fn generate_pseudo_inode(
        local_addr: &IpAddr,
        local_port: u16,
        remote_addr: &IpAddr,
        remote_port: u16,
    ) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        local_addr.hash(&mut hasher);
        local_port.hash(&mut hasher);
        remote_addr.hash(&mut hasher);
        remote_port.hash(&mut hasher);
        hasher.finish()
    }

    /// Convert TCP state constant to string
    fn tcp_state_to_string(state: i32) -> String {
        match state {
            TCPS_CLOSED => "CLOSED".to_string(),
            TCPS_LISTEN => "LISTEN".to_string(),
            TCPS_SYN_SENT => "SYN_SENT".to_string(),
            TCPS_SYN_RECEIVED => "SYN_RCVD".to_string(),
            TCPS_ESTABLISHED => "ESTABLISHED".to_string(),
            TCPS_CLOSE_WAIT => "CLOSE_WAIT".to_string(),
            TCPS_FIN_WAIT_1 => "FIN_WAIT1".to_string(),
            TCPS_CLOSING => "CLOSING".to_string(),
            TCPS_LAST_ACK => "LAST_ACK".to_string(),
            TCPS_FIN_WAIT_2 => "FIN_WAIT2".to_string(),
            TCPS_TIME_WAIT => "TIME_WAIT".to_string(),
            _ => format!("UNKNOWN({})", state),
        }
    }
}

impl SocketMapperBackend for LibprocSocketMapper {
    fn name(&self) -> &'static str {
        "libproc"
    }

    fn priority(&self) -> BackendPriority {
        BackendPriority::Best // Native API - most efficient
    }

    fn is_available() -> bool {
        // Always available on macOS 10.5+
        cfg!(target_os = "macos")
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            ipv4_support: true,
            ipv6_support: true,
            per_process: true,
            per_connection: true,
        }
    }

    fn get_connection_map(&self) -> Result<ConnectionMap> {
        Self::enumerate_connections()
    }
}
