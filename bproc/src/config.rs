// Configuration save/restore functionality

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_DIR: &str = ".config/bproc";
const CONFIG_FILE: &str = "throttles.json";

/// Saved throttle configuration for a process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedThrottle {
    pub process_name: String,
    /// Executable path; restoration matches name + exe because PIDs do not
    /// survive restarts (the map key is only the PID at save time).
    #[serde(default)]
    pub exe: Option<String>,
    /// Start time of the throttled process (seconds since the epoch), so a
    /// restart re-applies the throttle only to that same process.
    #[serde(default)]
    pub start_time: Option<u64>,
    /// Command line at save time; narrows re-targeting after a restart.
    #[serde(default)]
    pub cmdline: Option<String>,
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
    #[serde(default, deserialize_with = "lenient")]
    pub traffic_view_mode: Option<crate::process::TrafficType>,

    /// Process explorer preferences (columns, sort, refresh, layout). An
    /// unreadable section falls back to defaults without losing the rest.
    #[serde(default, deserialize_with = "lenient")]
    pub explorer: ExplorerSettings,

    /// Watch / alert rules. Rules this build cannot read are skipped one by
    /// one instead of discarding the whole file.
    #[serde(default, deserialize_with = "lenient_vec")]
    pub watch_rules: Vec<crate::procs::watch::WatchRule>,
}

/// Deserialize `T`, falling back to its default when the value does not fit
/// (an enum variant from a newer build, a hand-edit typo).
fn lenient<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|e| {
        log::warn!("Ignoring unreadable config value: {e}");
        T::default()
    }))
}

fn lenient_preset<'de, D>(
    deserializer: D,
) -> std::result::Result<crate::procs::columns::Preset, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or(ExplorerSettings::default().preset))
}

/// Deserialize a list, dropping elements that do not parse.
fn lenient_vec<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|v| match serde_json::from_value(v) {
            Ok(item) => Some(item),
            Err(e) => {
                log::warn!("Skipping unreadable config entry: {e}");
                None
            }
        })
        .collect())
}

/// Column lists per preset, dropping column ids this build does not know.
fn lenient_columns<'de, D>(
    deserializer: D,
) -> std::result::Result<HashMap<String, Vec<crate::procs::columns::ColumnId>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = HashMap::<String, serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|(preset, columns)| {
            let columns: Vec<serde_json::Value> = serde_json::from_value(columns).ok()?;
            Some((
                preset,
                columns
                    .into_iter()
                    .filter_map(|c| serde_json::from_value(c).ok())
                    .collect(),
            ))
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DockPosition {
    #[default]
    Bottom,
    Right,
}

/// Persisted process explorer preferences. Every field has a default so
/// older config files keep loading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExplorerSettings {
    #[serde(deserialize_with = "lenient_preset")]
    pub preset: crate::procs::columns::Preset,
    /// Column overrides per preset (absent = preset defaults).
    #[serde(deserialize_with = "lenient_columns")]
    pub preset_columns: HashMap<String, Vec<crate::procs::columns::ColumnId>>,
    /// User-resized column widths.
    #[serde(deserialize_with = "lenient")]
    pub column_widths: HashMap<String, u16>,
    #[serde(deserialize_with = "lenient")]
    pub sort_column: Option<crate::procs::columns::ColumnId>,
    pub sort_descending: bool,
    pub tree: bool,
    pub refresh_ms: u64,
    pub hide_kernel_threads: bool,
    pub only_mine: bool,
    pub net_active_only: bool,
    #[serde(deserialize_with = "lenient")]
    pub dock: DockPosition,
    /// Detail panel height (bottom) or width (right) in cells.
    pub dock_size: u16,
    pub show_meters: bool,
    pub per_core_meters: bool,
    pub heat_colors: bool,
    /// Pause re-sorting while the pointer hovers the table.
    pub hover_freeze: bool,
    pub confirm_signals: bool,
    /// Offer SIGKILL when a terminated process is still alive after this long.
    pub escalate_after_secs: u64,
}

impl Default for ExplorerSettings {
    fn default() -> Self {
        Self {
            preset: crate::procs::columns::Preset::Overview,
            preset_columns: HashMap::new(),
            column_widths: HashMap::new(),
            sort_column: None,
            sort_descending: true,
            tree: false,
            refresh_ms: 1500,
            hide_kernel_threads: true,
            only_mine: false,
            net_active_only: false,
            dock: DockPosition::Bottom,
            dock_size: 14,
            show_meters: true,
            per_core_meters: true,
            heat_colors: true,
            hover_freeze: true,
            confirm_signals: true,
            escalate_after_secs: 5,
        }
    }
}

impl ExplorerSettings {
    pub fn columns_for(
        &self,
        preset: crate::procs::columns::Preset,
    ) -> Vec<crate::procs::columns::ColumnId> {
        self.preset_columns
            .get(preset.label())
            .filter(|c| !c.is_empty())
            .cloned()
            .unwrap_or_else(|| preset.default_columns())
    }
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
            explorer: ExplorerSettings::default(),
            watch_rules: Vec::new(),
        }
    }
}

/// Result of reading the config file at startup.
#[derive(Debug, Default)]
pub struct Loaded {
    pub config: Config,
    /// Set when the file existed but could not be used; the session should
    /// tell the user (the unreadable file has been moved aside).
    pub warning: Option<String>,
}

impl Config {
    /// Get the config file path
    /// Uses platform-specific config directories:
    /// - Linux: ~/.config/bproc
    /// - macOS: ~/Library/Application Support/bproc
    /// - Windows: C:\Users\{user}\AppData\Roaming\bproc
    ///
    /// Under `sudo` this is the invoking user's config, so a privileged
    /// session shares watch rules, columns and saved throttles with normal
    /// runs instead of using root's (or leaving a root-owned file behind).
    pub fn config_path() -> Result<PathBuf> {
        let config_dir = config_dir()?;
        let existed = config_dir.is_dir();
        fs::create_dir_all(&config_dir).context(format!(
            "Failed to create config directory: {:?}",
            config_dir
        ))?;
        if !existed {
            chown_to_invoker(&config_dir);
        }
        Ok(config_dir.join(CONFIG_FILE))
    }

    /// Load configuration from disk
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path()?)
    }

    fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            log::debug!("Config file not found, using defaults");
            return Ok(Config::default());
        }

        let contents =
            fs::read_to_string(path).context(format!("Failed to read config file: {:?}", path))?;

        let config: Config =
            serde_json::from_str(&contents).context("Failed to parse config file")?;

        log::info!("Loaded configuration from {:?}", path);
        Ok(config)
    }

    /// Load for an interactive session. A missing file means defaults; a file
    /// that exists but cannot be parsed is renamed to
    /// `throttles.json.bak-<unix time>` so the next save cannot silently
    /// overwrite the user's rules and throttles, and a warning is returned.
    pub fn load_checked() -> Loaded {
        match Self::config_path() {
            Ok(path) => Self::load_checked_from(&path),
            Err(e) => Loaded {
                config: Config::default(),
                warning: Some(format!("Config unavailable: {e:#}")),
            },
        }
    }

    fn load_checked_from(path: &Path) -> Loaded {
        match Self::load_from(path) {
            Ok(config) => Loaded {
                config,
                warning: None,
            },
            Err(error) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or_default();
                let mut backup = path.as_os_str().to_owned();
                backup.push(format!(".bak-{stamp}"));
                let backup = PathBuf::from(backup);
                let warning = match fs::rename(path, &backup) {
                    Ok(()) => format!(
                        "Config could not be read ({error:#}); backed up to {} and starting with defaults",
                        backup.display()
                    ),
                    Err(e) => format!(
                        "Config could not be read ({error:#}) or backed up ({e}); starting with defaults"
                    ),
                };
                log::warn!("{warning}");
                Loaded {
                    config: Config::default(),
                    warning: Some(warning),
                }
            }
        }
    }

    /// Save configuration to disk
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::config_path()?)
    }

    /// Write atomically: a crash mid-save leaves the previous file intact
    /// instead of a truncated one.
    fn save_to(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let contents = serde_json::to_string_pretty(self).context("Failed to serialize config")?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let write = || -> std::io::Result<()> {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            Ok(())
        };
        if let Err(e) = write() {
            let _ = fs::remove_file(&tmp);
            return Err(e).context(format!("Failed to write config file: {:?}", tmp));
        }
        chown_to_invoker(&tmp);
        fs::rename(&tmp, path).context(format!("Failed to replace config file: {:?}", path))?;

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

fn config_dir() -> Result<PathBuf> {
    if let Some(home) = sudo_invoker().map(|i| i.home) {
        return Ok(platform_config_dir(&home));
    }
    if let Some(config_base) = dirs::config_dir() {
        // Prefer XDG config directory on Linux, AppData on Windows, Library on macOS
        Ok(config_base.join("bproc"))
    } else if let Some(home) = dirs::home_dir() {
        // Fallback: use ~/.config/bproc on all platforms
        Ok(home.join(CONFIG_DIR))
    } else {
        anyhow::bail!("Could not determine config directory (no home directory found)");
    }
}

/// `dirs::config_dir()` for a given home directory.
fn platform_config_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/bproc")
    } else {
        home.join(CONFIG_DIR)
    }
}

/// The user who ran `sudo bproc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invoker {
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

/// The invoking user when running as root through sudo, else `None`.
#[cfg(unix)]
pub fn sudo_invoker() -> Option<Invoker> {
    if unsafe { libc::geteuid() } != 0 {
        return None;
    }
    let user = std::env::var("SUDO_USER").ok().filter(|u| u != "root")?;
    let uid: u32 = std::env::var("SUDO_UID").ok()?.parse().ok()?;
    let gid: u32 = std::env::var("SUDO_GID").ok()?.parse().ok()?;
    let name = std::ffi::CString::new(user).ok()?;
    // SAFETY: getpwnam returns a pointer into static storage or null; the
    // home directory is copied out before any other passwd call.
    let home = unsafe {
        let pw = libc::getpwnam(name.as_ptr());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*pw).pw_dir)
            .to_string_lossy()
            .into_owned()
    };
    Some(Invoker {
        uid,
        gid,
        home: PathBuf::from(home),
    })
}

#[cfg(not(unix))]
pub fn sudo_invoker() -> Option<Invoker> {
    None
}

/// Hand a file bproc created under sudo back to the invoking user.
pub fn chown_to_invoker(path: &Path) {
    #[cfg(unix)]
    if let Some(invoker) = sudo_invoker() {
        use std::os::unix::ffi::OsStrExt;
        if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
            // SAFETY: valid NUL-terminated path; failure only leaves root ownership.
            unsafe {
                libc::chown(c.as_ptr(), invoker.uid, invoker.gid);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Home directory of the person running bproc (the sudo invoker under sudo).
pub fn invoking_home_dir() -> Option<PathBuf> {
    sudo_invoker().map(|i| i.home).or_else(dirs::home_dir)
}

/// Downloads directory of the person running bproc.
pub fn invoking_download_dir() -> Option<PathBuf> {
    match sudo_invoker() {
        Some(invoker) => Some(invoker.home.join("Downloads")),
        None => dirs::download_dir(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bproc-config-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(CONFIG_FILE)
    }

    #[test]
    fn unparseable_config_is_backed_up_not_overwritten() {
        let path = temp_path("corrupt");
        fs::write(&path, "{\"throttles\": {\"1\": ").unwrap();
        let loaded = Config::load_checked_from(&path);
        assert!(
            loaded
                .warning
                .as_deref()
                .is_some_and(|w| w.contains("backed up"))
        );
        assert!(!path.exists(), "the unreadable file was moved aside");
        let dir = path.parent().unwrap();
        let backups: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .collect();
        assert_eq!(backups.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_config_is_defaults_without_warning() {
        let path = temp_path("missing");
        let _ = fs::remove_file(&path);
        let loaded = Config::load_checked_from(&path);
        assert!(loaded.warning.is_none());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_is_atomic_and_round_trips() {
        let path = temp_path("atomic");
        let mut config = Config::default();
        config.explorer.tree = true;
        config.save_to(&path).unwrap();
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(!PathBuf::from(tmp).exists());
        assert!(Config::load_from(&path).unwrap().explorer.tree);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn unknown_columns_and_variants_do_not_discard_the_file() {
        let json = r#"{
            "throttles": {"7": {"process_name": "node", "upload_limit": 5, "download_limit": null}},
            "explorer": {
                "preset": "FuturePreset",
                "tree": true,
                "sort_column": "FutureColumn",
                "preset_columns": {"Overview": ["Pid", "FutureColumn", "Cpu"]}
            },
            "watch_rules": [{"bogus": true}]
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.throttles.len(), 1);
        assert!(config.explorer.tree);
        assert_eq!(config.explorer.sort_column, None);
        assert_eq!(
            config.explorer.preset_columns["Overview"],
            vec![
                crate::procs::columns::ColumnId::Pid,
                crate::procs::columns::ColumnId::Cpu
            ]
        );
        assert!(config.watch_rules.is_empty());
    }

    #[test]
    fn test_config_serialization() {
        let mut config = Config::default();
        config.set_throttle(
            1234,
            SavedThrottle {
                process_name: "firefox".to_string(),
                exe: None,
                start_time: None,
                cmdline: None,
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
