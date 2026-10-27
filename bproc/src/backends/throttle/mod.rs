// Throttling backend traits and implementations

use super::{BackendCapabilities, BackendPriority};
use anyhow::Result;
use std::collections::HashMap;

pub mod download;
pub mod manager;
pub mod upload;

#[cfg(target_os = "linux")]
pub mod linux_tc_utils;

#[cfg(target_os = "linux")]
pub mod linux_nft_utils;

#[cfg(all(target_os = "linux", feature = "throttle-ebpf"))]
pub mod linux_ebpf_utils;

#[cfg(all(target_os = "linux", feature = "throttle-ebpf"))]
pub use linux_ebpf_utils::{BpfAttachMethod, BpfConfig, init_bpf_config};

// Re-export manager
pub use manager::ThrottleManager;

/// Throttle statistics for a process/cgroup
#[derive(Debug, Clone, Default)]
// Optional backend contract; not every operation is used on each host.
#[allow(dead_code)]
pub struct BackendStats {
    pub packets_total: u64,
    pub bytes_total: u64,
    pub packets_dropped: u64,
    pub bytes_dropped: u64,
}

/// Upload (egress) throttling backend trait
pub trait UploadThrottleBackend: Send + Sync {
    /// Backend name (e.g., "tc_htb", "ebpf_cgroup", "wfp")
    fn name(&self) -> &'static str;

    /// Backend priority for auto-selection
    // Optional backend diagnostics/query contract retained across feature sets.
    #[allow(dead_code)]
    fn priority(&self) -> BackendPriority;

    /// Check if this backend is available on the current system
    fn is_available() -> bool
    where
        Self: Sized;

    /// Get backend capabilities
    fn capabilities(&self) -> BackendCapabilities;

    /// Initialize the backend
    fn init(&mut self) -> Result<()>;

    /// Apply upload throttle to a process
    fn throttle_upload(
        &mut self,
        pid: i32,
        process_name: String,
        limit_bytes_per_sec: u64,
        traffic_type: crate::process::TrafficType,
    ) -> Result<()>;

    /// Remove upload throttle from a process
    fn remove_upload_throttle(&mut self, pid: i32) -> Result<()>;

    /// Get active upload throttle for a process
    // Optional backend diagnostics/query contract retained across feature sets.
    #[allow(dead_code)]
    fn get_upload_throttle(&self, pid: i32) -> Option<u64>;

    /// Get all active upload throttles
    fn get_all_throttles(&self) -> HashMap<i32, u64>;

    /// Cleanup on shutdown
    fn cleanup(&mut self) -> Result<()>;

    /// Get statistics for a throttled process (if supported by backend)
    // Optional backend diagnostics/query contract retained across feature sets.
    #[allow(dead_code)]
    fn get_stats(&self, _pid: i32) -> Option<BackendStats> {
        None // Default implementation returns None for backends that don't support stats
    }

    /// Check if this backend supports a specific traffic type
    /// Default implementation: only supports TrafficType::All
    fn supports_traffic_type(&self, traffic_type: crate::process::TrafficType) -> bool {
        use crate::process::TrafficType;
        traffic_type == TrafficType::All
    }
}

/// Download (ingress) throttling backend trait
pub trait DownloadThrottleBackend: Send + Sync {
    /// Backend name (e.g., "ifb_tc", "ebpf_cgroup", "ebpf_xdp")
    fn name(&self) -> &'static str;

    /// Backend priority for auto-selection
    // Optional backend diagnostics/query contract retained across feature sets.
    #[allow(dead_code)]
    fn priority(&self) -> BackendPriority;

    /// Check if this backend is available on the current system
    fn is_available() -> bool
    where
        Self: Sized;

    /// Get backend capabilities
    fn capabilities(&self) -> BackendCapabilities;

    /// Initialize the backend
    fn init(&mut self) -> Result<()>;

    /// Apply download throttle to a process
    fn throttle_download(
        &mut self,
        pid: i32,
        process_name: String,
        limit_bytes_per_sec: u64,
        traffic_type: crate::process::TrafficType,
    ) -> Result<()>;

    /// Remove download throttle from a process
    fn remove_download_throttle(&mut self, pid: i32) -> Result<()>;

    /// Get active download throttle for a process
    // Optional backend diagnostics/query contract retained across feature sets.
    #[allow(dead_code)]
    fn get_download_throttle(&self, pid: i32) -> Option<u64>;

    /// Get all active download throttles
    fn get_all_throttles(&self) -> HashMap<i32, u64>;

    /// Cleanup on shutdown
    fn cleanup(&mut self) -> Result<()>;

    /// Get statistics for a throttled process (if supported by backend)
    // Optional backend diagnostics/query contract retained across feature sets.
    #[allow(dead_code)]
    fn get_stats(&self, _pid: i32) -> Option<BackendStats> {
        None // Default implementation returns None for backends that don't support stats
    }

    /// Log diagnostic information for a throttled process (for debugging)
    /// Default implementation does nothing - only eBPF backend implements this
    // Optional backend diagnostics/query contract retained across feature sets.
    #[allow(dead_code)]
    fn log_diagnostics(&mut self, _pid: i32) -> Result<()> {
        Ok(())
    }

    /// Check if this backend supports a specific traffic type
    /// Default implementation: only supports TrafficType::All
    fn supports_traffic_type(&self, traffic_type: crate::process::TrafficType) -> bool {
        use crate::process::TrafficType;
        traffic_type == TrafficType::All
    }
}

/// Upload backend metadata for selection
#[derive(Debug, Clone)]
pub struct UploadBackendInfo {
    pub name: &'static str,
    pub priority: BackendPriority,
    pub available: bool,
}

/// Download backend metadata for selection
#[derive(Debug, Clone)]
pub struct DownloadBackendInfo {
    pub name: &'static str,
    pub priority: BackendPriority,
    pub available: bool,
}

/// Complete backend information for UI display
#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub active_upload: Option<String>,
    pub active_download: Option<String>,
    pub active_monitoring: Option<String>, // NEW: e.g., "pnet"
    pub active_socket_mapper: Option<String>,
    pub available_upload: Vec<(String, BackendPriority, bool)>,
    pub available_download: Vec<(String, BackendPriority, bool)>,
    pub available_socket_mappers: Vec<(String, BackendPriority, bool)>,
    pub preferred_upload: Option<String>,
    pub preferred_download: Option<String>,
    pub preferred_socket_mapper: Option<String>,
    pub upload_capabilities: Option<BackendCapabilities>,
    pub download_capabilities: Option<BackendCapabilities>,
    pub socket_mapper_capabilities: Option<BackendCapabilities>,
    pub backend_stats: HashMap<String, usize>, // backend_name -> active throttle count
}

/// Detect all available upload backends
pub fn detect_upload_backends() -> Vec<UploadBackendInfo> {
    let mut backends = Vec::new();

    #[cfg(feature = "throttle-ebpf")]
    {
        backends.push(UploadBackendInfo {
            name: "ebpf",
            priority: BackendPriority::Best,
            available: upload::linux::ebpf::EbpfUpload::is_available(),
        });
    }

    #[cfg(feature = "throttle-nftables")]
    {
        backends.push(UploadBackendInfo {
            name: "nftables",
            priority: BackendPriority::Better,
            available: upload::linux::nftables::NftablesUpload::is_available(),
        });
    }

    #[cfg(feature = "throttle-tc-htb")]
    {
        backends.push(UploadBackendInfo {
            name: "tc_htb",
            priority: BackendPriority::Good,
            available: upload::linux::tc_htb::TcHtbUpload::is_available(),
        });
    }

    #[cfg(target_os = "macos")]
    {
        backends.push(UploadBackendInfo {
            name: "dnctl",
            priority: BackendPriority::Best,
            available: upload::macos::DnctlUpload::is_available(),
        });
    }

    backends
}

/// Detect all available download backends
pub fn detect_download_backends() -> Vec<DownloadBackendInfo> {
    let mut backends = Vec::new();

    #[cfg(feature = "throttle-ebpf")]
    {
        backends.push(DownloadBackendInfo {
            name: "ebpf",
            priority: BackendPriority::Best,
            available: download::linux::ebpf::EbpfDownload::is_available(),
        });
    }

    #[cfg(feature = "throttle-nftables")]
    {
        backends.push(DownloadBackendInfo {
            name: "nftables",
            priority: BackendPriority::Better,
            available: download::linux::nftables::NftablesDownload::is_available(),
        });
    }

    #[cfg(feature = "throttle-ifb-tc")]
    {
        backends.push(DownloadBackendInfo {
            name: "ifb_tc",
            priority: BackendPriority::Good,
            available: download::linux::ifb_tc::IfbTcDownload::is_available(),
        });
    }

    #[cfg(feature = "throttle-tc-police")]
    {
        backends.push(DownloadBackendInfo {
            name: "tc_police",
            priority: BackendPriority::Fallback,
            available: download::linux::tc_police::TcPoliceDownload::is_available(),
        });
    }

    #[cfg(target_os = "macos")]
    {
        backends.push(DownloadBackendInfo {
            name: "dnctl",
            priority: BackendPriority::Best,
            available: download::macos::DnctlDownload::is_available(),
        });
    }

    backends
}

/// Auto-select best available upload backend (returns None if unavailable)
pub fn select_upload_backend(preference: Option<&str>) -> Option<Box<dyn UploadThrottleBackend>> {
    if let Some(name) = preference {
        log::info!("Using preferred upload backend: {}", name);
        return create_upload_backend(name).ok();
    }

    let available = detect_upload_backends();

    // Log all backends and their status
    log::debug!("Available upload backends:");
    for backend in &available {
        log::debug!(
            "  {} - priority: {:?}, available: {}",
            backend.name,
            backend.priority,
            backend.available
        );
    }

    // Auto-select best available
    let selected = available
        .iter()
        .filter(|b| b.available)
        .max_by_key(|b| b.priority)
        .and_then(|info| {
            log::info!("Auto-selected upload backend: {}", info.name);
            create_upload_backend(info.name).ok()
        });

    if selected.is_none() {
        log::error!("❌ No upload throttling backend available");
    }

    selected
}

/// Auto-select best available download backend (returns None if unavailable)
pub fn select_download_backend(
    preference: Option<&str>,
) -> Option<Box<dyn DownloadThrottleBackend>> {
    let available = detect_download_backends();

    if let Some(name) = preference {
        log::info!("Using preferred download backend: {}", name);
        return create_download_backend(name).ok();
    }

    // Log all backends and their status
    log::debug!("Available download backends:");
    for backend in &available {
        log::debug!(
            "  {} - priority: {:?}, available: {}",
            backend.name,
            backend.priority,
            backend.available
        );
    }

    // Auto-select best available
    let selected = available
        .iter()
        .filter(|b| b.available)
        .max_by_key(|b| b.priority)
        .and_then(|info| {
            log::info!("Auto-selected download backend: {}", info.name);
            create_download_backend(info.name).ok()
        });

    if selected.is_none() {
        log::error!("❌ No download throttling backend available");
    }

    selected
}

/// Create an upload backend by name
pub fn create_upload_backend(name: &str) -> Result<Box<dyn UploadThrottleBackend>> {
    log::debug!("Creating upload backend {name}");
    match name {
        #[cfg(feature = "throttle-ebpf")]
        "ebpf" => Ok(Box::new(upload::linux::ebpf::EbpfUpload::new()?)),

        #[cfg(feature = "throttle-nftables")]
        "nftables" => Ok(Box::new(upload::linux::nftables::NftablesUpload::new()?)),

        #[cfg(feature = "throttle-tc-htb")]
        "tc_htb" => Ok(Box::new(upload::linux::tc_htb::TcHtbUpload::new()?)),

        #[cfg(target_os = "macos")]
        "dnctl" => Ok(Box::new(upload::macos::DnctlUpload::new()?)),

        _ => Err(anyhow::anyhow!("Unknown upload backend: {}", name)),
    }
}

/// Create a download backend by name
pub fn create_download_backend(name: &str) -> Result<Box<dyn DownloadThrottleBackend>> {
    match name {
        #[cfg(feature = "throttle-ebpf")]
        "ebpf" => Ok(Box::new(download::linux::ebpf::EbpfDownload::new()?)),

        #[cfg(feature = "throttle-nftables")]
        "nftables" => Ok(Box::new(download::linux::nftables::NftablesDownload::new()?)),

        #[cfg(feature = "throttle-ifb-tc")]
        "ifb_tc" => Ok(Box::new(download::linux::ifb_tc::IfbTcDownload::new()?)),

        #[cfg(feature = "throttle-tc-police")]
        "tc_police" => Ok(Box::new(
            download::linux::tc_police::TcPoliceDownload::new()?
        )),

        #[cfg(target_os = "macos")]
        "dnctl" => Ok(Box::new(download::macos::DnctlDownload::new()?)),

        _ => Err(anyhow::anyhow!("Unknown download backend: {}", name)),
    }
}
