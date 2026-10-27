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

#[derive(Debug, Clone)]
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
pub struct ConnectionDetail {
    pub protocol: String, // TCP/UDP/TCP6/UDP6
    pub local_addr: IpAddr,
    pub local_port: u16,
    pub remote_addr: IpAddr,
    pub remote_port: u16,
    pub state: String, // ESTABLISHED, LISTEN, etc. (empty for UDP)
}

/// Extended process information including system details
#[derive(Debug, Clone)]
pub struct ProcessDetails {
    pub pid: i32,
    // Retained domain metadata for platform collectors and diagnostics.
    #[allow(dead_code)]
    pub name: String,
    pub cmdline: Option<Vec<String>>,
    pub exe_path: Option<String>,
    pub cwd: Option<String>,
    pub state: Option<String>,
    pub ppid: Option<i32>,
    pub threads: Option<usize>,
    pub memory_rss: Option<u64>, // Resident Set Size in KB
    pub memory_vms: Option<u64>, // Virtual Memory Size in KB
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    // Retained domain metadata for platform collectors and diagnostics.
    #[allow(dead_code)]
    pub connections: Vec<ConnectionDetail>,
}

impl ProcessDetails {
    /// Collect detailed process information from /proc filesystem
    #[cfg(target_os = "linux")]
    pub fn from_pid(pid: i32) -> Self {
        use std::fs;
        use std::path::Path;

        let proc_path = format!("/proc/{}", pid);
        let proc_path_buf = Path::new(&proc_path);

        // Get process name from stat
        let name = fs::read_to_string(proc_path_buf.join("comm"))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| format!("PID {}", pid));

        // Read cmdline
        let cmdline = fs::read_to_string(proc_path_buf.join("cmdline"))
            .ok()
            .map(|s| {
                s.split('\0')
                    .filter(|arg| !arg.is_empty())
                    .map(|arg| arg.to_string())
                    .collect()
            });

        // Read exe path
        let exe_path = fs::read_link(proc_path_buf.join("exe"))
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()));

        // Read cwd
        let cwd = fs::read_link(proc_path_buf.join("cwd"))
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()));

        // Parse status file for detailed info
        let (state, ppid, threads, memory_rss, memory_vms, uid, gid) =
            fs::read_to_string(proc_path_buf.join("status"))
                .ok()
                .map(|content| Self::parse_status(&content))
                .unwrap_or((None, None, None, None, None, None, None));

        Self {
            pid,
            name,
            cmdline,
            exe_path,
            cwd,
            state,
            ppid,
            threads,
            memory_rss,
            memory_vms,
            uid,
            gid,
            connections: Vec::new(), // Will be populated separately
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn from_pid(pid: i32) -> Self {
        use sysinfo::{Pid, System};

        // Get process information from sysinfo
        // Note: System::new_all() is slow but necessary to get full process info
        let sys = System::new_all();
        let pid_obj = Pid::from_u32(pid as u32);

        if let Some(process) = sys.process(pid_obj) {
            Self {
                pid,
                name: process.name().to_str().unwrap_or("unknown").to_string(),
                cmdline: Some(
                    process
                        .cmd()
                        .iter()
                        .map(|s| s.to_str().unwrap_or("").to_string())
                        .collect(),
                ),
                exe_path: process
                    .exe()
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string()),
                cwd: process
                    .cwd()
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string()),
                state: Some(format!("{:?}", process.status())),
                ppid: process.parent().map(|p| p.as_u32() as i32),
                threads: None, // Not available on Windows via sysinfo
                memory_rss: Some(process.memory() / 1024), // Convert bytes to KB
                memory_vms: Some(process.virtual_memory() / 1024), // Convert bytes to KB
                uid: None,     // Not applicable on Windows
                gid: None,     // Not applicable on Windows
                connections: Vec::new(), // Populated separately
            }
        } else {
            // Fallback if process not found or has terminated
            Self {
                pid,
                name: format!("PID {}", pid),
                cmdline: None,
                exe_path: None,
                cwd: None,
                state: Some("Unknown".to_string()),
                ppid: None,
                threads: None,
                memory_rss: None,
                memory_vms: None,
                uid: None,
                gid: None,
                connections: Vec::new(),
            }
        }
    }

    /// Parse /proc/[pid]/status file
    #[cfg(target_os = "linux")]
    fn parse_status(
        content: &str,
    ) -> (
        Option<String>, // state
        Option<i32>,    // ppid
        Option<usize>,  // threads
        Option<u64>,    // memory_rss (KB)
        Option<u64>,    // memory_vms (KB)
        Option<u32>,    // uid
        Option<u32>,    // gid
    ) {
        let mut state = None;
        let mut ppid = None;
        let mut threads = None;
        let mut memory_rss = None;
        let mut memory_vms = None;
        let mut uid = None;
        let mut gid = None;

        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            match parts[0] {
                "State:" if parts.len() >= 2 => {
                    state = Some(parts[1].to_string());
                }
                "PPid:" if parts.len() >= 2 => {
                    ppid = parts[1].parse().ok();
                }
                "Threads:" if parts.len() >= 2 => {
                    threads = parts[1].parse().ok();
                }
                "VmRSS:" if parts.len() >= 2 => {
                    memory_rss = parts[1].parse().ok();
                }
                "VmSize:" if parts.len() >= 2 => {
                    memory_vms = parts[1].parse().ok();
                }
                "Uid:" if parts.len() >= 2 => {
                    uid = parts[1].parse().ok(); // Real UID (first value)
                }
                "Gid:" if parts.len() >= 2 => {
                    gid = parts[1].parse().ok(); // Real GID (first value)
                }
                _ => {}
            }
        }

        (state, ppid, threads, memory_rss, memory_vms, uid, gid)
    }

    /// Get human-readable state description
    pub fn state_description(&self) -> String {
        match self.state.as_deref() {
            Some("R") => "Running".to_string(),
            Some("S") => "Sleeping".to_string(),
            Some("D") => "Disk Sleep".to_string(),
            Some("Z") => "Zombie".to_string(),
            Some("T") => "Stopped".to_string(),
            Some("t") => "Tracing Stop".to_string(),
            Some("W") => "Paging".to_string(),
            Some("X") => "Dead".to_string(),
            Some("x") => "Dead".to_string(),
            Some("K") => "Wakekill".to_string(),
            Some("P") => "Parked".to_string(),
            Some("I") => "Idle".to_string(),
            Some(s) => s.to_string(),
            None => "Unknown".to_string(),
        }
    }

    /// Format memory in human-readable form (from KB)
    pub fn format_memory(kb: u64) -> String {
        if kb < 1024 {
            format!("{} KB", kb)
        } else if kb < 1024 * 1024 {
            format!("{:.1} MB", kb as f64 / 1024.0)
        } else {
            format!("{:.1} GB", kb as f64 / (1024.0 * 1024.0))
        }
    }
}
