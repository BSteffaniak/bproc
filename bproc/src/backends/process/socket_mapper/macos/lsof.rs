// macOS lsof-based socket mapper

use super::super::SocketMapperBackend;
use crate::backends::process::{ConnectionEntry, ConnectionMap};
use crate::backends::{BackendCapabilities, BackendPriority};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::process::Command;

/// Socket mapper using lsof command
///
/// This backend executes `lsof -i -n -P -F pcn` to get network connections
/// and parses the output to build socket-to-PID mappings.
///
/// Since macOS doesn't expose socket inodes like Linux, we generate
/// pseudo-inodes by hashing the connection tuple.
pub struct LsofSocketMapper;

impl LsofSocketMapper {
    pub fn new() -> Result<Self> {
        if !Self::is_available() {
            anyhow::bail!("lsof command not found");
        }
        Ok(Self)
    }

    /// Parse lsof -F output format
    ///
    /// Format:
    /// p1234        <- PID
    /// cFirefox     <- Command name
    /// n127.0.0.1:8080->93.184.216.34:80  <- Network connection
    fn parse_lsof_output(output: &str) -> Result<ConnectionMap> {
        let mut socket_to_pid = HashMap::new();
        let mut tcp_connections = Vec::new();
        let mut tcp6_connections = Vec::new();
        let mut udp_connections = Vec::new();
        let mut udp6_connections = Vec::new();

        let mut current_pid: Option<i32> = None;
        let mut current_name: Option<String> = None;

        for line in output.lines() {
            if line.is_empty() {
                continue;
            }

            let (marker, value) = line.split_at(1);

            match marker {
                "p" => {
                    // PID
                    current_pid = value.parse().ok();
                }
                "c" => {
                    // Command name
                    current_name = Some(value.to_string());
                }
                "n" => {
                    // Network address
                    if let (Some(pid), Some(name)) = (current_pid, &current_name) {
                        // Parse connection and determine if it's TCP or UDP
                        // lsof doesn't explicitly mark protocol in -F output,
                        // but TCP connections usually have -> separator
                        let is_tcp = value.contains("->");
                        if let Some(entry) = Self::parse_connection(value, pid, name, is_tcp) {
                            let inode = entry.inode;
                            socket_to_pid.insert(inode, (pid, name.clone()));

                            // Categorize by IP version and protocol
                            let is_ipv6 = entry.local_addr.is_ipv6();

                            match (is_tcp, is_ipv6) {
                                (true, false) => tcp_connections.push(entry),
                                (true, true) => tcp6_connections.push(entry),
                                (false, false) => udp_connections.push(entry),
                                (false, true) => udp6_connections.push(entry),
                            }
                        }
                    }
                }
                _ => {
                    // Ignore other markers (t for type, f for file descriptor, etc.)
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

    /// Parse connection string from lsof
    ///
    /// Format examples:
    /// - "127.0.0.1:8080->93.184.216.34:80" (TCP connection)
    /// - "*:8080" (listening socket)
    /// - "[::1]:8080->[::1]:54321" (IPv6)
    fn parse_connection(
        conn_str: &str,
        _pid: i32,
        _name: &str,
        is_tcp: bool,
    ) -> Option<ConnectionEntry> {
        // Split on -> to separate local and remote addresses
        let parts: Vec<&str> = conn_str.split("->").collect();

        if parts.is_empty() {
            return None;
        }

        let local = parts[0];
        let remote = parts.get(1);

        // Parse local address
        let (local_addr, local_port) = match Self::parse_address(local) {
            Ok(addr) => addr,
            Err(e) => {
                log::debug!("Failed to parse local address '{}': {}", local, e);
                return None;
            }
        };

        // Parse remote address (or use 0.0.0.0:0 for listening sockets)
        let (remote_addr, remote_port) = if let Some(remote) = remote {
            match Self::parse_address(remote) {
                Ok(addr) => addr,
                Err(e) => {
                    log::debug!("Failed to parse remote address '{}': {}", remote, e);
                    return None;
                }
            }
        } else {
            // Listening socket - use unspecified address as remote
            if local_addr.is_ipv6() {
                (IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
            } else {
                (IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
            }
        };

        // Generate pseudo-inode from connection tuple
        let inode = Self::generate_pseudo_inode(&local_addr, local_port, &remote_addr, remote_port);

        // Determine state based on connection type
        // lsof doesn't provide detailed TCP state, so we infer from the connection format
        let state = if is_tcp {
            if remote.is_some() {
                "ESTABLISHED".to_string()
            } else {
                "LISTEN".to_string()
            }
        } else {
            "UDP".to_string()
        };

        Some(ConnectionEntry {
            local_addr,
            local_port,
            remote_addr,
            remote_port,
            inode,
            state,
        })
    }

    /// Parse address:port string
    ///
    /// Formats:
    /// - "127.0.0.1:8080" (IPv4)
    /// - "[::1]:8080" (IPv6)
    /// - "*:8080" (wildcard)
    fn parse_address(addr_str: &str) -> Result<(IpAddr, u16)> {
        // Handle IPv6 [address]:port format
        if addr_str.starts_with('[') {
            let end_bracket = addr_str.find(']').context("Invalid IPv6 address format")?;
            let ip_str = &addr_str[1..end_bracket];
            let port_str = &addr_str[end_bracket + 2..]; // Skip "]:"

            let ip: Ipv6Addr = ip_str.parse()?;
            let port: u16 = port_str.parse()?;

            return Ok((IpAddr::V6(ip), port));
        }

        // Handle IPv4 or wildcard
        let parts: Vec<&str> = addr_str.split(':').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid address format: {}", addr_str);
        }

        let ip_str = parts[0];
        let port: u16 = parts[1].parse()?;

        let ip = if ip_str == "*" {
            // Wildcard - use unspecified address
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            ip_str.parse()?
        };

        Ok((ip, port))
    }

    /// Generate pseudo-inode from connection tuple
    ///
    /// Since macOS doesn't expose socket inodes like Linux,
    /// we generate a deterministic hash from the connection parameters.
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
}

impl SocketMapperBackend for LsofSocketMapper {
    fn name(&self) -> &'static str {
        "lsof"
    }

    fn priority(&self) -> BackendPriority {
        BackendPriority::Good // Works well, but spawns external process
    }

    fn is_available() -> bool {
        // Check if lsof command exists
        Command::new("which")
            .arg("lsof")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
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
        // Execute lsof with field output format
        // -i: Internet connections only
        // -n: Don't resolve hostnames (faster)
        // -P: Don't resolve port names (faster)
        // -F pcn: Field output (pid, command, network address)
        let output = Command::new("lsof")
            .args(["-i", "-n", "-P", "-F", "pcn"])
            .output()
            .context("Failed to execute lsof command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("lsof command failed: {}", stderr);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Self::parse_lsof_output(&stdout)
    }
}
