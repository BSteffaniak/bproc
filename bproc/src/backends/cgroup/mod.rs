//! Cgroup backend abstraction layer
//!
//! This module provides a unified interface for working with cgroups across
//! different kernel versions (v1 and v2). Different throttling backends can
//! use this abstraction to manage per-process network isolation without
//! worrying about cgroup version details.
//!
//! # Architecture
//!
//! - `CgroupBackend` trait: Core interface for cgroup operations
//! - `CgroupV1Backend`: Uses net_cls controller + classid tagging
//! - `CgroupV2NftablesBackend`: Uses unified hierarchy + nftables socket matching
//! - `CgroupV2EbpfBackend`: (Future) Uses unified hierarchy + eBPF TC classifiers
//!
//! # Feature Flags
//!
//! - `cgroup-v1`: Enable cgroup v1 backend (default)
//! - `cgroup-v2-nftables`: Enable v2 nftables backend (default)
//! - `cgroup-v2-ebpf`: Enable v2 eBPF TC backend (future)

use anyhow::Result;
use std::fmt;

#[cfg(feature = "cgroup-v1")]
pub mod v1;

#[cfg(any(feature = "cgroup-v2-nftables", feature = "cgroup-v2-ebpf"))]
pub mod v2;

/// Identifies which cgroup version/implementation to use
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupBackendType {
    /// Cgroup v1 with net_cls controller
    V1,
    /// Cgroup v2 with nftables socket matching
    V2Nftables,
    /// Cgroup v2 with eBPF TC classifier (future)
    V2Ebpf,
}

impl fmt::Display for CgroupBackendType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CgroupBackendType::V1 => write!(f, "cgroup-v1"),
            CgroupBackendType::V2Nftables => write!(f, "cgroup-v2-nftables"),
            CgroupBackendType::V2Ebpf => write!(f, "cgroup-v2-ebpf"),
        }
    }
}

/// Handle to a cgroup for a specific process
///
/// This is an opaque handle that different backends can use to track
/// cgroup state. The actual implementation varies by backend.
#[derive(Debug, Clone)]
pub struct CgroupHandle {
    /// Process ID this cgroup was created for
    pub pid: i32,
    /// Backend-specific identifier (path, classid, etc.)
    pub identifier: String,
    /// Which backend created this handle
    pub backend_type: CgroupBackendType,
}

/// Core trait for cgroup backend implementations
///
/// This trait provides the essential operations needed by throttling backends:
/// - Creating isolated cgroups for processes
/// - Getting identifiers for use in firewall/TC rules
/// - Cleaning up cgroups when done
pub trait CgroupBackend: Send + Sync {
    /// Get the backend type identifier
    fn backend_type(&self) -> CgroupBackendType;

    /// Check if this backend is available on the current system
    ///
    /// This should check for:
    /// - Cgroup filesystem mounted at expected location
    /// - Required controllers available
    /// - Necessary permissions
    fn is_available(&self) -> Result<bool>;

    /// Get a human-readable description of why this backend is unavailable
    ///
    /// Only called if `is_available()` returns false
    fn unavailable_reason(&self) -> String {
        "Backend is not available on this system".to_string()
    }

    /// Create or join a cgroup for the given process
    ///
    /// This should:
    /// 1. Create the cgroup hierarchy if needed
    /// 2. Add the process to the cgroup
    /// 3. Set up any backend-specific tagging (classid, etc.)
    /// 4. Return a handle for use in firewall/TC rules
    ///
    /// # Arguments
    ///
    /// * `pid` - Process ID to isolate
    /// * `name` - Human-readable name for the cgroup (e.g., process name)
    ///
    /// # Returns
    ///
    /// A `CgroupHandle` containing the identifier needed for firewall/TC rules
    fn create_cgroup(&self, pid: i32, name: &str) -> Result<CgroupHandle>;

    /// Remove a process from its cgroup and clean up
    ///
    /// This should:
    /// 1. Remove the process from the cgroup (if still running)
    /// 2. Delete the cgroup directory/hierarchy
    /// 3. Clean up any backend-specific state (classid allocations, etc.)
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle returned from `create_cgroup()`
    fn remove_cgroup(&self, handle: &CgroupHandle) -> Result<()>;

    /// Get the filter expression for use in firewall/TC rules
    ///
    /// This returns the backend-specific syntax for matching traffic from
    /// the cgroup in firewall rules or TC filters.
    ///
    /// # Examples
    ///
    /// - V1 net_cls: `"classid 1:42"` (for TC filter)
    /// - V2 nftables: `"socket cgroupv2 \"/sys/fs/cgroup/bproc/pid_1234\""` (for nft rule)
    /// - V2 eBPF: Path for TC classifier attachment
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle returned from `create_cgroup()`
    fn get_filter_expression(&self, handle: &CgroupHandle) -> String;

    /// List all active cgroups managed by this backend
    ///
    /// Useful for debugging and cleanup on startup
    fn list_active_cgroups(&self) -> Result<Vec<CgroupHandle>>;
}

/// Select the best available cgroup backend at runtime
///
/// Tries backends in order of preference:
/// 1. Cgroup v2 + eBPF (best performance, most flexible)
/// 2. Cgroup v2 + nftables (good, works on most modern systems)
/// 3. Cgroup v1 + net_cls (fallback for older systems)
///
/// # Returns
///
/// - `Ok(Some(backend))` - A working backend
/// - `Ok(None)` - No backends available
/// - `Err(_)` - Error checking backend availability
pub fn select_best_backend() -> Result<Option<Box<dyn CgroupBackend>>> {
    // Try v2 eBPF (future)
    #[cfg(feature = "cgroup-v2-ebpf")]
    {
        let backend = v2::ebpf::CgroupV2EbpfBackend::new()?;
        if backend.is_available()? {
            log::trace!("Selected cgroup backend: cgroup-v2-ebpf (best)");
            return Ok(Some(Box::new(backend)));
        }
    }

    // Try v2 nftables
    #[cfg(feature = "cgroup-v2-nftables")]
    {
        let backend = v2::nftables::CgroupV2NftablesBackend::new()?;
        if backend.is_available()? {
            log::trace!("Selected cgroup backend: cgroup-v2-nftables (good)");
            return Ok(Some(Box::new(backend)));
        }
    }

    // Try v1 net_cls (fallback)
    #[cfg(feature = "cgroup-v1")]
    {
        let backend = v1::CgroupV1Backend::new()?;
        if backend.is_available()? {
            log::trace!("Selected cgroup backend: cgroup-v1 (fallback)");
            return Ok(Some(Box::new(backend)));
        }
    }

    log::warn!("No cgroup backends available");
    Ok(None)
}

/// Check if cgroup v1 with net_cls controller is available
///
/// This is used by backends that specifically require cgroup v1,
/// such as ifb_tc which uses TC cgroup filter (only works with v1).
pub fn is_cgroup_v1_available() -> bool {
    #[cfg(feature = "cgroup-v1")]
    {
        if let Ok(backend) = v1::CgroupV1Backend::new() {
            if let Ok(available) = backend.is_available() {
                return available;
            }
        }
    }
    false
}

/// List all compiled-in cgroup backends and their availability
///
/// Useful for debugging and the `--list-backends` CLI flag
pub fn list_all_backends() -> Vec<(CgroupBackendType, bool, String)> {
    // Feature-gated backends append below; the monitoring-only build has none.
    #[cfg_attr(
        not(any(feature = "cgroup-v1", feature = "cgroup-v2-nftables")),
        allow(unused_mut)
    )]
    let mut backends = Vec::new();

    #[cfg(feature = "cgroup-v1")]
    {
        let backend = v1::CgroupV1Backend::new();
        match backend {
            Ok(b) => {
                let available = b.is_available().unwrap_or(false);
                let reason = if available {
                    "Available".to_string()
                } else {
                    b.unavailable_reason()
                };
                backends.push((CgroupBackendType::V1, available, reason));
            }
            Err(e) => {
                backends.push((CgroupBackendType::V1, false, format!("Error: {}", e)));
            }
        }
    }

    #[cfg(feature = "cgroup-v2-nftables")]
    {
        let backend = v2::nftables::CgroupV2NftablesBackend::new();
        match backend {
            Ok(b) => {
                let available = b.is_available().unwrap_or(false);
                let reason = if available {
                    "Available".to_string()
                } else {
                    b.unavailable_reason()
                };
                backends.push((CgroupBackendType::V2Nftables, available, reason));
            }
            Err(e) => {
                backends.push((
                    CgroupBackendType::V2Nftables,
                    false,
                    format!("Error: {}", e),
                ));
            }
        }
    }

    #[cfg(feature = "cgroup-v2-ebpf")]
    {
        let backend = v2::ebpf::CgroupV2EbpfBackend::new();
        match backend {
            Ok(b) => {
                let available = b.is_available().unwrap_or(false);
                let reason = if available {
                    "Available".to_string()
                } else {
                    b.unavailable_reason()
                };
                backends.push((CgroupBackendType::V2Ebpf, available, reason));
            }
            Err(e) => {
                backends.push((CgroupBackendType::V2Ebpf, false, format!("Error: {}", e)));
            }
        }
    }

    backends
}
