// Cross-platform socket-to-PID mapping backend system
//
// This module provides a trait-based abstraction for socket-to-PID mapping
// that works across different operating systems:
// - Linux: procfs (/proc/net/tcp, etc.)
// - macOS: lsof, libproc
// - Windows (future): netstat, WMI

use super::ConnectionMap;
use crate::backends::{BackendCapabilities, BackendPriority};
use anyhow::Result;

/// Cross-platform socket-to-PID mapping backend trait
///
/// This trait abstracts the platform-specific mechanisms for determining
/// which process owns which network socket/connection.
pub trait SocketMapperBackend: Send + Sync {
    /// Backend name (e.g., "procfs", "lsof", "libproc")
    fn name(&self) -> &'static str;

    /// Backend priority for auto-selection; detection metadata also carries this value.
    #[allow(dead_code)]
    fn priority(&self) -> BackendPriority;

    /// Check if this backend is available on the current system
    fn is_available() -> bool
    where
        Self: Sized;

    /// Get backend capabilities
    #[cfg_attr(not(feature = "monitor-pnet"), allow(dead_code))]
    fn capabilities(&self) -> BackendCapabilities;

    /// Get complete socket-to-PID connection map
    ///
    /// This is the core method that each platform implements differently:
    /// - Linux reads /proc filesystem
    /// - macOS calls lsof or libproc
    /// - Windows uses netstat or WMI
    fn get_connection_map(&self) -> Result<ConnectionMap>;
}

/// Socket mapper backend metadata for selection
#[derive(Debug, Clone)]
pub struct SocketMapperInfo {
    pub name: &'static str,
    pub priority: BackendPriority,
    pub available: bool,
}

// Platform-specific modules
#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

// Platform-specific re-exports and functions
#[cfg(target_os = "linux")]
pub use linux::{detect_socket_mappers, select_socket_mapper};

#[cfg(target_os = "macos")]
pub use macos::{detect_socket_mappers, select_socket_mapper};

#[cfg(target_os = "windows")]
pub use windows::{detect_socket_mappers, select_socket_mapper};
