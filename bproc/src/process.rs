use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;

/// Traffic type for throttling - determines which traffic to throttle
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TrafficType {
    #[default]
    All, // Throttle all traffic (default/current behavior)
    Internet, // Throttle only internet traffic
    Local,    // Throttle only local traffic
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: i32,
    pub name: String,

    // Aggregate rates (existing)
    pub download_rate: u64,  // bytes per second
    pub upload_rate: u64,    // bytes per second
    pub total_download: u64, // total bytes
    pub total_upload: u64,   // total bytes

    // NEW: Internet traffic
    pub internet_download_rate: u64,
    pub internet_upload_rate: u64,
    pub internet_total_download: u64,
    pub internet_total_upload: u64,

    // NEW: Local traffic
    pub local_download_rate: u64,
    pub local_upload_rate: u64,
    pub local_total_download: u64,
    pub local_total_upload: u64,

    pub throttle_limit: Option<ThrottleLimit>,
    pub is_terminated: bool, // whether the process has terminated
    pub interface_stats: HashMap<String, InterfaceStats>, // per-interface statistics
    pub connections: Vec<ConnectionDetail>, // active network connections
}

#[derive(Debug, Clone, Default)]
// Retained domain metadata for platform collectors and diagnostics.
#[allow(dead_code)]
pub struct InterfaceStats {
    // Aggregate rates (existing)
    pub download_rate: u64,
    pub upload_rate: u64,
    pub total_download: u64,
    pub total_upload: u64,

    // NEW: Categorized rates
    pub internet_download_rate: u64,
    pub internet_upload_rate: u64,
    pub local_download_rate: u64,
    pub local_upload_rate: u64,
}

#[derive(Debug, Clone)]
// Retained domain metadata for platform collectors and diagnostics.
#[allow(dead_code)]
pub struct InterfaceInfo {
    pub name: String,
    pub mac_address: Option<String>,
    pub ip_addresses: Vec<IpAddr>,
    pub is_up: bool,
    pub is_loopback: bool,
    pub total_download_rate: u64,
    pub total_upload_rate: u64,
    pub process_count: usize,
}

#[derive(Debug, Clone)]
pub struct ThrottleLimit {
    pub download_limit: Option<u64>, // bytes per second
    pub upload_limit: Option<u64>,   // bytes per second
    pub traffic_type: TrafficType,   // NEW: which traffic to throttle
}

impl ProcessInfo {
    #[cfg(any(feature = "monitor-pnet", target_os = "windows", test))]
    pub fn new(pid: i32, name: String) -> Self {
        Self {
            pid,
            name,
            download_rate: 0,
            upload_rate: 0,
            total_download: 0,
            total_upload: 0,
            internet_download_rate: 0,
            internet_upload_rate: 0,
            internet_total_download: 0,
            internet_total_upload: 0,
            local_download_rate: 0,
            local_upload_rate: 0,
            local_total_download: 0,
            local_total_upload: 0,
            throttle_limit: None,
            is_terminated: false,
            interface_stats: HashMap::new(),
            connections: Vec::new(),
        }
    }

    pub fn format_rate(bytes_per_sec: u64) -> String {
        if bytes_per_sec < 1024 {
            format!("{} B/s", bytes_per_sec)
        } else if bytes_per_sec < 1024 * 1024 {
            format!("{:.1} KB/s", bytes_per_sec as f64 / 1024.0)
        } else if bytes_per_sec < 1024 * 1024 * 1024 {
            format!("{:.1} MB/s", bytes_per_sec as f64 / (1024.0 * 1024.0))
        } else {
            format!(
                "{:.1} GB/s",
                bytes_per_sec as f64 / (1024.0 * 1024.0 * 1024.0)
            )
        }
    }

    pub fn format_bytes(bytes: u64) -> String {
        if bytes < 1024 {
            format!("{} B", bytes)
        } else if bytes < 1024 * 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else if bytes < 1024 * 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    }

    pub fn is_throttled(&self) -> bool {
        self.throttle_limit.is_some()
    }

    /// Populate connections for this process from the connection map
    #[cfg(feature = "monitor-pnet")]
    pub fn populate_connections(
        &mut self,
        connection_map: &crate::backends::process::ConnectionMap,
        socket_to_pid: &HashMap<u64, (i32, String)>,
    ) {
        self.connections.clear();

        // Helper closure to add connections from a list
        let mut add_connections = |entries: &[crate::backends::process::ConnectionEntry],
                                   protocol: &str| {
            for entry in entries {
                if let Some((pid, _name)) = socket_to_pid.get(&entry.inode)
                    && *pid == self.pid
                {
                    self.connections.push(ConnectionDetail {
                        protocol: protocol.to_string(),
                        local_addr: entry.local_addr,
                        local_port: entry.local_port,
                        remote_addr: entry.remote_addr,
                        remote_port: entry.remote_port,
                        state: entry.state.clone(),
                    });
                }
            }
        };

        add_connections(&connection_map.tcp_connections, "TCP");
        add_connections(&connection_map.tcp6_connections, "TCP6");
        add_connections(&connection_map.udp_connections, "UDP");
        add_connections(&connection_map.udp6_connections, "UDP6");
    }
}

pub type ProcessMap = HashMap<i32, ProcessInfo>;
pub type InterfaceMap = HashMap<String, InterfaceInfo>;

/// Detailed connection information for a single network connection
#[derive(Debug, Clone)]
// Collected by the monitor for the explorer's per-process network views.
#[allow(dead_code)]
pub struct ConnectionDetail {
    pub protocol: String, // TCP/UDP/TCP6/UDP6
    pub local_addr: IpAddr,
    pub local_port: u16,
    pub remote_addr: IpAddr,
    pub remote_port: u16,
    pub state: String, // ESTABLISHED, LISTEN, etc. (empty for UDP)
}
