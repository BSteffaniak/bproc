// ThrottleManager coordinates upload and download throttling backends

use super::{
    BackendInfo, DownloadThrottleBackend, UploadThrottleBackend, create_download_backend,
    create_upload_backend, detect_download_backends, detect_upload_backends,
};
use crate::backends::ActiveThrottle;
use crate::process::ThrottleLimit;
use anyhow::Result;
use std::collections::HashMap;

/// Manages throttling by coordinating multiple concurrent backends
///
/// Each throttle "remembers" which backend it was created with, allowing
/// multiple backends to coexist. New backend selection only affects future
/// throttles, not existing ones.
pub struct ThrottleManager {
    // Pool of initialized backends (lazy-loaded on first use)
    upload_backends: HashMap<String, Box<dyn UploadThrottleBackend>>,
    download_backends: HashMap<String, Box<dyn DownloadThrottleBackend>>,

    // Track which backend each PID uses
    upload_backend_map: HashMap<i32, String>, // pid -> backend_name
    download_backend_map: HashMap<i32, String>, // pid -> backend_name

    // Track process names for each PID
    process_names: HashMap<i32, String>,

    // Default backend for NEW throttles
    default_upload: Option<String>,
    default_download: Option<String>,
}

impl ThrottleManager {
    /// Create a new ThrottleManager with initial default backends
    pub fn new(
        upload_backend: Option<Box<dyn UploadThrottleBackend>>,
        download_backend: Option<Box<dyn DownloadThrottleBackend>>,
    ) -> Self {
        let mut upload_backends = HashMap::new();
        let mut download_backends = HashMap::new();

        let default_upload = if let Some(backend) = upload_backend {
            let name = backend.name().to_string();
            upload_backends.insert(name.clone(), backend);
            Some(name)
        } else {
            None
        };

        let default_download = if let Some(backend) = download_backend {
            let name = backend.name().to_string();
            download_backends.insert(name.clone(), backend);
            Some(name)
        } else {
            None
        };

        Self {
            upload_backends,
            download_backends,
            upload_backend_map: HashMap::new(),
            download_backend_map: HashMap::new(),
            process_names: HashMap::new(),
            default_upload,
            default_download,
        }
    }

    /// Get names of default backends for new throttles
    pub fn backend_names(&self) -> (Option<String>, Option<String>) {
        (self.default_upload.clone(), self.default_download.clone())
    }

    /// Get current default backends (upload, download)
    pub fn get_default_backends(&self) -> (Option<String>, Option<String>) {
        self.backend_names()
    }

    /// Restore a previously captured default selection after a temporary operation.
    /// This is not a new backend selection: `None` must remain `None`, and a
    /// previously selected backend need not be redetected to restore bookkeeping.
    pub(crate) fn restore_default_backends(&mut self, defaults: (Option<String>, Option<String>)) {
        (self.default_upload, self.default_download) = defaults;
    }

    /// Set default upload backend for new throttles
    pub fn set_default_upload_backend(&mut self, name: &str) -> Result<()> {
        // Validate backend is available
        let available = detect_upload_backends();
        if !available.iter().any(|b| b.name == name && b.available) {
            return Err(anyhow::anyhow!("Backend '{}' is not available", name));
        }

        self.default_upload = Some(name.to_string());
        log::info!("Default upload backend set to: {}", name);
        Ok(())
    }

    /// Set default download backend for new throttles
    pub fn set_default_download_backend(&mut self, name: &str) -> Result<()> {
        // Validate backend is available
        let available = detect_download_backends();
        if !available.iter().any(|b| b.name == name && b.available) {
            return Err(anyhow::anyhow!("Backend '{}' is not available", name));
        }

        self.default_download = Some(name.to_string());
        log::info!("Default download backend set to: {}", name);
        Ok(())
    }

    /// Get or create upload backend (lazy initialization)
    fn get_or_create_upload_backend(
        &mut self,
        name: &str,
    ) -> Result<&mut Box<dyn UploadThrottleBackend>> {
        if !self.upload_backends.contains_key(name) {
            log::info!("Initializing upload backend: {}", name);
            let mut backend = create_upload_backend(name)?;
            backend.init()?;
            self.upload_backends.insert(name.to_string(), backend);
        }
        Ok(self.upload_backends.get_mut(name).unwrap())
    }

    /// Get or create download backend (lazy initialization)
    fn get_or_create_download_backend(
        &mut self,
        name: &str,
    ) -> Result<&mut Box<dyn DownloadThrottleBackend>> {
        if !self.download_backends.contains_key(name) {
            log::info!("Initializing download backend: {}", name);
            let mut backend = create_download_backend(name)?;
            backend.init()?;
            self.download_backends.insert(name.to_string(), backend);
        }
        Ok(self.download_backends.get_mut(name).unwrap())
    }

    /// Get statistics about active backends and their throttle counts
    pub fn get_active_backend_stats(&self) -> HashMap<String, usize> {
        let mut stats = HashMap::new();
        for backend_name in self.upload_backend_map.values() {
            *stats.entry(backend_name.clone()).or_insert(0) += 1;
        }
        for backend_name in self.download_backend_map.values() {
            *stats.entry(backend_name.clone()).or_insert(0) += 1;
        }
        stats
    }

    /// Get comprehensive backend information for UI display
    pub fn get_backend_info(
        &self,
        preferred_upload: Option<String>,
        preferred_download: Option<String>,
    ) -> BackendInfo {
        // Get capabilities from default backends if available
        let upload_capabilities = self
            .default_upload
            .as_ref()
            .and_then(|name| self.upload_backends.get(name))
            .map(|b| b.capabilities());

        let download_capabilities = self
            .default_download
            .as_ref()
            .and_then(|name| self.download_backends.get(name))
            .map(|b| b.capabilities());

        BackendInfo {
            active_upload: self.default_upload.clone(),
            active_download: self.default_download.clone(),
            active_monitoring: None, // Will be populated by caller from NetworkMonitor
            active_socket_mapper: None, // Will be populated by caller from NetworkMonitor
            available_upload: detect_upload_backends()
                .into_iter()
                .map(|b| (b.name.to_string(), b.priority, b.available))
                .collect(),
            available_download: detect_download_backends()
                .into_iter()
                .map(|b| (b.name.to_string(), b.priority, b.available))
                .collect(),
            available_socket_mappers: Vec::new(), // Will be populated by caller
            preferred_upload,
            preferred_download,
            preferred_socket_mapper: None, // Will be populated by caller
            upload_capabilities,
            download_capabilities,
            socket_mapper_capabilities: None, // Will be populated by caller
            backend_stats: self.get_active_backend_stats(),
        }
    }

    /// Apply throttle to a process using current default backends
    pub fn throttle_process(
        &mut self,
        pid: i32,
        process_name: String,
        limit: &ThrottleLimit,
    ) -> Result<()> {
        let mut applied_any = false;

        // Store process name for tracking
        self.process_names.insert(pid, process_name.clone());

        // Apply upload throttle if specified AND default backend set
        if let Some(upload_limit) = limit.upload_limit {
            if let Some(backend_name) = &self.default_upload.clone() {
                let backend = self.get_or_create_upload_backend(backend_name)?;
                backend.throttle_upload(
                    pid,
                    process_name.clone(),
                    upload_limit,
                    limit.traffic_type,
                )?;
                self.upload_backend_map.insert(pid, backend_name.clone());
                applied_any = true;
                log::info!(
                    "Applied upload throttle to PID {} using {} backend (traffic type: {:?})",
                    pid,
                    backend_name,
                    limit.traffic_type
                );
            } else {
                log::error!("⚠️  Upload throttling requested but no default backend set");
                log::error!("    Use the 'b' menu to select an upload backend.");
            }
        }

        // Apply download throttle if specified AND default backend set
        if let Some(download_limit) = limit.download_limit {
            if let Some(backend_name) = &self.default_download.clone() {
                let backend = self.get_or_create_download_backend(backend_name)?;
                backend.throttle_download(
                    pid,
                    process_name.clone(),
                    download_limit,
                    limit.traffic_type,
                )?;
                self.download_backend_map.insert(pid, backend_name.clone());
                applied_any = true;
                log::info!(
                    "Applied download throttle to PID {} using {} backend (traffic type: {:?})",
                    pid,
                    backend_name,
                    limit.traffic_type
                );
            } else {
                log::error!("⚠️  Download throttling requested but no default backend set");
                log::error!("    Use the 'b' menu to select a download backend.");
            }
        }

        if !applied_any && (limit.upload_limit.is_some() || limit.download_limit.is_some()) {
            return Err(anyhow::anyhow!("No throttling backends available"));
        }

        Ok(())
    }

    /// Remove all throttles from a process
    /// Routes to the correct backend that created the throttle
    pub fn remove_throttle(&mut self, pid: i32) -> Result<()> {
        let mut errors = Vec::new();

        // Remove upload throttle if it exists
        if let Some(backend_name) = self.upload_backend_map.remove(&pid)
            && let Some(backend) = self.upload_backends.get_mut(&backend_name)
        {
            if let Err(e) = backend.remove_upload_throttle(pid) {
                log::warn!(
                    "Failed to remove upload throttle for PID {} from {} backend: {}",
                    pid,
                    backend_name,
                    e
                );
                errors.push(e);
            } else {
                log::info!(
                    "Removed upload throttle for PID {} from {} backend",
                    pid,
                    backend_name
                );
            }
        }

        // Remove download throttle if it exists
        if let Some(backend_name) = self.download_backend_map.remove(&pid)
            && let Some(backend) = self.download_backends.get_mut(&backend_name)
        {
            if let Err(e) = backend.remove_download_throttle(pid) {
                log::warn!(
                    "Failed to remove download throttle for PID {} from {} backend: {}",
                    pid,
                    backend_name,
                    e
                );
                errors.push(e);
            } else {
                log::info!(
                    "Removed download throttle for PID {} from {} backend",
                    pid,
                    backend_name
                );
            }
        }

        // Clean up process name
        self.process_names.remove(&pid);

        if !errors.is_empty() {
            return Err(anyhow::anyhow!(
                "Failed to remove some throttles: {:?}",
                errors
            ));
        }

        Ok(())
    }

    /// Get all active throttles (merged from all backends)
    pub fn get_all_throttles(&self) -> HashMap<i32, ActiveThrottle> {
        let mut throttles: HashMap<i32, ActiveThrottle> = HashMap::new();

        // Collect upload throttles from all backends
        for backend in self.upload_backends.values() {
            for (pid, upload_limit) in backend.get_all_throttles() {
                let process_name = self.process_names.get(&pid).cloned().unwrap_or_default();
                throttles.entry(pid).or_insert_with(|| ActiveThrottle {
                    pid,
                    process_name,
                    upload_limit: Some(upload_limit),
                    download_limit: None,
                });
                if let Some(throttle) = throttles.get_mut(&pid) {
                    throttle.upload_limit = Some(upload_limit);
                }
            }
        }

        // Collect download throttles from all backends
        for backend in self.download_backends.values() {
            for (pid, download_limit) in backend.get_all_throttles() {
                let process_name = self.process_names.get(&pid).cloned().unwrap_or_default();
                throttles.entry(pid).or_insert_with(|| ActiveThrottle {
                    pid,
                    process_name,
                    upload_limit: None,
                    download_limit: Some(download_limit),
                });
                if let Some(throttle) = throttles.get_mut(&pid) {
                    throttle.download_limit = Some(download_limit);
                }
            }
        }

        throttles
    }

    /// Cleanup all throttles from all backends
    pub fn cleanup(&mut self) -> Result<()> {
        let mut errors = Vec::new();

        // Cleanup all upload backends
        for (name, backend) in &mut self.upload_backends {
            if let Err(e) = backend.cleanup() {
                log::error!("Failed to cleanup upload backend {}: {}", name, e);
                errors.push(e);
            }
        }

        // Cleanup all download backends
        for (name, backend) in &mut self.download_backends {
            if let Err(e) = backend.cleanup() {
                log::error!("Failed to cleanup download backend {}: {}", name, e);
                errors.push(e);
            }
        }

        if !errors.is_empty() {
            return Err(anyhow::anyhow!(
                "Failed to cleanup some backends: {:?}",
                errors
            ));
        }

        Ok(())
    }

    /// Check if current upload backend supports a specific traffic type
    pub fn current_upload_backend_supports(
        &self,
        traffic_type: crate::process::TrafficType,
    ) -> bool {
        if let Some(backend_name) = &self.default_upload
            && let Some(backend) = self.upload_backends.get(backend_name)
        {
            return backend.supports_traffic_type(traffic_type);
        }
        false
    }

    /// Check if current download backend supports a specific traffic type
    pub fn current_download_backend_supports(
        &self,
        traffic_type: crate::process::TrafficType,
    ) -> bool {
        if let Some(backend_name) = &self.default_download
            && let Some(backend) = self.download_backends.get(backend_name)
        {
            return backend.supports_traffic_type(traffic_type);
        }
        false
    }

    /// Find all available upload backends that support the given traffic type
    pub fn find_compatible_upload_backends(
        &self,
        traffic_type: crate::process::TrafficType,
    ) -> Vec<String> {
        self.upload_backends
            .iter()
            .filter(|(_, backend)| backend.supports_traffic_type(traffic_type))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Find all available download backends that support the given traffic type
    pub fn find_compatible_download_backends(
        &self,
        traffic_type: crate::process::TrafficType,
    ) -> Vec<String> {
        self.download_backends
            .iter()
            .filter(|(_, backend)| backend.supports_traffic_type(traffic_type))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

impl Drop for ThrottleManager {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod default_selection_tests {
    use super::ThrottleManager;

    #[test]
    fn temporary_selection_restores_absent_and_existing_defaults_exactly() {
        let mut manager = ThrottleManager::new(None, None);
        let previous = manager.get_default_backends();
        manager.default_upload = Some("temporary".into());
        manager.restore_default_backends(previous);
        assert_eq!(manager.get_default_backends(), (None, None));

        let captured = (
            Some("original-upload".into()),
            Some("original-download".into()),
        );
        manager.restore_default_backends(captured.clone());
        assert_eq!(manager.get_default_backends(), captured);
    }
}
