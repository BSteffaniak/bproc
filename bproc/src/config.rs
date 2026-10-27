// Configuration save/restore functionality

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

const CONFIG_DIR: &str = ".config/bproc";
const CONFIG_FILE: &str = "throttles.json";

/// Saved throttle configuration for a process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedThrottle {
    pub process_name: String,
    pub upload_limit: Option<u64>,
    pub download_limit: Option<u64>,
}

/// Configuration file structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Throttles by PID
    #[serde(default)]
    pub throttles: HashMap<i32, SavedThrottle>,

    /// Auto-restore throttles on startup
    #[serde(default = "default_auto_restore")]
    pub auto_restore: bool,

    /// Preferred upload backend
    #[serde(default)]
    pub preferred_upload_backend: Option<String>,

    /// Preferred download backend
    #[serde(default)]
    pub preferred_download_backend: Option<String>,

    /// Preferred socket mapper backend
    #[serde(default)]
    pub preferred_socket_mapper: Option<String>,

    /// Interface filter: None = show all, Some([]) = show nothing, Some([...]) = filter to these
    #[serde(default)]
    pub filtered_interfaces: Option<Vec<String>>,

    /// Traffic view mode: All, Internet, or Local
    #[serde(default)]
    pub traffic_view_mode: Option<crate::process::TrafficType>,
}

fn default_auto_restore() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            throttles: HashMap::new(),
            auto_restore: true,
            preferred_upload_backend: None,
            preferred_download_backend: None,
            preferred_socket_mapper: None,
            filtered_interfaces: None, // Show all by default
            traffic_view_mode: None,   // Use default (All) if not set
        }
    }
}

impl Config {
    /// Get the config file path
    /// Uses platform-specific config directories:
    /// - Linux: ~/.config/bproc
    /// - macOS: ~/Library/Application Support/bproc
    /// - Windows: C:\Users\{user}\AppData\Roaming\bproc
    pub fn config_path() -> Result<PathBuf> {
        // Try to use platform-specific config directory first
        let config_dir = if let Some(config_base) = dirs::config_dir() {
            // Prefer XDG config directory on Linux, AppData on Windows, Library on macOS
            config_base.join("bproc")
        } else if let Some(home) = dirs::home_dir() {
            // Fallback: use ~/.config/bproc on all platforms
            home.join(CONFIG_DIR)
        } else {
            // Last resort fallback
            anyhow::bail!("Could not determine config directory (no home directory found)");
        };

        // Create config directory if it doesn't exist
        fs::create_dir_all(&config_dir).context(format!(
            "Failed to create config directory: {:?}",
            config_dir
        ))?;

        Ok(config_dir.join(CONFIG_FILE))
    }

    /// Load configuration from disk
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;

        if !path.exists() {
            log::debug!("Config file not found, using defaults");
            return Ok(Config::default());
        }

        let contents =
            fs::read_to_string(&path).context(format!("Failed to read config file: {:?}", path))?;

        let config: Config =
            serde_json::from_str(&contents).context("Failed to parse config file")?;

        log::info!("Loaded configuration from {:?}", path);
        Ok(config)
    }

    /// Save configuration to disk
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;

        let contents = serde_json::to_string_pretty(self).context("Failed to serialize config")?;

        fs::write(&path, contents).context(format!("Failed to write config file: {:?}", path))?;

        log::info!("Saved configuration to {:?}", path);
        Ok(())
    }

    /// Add or update a throttle
    pub fn set_throttle(&mut self, pid: i32, throttle: SavedThrottle) {
        self.throttles.insert(pid, throttle);
    }

    /// Get all saved throttles
    pub fn get_throttles(&self) -> &HashMap<i32, SavedThrottle> {
        &self.throttles
    }

    /// Clear all throttles
    pub fn clear_throttles(&mut self) {
        self.throttles.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_serialization() {
        let mut config = Config::default();
        config.set_throttle(
            1234,
            SavedThrottle {
                process_name: "firefox".to_string(),
                upload_limit: Some(1000000),
                download_limit: Some(5000000),
            },
        );

        let json = serde_json::to_string_pretty(&config).unwrap();
        println!("{}", json);

        let deserialized: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.throttles.len(), 1);
        assert!(deserialized.auto_restore);
    }
}
