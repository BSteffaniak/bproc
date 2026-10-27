mod backends;
mod config;
mod history;
mod keybindings;

// NetworkMonitor module - conditionally compiled based on available backends
#[cfg(feature = "monitor-pnet")]
mod monitor;

// Windows polling monitor wrapper when pnet is not available
#[cfg(all(target_os = "windows", not(feature = "monitor-pnet")))]
mod monitor {
    use crate::backends::monitor::MonitorBackend;
    use crate::backends::monitor::windows_poll::WindowsPollingMonitor;
    use crate::process::{InterfaceMap, ProcessMap};
    use anyhow::Result;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

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
        pub acquired_at: u64,
        pub process_map: ProcessMap,
        pub interface_map: InterfaceMap,
        pub socket_mapper_name: String,
        pub socket_mapper_capabilities: crate::backends::BackendCapabilities,
    }

    /// Update messages sent from monitoring thread to UI thread
    pub type MonitorUpdate = MonitorUpdateData;

    pub struct NetworkMonitor {
        backend: WindowsPollingMonitor,
    }

    pub struct ProcessBandwidth {
        name: String,
        rx_bytes: u64,
        tx_bytes: u64,
        last_rx_bytes: u64,
        last_tx_bytes: u64,
    }

    impl NetworkMonitor {
        pub fn with_socket_mapper(_: Option<&str>) -> Result<Self> {
            log::info!("Using Windows polling monitor backend");
            Ok(NetworkMonitor {
                backend: WindowsPollingMonitor::new()?,
            })
        }

        pub fn get_monitoring_backend_name(&self) -> &'static str {
            "windows-poll"
        }

        pub fn get_socket_mapper_info(&self) -> (&str, &crate::backends::BackendCapabilities) {
            // Windows polling monitor uses iphelper socket mapper
            static CAPS: crate::backends::BackendCapabilities =
                crate::backends::BackendCapabilities {
                    ipv4_support: true,
                    ipv6_support: true,
                    per_process: true,
                    per_connection: true,
                };
            ("iphelper", &CAPS)
        }

        pub fn update(&mut self) -> Result<(ProcessMap, InterfaceMap)> {
            self.backend.update()
        }

        pub fn extract_bandwidth_data(
            &self,
        ) -> (HashMap<i32, ProcessBandwidth>, HashMap<i32, Instant>) {
            // Not implemented for polling backend
            (HashMap::new(), HashMap::new())
        }

        pub fn restore_bandwidth_data(
            &mut self,
            _: HashMap<i32, ProcessBandwidth>,
            _: HashMap<i32, Instant>,
        ) {
            // No-op for polling backend
        }

        pub fn get_bandwidth_data(&self) -> HashMap<i32, Vec<crate::history::BandwidthSample>> {
            // Not implemented for polling backend
            HashMap::new()
        }

        /// Run the monitoring loop in a background thread
        pub fn run_monitoring_loop(
            mut self,
            mut cmd_rx: mpsc::UnboundedReceiver<MonitorCommand>,
            update_tx: mpsc::Sender<MonitorUpdate>,
        ) {
            log::info!("Starting monitoring background thread (windows-poll)");
            let mut last_update = Instant::now();

            loop {
                // Check for commands (non-blocking)
                match cmd_rx.try_recv() {
                    Ok(MonitorCommand::Shutdown) => {
                        log::info!("Monitoring thread received shutdown command");
                        break;
                    }
                    Ok(MonitorCommand::SwitchSocketMapper {
                        backend_name: _,
                        response_tx,
                    }) => {
                        log::warn!("Socket mapper switching not supported in windows-poll backend");
                        let _ = response_tx.send(Err(anyhow::anyhow!("Not supported")));
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
                    match self.update() {
                        Ok((process_map, interface_map)) => {
                            // Send update to UI thread (non-blocking) with socket mapper info
                            let update_data = MonitorUpdateData {
                                acquired_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                process_map,
                                interface_map,
                                socket_mapper_name: "iphelper".to_string(),
                                socket_mapper_capabilities: crate::backends::BackendCapabilities {
                                    ipv4_support: true,
                                    ipv6_support: true,
                                    per_process: true,
                                    per_connection: true,
                                },
                            };

                            if update_tx.blocking_send(update_data).is_err() {
                                log::warn!(
                                    "Update channel disconnected, shutting down monitoring thread"
                                );
                                break;
                            }
                        }
                        Err(e) => {
                            log::error!("Monitor update failed: {}", e);
                        }
                    }

                    last_update = now;
                }

                // Sleep briefly to avoid busy-waiting
                std::thread::sleep(Duration::from_millis(100));
            }

            log::info!("Monitoring background thread exiting");
        }
    }
}

// Stub monitor module when no backends are available (non-Windows without pnet)
#[cfg(not(any(feature = "monitor-pnet", target_os = "windows")))]
mod monitor {
    use crate::process::{InterfaceMap, ProcessMap};
    use anyhow::Result;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    /// Commands sent from UI thread to monitoring thread
    pub enum MonitorCommand {
        /// Switch to a different socket mapper backend
        SwitchSocketMapper {
            // The disabled backend rejects every switch, irrespective of the name.
            #[allow(dead_code)]
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
        pub acquired_at: u64,
        pub process_map: ProcessMap,
        pub interface_map: InterfaceMap,
        pub socket_mapper_name: String,
        pub socket_mapper_capabilities: crate::backends::BackendCapabilities,
    }

    /// Update messages sent from monitoring thread to UI thread
    pub type MonitorUpdate = MonitorUpdateData;

    pub struct NetworkMonitor;

    impl NetworkMonitor {
        pub fn with_socket_mapper(_: Option<&str>) -> Result<Self> {
            log::warn!("No monitoring backend available - monitoring disabled");
            Ok(NetworkMonitor)
        }

        pub fn run_monitoring_loop(
            self,
            mut cmd_rx: mpsc::UnboundedReceiver<MonitorCommand>,
            update_tx: mpsc::Sender<MonitorUpdate>,
        ) {
            log::warn!("Monitoring background thread started but no backend available");

            loop {
                // Check for commands
                match cmd_rx.try_recv() {
                    Ok(MonitorCommand::Shutdown) => {
                        break;
                    }
                    Ok(MonitorCommand::SwitchSocketMapper {
                        backend_name: _,
                        response_tx,
                    }) => {
                        let _ = response_tx.send(Err(anyhow::anyhow!("No backend available")));
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        break;
                    }
                    _ => {}
                }

                // Send empty updates periodically
                let _now = Instant::now();
                std::thread::sleep(Duration::from_secs(1));

                let update_data = MonitorUpdateData {
                    acquired_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    process_map: HashMap::new(),
                    interface_map: HashMap::new(),
                    socket_mapper_name: "none".to_string(),
                    socket_mapper_capabilities: crate::backends::BackendCapabilities {
                        ipv4_support: false,
                        ipv6_support: false,
                        per_process: false,
                        per_connection: false,
                    },
                };

                if update_tx.blocking_send(update_data).is_err() {
                    break;
                }
            }

            log::info!("Stub monitoring thread exiting");
        }
    }
}

mod process;
#[cfg(any(feature = "monitor-pnet", test))]
mod traffic_classifier;
mod tui_runtime;
mod ui;

use anyhow::Result;
use clap::Parser;
use std::time::Duration;

use crate::backends::throttle::ThrottleManager;
use crate::backends::throttle::{
    detect_download_backends, detect_upload_backends, select_download_backend,
    select_upload_backend,
};
use crate::monitor::{MonitorCommand, MonitorUpdate, NetworkMonitor};
use crate::process::ThrottleLimit;
use crate::ui::AppState;

/// Format bytes as human-readable string (e.g., "1.5 MB", "500 KB")
fn human_readable(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// bproc - A TUI network monitor and throttler for Linux
#[derive(Parser, Debug)]
#[command(name = "bproc")]
#[command(version = "0.6.0")]
#[command(about = "Per-process network monitoring and bandwidth control for Linux", long_about = None)]
struct Args {
    /// Upload throttling backend to use
    #[arg(long, value_name = "BACKEND")]
    upload_backend: Option<String>,

    /// Download throttling backend to use
    #[arg(long, value_name = "BACKEND")]
    download_backend: Option<String>,

    /// Socket mapper backend to use for network monitoring
    #[arg(long, value_name = "BACKEND")]
    socket_mapper: Option<String>,

    /// List all available backends and exit
    #[arg(long)]
    list_backends: bool,

    /// Don't restore saved throttles on startup (default: restore is enabled)
    #[arg(long)]
    no_restore: bool,

    /// Don't save throttles on exit
    #[arg(long)]
    no_save: bool,

    // CLI mode arguments
    /// PID to throttle (CLI mode - skips TUI)
    #[arg(long, value_name = "PID")]
    pid: Option<i32>,

    /// Download limit (e.g., "1M", "500K", "1.5M") - requires --pid
    #[arg(long, value_name = "LIMIT")]
    download_limit: Option<String>,

    /// Upload limit (e.g., "1M", "500K", "1.5M") - requires --pid
    #[arg(long, value_name = "LIMIT")]
    upload_limit: Option<String>,

    /// Duration to run throttle in seconds (default: run until Ctrl+C)
    #[arg(long, value_name = "SECONDS")]
    duration: Option<u64>,

    /// BPF attach method: auto (try link, fallback to legacy), link (bpf_link_create), legacy (bpf_prog_attach)
    #[arg(long, value_name = "METHOD")]
    bpf_attach_method: Option<String>,
}

fn print_available_backends() {
    use crate::backends::process::socket_mapper::detect_socket_mappers;

    println!("bproc v0.6.0 - Available Backends\n");

    // Socket mapper backends
    println!("Socket Mapper Backends:");
    let socket_mappers = detect_socket_mappers();
    if socket_mappers.is_empty() {
        println!("  (none compiled in)");
    } else {
        for backend in socket_mappers {
            let status = if backend.available {
                "✅ available"
            } else {
                "❌ unavailable"
            };
            println!(
                "  {:20} [priority: {:?}] {}",
                backend.name, backend.priority, status
            );
        }
    }

    println!();

    // Upload backends
    println!("Upload Backends:");
    let upload_backends = detect_upload_backends();
    if upload_backends.is_empty() {
        println!("  (none compiled in)");
    } else {
        for backend in upload_backends {
            let status = if backend.available {
                "✅ available"
            } else {
                "❌ unavailable"
            };
            println!(
                "  {:20} [priority: {:?}] {}",
                backend.name, backend.priority, status
            );
        }
    }

    println!();

    // Download backends
    println!("Download Backends:");
    let download_backends = detect_download_backends();
    if download_backends.is_empty() {
        println!("  (none compiled in)");
    } else {
        for backend in download_backends {
            let status = if backend.available {
                "✅ available"
            } else {
                "❌ unavailable"
            };
            println!(
                "  {:20} [priority: {:?}] {}",
                backend.name, backend.priority, status
            );
        }
    }

    println!();
    println!("Usage:");
    println!("  TUI Mode:");
    println!(
        "    bproc [--upload-backend <name>] [--download-backend <name>] [--socket-mapper <name>]"
    );
    println!();
    println!("  CLI Mode:");
    println!(
        "    bproc --pid <PID> [--download-limit <LIMIT>] [--upload-limit <LIMIT>] [--duration <SECONDS>]"
    );
    println!("    Examples:");
    println!("      bproc --pid 1234 --download-limit 1M --upload-limit 500K");
    println!("      bproc --pid 1234 --download-limit 1.5M --duration 60");
    println!();
    println!("  BPF Options:");
    println!(
        "    --bpf-attach-method <METHOD>   BPF attach method: auto, link, legacy (default: auto)"
    );
    println!("      auto   - Try modern method, fallback to legacy on error");
    println!("      link   - Use bpf_link_create only");
    println!("      legacy - Use bpf_prog_attach only");
}

/// Parse bandwidth limit string (e.g., "1M", "500K", "1.5M") to bytes per second
fn parse_bandwidth_limit(limit_str: &str) -> Result<u64> {
    let limit_str = limit_str.trim().to_uppercase();

    // Try to split into number and unit
    let (num_str, unit) = if limit_str.ends_with("M") || limit_str.ends_with("MB") {
        if limit_str.ends_with("MB") {
            (&limit_str[..limit_str.len() - 2], "M")
        } else {
            (&limit_str[..limit_str.len() - 1], "M")
        }
    } else if limit_str.ends_with("K") || limit_str.ends_with("KB") {
        if limit_str.ends_with("KB") {
            (&limit_str[..limit_str.len() - 2], "K")
        } else {
            (&limit_str[..limit_str.len() - 1], "K")
        }
    } else if limit_str.ends_with("G") || limit_str.ends_with("GB") {
        if limit_str.ends_with("GB") {
            (&limit_str[..limit_str.len() - 2], "G")
        } else {
            (&limit_str[..limit_str.len() - 1], "G")
        }
    } else {
        // Assume bytes if no unit
        return limit_str
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("Invalid bandwidth limit: {}", limit_str));
    };

    let number: f64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid bandwidth limit number: {}", num_str))?;

    let bytes_per_sec = match unit {
        "K" => (number * 1024.0) as u64,
        "M" => (number * 1024.0 * 1024.0) as u64,
        "G" => (number * 1024.0 * 1024.0 * 1024.0) as u64,
        _ => return Err(anyhow::anyhow!("Unknown unit: {}", unit)),
    };

    Ok(bytes_per_sec)
}

/// Run CLI mode - apply throttle and wait
async fn run_cli_mode(args: &Args) -> Result<()> {
    use tokio::signal;

    let pid = args.pid.unwrap();

    // Parse bandwidth limits
    let download_limit = if let Some(ref limit_str) = args.download_limit {
        Some(parse_bandwidth_limit(limit_str)?)
    } else {
        None
    };

    let upload_limit = if let Some(ref limit_str) = args.upload_limit {
        Some(parse_bandwidth_limit(limit_str)?)
    } else {
        None
    };

    if download_limit.is_none() && upload_limit.is_none() {
        return Err(anyhow::anyhow!(
            "At least one of --download-limit or --upload-limit is required with --pid"
        ));
    }

    // Get process name using platform-specific utils
    use crate::backends::process::create_process_utils;
    let process_utils = create_process_utils();
    let process_name = process_utils
        .get_process_name(pid)
        .unwrap_or_else(|_| format!("PID {}", pid));

    println!("bproc v0.6.0 - CLI Mode");
    println!();
    println!("Throttling process: {} (PID {})", process_name, pid);
    if let Some(dl) = download_limit {
        println!("  Download limit: {}/s", human_readable(dl));
    }
    if let Some(ul) = upload_limit {
        println!("  Upload limit:   {}/s", human_readable(ul));
    }
    if let Some(dur) = args.duration {
        println!("  Duration:       {} seconds", dur);
    } else {
        println!("  Duration:       Until Ctrl+C");
    }
    println!();

    // Load config to get backend preferences (CLI mode also respects config)
    let config = config::Config::load().unwrap_or_default();

    // Determine backend preferences: CLI args override config file preferences
    let upload_preference = args
        .upload_backend
        .as_deref()
        .or(config.preferred_upload_backend.as_deref());
    let download_preference = args
        .download_backend
        .as_deref()
        .or(config.preferred_download_backend.as_deref());

    // Select backends
    let upload_backend = select_upload_backend(upload_preference);
    let download_backend = select_download_backend(download_preference);

    if let Some(ref backend) = upload_backend {
        println!("Using upload backend:   {}", backend.name());
    } else {
        println!("Upload backend:         Not available");
    }

    if let Some(ref backend) = download_backend {
        println!("Using download backend: {}", backend.name());
    } else {
        println!("Download backend:       Not available");
    }
    println!();

    // Create throttle manager
    let mut throttle_manager = ThrottleManager::new(upload_backend, download_backend);

    // Apply throttle
    let limit = ThrottleLimit {
        upload_limit,
        download_limit,
        traffic_type: crate::process::TrafficType::All, // Default to all traffic in CLI mode
    };

    throttle_manager.throttle_process(pid, process_name.clone(), &limit)?;
    println!("✅ Throttle applied successfully!");
    println!();

    // Wait for duration or Ctrl+C
    if let Some(duration) = args.duration {
        println!(
            "Running for {} seconds... (Press Ctrl+C to stop early)",
            duration
        );
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(duration)) => {
                println!("\n⏱️  Duration elapsed, removing throttle...");
            }
            _ = signal::ctrl_c() => {
                println!("\n🛑 Received Ctrl+C, removing throttle...");
            }
        }
    } else {
        println!("Press Ctrl+C to stop and remove throttle...");
        signal::ctrl_c().await?;
        println!("\n🛑 Received Ctrl+C, removing throttle...");
    }

    // Remove throttle
    throttle_manager.remove_throttle(pid)?;
    println!("✅ Throttle removed successfully!");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse CLI arguments
    let args = Args::parse();

    if std::env::var("RUST_LOG").is_ok() {
        pretty_env_logger::formatted_builder()
            .parse_default_env()
            .init();
    }

    // Initialize BPF configuration
    #[cfg(feature = "throttle-ebpf")]
    {
        use crate::backends::throttle::{BpfAttachMethod, BpfConfig, init_bpf_config};

        // Parse attach method from CLI arg or environment
        let attach_method = BpfAttachMethod::from_env_and_arg(args.bpf_attach_method.as_deref());
        init_bpf_config(BpfConfig::new(attach_method));

        log::info!("BPF attach method: {:?}", attach_method);
    }

    // Handle --list-backends
    if args.list_backends {
        print_available_backends();
        return Ok(());
    }

    // Handle CLI mode (--pid specified)
    if args.pid.is_some() {
        return run_cli_mode(&args).await;
    }

    // Create app state
    let mut app = AppState::new();

    // Load config FIRST to get backend preferences
    let config = config::Config::load().unwrap_or_default();

    // Load interface filter from config
    if let Some(ref filters) = config.filtered_interfaces {
        app.active_interface_filters = Some(filters.clone());

        if filters.is_empty() {
            log::info!("Loaded filter: showing no interfaces");
            app.status_message = "Loaded filter: showing no interfaces".to_string();
        } else {
            log::info!("Loaded interface filter: {}", filters.join(", "));
            app.status_message = format!("Loaded interface filter: {}", filters.join(", "));
        }
    }

    // Load traffic view mode from config
    if let Some(traffic_type) = config.traffic_view_mode {
        app.traffic_view_mode = match traffic_type {
            crate::process::TrafficType::All => ui::TrafficViewMode::All,
            crate::process::TrafficType::Internet => ui::TrafficViewMode::Internet,
            crate::process::TrafficType::Local => ui::TrafficViewMode::Local,
        };
        log::info!("Loaded traffic view mode: {:?}", app.traffic_view_mode);
    }

    // Determine backend preferences: CLI args override config file preferences
    let upload_preference = args
        .upload_backend
        .as_deref()
        .or(config.preferred_upload_backend.as_deref());
    let download_preference = args
        .download_backend
        .as_deref()
        .or(config.preferred_download_backend.as_deref());
    let socket_mapper_preference_str = args
        .socket_mapper
        .clone()
        .or_else(|| config.preferred_socket_mapper.clone());
    let socket_mapper_preference = socket_mapper_preference_str.as_deref();

    // Log which preference source is being used
    if let Some(pref) = upload_preference {
        if args.upload_backend.is_some() {
            log::info!("Using upload backend from CLI: {}", pref);
        } else {
            log::info!("Using upload backend from config: {}", pref);
        }
    }
    if let Some(pref) = download_preference {
        if args.download_backend.is_some() {
            log::info!("Using download backend from CLI: {}", pref);
        } else {
            log::info!("Using download backend from config: {}", pref);
        }
    }
    if let Some(pref) = socket_mapper_preference {
        if args.socket_mapper.is_some() {
            log::info!("Using socket mapper from CLI: {}", pref);
        } else {
            log::info!("Using socket mapper from config: {}", pref);
        }
    }
    if let Some(pref) = socket_mapper_preference {
        log::info!("Using socket mapper from CLI: {}", pref);
    }

    // Select and create backends with preferences
    let upload_backend = select_upload_backend(upload_preference);
    let download_backend = select_download_backend(download_preference);

    // Show backend status
    log::error!("🔥 bproc v0.6.0 - Backend Status:");
    log::error!("");

    if let Some(ref backend) = upload_backend {
        log::error!("  ✅ Upload throttling:   {} (available)", backend.name());
    } else {
        log::error!("  ⚠️  Upload throttling:   Not available");
        log::error!("      → Install 'tc' (traffic control) and enable cgroups");
    }

    if let Some(ref backend) = download_backend {
        log::error!("  ✅ Download throttling: {} (available)", backend.name());
    } else {
        log::error!("  ⚠️  Download throttling: Not available");
        log::error!("      → Enable 'ifb' kernel module (see IFB_SETUP.md)");
    }

    log::error!("");

    if upload_backend.is_none() && download_backend.is_none() {
        log::error!("⚠️  Warning: No throttling backends available!");
        log::error!(
            "   Network monitoring will work, but you won't be able to throttle processes."
        );
        log::error!("");
    }

    // Create managers with selected backends
    let mut throttle_manager = ThrottleManager::new(upload_backend, download_backend);

    // Create monitoring channels for async communication
    let (monitor_cmd_tx, monitor_cmd_rx) = tokio::sync::mpsc::unbounded_channel::<MonitorCommand>();
    let (monitor_update_tx, monitor_update_rx) = tokio::sync::mpsc::channel::<MonitorUpdate>(32);

    // Create monitor and move it to background thread
    let monitor = NetworkMonitor::with_socket_mapper(socket_mapper_preference)?;

    // Restore throttles before spawning monitor thread
    if !args.no_restore {
        log::info!("Restoring saved throttles...");
        for (pid, saved_throttle) in config.get_throttles() {
            let limit = ThrottleLimit {
                upload_limit: saved_throttle.upload_limit,
                download_limit: saved_throttle.download_limit,
                traffic_type: crate::process::TrafficType::All, // Default for restored throttles
            };
            if let Err(e) =
                throttle_manager.throttle_process(*pid, saved_throttle.process_name.clone(), &limit)
            {
                log::warn!("Failed to restore throttle for PID {}: {}", pid, e);
            } else {
                log::info!(
                    "Restored throttle for {} (PID {})",
                    saved_throttle.process_name,
                    pid
                );
            }
        }
    } else {
        log::info!("Skipping throttle restoration (--no-restore flag)");
    }

    // Spawn monitoring thread with ownership of monitor
    let monitor_thread = std::thread::spawn(move || {
        monitor.run_monitoring_loop(monitor_cmd_rx, monitor_update_tx);
    });
    log::info!("Spawned background monitoring thread");

    // Run the app with channel receivers
    tui_runtime::run(tui_runtime::Startup {
        app,
        manager: throttle_manager,
        config,
        monitor: monitor_update_rx,
        monitor_commands: monitor_cmd_tx,
        monitor_thread,
        save: !args.no_save,
        socket_mapper: socket_mapper_preference_str,
    })
    .await
}
