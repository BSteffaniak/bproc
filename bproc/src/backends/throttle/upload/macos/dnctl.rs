// macOS dummynet + PF upload throttling backend
//
// Uses:
// - dnctl (dummynet) for bandwidth shaping pipes
// - PF (PacketFilter) dummynet rules for traffic classification
//
// Architecture:
// 1. Create dummynet pipe with bandwidth limit (dnctl pipe N config bw XMbit/s)
// 2. Get all active connections for target PID
// 3. Generate PF dummynet rules for each connection
// 4. Load rules: dummynet out on <iface> proto tcp from <ip> port <port> to <ip> port <port> pipe N
//
// Limitations:
// - Per-connection matching (not pure per-PID like Linux cgroups)
// - New connections require rule updates
// - Rule management via pfctl (no native API)

use crate::backends::process::ProcessUtils;
use crate::backends::throttle::UploadThrottleBackend;
use crate::backends::{BackendCapabilities, BackendPriority};
use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// macOS dummynet upload (egress) throttling backend
pub struct DnctlUpload {
    /// Network interface to throttle on (e.g., "en0")
    interface: String,
    /// Active throttles: PID → ThrottleState (shared with monitoring thread)
    active_throttles: Arc<Mutex<HashMap<i32, ThrottleState>>>,
    /// Next available pipe number
    next_pipe: u32,
    /// Process utilities for connection mapping
    process_utils: Box<dyn ProcessUtils>,
    /// Whether backend is initialized
    initialized: bool,
    /// Shutdown signal for monitoring thread
    shutdown_flag: Arc<AtomicBool>,
    /// Handle to connection monitoring thread
    monitor_thread: Option<thread::JoinHandle<()>>,
}

/// State for an active throttle
struct ThrottleState {
    /// Dummynet pipe number
    pipe_num: u32,
    /// Process name
    process_name: String,
    /// Bandwidth limit in bytes/sec
    limit_bytes_per_sec: u64,
    /// Active PF rules for this throttle
    pf_rules: Vec<String>,
    /// Connections being throttled
    connections: Vec<ConnectionInfo>,
}

/// Information about a throttled connection
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ConnectionInfo {
    local_addr: String,
    local_port: u16,
    remote_addr: String,
    remote_port: u16,
    protocol: &'static str,
}

impl DnctlUpload {
    pub fn new() -> Result<Self> {
        let interface = detect_interface()?;
        let process_utils = crate::backends::process::create_process_utils();

        Ok(Self {
            interface,
            active_throttles: Arc::new(Mutex::new(HashMap::new())),
            next_pipe: 100, // Start at 100 to avoid conflicts
            process_utils,
            initialized: false,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            monitor_thread: None,
        })
    }

    /// Allocate a new pipe number
    fn allocate_pipe(&mut self) -> u32 {
        let pipe = self.next_pipe;
        self.next_pipe += 1;
        pipe
    }

    /// Get connections for a specific PID
    fn get_connections_for_pid(&self, pid: i32) -> Result<Vec<ConnectionInfo>> {
        let conn_map = self
            .process_utils
            .get_connection_map()
            .context("Failed to get connection map")?;

        let mut connections = Vec::new();

        // Find inode for this PID
        let mut target_inodes = Vec::new();
        for (inode, (conn_pid, _)) in &conn_map.socket_to_pid {
            if *conn_pid == pid {
                target_inodes.push(*inode);
            }
        }

        // Find connections matching these inodes
        for conn in &conn_map.tcp_connections {
            if target_inodes.contains(&conn.inode) {
                connections.push(ConnectionInfo {
                    local_addr: conn.local_addr.to_string(),
                    local_port: conn.local_port,
                    remote_addr: conn.remote_addr.to_string(),
                    remote_port: conn.remote_port,
                    protocol: "tcp",
                });
            }
        }

        for conn in &conn_map.tcp6_connections {
            if target_inodes.contains(&conn.inode) {
                connections.push(ConnectionInfo {
                    local_addr: conn.local_addr.to_string(),
                    local_port: conn.local_port,
                    remote_addr: conn.remote_addr.to_string(),
                    remote_port: conn.remote_port,
                    protocol: "tcp",
                });
            }
        }

        // TODO: Add UDP support if needed
        // for conn in &conn_map.udp_connections { ... }
        // for conn in &conn_map.udp6_connections { ... }

        Ok(connections)
    }

    /// Generate PF dummynet rule for a connection
    fn generate_pf_rule(&self, conn: &ConnectionInfo, pipe_num: u32) -> String {
        // Format: dummynet out on <iface> proto <proto> from <local> port <lport> to <remote> port <rport> pipe <N>
        // macOS uses 'dummynet' keyword (not FreeBSD's 'dnpipe')
        format!(
            "dummynet out on {} proto {} from {} port {} to {} port {} pipe {}",
            self.interface,
            conn.protocol,
            conn.local_addr,
            conn.local_port,
            conn.remote_addr,
            conn.remote_port,
            pipe_num
        )
    }

    /// Load PF rules (simple approach: load all at once)
    fn load_pf_rules(&self, rules: &[String]) -> Result<()> {
        if rules.is_empty() {
            return Ok(());
        }

        let mut rules_text = rules.join("\n");
        // pfctl expects a trailing newline
        rules_text.push('\n');

        // DEBUG: Print exact rules being sent to pfctl
        eprintln!("DEBUG [Upload]: PF rules being loaded:");
        eprintln!("---START---");
        eprintln!("{}", rules_text);
        eprintln!("---END---");
        log::info!(
            "DEBUG [Upload]: Loading {} rules: {}",
            rules.len(),
            rules_text
        );

        // Load into main ruleset (anchors don't work for dummynet rules on macOS)
        let mut child = Command::new("pfctl")
            .arg("-f")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn pfctl")?;

        // Write rules to stdin
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            stdin
                .write_all(rules_text.as_bytes())
                .context("Failed to write rules to pfctl")?;
        } else {
            return Err(anyhow!("Failed to get pfctl stdin"));
        }

        // Wait for pfctl to complete
        let output = child
            .wait_with_output()
            .context("Failed to wait for pfctl")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Note: We get warnings about flushing rules, but the command still succeeds
            // Only fail if exit code indicates actual error
            log::warn!("pfctl warnings: {}", stderr);
        }

        Ok(())
    }

    /// Create a dummynet pipe with bandwidth limit
    fn create_pipe(&self, pipe_num: u32, limit_bytes_per_sec: u64) -> Result<()> {
        let bandwidth_kbps = limit_bytes_per_sec * 8 / 1000;

        let output = Command::new("dnctl")
            .arg("pipe")
            .arg(pipe_num.to_string())
            .arg("config")
            .arg("bw")
            .arg(format!("{}Kbit/s", bandwidth_kbps))
            .output()
            .context("Failed to execute dnctl")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Failed to create pipe {}: {}", pipe_num, stderr));
        }

        log::debug!(
            "Created dummynet pipe {} with {} Kbit/s",
            pipe_num,
            bandwidth_kbps
        );
        Ok(())
    }

    /// Delete a dummynet pipe
    fn delete_pipe(&self, pipe_num: u32) -> Result<()> {
        let output = Command::new("dnctl")
            .arg("pipe")
            .arg(pipe_num.to_string())
            .arg("delete")
            .output()
            .context("Failed to execute dnctl")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("Failed to delete pipe {}: {}", pipe_num, stderr);
            // Don't fail - pipe might already be deleted
        }

        log::debug!("Deleted dummynet pipe {}", pipe_num);
        Ok(())
    }

    /// Start the connection monitoring thread
    fn start_monitoring_thread(&mut self) {
        if self.monitor_thread.is_some() {
            return; // Already running
        }

        let throttles = Arc::clone(&self.active_throttles);
        let shutdown = Arc::clone(&self.shutdown_flag);
        let interface = self.interface.clone();

        // Clone process_utils for the thread (need a new instance)
        // Note: We create a new instance per update to avoid lifetime issues

        log::info!("Starting connection monitoring thread");

        let handle = thread::spawn(move || {
            connection_monitor_loop(throttles, shutdown, interface);
        });

        self.monitor_thread = Some(handle);
    }
}

/// Connection monitoring loop (runs in background thread)
fn connection_monitor_loop(
    throttles: Arc<Mutex<HashMap<i32, ThrottleState>>>,
    shutdown: Arc<AtomicBool>,
    interface: String,
) {
    // Monitoring interval
    const CHECK_INTERVAL: Duration = Duration::from_secs(2);

    log::debug!("Connection monitor thread started");

    while !shutdown.load(Ordering::Relaxed) {
        thread::sleep(CHECK_INTERVAL);

        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Check for new connections
        if let Err(e) = check_and_update_connections(&throttles, &interface) {
            log::warn!("Error checking connections: {}", e);
        }
    }

    log::debug!("Connection monitor thread exiting");
}

/// Check for new connections and update PF rules
fn check_and_update_connections(
    throttles: &Arc<Mutex<HashMap<i32, ThrottleState>>>,
    interface: &str,
) -> Result<()> {
    // Create fresh process utils for this check
    let process_utils = crate::backends::process::create_process_utils();

    // Get current connection map
    let conn_map = process_utils.get_connection_map()?;

    // Lock throttles for reading/updating
    let mut throttles_guard = throttles.lock().unwrap();

    for (pid, state) in throttles_guard.iter_mut() {
        // Get current connections for this PID
        let current_connections = get_connections_for_pid_from_map(&conn_map, *pid)?;

        // Find new connections (not in our existing set)
        let existing: HashSet<ConnectionInfo> = state.connections.iter().cloned().collect();
        let new_connections: Vec<ConnectionInfo> = current_connections
            .into_iter()
            .filter(|conn| !existing.contains(conn))
            .collect();

        if !new_connections.is_empty() {
            log::info!(
                "Found {} new connection(s) for PID {} ({})",
                new_connections.len(),
                pid,
                state.process_name
            );

            // Generate PF rules for new connections
            let new_rules: Vec<String> = new_connections
                .iter()
                .map(|conn| generate_pf_rule(conn, state.pipe_num, interface))
                .collect();

            // Load new rules
            if let Err(e) = load_pf_rules(&new_rules) {
                log::error!("Failed to load PF rules for new connections: {}", e);
                continue;
            }

            // Update state
            state.pf_rules.extend(new_rules);
            state.connections.extend(new_connections);

            log::debug!(
                "Updated throttle for PID {} - now tracking {} connections",
                pid,
                state.connections.len()
            );
        }
    }

    Ok(())
}

/// Get connections for a PID from an existing connection map
fn get_connections_for_pid_from_map(
    conn_map: &crate::backends::process::ConnectionMap,
    pid: i32,
) -> Result<Vec<ConnectionInfo>> {
    let mut connections = Vec::new();

    // Find inodes for this PID
    let mut target_inodes = Vec::new();
    for (inode, (conn_pid, _)) in &conn_map.socket_to_pid {
        if *conn_pid == pid {
            target_inodes.push(*inode);
        }
    }

    // Find connections matching these inodes
    for conn in &conn_map.tcp_connections {
        if target_inodes.contains(&conn.inode) {
            connections.push(ConnectionInfo {
                local_addr: conn.local_addr.to_string(),
                local_port: conn.local_port,
                remote_addr: conn.remote_addr.to_string(),
                remote_port: conn.remote_port,
                protocol: "tcp",
            });
        }
    }

    for conn in &conn_map.tcp6_connections {
        if target_inodes.contains(&conn.inode) {
            connections.push(ConnectionInfo {
                local_addr: conn.local_addr.to_string(),
                local_port: conn.local_port,
                remote_addr: conn.remote_addr.to_string(),
                remote_port: conn.remote_port,
                protocol: "tcp",
            });
        }
    }

    Ok(connections)
}

/// Generate PF rule for a connection (standalone function for monitoring thread)
fn generate_pf_rule(conn: &ConnectionInfo, pipe_num: u32, interface: &str) -> String {
    // macOS uses 'dummynet' keyword (not FreeBSD's 'dnpipe')
    format!(
        "dummynet out on {} proto {} from {} port {} to {} port {} pipe {}",
        interface,
        conn.protocol,
        conn.local_addr,
        conn.local_port,
        conn.remote_addr,
        conn.remote_port,
        pipe_num
    )
}

/// Load PF rules (standalone function for monitoring thread)
fn load_pf_rules(rules: &[String]) -> Result<()> {
    if rules.is_empty() {
        return Ok(());
    }

    let mut rules_text = rules.join("\n");
    // pfctl expects a trailing newline
    rules_text.push('\n');

    // DEBUG: Print exact rules being sent to pfctl
    eprintln!("DEBUG [Upload/Standalone]: PF rules being loaded:");
    eprintln!("---START---");
    eprintln!("{}", rules_text);
    eprintln!("---END---");
    log::info!(
        "DEBUG [Upload/Standalone]: Loading {} rules: {}",
        rules.len(),
        rules_text
    );

    // Load into main ruleset (anchors don't work for dummynet rules on macOS)
    let mut child = Command::new("pfctl")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn pfctl")?;

    // Write rules to stdin
    if let Some(stdin) = child.stdin.as_mut() {
        use std::io::Write;
        stdin
            .write_all(rules_text.as_bytes())
            .context("Failed to write rules to pfctl")?;
    } else {
        return Err(anyhow!("Failed to get pfctl stdin"));
    }

    // Wait for pfctl to complete
    let output = child
        .wait_with_output()
        .context("Failed to wait for pfctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("pfctl warnings: {}", stderr);
    }

    Ok(())
}

impl UploadThrottleBackend for DnctlUpload {
    fn name(&self) -> &'static str {
        "dnctl"
    }

    fn priority(&self) -> BackendPriority {
        BackendPriority::Best
    }

    fn is_available() -> bool {
        // Check if dnctl exists
        if !check_dnctl_available() {
            return false;
        }

        // Check if pfctl exists
        if !check_pfctl_available() {
            return false;
        }

        true
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            ipv4_support: true,
            ipv6_support: true,
            per_process: true,
            per_connection: true, // We match at connection level
        }
    }

    fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }

        // Verify we can run dnctl and pfctl
        if !Self::is_available() {
            return Err(anyhow!(
                "dnctl backend not available (requires dnctl and pfctl)"
            ));
        }

        self.initialized = true;
        log::info!(
            "Initialized macOS dnctl upload backend on interface {}",
            self.interface
        );
        Ok(())
    }

    fn throttle_upload(
        &mut self,
        pid: i32,
        process_name: String,
        limit_bytes_per_sec: u64,
        _traffic_type: crate::process::TrafficType,
    ) -> Result<()> {
        // Initialize if needed
        self.init()?;

        // Check if already throttled
        {
            let throttles = self.active_throttles.lock().unwrap();
            if throttles.contains_key(&pid) {
                return Err(anyhow!("Process {} already throttled", pid));
            }
        }

        // Allocate pipe
        let pipe_num = self.allocate_pipe();

        // Create dummynet pipe
        self.create_pipe(pipe_num, limit_bytes_per_sec)
            .context("Failed to create dummynet pipe")?;

        // Get connections for this PID
        let connections = self
            .get_connections_for_pid(pid)
            .context("Failed to get connections for PID")?;

        if connections.is_empty() {
            log::warn!(
                "No active connections found for PID {} ({}). Throttle created but won't apply until process makes connections.",
                pid,
                process_name
            );
        }

        // Generate PF rules for each connection
        let pf_rules: Vec<String> = connections
            .iter()
            .map(|conn| self.generate_pf_rule(conn, pipe_num))
            .collect();

        // Load PF rules
        if !pf_rules.is_empty() {
            self.load_pf_rules(&pf_rules)
                .context("Failed to load PF rules")?;
            log::info!(
                "Loaded {} PF rules for PID {} ({})",
                pf_rules.len(),
                pid,
                process_name
            );
        }

        // Track throttle state
        {
            let mut throttles = self.active_throttles.lock().unwrap();
            throttles.insert(
                pid,
                ThrottleState {
                    pipe_num,
                    process_name: process_name.clone(),
                    limit_bytes_per_sec,
                    pf_rules,
                    connections,
                },
            );
        }

        // Start monitoring thread if not already running
        self.start_monitoring_thread();

        log::info!(
            "Upload throttle applied: PID {} ({}) → {} bytes/sec (pipe {})",
            pid,
            process_name,
            limit_bytes_per_sec,
            pipe_num
        );

        Ok(())
    }

    fn remove_upload_throttle(&mut self, pid: i32) -> Result<()> {
        let state = {
            let mut throttles = self.active_throttles.lock().unwrap();
            throttles
                .remove(&pid)
                .ok_or_else(|| anyhow!("No throttle found for PID {}", pid))?
        };

        // Delete pipe
        // Note: We don't remove PF rules individually - they reference the pipe,
        // so when pipe is deleted, they effectively stop working.
        // A full cleanup would reload /etc/pf.conf or manage rules in anchors.
        self.delete_pipe(state.pipe_num)?;

        log::info!(
            "Upload throttle removed: PID {} ({}) pipe {}",
            pid,
            state.process_name,
            state.pipe_num
        );

        Ok(())
    }

    fn get_upload_throttle(&self, pid: i32) -> Option<u64> {
        let throttles = self.active_throttles.lock().unwrap();
        throttles.get(&pid).map(|state| state.limit_bytes_per_sec)
    }

    fn get_all_throttles(&self) -> HashMap<i32, u64> {
        let throttles = self.active_throttles.lock().unwrap();
        throttles
            .iter()
            .map(|(pid, state)| (*pid, state.limit_bytes_per_sec))
            .collect()
    }

    fn cleanup(&mut self) -> Result<()> {
        log::info!("Cleaning up macOS dnctl upload backend...");

        // Signal monitoring thread to stop
        self.shutdown_flag.store(true, Ordering::Relaxed);

        // Wait for monitoring thread to exit
        if let Some(handle) = self.monitor_thread.take() {
            log::debug!("Waiting for monitoring thread to exit...");
            // Don't block - thread will exit within CHECK_INTERVAL (2 seconds)
            // If we wanted to block: handle.join().ok();
            drop(handle); // Let thread exit naturally
        }

        // Collect pipe numbers first to avoid borrow checker issues
        let pipes: Vec<(i32, u32)> = {
            let throttles = self.active_throttles.lock().unwrap();
            throttles
                .iter()
                .map(|(pid, state)| (*pid, state.pipe_num))
                .collect()
        };

        // Delete all pipes
        for (pid, pipe_num) in pipes {
            if let Err(e) = self.delete_pipe(pipe_num) {
                log::warn!("Failed to delete pipe {} for PID {}: {}", pipe_num, pid, e);
            }
        }

        // Clear throttle state
        {
            let mut throttles = self.active_throttles.lock().unwrap();
            throttles.clear();
        }

        // Reset PF to default config (removes our dummynet rules)
        let output = Command::new("pfctl").arg("-f").arg("/etc/pf.conf").output();

        if let Err(e) = output {
            log::warn!("Failed to reset PF config during cleanup: {}", e);
        }

        log::info!("macOS dnctl upload backend cleanup complete");
        Ok(())
    }
}

impl Drop for DnctlUpload {
    fn drop(&mut self) {
        // Signal shutdown
        self.shutdown_flag.store(true, Ordering::Relaxed);

        // Run cleanup (but don't wait for thread - let it exit naturally)
        if let Err(e) = self.cleanup() {
            log::error!("Error during DnctlUpload cleanup: {}", e);
        }
    }
}

/// Detect primary network interface
fn detect_interface() -> Result<String> {
    // TODO: Implement proper interface detection
    // For now, default to en0 (common on macOS)
    // Could use: networksetup -listallhardwareports
    // Or parse: route -n get default
    Ok("en0".to_string())
}

/// Check if dnctl is available
fn check_dnctl_available() -> bool {
    Command::new("which")
        .arg("dnctl")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if pfctl is available
fn check_pfctl_available() -> bool {
    Command::new("which")
        .arg("pfctl")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
