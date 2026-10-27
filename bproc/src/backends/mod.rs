// Backend trait definitions and core types

pub mod capability;
#[cfg(target_os = "linux")]
pub mod cgroup;
#[cfg(all(target_os = "windows", not(feature = "monitor-pnet")))]
pub mod monitor;
pub mod process;
pub mod throttle;

/// Backend priority ranking (higher = better)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackendPriority {
    // Constructed by Linux socket-mapper fallbacks, not every host build.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Fallback = 1, // Works but limited (iptables, /proc parsing)
    Good = 2, // Solid implementation (IFB+TC, WFP)
    // Constructed by opt-in Linux throttling backends.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Better = 3, // Modern, efficient (eBPF XDP, nftables)
    Best = 4, // Optimal (eBPF cgroup, native APIs)
}

/// Capabilities that a backend supports
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackendCapabilities {
    pub ipv4_support: bool,
    pub ipv6_support: bool,
    pub per_process: bool,
    pub per_connection: bool,
}

/// Active throttle information
#[derive(Debug, Clone)]
pub struct ActiveThrottle {
    // Retained domain metadata for platform collectors and diagnostics.
    #[allow(dead_code)]
    pub pid: i32,
    pub process_name: String,
    pub upload_limit: Option<u64>,   // bytes/sec
    pub download_limit: Option<u64>, // bytes/sec
}
