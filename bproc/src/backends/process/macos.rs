// macOS-specific process utilities

use super::socket_mapper::{SocketMapperBackend, select_socket_mapper};
use super::{ConnectionMap, ProcessEntry, ProcessUtils};
use anyhow::Result;
use sysinfo::{Pid, System};

/// macOS process utilities with pluggable socket mapping
pub struct MacOSProcessUtils {
    socket_mapper: Box<dyn SocketMapperBackend>,
    #[cfg_attr(not(feature = "monitor-pnet"), allow(dead_code))]
    socket_mapper_name: String,
}

impl MacOSProcessUtils {
    /// Create with a specific socket mapper backend
    pub fn with_socket_mapper(backend_name: Option<&str>) -> Self {
        let socket_mapper = select_socket_mapper(backend_name)
            .expect("Failed to initialize socket mapper on macOS");

        let socket_mapper_name = socket_mapper.name().to_string();
        log::debug!("Using socket mapper backend: {}", socket_mapper_name);

        Self {
            socket_mapper,
            socket_mapper_name,
        }
    }

    /// Get the name of the socket mapper backend
    #[cfg(feature = "monitor-pnet")]
    pub fn socket_mapper_name(&self) -> &str {
        &self.socket_mapper_name
    }

    /// Get capabilities of the socket mapper backend
    #[cfg(feature = "monitor-pnet")]
    pub fn socket_mapper_capabilities(&self) -> crate::backends::BackendCapabilities {
        self.socket_mapper.capabilities()
    }
}

impl ProcessUtils for MacOSProcessUtils {
    fn get_process_name(&self, pid: i32) -> Result<String> {
        let sys = System::new_all();
        let pid_obj = Pid::from_u32(pid as u32);

        sys.process(pid_obj)
            .map(|p| p.name().to_str().unwrap_or("unknown").to_string())
            .ok_or_else(|| anyhow::anyhow!("Process {} not found", pid))
    }

    fn process_exists(&self, pid: i32) -> bool {
        let sys = System::new_all();
        let pid_obj = Pid::from_u32(pid as u32);
        sys.process(pid_obj).is_some()
    }

    fn get_all_processes(&self) -> Result<Vec<ProcessEntry>> {
        let sys = System::new_all();

        let entries = sys
            .processes()
            .iter()
            .map(|(pid, proc)| ProcessEntry {
                pid: pid.as_u32() as i32,
                name: proc.name().to_str().unwrap_or("unknown").to_string(),
            })
            .collect();

        Ok(entries)
    }

    fn get_connection_map(&self) -> Result<ConnectionMap> {
        // Delegate to pluggable socket mapper backend
        self.socket_mapper.get_connection_map()
    }
}
