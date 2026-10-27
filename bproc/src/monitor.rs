use crate::backends::process::{ConnectionMap, ProcessUtils};
use crate::process::{InterfaceInfo, InterfaceMap, ProcessInfo, ProcessMap};
use anyhow::{Context, Result};

#[cfg(feature = "monitor-pnet")]
use pnet::datalink::{self, Channel, NetworkInterface};
#[cfg(feature = "monitor-pnet")]
use pnet::packet::Packet;
#[cfg(feature = "monitor-pnet")]
use pnet::packet::ethernet::{EtherTypes, EthernetPacket};
#[cfg(feature = "monitor-pnet")]
use pnet::packet::ip::IpNextHeaderProtocols;
#[cfg(feature = "monitor-pnet")]
use pnet::packet::ipv4::Ipv4Packet;
#[cfg(feature = "monitor-pnet")]
use pnet::packet::ipv6::Ipv6Packet;
#[cfg(feature = "monitor-pnet")]
use pnet::packet::tcp::TcpPacket;
#[cfg(feature = "monitor-pnet")]
use pnet::packet::udp::UdpPacket;

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[cfg(test)]
#[path = "monitor_delivery_tests.rs"]
mod monitor_delivery_tests;

/// How long to keep terminated processes visible (in seconds)
const TERMINATED_DISPLAY_DURATION: Duration = Duration::from_secs(5);

/// Commands sent from UI thread to monitoring thread
pub enum MonitorCommand {
    /// Switch to a different socket mapper backend
    SwitchSocketMapper {
        backend_name: String,
        /// Channel to send back success or error (hot-swap, no thread restart)
        response_tx: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// Signal to shutdown the monitoring thread
    Shutdown,
}

/// Update data sent from monitoring thread to UI thread
#[derive(Debug, Clone)]
pub struct MonitorUpdateData {
    /// Acquisition time is carried across bounded delivery for graph history.
    pub acquired_at: u64,
    pub process_map: ProcessMap,
    pub interface_map: InterfaceMap,
    pub socket_mapper_name: String,
    pub socket_mapper_capabilities: crate::backends::BackendCapabilities,
}

/// Update messages sent from monitoring thread to UI thread
pub type MonitorUpdate = MonitorUpdateData;

/// Connection identifier for tracking packets
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct ConnectionKey {
    local_addr: IpAddr,
    local_port: u16,
    remote_addr: IpAddr,
    remote_port: u16,
    protocol: Protocol,
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
enum Protocol {
    Tcp,
    Udp,
}

/// Tracks network activity per process using packet capture
pub struct NetworkMonitor {
    // Shared state between packet capture thread and update thread
    bandwidth_tracker: Arc<Mutex<BandwidthTracker>>,
    // Platform-specific process utilities
    process_utils: Box<dyn ProcessUtils>,
    // Monitoring backend name (e.g., "pnet", "windows-poll")
    // Socket mapper backend information
    socket_mapper_name: String,
    socket_mapper_capabilities: crate::backends::BackendCapabilities,
    // Shutdown signal for packet capture threads
    shutdown_flag: Arc<AtomicBool>,
    // Handles to packet capture threads (one per interface) - no underscore, we'll join them!
    capture_handles: Vec<thread::JoinHandle<()>>,
    last_update: Instant,
    // Cached PROCESSED connection data updated asynchronously
    cached_processed_data: Arc<Mutex<ProcessedConnectionData>>,
    // Cached interface list (doesn't change at runtime)
    cached_interfaces: Vec<NetworkInterface>,
    // Cached process existence checks (updated every update cycle)
    cached_process_exists: HashMap<i32, bool>,
    last_process_check: Instant,
}

/// Pre-processed connection data ready for use by the UI thread
/// This is built by the background async task to avoid blocking the UI
#[derive(Default, Clone)]
struct ProcessedConnectionData {
    socket_map: HashMap<u64, (i32, String)>,
    connection_map: HashMap<ConnectionKey, i32>,
    pids_with_names: Vec<(i32, String)>,
    raw_connection_map: ConnectionMap, // Kept for populate_connections
}

struct BandwidthTracker {
    // Connection -> PID mapping
    connection_map: HashMap<ConnectionKey, i32>,
    // Socket inode -> (PID, process name) mapping
    socket_map: HashMap<u64, (i32, String)>,
    // PID -> bandwidth tracking
    process_bandwidth: HashMap<i32, ProcessBandwidth>,
    // PID -> termination time (for showing terminated processes temporarily)
    terminated_processes: HashMap<i32, Instant>,
    // Interface statistics: interface_name -> InterfaceBandwidth
    interface_bandwidth: HashMap<String, InterfaceBandwidth>,
    // Per-process, per-interface stats: (pid, interface) -> bandwidth
    process_interface_bandwidth: HashMap<(i32, String), ProcessInterfaceBandwidth>,
    // Packet capture statistics for debugging
    packets_captured: u64,
    packets_matched: u64,
    packets_unmatched: u64,
}

#[derive(Clone)]
pub struct ProcessBandwidth {
    name: String,
    rx_bytes: u64,
    tx_bytes: u64,
    last_rx_bytes: u64,
    last_tx_bytes: u64,
    // NEW: Traffic categorization
    internet_rx_bytes: u64,
    internet_tx_bytes: u64,
    local_rx_bytes: u64,
    local_tx_bytes: u64,
    last_internet_rx_bytes: u64,
    last_internet_tx_bytes: u64,
    last_local_rx_bytes: u64,
    last_local_tx_bytes: u64,
}

#[derive(Clone)]
struct InterfaceBandwidth {
    rx_bytes: u64,
    tx_bytes: u64,
    last_rx_bytes: u64,
    last_tx_bytes: u64,
}

#[derive(Clone)]
struct ProcessInterfaceBandwidth {
    rx_bytes: u64,
    tx_bytes: u64,
    last_rx_bytes: u64,
    last_tx_bytes: u64,
    // NEW: Traffic categorization
    internet_rx_bytes: u64,
    internet_tx_bytes: u64,
    local_rx_bytes: u64,
    local_tx_bytes: u64,
    last_internet_rx_bytes: u64,
    last_internet_tx_bytes: u64,
    last_local_rx_bytes: u64,
    last_local_tx_bytes: u64,
}

impl NetworkMonitor {
    pub fn with_socket_mapper(socket_mapper_preference: Option<&str>) -> Result<Self> {
        let bandwidth_tracker = Arc::new(Mutex::new(BandwidthTracker {
            connection_map: HashMap::new(),
            socket_map: HashMap::new(),
            process_bandwidth: HashMap::new(),
            terminated_processes: HashMap::new(),
            interface_bandwidth: HashMap::new(),
            process_interface_bandwidth: HashMap::new(),
            packets_captured: 0,
            packets_matched: 0,
            packets_unmatched: 0,
        }));

        // Create platform-specific process utilities
        let process_utils = crate::backends::process::create_process_utils_with_socket_mapper(
            socket_mapper_preference,
        );

        // Get socket mapper info before moving process_utils
        let (socket_mapper_name, socket_mapper_capabilities) = {
            #[cfg(target_os = "linux")]
            {
                use crate::backends::process::LinuxProcessUtils;
                // We need to get the info from the actual implementation
                // Create a temporary one to get the info
                let temp_utils = crate::backends::process::LinuxProcessUtils::with_socket_mapper(
                    socket_mapper_preference,
                );
                (
                    temp_utils.socket_mapper_name().to_string(),
                    temp_utils.socket_mapper_capabilities(),
                )
            }
            #[cfg(target_os = "macos")]
            {
                let temp_utils = crate::backends::process::MacOSProcessUtils::with_socket_mapper(
                    socket_mapper_preference,
                );
                (
                    temp_utils.socket_mapper_name().to_string(),
                    temp_utils.socket_mapper_capabilities(),
                )
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                (
                    "unknown".to_string(),
                    crate::backends::BackendCapabilities::default(),
                )
            }
        };

        // Create shutdown flag for graceful thread termination
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        // Initialize cached processed connection data
        let cached_processed_data = Arc::new(Mutex::new(ProcessedConnectionData::default()));

        // Cache interface list once at startup
        let cached_interfaces = Self::find_all_interfaces();
        log::info!("Cached {} network interfaces", cached_interfaces.len());

        // Create monitor instance first (without starting capture thread yet)
        let mut monitor = Self {
            bandwidth_tracker,
            process_utils,
            socket_mapper_name,
            socket_mapper_capabilities,
            shutdown_flag: Arc::clone(&shutdown_flag),
            capture_handles: Vec::new(),
            last_update: Instant::now(),
            cached_processed_data: Arc::clone(&cached_processed_data),
            cached_interfaces: cached_interfaces.clone(),
            cached_process_exists: HashMap::new(),
            last_process_check: Instant::now(),
        };

        // Spawn background async task to update connection map
        // This prevents blocking the UI thread during connection map updates
        let cached_data_clone = Arc::clone(&cached_processed_data);
        let shutdown_clone = Arc::clone(&shutdown_flag);
        let process_utils = crate::backends::process::create_process_utils_with_socket_mapper(
            socket_mapper_preference,
        );

        // Initialize connection map BEFORE starting packet capture to avoid race condition
        log::info!("Initializing connection map before packet capture...");
        if let Ok(initial_conn_map) = monitor.process_utils.get_connection_map() {
            log::info!(
                "Initial connection map fetched with {} sockets",
                initial_conn_map.socket_to_pid.len()
            );

            // Process the connection map in the main thread for initialization
            let processed = Self::process_connection_map(initial_conn_map);

            if let Ok(mut cached) = cached_data_clone.lock() {
                *cached = processed;
            }
        } else {
            log::warn!(
                "Failed to get initial connection map - processes may not appear immediately"
            );
        }

        // Update socket map from initial processed data
        if let Err(e) = monitor.update_socket_map() {
            log::warn!("Failed to initialize socket map: {}", e);
        }

        // Spawn background async task to fetch AND process connection maps
        // This keeps ALL heavy computation out of the UI thread
        tokio::spawn(async move {
            while !shutdown_clone.load(Ordering::Relaxed) {
                // Fetch connection map (blocking I/O)
                if let Ok(conn_map) = process_utils.get_connection_map() {
                    // Process it (heavy computation - done in background!)
                    let processed = NetworkMonitor::process_connection_map(conn_map);

                    // Store pre-processed results (fast!)
                    if let Ok(mut cached) = cached_data_clone.lock() {
                        *cached = processed;
                    }
                }

                // Sleep for 1 second before next update
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });

        // Start packet capture threads for all interfaces
        let interfaces = Self::find_all_interfaces();
        log::info!("Starting packet capture on {} interfaces", interfaces.len());

        for interface in interfaces {
            let tracker_clone = Arc::clone(&monitor.bandwidth_tracker);
            let shutdown_clone = Arc::clone(&shutdown_flag);
            let iface_name = interface.name.clone();

            let capture_handle = thread::spawn(move || {
                if let Err(e) =
                    Self::capture_packets_on_interface(interface, tracker_clone, shutdown_clone)
                {
                    log::error!("Packet capture error on {}: {}", iface_name, e);
                }
            });

            monitor.capture_handles.push(capture_handle);
        }

        Ok(monitor)
    }

    pub fn update(&mut self) -> Result<(ProcessMap, InterfaceMap)> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();

        // CRITICAL OPTIMIZATION: Refresh process_utils caches ONCE per update
        // This refreshes the System instance on Windows, making all subsequent
        // process_exists() calls use the same cached data instead of creating
        // new System instances (which is extremely expensive: 10-20ms each)
        self.process_utils.refresh_caches();

        // Update socket/connection maps from cached data
        // The background async task updates cached_connection_map every second
        // We rebuild our internal maps from the cache to avoid blocking
        self.update_socket_map()?;

        // CRITICAL: Update process existence cache OUTSIDE the lock
        // Only check every 2 seconds to avoid excessive Windows API calls
        if now.duration_since(self.last_process_check).as_secs() >= 2 {
            // Get list of PIDs to check
            let pids_to_check: Vec<i32> = {
                let tracker = self.bandwidth_tracker.lock().unwrap();
                tracker.process_bandwidth.keys().copied().collect()
            };

            // Check process existence WITHOUT holding the lock
            let mut new_cache = HashMap::new();
            for pid in pids_to_check {
                new_cache.insert(pid, self.process_utils.process_exists(pid));
            }

            self.cached_process_exists = new_cache;
            self.last_process_check = now;
        }

        // PHASE 1: Collect data from tracker (hold lock briefly)
        let (
            process_bandwidth_snapshot,
            interface_bandwidth_snapshot,
            process_interface_snapshot,
            terminated_snapshot,
        ) = {
            let tracker = self.bandwidth_tracker.lock().unwrap();
            (
                tracker.process_bandwidth.clone(),
                tracker.interface_bandwidth.clone(),
                tracker.process_interface_bandwidth.clone(),
                tracker.terminated_processes.clone(),
            )
        };
        // Lock is released here!

        // PHASE 2: Process data WITHOUT holding the lock
        let mut process_map = ProcessMap::new();
        let mut interface_map = InterfaceMap::new();
        let mut process_data = Vec::new();

        for (&pid, bandwidth) in &process_bandwidth_snapshot {
            let rx_diff = bandwidth.rx_bytes.saturating_sub(bandwidth.last_rx_bytes);
            let tx_diff = bandwidth.tx_bytes.saturating_sub(bandwidth.last_tx_bytes);

            let download_rate = if elapsed > 0.0 {
                (rx_diff as f64 / elapsed) as u64
            } else {
                0
            };

            let upload_rate = if elapsed > 0.0 {
                (tx_diff as f64 / elapsed) as u64
            } else {
                0
            };

            // NEW: Calculate categorized rates
            let internet_rx_diff = bandwidth
                .internet_rx_bytes
                .saturating_sub(bandwidth.last_internet_rx_bytes);
            let internet_tx_diff = bandwidth
                .internet_tx_bytes
                .saturating_sub(bandwidth.last_internet_tx_bytes);
            let local_rx_diff = bandwidth
                .local_rx_bytes
                .saturating_sub(bandwidth.last_local_rx_bytes);
            let local_tx_diff = bandwidth
                .local_tx_bytes
                .saturating_sub(bandwidth.last_local_tx_bytes);

            let internet_download_rate = if elapsed > 0.0 {
                (internet_rx_diff as f64 / elapsed) as u64
            } else {
                0
            };
            let internet_upload_rate = if elapsed > 0.0 {
                (internet_tx_diff as f64 / elapsed) as u64
            } else {
                0
            };
            let local_download_rate = if elapsed > 0.0 {
                (local_rx_diff as f64 / elapsed) as u64
            } else {
                0
            };
            let local_upload_rate = if elapsed > 0.0 {
                (local_tx_diff as f64 / elapsed) as u64
            } else {
                0
            };

            let process_exists = self
                .cached_process_exists
                .get(&pid)
                .copied()
                .unwrap_or(true);
            let term_time = terminated_snapshot.get(&pid).copied();

            process_data.push((
                pid,
                bandwidth.name.clone(),
                bandwidth.rx_bytes,
                bandwidth.tx_bytes,
                download_rate,
                upload_rate,
                bandwidth.internet_rx_bytes,
                bandwidth.internet_tx_bytes,
                internet_download_rate,
                internet_upload_rate,
                bandwidth.local_rx_bytes,
                bandwidth.local_tx_bytes,
                local_download_rate,
                local_upload_rate,
                process_exists,
                term_time,
            ));
        }

        // Track PIDs to remove and newly terminated
        let mut pids_to_remove = Vec::new();
        let mut newly_terminated = Vec::new();

        // Second pass: build process map based on collected data
        for (
            pid,
            name,
            rx_bytes,
            tx_bytes,
            download_rate,
            upload_rate,
            internet_rx_bytes,
            internet_tx_bytes,
            internet_download_rate,
            internet_upload_rate,
            local_rx_bytes,
            local_tx_bytes,
            local_download_rate,
            local_upload_rate,
            process_exists,
            term_time,
        ) in process_data
        {
            if process_exists {
                // Process is alive - include it normally
                let mut proc_info = ProcessInfo::new(pid, name);
                proc_info.download_rate = download_rate;
                proc_info.upload_rate = upload_rate;
                proc_info.total_download = rx_bytes;
                proc_info.total_upload = tx_bytes;
                proc_info.is_terminated = false;

                // Populate categorized traffic fields
                proc_info.internet_download_rate = internet_download_rate;
                proc_info.internet_upload_rate = internet_upload_rate;
                proc_info.internet_total_download = internet_rx_bytes;
                proc_info.internet_total_upload = internet_tx_bytes;
                proc_info.local_download_rate = local_download_rate;
                proc_info.local_upload_rate = local_upload_rate;
                proc_info.local_total_download = local_rx_bytes;
                proc_info.local_total_upload = local_tx_bytes;

                // Populate per-interface stats for this process
                proc_info.interface_stats = process_interface_snapshot
                    .iter()
                    .filter(|((p, _), _)| *p == pid)
                    .map(|((_, iface), bw)| {
                        let rx_diff = bw.rx_bytes.saturating_sub(bw.last_rx_bytes);
                        let tx_diff = bw.tx_bytes.saturating_sub(bw.last_tx_bytes);

                        let iface_download_rate = if elapsed > 0.0 {
                            (rx_diff as f64 / elapsed) as u64
                        } else {
                            0
                        };

                        let iface_upload_rate = if elapsed > 0.0 {
                            (tx_diff as f64 / elapsed) as u64
                        } else {
                            0
                        };

                        // NEW: Calculate categorized rates
                        let internet_rx_diff = bw
                            .internet_rx_bytes
                            .saturating_sub(bw.last_internet_rx_bytes);
                        let internet_tx_diff = bw
                            .internet_tx_bytes
                            .saturating_sub(bw.last_internet_tx_bytes);
                        let local_rx_diff =
                            bw.local_rx_bytes.saturating_sub(bw.last_local_rx_bytes);
                        let local_tx_diff =
                            bw.local_tx_bytes.saturating_sub(bw.last_local_tx_bytes);

                        let internet_download_rate = if elapsed > 0.0 {
                            (internet_rx_diff as f64 / elapsed) as u64
                        } else {
                            0
                        };
                        let internet_upload_rate = if elapsed > 0.0 {
                            (internet_tx_diff as f64 / elapsed) as u64
                        } else {
                            0
                        };
                        let local_download_rate = if elapsed > 0.0 {
                            (local_rx_diff as f64 / elapsed) as u64
                        } else {
                            0
                        };
                        let local_upload_rate = if elapsed > 0.0 {
                            (local_tx_diff as f64 / elapsed) as u64
                        } else {
                            0
                        };

                        (
                            iface.clone(),
                            crate::process::InterfaceStats {
                                download_rate: iface_download_rate,
                                upload_rate: iface_upload_rate,
                                total_download: bw.rx_bytes,
                                total_upload: bw.tx_bytes,
                                internet_download_rate,
                                internet_upload_rate,
                                local_download_rate,
                                local_upload_rate,
                            },
                        )
                    })
                    .collect();

                process_map.insert(pid, proc_info);
            } else {
                // Process terminated
                if let Some(term_time) = term_time {
                    let since_termination = now.duration_since(term_time);

                    if since_termination < TERMINATED_DISPLAY_DURATION {
                        // Still within display window - show with skull icon
                        let mut proc_info = ProcessInfo::new(pid, name);
                        proc_info.download_rate = 0;
                        proc_info.upload_rate = 0;
                        proc_info.total_download = rx_bytes;
                        proc_info.total_upload = tx_bytes;
                        proc_info.is_terminated = true;

                        // Populate categorized traffic totals (rates are 0 for terminated)
                        proc_info.internet_download_rate = 0;
                        proc_info.internet_upload_rate = 0;
                        proc_info.internet_total_download = internet_rx_bytes;
                        proc_info.internet_total_upload = internet_tx_bytes;
                        proc_info.local_download_rate = 0;
                        proc_info.local_upload_rate = 0;
                        proc_info.local_total_download = local_rx_bytes;
                        proc_info.local_total_upload = local_tx_bytes;

                        process_map.insert(pid, proc_info);
                    } else {
                        // Too old - mark for removal
                        pids_to_remove.push(pid);
                    }
                } else {
                    // Newly terminated - show it and record termination time
                    newly_terminated.push(pid);

                    let mut proc_info = ProcessInfo::new(pid, name);
                    proc_info.download_rate = 0;
                    proc_info.upload_rate = 0;
                    proc_info.total_download = rx_bytes;
                    proc_info.total_upload = tx_bytes;
                    proc_info.is_terminated = true;

                    // Populate categorized traffic totals (rates are 0 for terminated)
                    proc_info.internet_download_rate = 0;
                    proc_info.internet_upload_rate = 0;
                    proc_info.internet_total_download = internet_rx_bytes;
                    proc_info.internet_total_upload = internet_tx_bytes;
                    proc_info.local_download_rate = 0;
                    proc_info.local_upload_rate = 0;
                    proc_info.local_total_download = local_rx_bytes;
                    proc_info.local_total_upload = local_tx_bytes;

                    process_map.insert(pid, proc_info);
                }
            }
        }

        // Build interface map with aggregated statistics (use cached interfaces!)
        for interface in &self.cached_interfaces {
            let iface_name = interface.name.clone();

            // Get interface bandwidth stats
            let iface_bandwidth = interface_bandwidth_snapshot.get(&iface_name);

            let (total_download_rate, total_upload_rate) = if let Some(bw) = iface_bandwidth {
                let rx_diff = bw.rx_bytes.saturating_sub(bw.last_rx_bytes);
                let tx_diff = bw.tx_bytes.saturating_sub(bw.last_tx_bytes);

                let download_rate = if elapsed > 0.0 {
                    (rx_diff as f64 / elapsed) as u64
                } else {
                    0
                };

                let upload_rate = if elapsed > 0.0 {
                    (tx_diff as f64 / elapsed) as u64
                } else {
                    0
                };

                (download_rate, upload_rate)
            } else {
                (0, 0)
            };

            // Count processes using this interface
            let process_count = process_interface_snapshot
                .keys()
                .filter(|(_, iface)| iface == &iface_name)
                .map(|(pid, _)| pid)
                .collect::<HashSet<_>>()
                .len();

            // Extract MAC address
            let mac_address = interface.mac.map(|mac| {
                format!(
                    "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    mac.0, mac.1, mac.2, mac.3, mac.4, mac.5
                )
            });

            // Extract IP addresses
            let ip_addresses: Vec<IpAddr> = interface.ips.iter().map(|ip| ip.ip()).collect();

            interface_map.insert(
                iface_name.clone(),
                InterfaceInfo {
                    name: iface_name,
                    mac_address,
                    ip_addresses,
                    is_up: interface.is_up(),
                    is_loopback: interface.is_loopback(),
                    total_download_rate,
                    total_upload_rate,
                    process_count,
                },
            );
        }

        // PHASE 3: Update tracker with results (hold lock briefly)
        {
            let mut tracker = self.bandwidth_tracker.lock().unwrap();

            // Update last values for process bandwidth
            for (&_pid, bandwidth) in &mut tracker.process_bandwidth {
                bandwidth.last_rx_bytes = bandwidth.rx_bytes;
                bandwidth.last_tx_bytes = bandwidth.tx_bytes;
                bandwidth.last_internet_rx_bytes = bandwidth.internet_rx_bytes;
                bandwidth.last_internet_tx_bytes = bandwidth.internet_tx_bytes;
                bandwidth.last_local_rx_bytes = bandwidth.local_rx_bytes;
                bandwidth.last_local_tx_bytes = bandwidth.local_tx_bytes;
            }

            // Update last values for interface bandwidth
            for bw in tracker.interface_bandwidth.values_mut() {
                bw.last_rx_bytes = bw.rx_bytes;
                bw.last_tx_bytes = bw.tx_bytes;
            }

            // Update last values for process-interface bandwidth
            for bw in tracker.process_interface_bandwidth.values_mut() {
                bw.last_rx_bytes = bw.rx_bytes;
                bw.last_tx_bytes = bw.tx_bytes;
                bw.last_internet_rx_bytes = bw.internet_rx_bytes;
                bw.last_internet_tx_bytes = bw.internet_tx_bytes;
                bw.last_local_rx_bytes = bw.local_rx_bytes;
                bw.last_local_tx_bytes = bw.local_tx_bytes;
            }

            // Record newly terminated processes
            for pid in newly_terminated {
                tracker.terminated_processes.insert(pid, now);
            }

            // Clean up old terminated processes
            for pid in pids_to_remove {
                tracker.process_bandwidth.remove(&pid);
                tracker.terminated_processes.remove(&pid);
            }
        }
        // Lock is released here!

        self.last_update = now;

        // Populate connections for each process using cached data
        // This is fast because we're reading pre-processed data from the cache
        let processed = if let Ok(cached) = self.cached_processed_data.lock() {
            cached.clone()
        } else {
            ProcessedConnectionData::default()
        };

        for process in process_map.values_mut() {
            process.populate_connections(&processed.raw_connection_map, &processed.socket_map);
        }

        Ok((process_map, interface_map))
    }

    /// Run the monitoring loop in a background thread
    /// This is the truly async solution - the monitor runs independently and sends updates
    /// to the UI thread via a channel, allowing the UI to remain responsive
    pub fn run_monitoring_loop(
        mut self,
        mut cmd_rx: mpsc::UnboundedReceiver<MonitorCommand>,
        update_tx: mpsc::Sender<MonitorUpdate>,
    ) {
        log::info!("Starting monitoring background thread");
        let mut last_update = Instant::now();

        loop {
            // Check for commands (non-blocking)
            match cmd_rx.try_recv() {
                Ok(MonitorCommand::Shutdown) => {
                    log::info!("Monitoring thread received shutdown command");
                    break;
                }
                Ok(MonitorCommand::SwitchSocketMapper {
                    backend_name,
                    response_tx,
                }) => {
                    log::info!(
                        "Monitoring thread hot-swapping socket mapper to: {}",
                        backend_name
                    );

                    // Extract bandwidth data before replacing monitor
                    let (process_bandwidth, terminated_processes) = self.extract_bandwidth_data();
                    let process_count = process_bandwidth.len();
                    let terminated_count = terminated_processes.len();

                    // Create new monitor with different socket mapper
                    match NetworkMonitor::with_socket_mapper(Some(&backend_name)) {
                        Ok(mut new_monitor) => {
                            // Restore bandwidth data
                            new_monitor
                                .restore_bandwidth_data(process_bandwidth, terminated_processes);

                            log::info!(
                                "Socket mapper switched successfully (preserved {} processes, {} terminated) - hot-swapping monitor",
                                process_count,
                                terminated_count
                            );

                            // HOT-SWAP: Replace self with new monitor
                            // Old monitor will be dropped (cleanup threads), new one takes over
                            self = new_monitor;

                            // Send success response
                            let _ = response_tx.send(Ok(()));

                            // Continue monitoring with new backend!
                        }
                        Err(e) => {
                            log::error!("Failed to switch socket mapper: {}", e);
                            let _ = response_tx.send(Err(e));
                            // Continue with existing monitor
                        }
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    // No command, continue monitoring
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    log::warn!("Command channel disconnected, shutting down monitoring thread");
                    break;
                }
            }

            // Perform update approximately once per second
            let now = Instant::now();
            if now.duration_since(last_update) >= Duration::from_secs(1) {
                let update_start = Instant::now();

                match self.update() {
                    Ok((process_map, interface_map)) => {
                        let update_time = update_start.elapsed();

                        // Send update to UI thread (non-blocking) with socket mapper info
                        let update_data = MonitorUpdateData {
                            acquired_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            process_map,
                            interface_map,
                            socket_mapper_name: self.socket_mapper_name.clone(),
                            socket_mapper_capabilities: self.socket_mapper_capabilities,
                        };

                        // Lossless bounded delivery: the dedicated monitoring thread applies
                        // backpressure instead of dropping graph/history samples. Closing
                        // the receiver before joining this thread unblocks shutdown.
                        if update_tx.blocking_send(update_data).is_err() {
                            log::warn!(
                                "Update channel disconnected, shutting down monitoring thread"
                            );
                            break;
                        }

                        // Log performance occasionally
                        if update_time.as_millis() > 100 {
                            log::info!("⏱️  Background monitor.update() took {:?}", update_time);
                        }
                    }
                    Err(e) => {
                        log::error!("Monitor update failed: {}", e);
                        // Continue trying - don't break the loop
                    }
                }

                last_update = now;
            }

            // Sleep briefly to avoid busy-waiting
            std::thread::sleep(Duration::from_millis(100));
        }

        log::info!("Monitoring background thread exiting");
    }

    /// Process raw connection map into usable data structures
    /// This is the heavy computation that should run in the background task
    fn process_connection_map(conn_map: ConnectionMap) -> ProcessedConnectionData {
        // Clone the raw map first since we'll be moving parts of it
        let raw_map = conn_map.clone();

        let new_socket_map = conn_map.socket_to_pid;
        let mut new_connection_map = HashMap::new();

        // Map TCP connections to PIDs via socket inodes
        for entry in conn_map.tcp_connections {
            if let Some(&(pid, _)) = new_socket_map.get(&entry.inode) {
                let key = ConnectionKey {
                    local_addr: entry.local_addr,
                    local_port: entry.local_port,
                    remote_addr: entry.remote_addr,
                    remote_port: entry.remote_port,
                    protocol: Protocol::Tcp,
                };
                new_connection_map.insert(key, pid);
            }
        }

        // Map TCP6 connections
        for entry in conn_map.tcp6_connections {
            if let Some(&(pid, _)) = new_socket_map.get(&entry.inode) {
                let key = ConnectionKey {
                    local_addr: entry.local_addr,
                    local_port: entry.local_port,
                    remote_addr: entry.remote_addr,
                    remote_port: entry.remote_port,
                    protocol: Protocol::Tcp,
                };
                new_connection_map.insert(key, pid);
            }
        }

        // Map UDP connections
        for entry in conn_map.udp_connections {
            if let Some(&(pid, _)) = new_socket_map.get(&entry.inode) {
                let key = ConnectionKey {
                    local_addr: entry.local_addr,
                    local_port: entry.local_port,
                    remote_addr: entry.remote_addr,
                    remote_port: entry.remote_port,
                    protocol: Protocol::Udp,
                };
                new_connection_map.insert(key, pid);
            }
        }

        // Map UDP6 connections
        for entry in conn_map.udp6_connections {
            if let Some(&(pid, _)) = new_socket_map.get(&entry.inode) {
                let key = ConnectionKey {
                    local_addr: entry.local_addr,
                    local_port: entry.local_port,
                    remote_addr: entry.remote_addr,
                    remote_port: entry.remote_port,
                    protocol: Protocol::Udp,
                };
                new_connection_map.insert(key, pid);
            }
        }

        // Build list of PIDs with connections and their names
        let pids_with_names: Vec<(i32, String)> = new_connection_map
            .values()
            .copied()
            .collect::<HashSet<i32>>()
            .into_iter()
            .map(|pid| {
                let name = new_socket_map
                    .values()
                    .find(|(p, _)| *p == pid)
                    .map(|(_, n)| n.clone())
                    .unwrap_or_else(|| format!("PID {}", pid));
                (pid, name)
            })
            .collect();

        ProcessedConnectionData {
            socket_map: new_socket_map,
            connection_map: new_connection_map,
            pids_with_names,
            raw_connection_map: raw_map, // Keep the raw map for populate_connections
        }
    }

    /// Update socket inode -> PID mapping from pre-processed cache
    /// This is now a fast operation that just copies from the cache
    fn update_socket_map(&self) -> Result<()> {
        // Get pre-processed data from cache (fast - already processed!)
        let processed = if let Ok(cached) = self.cached_processed_data.lock() {
            cached.clone()
        } else {
            ProcessedConnectionData::default()
        };

        // Atomically update tracker with pre-processed data (fast!)
        let mut tracker = self.bandwidth_tracker.lock().unwrap();

        log::debug!(
            "Updating connection maps: {} sockets, {} connections (TCP+UDP)",
            processed.socket_map.len(),
            processed.connection_map.len()
        );

        // Log sample of UDP connections for debugging
        if log::log_enabled!(log::Level::Debug) {
            let udp_count = processed
                .connection_map
                .iter()
                .filter(|(k, _)| k.protocol == Protocol::Udp)
                .count();
            log::debug!("  UDP connections: {}", udp_count);

            // Log first 5 UDP connections
            for (i, (key, pid)) in processed
                .connection_map
                .iter()
                .filter(|(k, _)| k.protocol == Protocol::Udp)
                .take(5)
                .enumerate()
            {
                log::debug!(
                    "  UDP[{}]: {}:{} <-> {}:{} -> PID {}",
                    i,
                    key.local_addr,
                    key.local_port,
                    key.remote_addr,
                    key.remote_port,
                    pid
                );
            }
        }

        tracker.socket_map = processed.socket_map;
        tracker.connection_map = processed.connection_map;

        // Initialize process_bandwidth entries for all processes with active connections
        // This prevents processes from being "invisible" if they have connections
        // but haven't had packets captured yet (solves the cold start problem)
        for (pid, name) in processed.pids_with_names {
            tracker
                .process_bandwidth
                .entry(pid)
                .or_insert(ProcessBandwidth {
                    name,
                    rx_bytes: 0,
                    tx_bytes: 0,
                    last_rx_bytes: 0,
                    last_tx_bytes: 0,
                    internet_rx_bytes: 0,
                    internet_tx_bytes: 0,
                    local_rx_bytes: 0,
                    local_tx_bytes: 0,
                    last_internet_rx_bytes: 0,
                    last_internet_tx_bytes: 0,
                    last_local_rx_bytes: 0,
                    last_local_tx_bytes: 0,
                });
        }

        log::debug!(
            "Initialized {} processes in process_bandwidth map",
            tracker.process_bandwidth.len()
        );

        Ok(())
    }

    /// Packet capture thread - runs continuously on a specific interface
    fn capture_packets_on_interface(
        interface: NetworkInterface,
        tracker: Arc<Mutex<BandwidthTracker>>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<()> {
        let iface_name = interface.name.clone();
        log::info!("Packet capture thread started on interface: {}", iface_name);
        log::debug!("Interface details: {:?}", interface);

        // Create channel for packet capture
        let (_, mut rx) = match datalink::channel(&interface, Default::default()) {
            Ok(Channel::Ethernet(tx, rx)) => {
                log::info!(
                    "Successfully created packet capture channel for {}",
                    iface_name
                );
                (tx, rx)
            }
            Ok(_) => {
                return Err(anyhow::anyhow!(
                    "Unsupported channel type for {}",
                    iface_name
                ));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to create channel for {}: {}",
                    iface_name,
                    e
                ));
            }
        };

        // Capture packets until shutdown signal received
        loop {
            // Check shutdown flag
            if shutdown.load(Ordering::Relaxed) {
                log::info!(
                    "Packet capture thread stopping on interface: {}",
                    iface_name
                );
                break;
            }

            match rx.next() {
                Ok(packet) => {
                    if let Err(e) = Self::process_packet(packet, &iface_name, &tracker) {
                        // Don't spam errors, just continue
                        if std::env::var("DEBUG").is_ok() {
                            log::error!("Packet processing error on {}: {}", iface_name, e);
                        }
                    }
                }
                Err(e) => {
                    log::error!("Packet receive error on {}: {}", iface_name, e);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }

        log::debug!(
            "Packet capture thread exited cleanly on interface: {}",
            iface_name
        );
        Ok(())
    }

    fn find_all_interfaces() -> Vec<NetworkInterface> {
        let interfaces = datalink::interfaces();

        log::debug!("Found {} total interfaces from pnet", interfaces.len());
        for iface in &interfaces {
            log::debug!(
                "  Interface: {} (up={}, ips={})",
                iface.name,
                iface.is_up(),
                iface.ips.len()
            );
        }

        // Return all interfaces that have IP addresses
        // On Windows/Npcap, interfaces may report as "not up" even when functional
        // We include loopback as it can be useful for local services
        let filtered: Vec<_> = interfaces
            .into_iter()
            .filter(|iface| {
                // On Windows, just check for non-zero IP (0.0.0.0 means unconfigured)
                #[cfg(target_os = "windows")]
                {
                    !iface.ips.is_empty() && !iface.ips.iter().all(|ip| ip.ip().is_unspecified())
                }
                #[cfg(not(target_os = "windows"))]
                {
                    iface.is_up() && !iface.ips.is_empty()
                }
            })
            .collect();

        if filtered.is_empty() {
            log::warn!("No suitable network interfaces found for packet capture!");
            log::warn!(
                "This may be due to insufficient permissions (try running as Administrator)"
            );
            log::warn!("Or Npcap may not be installed correctly");
        }

        filtered
    }

    fn process_packet(
        packet: &[u8],
        interface_name: &str,
        tracker: &Arc<Mutex<BandwidthTracker>>,
    ) -> Result<()> {
        let ethernet = EthernetPacket::new(packet).context("Failed to parse Ethernet packet")?;

        match ethernet.get_ethertype() {
            EtherTypes::Ipv4 => {
                if let Some(ipv4) = Ipv4Packet::new(ethernet.payload()) {
                    Self::process_ipv4_packet(&ipv4, packet.len(), interface_name, tracker)?;
                }
            }
            EtherTypes::Ipv6 => {
                if let Some(ipv6) = Ipv6Packet::new(ethernet.payload()) {
                    Self::process_ipv6_packet(&ipv6, packet.len(), interface_name, tracker)?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn process_ipv4_packet(
        ipv4: &Ipv4Packet,
        packet_len: usize,
        interface_name: &str,
        tracker: &Arc<Mutex<BandwidthTracker>>,
    ) -> Result<()> {
        let src_addr = IpAddr::V4(ipv4.get_source());
        let dst_addr = IpAddr::V4(ipv4.get_destination());

        match ipv4.get_next_level_protocol() {
            IpNextHeaderProtocols::Tcp => {
                if let Some(tcp) = TcpPacket::new(ipv4.payload()) {
                    Self::track_connection(
                        (src_addr, tcp.get_source()),
                        (dst_addr, tcp.get_destination()),
                        Protocol::Tcp,
                        packet_len,
                        interface_name,
                        tracker,
                    );
                }
            }
            IpNextHeaderProtocols::Udp => {
                if let Some(udp) = UdpPacket::new(ipv4.payload()) {
                    Self::track_connection(
                        (src_addr, udp.get_source()),
                        (dst_addr, udp.get_destination()),
                        Protocol::Udp,
                        packet_len,
                        interface_name,
                        tracker,
                    );
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn process_ipv6_packet(
        ipv6: &Ipv6Packet,
        packet_len: usize,
        interface_name: &str,
        tracker: &Arc<Mutex<BandwidthTracker>>,
    ) -> Result<()> {
        let src_addr = IpAddr::V6(ipv6.get_source());
        let dst_addr = IpAddr::V6(ipv6.get_destination());

        match ipv6.get_next_header() {
            IpNextHeaderProtocols::Tcp => {
                if let Some(tcp) = TcpPacket::new(ipv6.payload()) {
                    Self::track_connection(
                        (src_addr, tcp.get_source()),
                        (dst_addr, tcp.get_destination()),
                        Protocol::Tcp,
                        packet_len,
                        interface_name,
                        tracker,
                    );
                }
            }
            IpNextHeaderProtocols::Udp => {
                if let Some(udp) = UdpPacket::new(ipv6.payload()) {
                    Self::track_connection(
                        (src_addr, udp.get_source()),
                        (dst_addr, udp.get_destination()),
                        Protocol::Udp,
                        packet_len,
                        interface_name,
                        tracker,
                    );
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn track_connection(
        source: (IpAddr, u16),
        destination: (IpAddr, u16),
        protocol: Protocol,
        packet_len: usize,
        interface_name: &str,
        tracker: &Arc<Mutex<BandwidthTracker>>,
    ) {
        let (src_addr, src_port) = source;
        let (dst_addr, dst_port) = destination;
        let mut tracker = tracker.lock().unwrap();
        tracker.packets_captured += 1;

        // Try both directions to find which one matches our connection map
        let key_outbound = ConnectionKey {
            local_addr: src_addr,
            local_port: src_port,
            remote_addr: dst_addr,
            remote_port: dst_port,
            protocol,
        };

        let key_inbound = ConnectionKey {
            local_addr: dst_addr,
            local_port: dst_port,
            remote_addr: src_addr,
            remote_port: src_port,
            protocol,
        };

        // Check for exact matches first
        let mut pid_and_direction: Option<(i32, bool)> = None; // (pid, is_outbound)
        let debug_match = tracker.packets_unmatched < 10;

        if let Some(&pid) = tracker.connection_map.get(&key_outbound) {
            pid_and_direction = Some((pid, true));
            if debug_match {
                log::debug!(
                    "✓ Matched outbound: {}:{} -> {}:{} to PID {}",
                    src_addr,
                    src_port,
                    dst_addr,
                    dst_port,
                    pid
                );
            }
        } else if let Some(&pid) = tracker.connection_map.get(&key_inbound) {
            pid_and_direction = Some((pid, false));
            if debug_match {
                log::debug!(
                    "✓ Matched inbound: {}:{} <- {}:{} to PID {}",
                    dst_addr,
                    dst_port,
                    src_addr,
                    src_port,
                    pid
                );
            }
        } else if protocol == Protocol::Tcp && debug_match {
            // TCP should always have exact matches - log why it failed
            log::debug!(
                "✗ TCP no exact match: {}:{} <-> {}:{}",
                src_addr,
                src_port,
                dst_addr,
                dst_port
            );
            log::debug!(
                "  Available TCP connections in map: {}",
                tracker
                    .connection_map
                    .keys()
                    .filter(|k| k.protocol == Protocol::Tcp)
                    .count()
            );
        } else if protocol == Protocol::Udp {
            // Enhanced UDP wildcard matching for Windows
            // Level 1: Try exact match with wildcard remote (original logic)
            for (key, &pid) in tracker.connection_map.iter() {
                if key.protocol == Protocol::Udp
                    && key.local_addr == src_addr
                    && key.local_port == src_port
                    && (key.remote_addr.is_unspecified() || key.remote_port == 0)
                {
                    pid_and_direction = Some((pid, true));
                    if debug_match {
                        log::debug!(
                            "✓ UDP Level 1 match (exact IP+port, wildcard remote): {}:{} -> PID {}",
                            src_addr,
                            src_port,
                            pid
                        );
                    }
                    break;
                }
            }

            // Level 2: If no match, try matching on port with 0.0.0.0 local addr
            // Windows often reports UDP sockets as 0.0.0.0:PORT
            if pid_and_direction.is_none() {
                for (key, &pid) in tracker.connection_map.iter() {
                    if key.protocol == Protocol::Udp
                        && key.local_addr.is_unspecified()  // 0.0.0.0 or ::
                        && key.local_port == src_port
                        && (key.remote_addr.is_unspecified() || key.remote_port == 0)
                    {
                        pid_and_direction = Some((pid, true));
                        if debug_match {
                            log::debug!(
                                "✓ UDP Level 2 match (0.0.0.0:{}): {}:{} -> PID {}",
                                src_port,
                                src_addr,
                                src_port,
                                pid
                            );
                        }
                        break;
                    }
                }
            }

            // Level 3: For inbound UDP, try matching destination with wildcard binding
            if pid_and_direction.is_none() {
                for (key, &pid) in tracker.connection_map.iter() {
                    if key.protocol == Protocol::Udp
                        && (key.local_addr == dst_addr || key.local_addr.is_unspecified())
                        && key.local_port == dst_port
                        && (key.remote_addr.is_unspecified() || key.remote_port == 0)
                    {
                        pid_and_direction = Some((pid, false)); // inbound
                        if debug_match {
                            log::debug!(
                                "✓ UDP Level 3 match (inbound to {}:{}): PID {}",
                                dst_addr,
                                dst_port,
                                pid
                            );
                        }
                        break;
                    }
                }
            }

            // Level 4: Last resort - match ANY UDP socket from same source IP
            // Only for ephemeral ports (>1024) to avoid false positives
            if pid_and_direction.is_none() && src_port > 1024 {
                for (key, &pid) in tracker.connection_map.iter() {
                    if key.protocol == Protocol::Udp
                        && key.local_port > 1024  // Also ephemeral
                        && (key.local_addr == src_addr || key.local_addr.is_unspecified())
                        && (key.remote_addr.is_unspecified() || key.remote_port == 0)
                    {
                        pid_and_direction = Some((pid, true));
                        if debug_match {
                            log::debug!(
                                "✓ UDP Level 4 match (same IP, ephemeral port): {}:{} -> PID {} (approx)",
                                src_addr,
                                src_port,
                                pid
                            );
                        }
                        break;
                    }
                }
            }
        }

        if let Some((pid, is_outbound)) = pid_and_direction {
            tracker.packets_matched += 1;

            let name = tracker
                .socket_map
                .values()
                .find(|(p, _)| *p == pid)
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| format!("PID {}", pid));

            // NEW: Categorize traffic based on remote IP
            let remote_ip = if is_outbound { dst_addr } else { src_addr };
            let traffic_category = crate::traffic_classifier::categorize_traffic(&remote_ip);

            // Track overall process bandwidth
            let bandwidth = tracker
                .process_bandwidth
                .entry(pid)
                .or_insert(ProcessBandwidth {
                    name,
                    rx_bytes: 0,
                    tx_bytes: 0,
                    last_rx_bytes: 0,
                    last_tx_bytes: 0,
                    internet_rx_bytes: 0,
                    internet_tx_bytes: 0,
                    local_rx_bytes: 0,
                    local_tx_bytes: 0,
                    last_internet_rx_bytes: 0,
                    last_internet_tx_bytes: 0,
                    last_local_rx_bytes: 0,
                    last_local_tx_bytes: 0,
                });

            if is_outbound {
                bandwidth.tx_bytes += packet_len as u64;
                // NEW: Categorized upload
                match traffic_category {
                    crate::traffic_classifier::TrafficCategory::Internet => {
                        bandwidth.internet_tx_bytes += packet_len as u64;
                    }
                    crate::traffic_classifier::TrafficCategory::Local => {
                        bandwidth.local_tx_bytes += packet_len as u64;
                    }
                }
            } else {
                bandwidth.rx_bytes += packet_len as u64;
                // NEW: Categorized download
                match traffic_category {
                    crate::traffic_classifier::TrafficCategory::Internet => {
                        bandwidth.internet_rx_bytes += packet_len as u64;
                    }
                    crate::traffic_classifier::TrafficCategory::Local => {
                        bandwidth.local_rx_bytes += packet_len as u64;
                    }
                }
            }

            // Track per-process, per-interface bandwidth
            let proc_iface_bandwidth = tracker
                .process_interface_bandwidth
                .entry((pid, interface_name.to_string()))
                .or_insert(ProcessInterfaceBandwidth {
                    rx_bytes: 0,
                    tx_bytes: 0,
                    last_rx_bytes: 0,
                    last_tx_bytes: 0,
                    internet_rx_bytes: 0,
                    internet_tx_bytes: 0,
                    local_rx_bytes: 0,
                    local_tx_bytes: 0,
                    last_internet_rx_bytes: 0,
                    last_internet_tx_bytes: 0,
                    last_local_rx_bytes: 0,
                    last_local_tx_bytes: 0,
                });

            if is_outbound {
                proc_iface_bandwidth.tx_bytes += packet_len as u64;
                // NEW: Categorized upload
                match traffic_category {
                    crate::traffic_classifier::TrafficCategory::Internet => {
                        proc_iface_bandwidth.internet_tx_bytes += packet_len as u64;
                    }
                    crate::traffic_classifier::TrafficCategory::Local => {
                        proc_iface_bandwidth.local_tx_bytes += packet_len as u64;
                    }
                }
            } else {
                proc_iface_bandwidth.rx_bytes += packet_len as u64;
                // NEW: Categorized download
                match traffic_category {
                    crate::traffic_classifier::TrafficCategory::Internet => {
                        proc_iface_bandwidth.internet_rx_bytes += packet_len as u64;
                    }
                    crate::traffic_classifier::TrafficCategory::Local => {
                        proc_iface_bandwidth.local_rx_bytes += packet_len as u64;
                    }
                }
            }

            // Track interface-level bandwidth
            let iface_bandwidth = tracker
                .interface_bandwidth
                .entry(interface_name.to_string())
                .or_insert(InterfaceBandwidth {
                    rx_bytes: 0,
                    tx_bytes: 0,
                    last_rx_bytes: 0,
                    last_tx_bytes: 0,
                });

            if is_outbound {
                iface_bandwidth.tx_bytes += packet_len as u64;
            } else {
                iface_bandwidth.rx_bytes += packet_len as u64;
            }
        } else {
            tracker.packets_unmatched += 1;

            // Still track interface bandwidth even if we can't match to a process
            // This is important for seeing total interface utilization
            // We'll count it as RX since we don't know the direction
            let iface_bandwidth = tracker
                .interface_bandwidth
                .entry(interface_name.to_string())
                .or_insert(InterfaceBandwidth {
                    rx_bytes: 0,
                    tx_bytes: 0,
                    last_rx_bytes: 0,
                    last_tx_bytes: 0,
                });
            iface_bandwidth.rx_bytes += packet_len as u64;

            // Log first few unmatched packets for debugging with more detail
            if tracker.packets_unmatched <= 10 {
                log::debug!(
                    "Unmatched packet: {}:{} -> {}:{} ({:?}, {} bytes)",
                    src_addr,
                    src_port,
                    dst_addr,
                    dst_port,
                    protocol,
                    packet_len
                );

                // Show why it didn't match
                if protocol == Protocol::Udp {
                    let has_matching_port = tracker.connection_map.keys().any(|k| {
                        k.protocol == Protocol::Udp
                            && (k.local_port == src_port || k.local_port == dst_port)
                    });
                    let has_matching_ip = tracker.connection_map.keys().any(|k| {
                        k.protocol == Protocol::Udp
                            && (k.local_addr == src_addr || k.local_addr == dst_addr)
                    });
                    log::debug!(
                        "  Match debug: port_in_map={}, ip_in_map={}, total_udp_conns={}",
                        has_matching_port,
                        has_matching_ip,
                        tracker
                            .connection_map
                            .keys()
                            .filter(|k| k.protocol == Protocol::Udp)
                            .count()
                    );
                }
            } else if tracker.packets_unmatched == 11 {
                log::debug!("Further unmatched packets will not be logged");
            }
        }

        // Periodic statistics logging
        if tracker.packets_captured.is_multiple_of(1000) {
            log::debug!(
                "Packet stats: captured={}, matched={}, unmatched={} (match rate: {:.1}%)",
                tracker.packets_captured,
                tracker.packets_matched,
                tracker.packets_unmatched,
                (tracker.packets_matched as f64 / tracker.packets_captured as f64) * 100.0
            );
        }
    }

    /// Extract bandwidth data for preservation when switching backends
    pub fn extract_bandwidth_data(
        &self,
    ) -> (HashMap<i32, ProcessBandwidth>, HashMap<i32, Instant>) {
        let tracker = self.bandwidth_tracker.lock().unwrap();
        (
            tracker.process_bandwidth.clone(),
            tracker.terminated_processes.clone(),
        )
    }

    /// Restore bandwidth data after switching backends
    pub fn restore_bandwidth_data(
        &mut self,
        process_bandwidth: HashMap<i32, ProcessBandwidth>,
        terminated_processes: HashMap<i32, Instant>,
    ) {
        let mut tracker = self.bandwidth_tracker.lock().unwrap();
        tracker.process_bandwidth = process_bandwidth;
        tracker.terminated_processes = terminated_processes;
        log::info!(
            "Restored bandwidth data for {} processes ({} terminated)",
            tracker.process_bandwidth.len(),
            tracker.terminated_processes.len()
        );
    }
}

impl Drop for NetworkMonitor {
    fn drop(&mut self) {
        log::info!(
            "Shutting down NetworkMonitor - signaling {} packet capture threads to stop",
            self.capture_handles.len()
        );

        // Signal all threads to stop
        self.shutdown_flag.store(true, Ordering::Relaxed);

        // Don't wait for threads to finish - they will exit naturally when they process
        // their next packet (or on program termination). This prevents UI freeze since
        // threads are blocked in rx.next() and only check the shutdown flag between packets.
        //
        // This is safe because:
        // 1. Threads will exit within seconds (on next packet) or on program exit
        // 2. They only read network data and write to bandwidth_tracker Arc
        // 3. No resource leaks - datalink channels close when threads exit
        // 4. bandwidth_tracker won't be freed until all thread references are dropped

        log::info!(
            "NetworkMonitor shutdown signaled (threads will exit on next packet or program termination)"
        );
    }
}
