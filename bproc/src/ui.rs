use crate::backends::BackendPriority;
use crate::backends::throttle::BackendInfo;
use crate::history::HistoryTracker;
use crate::process::{InterfaceInfo, InterfaceMap, ProcessInfo, ProcessMap};
pub use bmux_tui::geometry::Rect;
use bmux_tui::{
    paint::PaintCx,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
mod events;
mod render;
mod scene;
use bmux_tui::text_width::display_width;
pub use render::ListState;
use render::{
    PanelChrome, TextPresentation, clear, line, paint_border, paint_list, paint_text, root_area,
    split_vertical,
};
use std::collections::HashMap;
use std::net::IpAddr;

pub struct AppState {
    component_scene: std::cell::RefCell<scene::ComponentScene>,
    /// Runtime-owned process metadata; rendering never performs process I/O.
    pub cached_process_details: Option<crate::process::ProcessDetails>,
    pub process_list: Vec<ProcessInfo>,
    pub unfiltered_process_list: Vec<ProcessInfo>, // Full list before interface filtering
    pub selected_index: Option<usize>,
    pub list_state: ListState,
    pub show_help: bool,
    pub show_throttle_dialog: bool,
    pub show_backend_info: bool,
    pub throttle_dialog: ThrottleDialog,
    pub status_message: String,
    pub history: HistoryTracker,
    pub show_graph: bool,
    pub sort_frozen: bool,
    frozen_order: HashMap<i32, usize>, // PID -> position index
    frozen_process_snapshot: Vec<ProcessInfo>, // Frozen snapshot of process list
    // Interface view state
    pub view_mode: ViewMode,
    pub interface_list: Vec<InterfaceInfo>,
    pub interface_list_state: ListState,
    pub selected_interface_index: Option<usize>,
    pub selected_interface_name: Option<String>,
    // Interface filter state
    pub active_interface_filters: Option<Vec<String>>, // None = show all, Some([]) = show nothing, Some([...]) = filter
    // Traffic categorization view state
    pub traffic_view_mode: TrafficViewMode,
    // Backend compatibility dialog state
    pub show_backend_compatibility_dialog: bool,
    pub backend_compatibility_dialog: Option<BackendCompatibilityDialog>,
    // Backend selection state (for interactive backend modal)
    pub backend_items: Vec<BackendSelectorItem>,
    pub backend_selected_index: usize,
    // Process detail view state
    pub selected_process_detail_pid: Option<i32>, // PID of process being detailed
    pub detail_scroll_offset: usize,              // For scrolling long content
    pub detail_tab: ProcessDetailTab,             // Which tab is active
    // Modal scroll offsets
    pub help_scroll_offset: usize,         // For help overlay scrolling
    pub backend_info_scroll_offset: usize, // For backend info modal scrolling
    pub interface_modal_scroll_offset: usize, // For interface filter modal scrolling
    pub backend_compat_scroll_offset: usize, // For backend compatibility dialog scrolling
    // Auto-scroll optimization - track last selection to avoid redundant scrolling
    pub last_interface_selection: Option<usize>,
    pub last_backend_selection: usize,
    // Mouse click selection - track clickable regions for each frame
    pub clickable_regions: Vec<ClickableRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    ProcessView,     // Show all processes
    InterfaceList,   // Show list of interfaces
    InterfaceDetail, // Show processes on selected interface
    ProcessDetail,   // Show detailed info about a single process
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProcessDetailTab {
    Overview,    // General info + bandwidth stats
    Connections, // Active network connections
    Traffic,     // Detailed traffic breakdown
    System,      // System/proc info
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrafficViewMode {
    All,      // Show combined traffic
    Internet, // Show only internet traffic
    Local,    // Show only local network traffic
}

#[derive(Debug, Clone)]
pub struct ClickableRegion {
    pub area: Rect,
    pub region_type: ClickableRegionType,
}

#[derive(Debug, Clone)]
pub enum ClickableRegionType {
    ProcessList {
        first_visible_index: usize, // Index of first visible item in list
        visible_count: usize,       // Number of visible items
    },
    ProcessDetailTabs {
        // Column ranges for each tab: (start_col, end_col, tab)
        tab_ranges: Vec<(u16, u16, ProcessDetailTab)>,
    },
    InterfaceModal {
        header_lines: usize, // Number of header lines before interface list starts
    },
    BackendModal {
        // Maps visual line (accounting for scroll) to backend item index
        line_to_item: HashMap<usize, usize>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackendSelectorItem {
    GroupHeader(BackendGroup),
    Backend {
        name: String,
        group: BackendGroup,
        priority: BackendPriority,
        available: bool,
        is_current_default: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackendGroup {
    SocketMapper,
    Upload,
    Download,
}

pub struct ThrottleDialog {
    pub download_input: String,
    pub upload_input: String,
    pub selected_field: ThrottleField,
    pub target_pid: Option<i32>,
    pub target_name: Option<String>,
    pub traffic_type_index: usize, // NEW: 0=All, 1=Internet, 2=Local
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThrottleField {
    Download,
    Upload,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackendCompatibilityAction {
    Cancel,
    SwitchTemporary(String),      // backend name
    SwitchAndMakeDefault(String), // backend name
    ConvertToAll,
}

#[derive(Clone)]
pub struct BackendCompatibilityDialog {
    pub current_backend: String,
    pub traffic_type: crate::process::TrafficType,
    pub compatible_backends: Vec<String>,
    pub selected_action: usize, // Index into available options
    pub is_upload: bool,        // true if upload backend issue, false if download
}

impl BackendCompatibilityDialog {
    pub fn new(
        current_backend: String,
        traffic_type: crate::process::TrafficType,
        compatible_backends: Vec<String>,
        is_upload: bool,
    ) -> Self {
        Self {
            current_backend,
            traffic_type,
            compatible_backends: compatible_backends.clone(),
            selected_action: if compatible_backends.is_empty() { 0 } else { 1 },
            is_upload,
        }
    }

    pub fn select_next(&mut self) {
        let total = self.get_total_options();
        if total > 0 {
            self.selected_action = (self.selected_action + 1) % total;
        }
    }

    pub fn select_previous(&mut self) {
        let total = self.get_total_options();
        if total > 0 {
            self.selected_action = if self.selected_action == 0 {
                total - 1
            } else {
                self.selected_action - 1
            };
        }
    }

    pub fn get_total_options(&self) -> usize {
        // Cancel + (2 options per compatible backend) + Convert to All
        1 + (self.compatible_backends.len() * 2) + 1
    }

    pub fn get_action(&self) -> BackendCompatibilityAction {
        if self.selected_action == 0 {
            return BackendCompatibilityAction::Cancel;
        }

        let last_option = self.get_total_options() - 1;
        if self.selected_action == last_option {
            return BackendCompatibilityAction::ConvertToAll;
        }

        // Options are: Cancel, [Switch temp, Switch default] * N, Convert to All
        let backend_option_index = self.selected_action - 1;
        let backend_index = backend_option_index / 2;
        let is_make_default = backend_option_index % 2 == 1;

        if let Some(backend_name) = self.compatible_backends.get(backend_index) {
            if is_make_default {
                BackendCompatibilityAction::SwitchAndMakeDefault(backend_name.clone())
            } else {
                BackendCompatibilityAction::SwitchTemporary(backend_name.clone())
            }
        } else {
            BackendCompatibilityAction::Cancel
        }
    }
}

impl ThrottleDialog {
    pub fn new() -> Self {
        Self {
            download_input: String::new(),
            upload_input: String::new(),
            selected_field: ThrottleField::Download,
            target_pid: None,
            target_name: None,
            traffic_type_index: 0, // Default to "All"
        }
    }

    pub fn reset(&mut self) {
        self.download_input.clear();
        self.upload_input.clear();
        self.selected_field = ThrottleField::Download;
        self.target_pid = None;
        self.target_name = None;
        self.traffic_type_index = 0; // Reset to "All"
    }

    pub fn cycle_traffic_type(&mut self) {
        self.traffic_type_index = (self.traffic_type_index + 1) % 3;
    }

    pub fn get_traffic_type(&self) -> crate::process::TrafficType {
        use crate::process::TrafficType;
        match self.traffic_type_index {
            0 => TrafficType::All,
            1 => TrafficType::Internet,
            2 => TrafficType::Local,
            _ => TrafficType::All,
        }
    }

    pub fn handle_char(&mut self, c: char) {
        match self.selected_field {
            ThrottleField::Download => self.download_input.push(c),
            ThrottleField::Upload => self.upload_input.push(c),
        }
    }

    pub fn handle_backspace(&mut self) {
        match self.selected_field {
            ThrottleField::Download => {
                self.download_input.pop();
            }
            ThrottleField::Upload => {
                self.upload_input.pop();
            }
        }
    }

    pub fn toggle_field(&mut self) {
        self.selected_field = match self.selected_field {
            ThrottleField::Download => ThrottleField::Upload,
            ThrottleField::Upload => ThrottleField::Download,
        };
    }

    pub fn parse_limits(&self) -> Option<(Option<u64>, Option<u64>)> {
        // Parse KB/s to bytes/sec
        let download = if self.download_input.is_empty() {
            None
        } else {
            self.download_input.parse::<u64>().ok().map(|kb| kb * 1024)
        };

        let upload = if self.upload_input.is_empty() {
            None
        } else {
            self.upload_input.parse::<u64>().ok().map(|kb| kb * 1024)
        };

        Some((download, upload))
    }
}

impl AppState {
    /// Supply details collected by the runtime refresh service (or clear stale details).
    pub fn update_process_details(&mut self, details: Option<crate::process::ProcessDetails>) {
        self.cached_process_details = details;
    }

    fn details_for(&self, process: &ProcessInfo) -> crate::process::ProcessDetails {
        self.cached_process_details
            .as_ref()
            .filter(|d| d.pid == process.pid)
            .cloned()
            .unwrap_or_else(|| crate::process::ProcessDetails {
                pid: process.pid,
                name: process.name.clone(),
                cmdline: None,
                exe_path: None,
                cwd: None,
                state: None,
                ppid: None,
                threads: None,
                memory_rss: None,
                memory_vms: None,
                uid: None,
                gid: None,
                connections: Vec::new(),
            })
    }

    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(None); // Nothing selected initially

        let mut interface_list_state = ListState::default();
        interface_list_state.select(None);

        Self {
            component_scene: Default::default(),
            cached_process_details: None,
            process_list: Vec::new(),
            unfiltered_process_list: Vec::new(),
            selected_index: None, // Nothing selected initially
            list_state,
            history: HistoryTracker::new(),
            show_graph: false,
            show_help: false,
            show_throttle_dialog: false,
            show_backend_info: false,
            throttle_dialog: ThrottleDialog::new(),
            status_message: String::from("bproc started. Press 'h' for help."),
            sort_frozen: false,
            frozen_order: HashMap::new(),
            frozen_process_snapshot: Vec::new(),
            view_mode: ViewMode::ProcessView,
            interface_list: Vec::new(),
            interface_list_state,
            selected_interface_index: None,
            selected_interface_name: None,
            active_interface_filters: None, // Show all by default
            traffic_view_mode: TrafficViewMode::All, // Show all traffic by default
            show_backend_compatibility_dialog: false,
            backend_compatibility_dialog: None,
            backend_items: Vec::new(),
            backend_selected_index: 0,
            selected_process_detail_pid: None,
            detail_scroll_offset: 0,
            detail_tab: ProcessDetailTab::Overview,
            help_scroll_offset: 0,
            backend_info_scroll_offset: 0,
            interface_modal_scroll_offset: 0,
            backend_compat_scroll_offset: 0,
            last_interface_selection: None,
            last_backend_selection: 0,
            clickable_regions: Vec::new(),
        }
    }

    pub fn update_interfaces(&mut self, interface_map: InterfaceMap) {
        let mut interfaces: Vec<InterfaceInfo> = interface_map.into_values().collect();

        // Sort by name for consistent display
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));

        self.interface_list = interfaces;

        // Adjust selection if out of bounds
        if let Some(index) = self.selected_interface_index {
            if index >= self.interface_list.len() && !self.interface_list.is_empty() {
                self.selected_interface_index = Some(self.interface_list.len() - 1);
                self.interface_list_state
                    .select(Some(self.interface_list.len() - 1));
            }
        }
    }

    pub fn toggle_view_mode(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::ProcessView => {
                // Switch to interface list view
                if !self.interface_list.is_empty() {
                    self.selected_interface_index = Some(0);
                    self.interface_list_state.select(Some(0));
                }
                self.reset_interface_modal_scroll();
                ViewMode::InterfaceList
            }
            ViewMode::InterfaceList | ViewMode::InterfaceDetail | ViewMode::ProcessDetail => {
                // Switch back to process view
                ViewMode::ProcessView
            }
        };
    }

    pub fn toggle_traffic_view_mode(&mut self) {
        self.traffic_view_mode = match self.traffic_view_mode {
            TrafficViewMode::All => TrafficViewMode::Internet,
            TrafficViewMode::Internet => TrafficViewMode::Local,
            TrafficViewMode::Local => TrafficViewMode::All,
        };

        // Update status message to inform user
        let mode_str = match self.traffic_view_mode {
            TrafficViewMode::All => "All Traffic",
            TrafficViewMode::Internet => "Internet Traffic Only",
            TrafficViewMode::Local => "Local Traffic Only",
        };
        self.status_message = format!("Traffic View: {}", mode_str);
    }

    pub fn select_next_interface(&mut self) {
        if self.interface_list.is_empty() {
            return;
        }

        let new_index = match self.selected_interface_index {
            None => 0,
            Some(idx) => (idx + 1) % self.interface_list.len(),
        };

        self.selected_interface_index = Some(new_index);
        self.interface_list_state.select(Some(new_index));
    }

    pub fn select_previous_interface(&mut self) {
        if self.interface_list.is_empty() {
            return;
        }

        let new_index = match self.selected_interface_index {
            None => 0,
            Some(0) => self.interface_list.len() - 1,
            Some(idx) => idx - 1,
        };

        self.selected_interface_index = Some(new_index);
        self.interface_list_state.select(Some(new_index));
    }

    pub fn get_selected_interface(&self) -> Option<&InterfaceInfo> {
        self.selected_interface_index
            .and_then(|idx| self.interface_list.get(idx))
    }

    pub fn enter_interface_detail(&mut self) {
        if let Some(interface) = self.get_selected_interface() {
            self.selected_interface_name = Some(interface.name.clone());
            self.view_mode = ViewMode::InterfaceDetail;
        }
    }

    pub fn exit_interface_detail(&mut self) {
        self.view_mode = ViewMode::InterfaceList;
        self.selected_interface_name = None;
    }

    /// Check if an interface is in the current filter (i.e., should be shown)
    pub fn is_interface_filtered(&self, interface_name: &str) -> bool {
        match &self.active_interface_filters {
            None => true, // No filter = all shown
            Some(filters) => filters.contains(&interface_name.to_string()),
        }
    }

    /// Toggle a single interface in/out of the filter
    pub fn toggle_interface_filter(&mut self, interface_name: String) {
        match &mut self.active_interface_filters {
            None => {
                // No filter active - create one with just this interface unchecked
                // Collect all OTHER interfaces
                let all_others: Vec<String> = self
                    .interface_list
                    .iter()
                    .filter(|i| i.name != interface_name)
                    .map(|i| i.name.clone())
                    .collect();

                if all_others.is_empty() {
                    self.status_message = "Filter: showing nothing".to_string();
                } else {
                    self.status_message = format!("Filter: hiding {}", interface_name);
                }

                self.active_interface_filters = Some(all_others);
            }
            Some(filters) => {
                if filters.contains(&interface_name) {
                    // Remove from filter (hide this interface)
                    filters.retain(|f| f != &interface_name);

                    if filters.is_empty() {
                        self.status_message = "Filter: showing nothing".to_string();
                    } else {
                        self.status_message = format!("Filter: {}", filters.join(", "));
                    }
                } else {
                    // Add to filter (show this interface)
                    filters.push(interface_name.clone());
                    filters.sort(); // Keep sorted

                    // Check if all interfaces now selected
                    if filters.len() == self.interface_list.len() {
                        self.active_interface_filters = None;
                        self.status_message = "Filter cleared (all selected)".to_string();
                    } else {
                        self.status_message = format!("Filter: {}", filters.join(", "));
                    }
                }
            }
        }
    }

    /// Clear filter (show all)
    pub fn clear_interface_filters(&mut self) {
        self.active_interface_filters = None;
        self.status_message = "Filter cleared - showing all interfaces".to_string();
    }

    /// Set empty filter (show nothing)
    pub fn set_empty_filter(&mut self) {
        self.active_interface_filters = Some(vec![]);
        self.status_message = "Filter: showing no interfaces".to_string();
    }

    /// Toggle between all interfaces selected and none selected
    pub fn toggle_all_interface_filters(&mut self) {
        // Check if all interfaces are currently selected
        let all_selected = match &self.active_interface_filters {
            None => true, // No filter = all selected
            Some(filters) => {
                // All selected if filter contains all interfaces
                filters.len() == self.interface_list.len()
                    && self
                        .interface_list
                        .iter()
                        .all(|iface| filters.contains(&iface.name))
            }
        };

        if all_selected {
            // Deselect all
            self.active_interface_filters = Some(vec![]);
            self.status_message = "Filter: showing no interfaces".to_string();
        } else {
            // Select all
            self.active_interface_filters = None;
            self.status_message = "Filter cleared - showing all interfaces".to_string();
        }
    }

    /// Helper function to get the appropriate rates for sorting based on traffic view mode
    /// Returns (download_rate, total_download, upload_rate, total_upload)
    fn get_sort_rates(&self, process: &ProcessInfo) -> (u64, u64, u64, u64) {
        match self.traffic_view_mode {
            TrafficViewMode::All => (
                process.download_rate,
                process.total_download,
                process.upload_rate,
                process.total_upload,
            ),
            TrafficViewMode::Internet => (
                process.internet_download_rate,
                process.internet_total_download,
                process.internet_upload_rate,
                process.internet_total_upload,
            ),
            TrafficViewMode::Local => (
                process.local_download_rate,
                process.local_total_download,
                process.local_upload_rate,
                process.local_upload_rate,
            ),
        }
    }

    pub fn update_processes(&mut self, process_map: ProcessMap) {
        let mut processes: Vec<ProcessInfo>;

        // Sort first (applies to both filtered and unfiltered lists)
        if self.sort_frozen {
            // FREEZE MODE: Use frozen snapshot, only update stats

            // Start with the frozen snapshot
            let mut frozen_processes = self.frozen_process_snapshot.clone();

            // Update existing processes with new data from process_map
            for frozen_proc in &mut frozen_processes {
                if let Some(updated_proc) = process_map.get(&frozen_proc.pid) {
                    // Process still exists - update its stats
                    frozen_proc.download_rate = updated_proc.download_rate;
                    frozen_proc.upload_rate = updated_proc.upload_rate;
                    frozen_proc.total_download = updated_proc.total_download;
                    frozen_proc.total_upload = updated_proc.total_upload;
                    frozen_proc.internet_download_rate = updated_proc.internet_download_rate;
                    frozen_proc.internet_upload_rate = updated_proc.internet_upload_rate;
                    frozen_proc.internet_total_download = updated_proc.internet_total_download;
                    frozen_proc.internet_total_upload = updated_proc.internet_total_upload;
                    frozen_proc.local_download_rate = updated_proc.local_download_rate;
                    frozen_proc.local_upload_rate = updated_proc.local_upload_rate;
                    frozen_proc.local_total_download = updated_proc.local_total_download;
                    frozen_proc.local_total_upload = updated_proc.local_total_upload;
                    frozen_proc.throttle_limit = updated_proc.throttle_limit.clone();
                    frozen_proc.interface_stats = updated_proc.interface_stats.clone();
                    frozen_proc.connections = updated_proc.connections.clone();
                    frozen_proc.is_terminated = false; // Still running
                } else {
                    // Process no longer exists - mark as terminated but keep in list
                    frozen_proc.is_terminated = true;
                    frozen_proc.download_rate = 0;
                    frozen_proc.upload_rate = 0;
                    frozen_proc.internet_download_rate = 0;
                    frozen_proc.internet_upload_rate = 0;
                    frozen_proc.local_download_rate = 0;
                    frozen_proc.local_upload_rate = 0;
                }
            }

            // Find and add any NEW processes (not in snapshot) to the end
            use std::collections::HashSet;
            let frozen_pids: HashSet<i32> = frozen_processes.iter().map(|p| p.pid).collect();
            let mut new_processes: Vec<ProcessInfo> = process_map
                .into_values()
                .filter(|p| !frozen_pids.contains(&p.pid))
                .collect();

            // Add new processes to frozen_order map
            let current_max = self.frozen_order.values().max().copied().unwrap_or(0);
            let mut next_pos = current_max + 1;
            for process in &new_processes {
                self.frozen_order.insert(process.pid, next_pos);
                next_pos += 1;
            }

            // Append new processes to the end
            frozen_processes.append(&mut new_processes);

            // Update the frozen snapshot
            self.frozen_process_snapshot = frozen_processes.clone();

            processes = frozen_processes;
        } else {
            // NORMAL MODE: Use fresh data from process_map
            processes = process_map.into_values().collect();
            // NORMAL MODE: Deterministic multi-level sort to prevent UI jumping
            // Priority: terminated status -> DL rate -> total DL -> UL rate -> total UL -> throttle status -> name -> PID
            processes.sort_by(|a, b| {
                use std::cmp::Ordering;

                // 1. Terminated processes always go to bottom
                match (a.is_terminated, b.is_terminated) {
                    (true, false) => return Ordering::Greater, // a terminated, b active -> a goes after b
                    (false, true) => return Ordering::Less, // a active, b terminated -> a goes before b
                    _ => {} // Both same state, continue to next criteria
                }

                // Get rates based on current traffic view mode
                let (a_dl_rate, a_dl_total, a_ul_rate, a_ul_total) = self.get_sort_rates(a);
                let (b_dl_rate, b_dl_total, b_ul_rate, b_ul_total) = self.get_sort_rates(b);

                // 2. Download rate (descending - higher rates first)
                match b_dl_rate.cmp(&a_dl_rate) {
                    Ordering::Equal => {} // Continue to next criteria
                    other => return other,
                }

                // 3. Total download (descending - higher totals first)
                match b_dl_total.cmp(&a_dl_total) {
                    Ordering::Equal => {}
                    other => return other,
                }

                // 4. Upload rate (descending - higher rates first)
                match b_ul_rate.cmp(&a_ul_rate) {
                    Ordering::Equal => {}
                    other => return other,
                }

                // 5. Total upload (descending - higher totals first)
                match b_ul_total.cmp(&a_ul_total) {
                    Ordering::Equal => {}
                    other => return other,
                }

                // 6. Throttle status (throttled processes first for visibility)
                match (a.is_throttled(), b.is_throttled()) {
                    (true, false) => return Ordering::Less, // a throttled, b not -> a goes first
                    (false, true) => return Ordering::Greater, // a not throttled, b is -> b goes first
                    _ => {}                                    // Both same throttle state, continue
                }

                // 7. Process name (alphabetical)
                match a.name.cmp(&b.name) {
                    Ordering::Equal => {}
                    other => return other,
                }

                // 8. PID (ascending - smaller PIDs first for determinism)
                a.pid.cmp(&b.pid)
            });
        }

        // Store sorted unfiltered list (for InterfaceDetail view)
        self.unfiltered_process_list = processes.clone();

        // Apply interface filter for ProcessView/InterfaceList views
        processes = self.apply_process_filter(processes);

        self.process_list = processes;

        // Adjust selection if out of bounds
        if let Some(index) = self.selected_index {
            if index >= self.process_list.len() && !self.process_list.is_empty() {
                self.selected_index = Some(self.process_list.len() - 1);
                self.list_state.select(Some(self.process_list.len() - 1));
            }
        }
    }

    pub fn select_next(&mut self) {
        if self.process_list.is_empty() {
            return;
        }

        let new_index = match self.selected_index {
            None => 0, // If nothing selected, select first item
            Some(idx) => (idx + 1) % self.process_list.len(),
        };

        self.selected_index = Some(new_index);
        self.list_state.select(Some(new_index));
    }

    pub fn select_previous(&mut self) {
        if self.process_list.is_empty() {
            return;
        }

        let new_index = match self.selected_index {
            None => 0, // If nothing selected, select first item
            Some(0) => self.process_list.len() - 1,
            Some(idx) => idx - 1,
        };

        self.selected_index = Some(new_index);
        self.list_state.select(Some(new_index));
    }

    pub fn get_selected_process(&self) -> Option<&ProcessInfo> {
        self.selected_index
            .and_then(|idx| self.process_list.get(idx))
    }

    /// Toggle sort freeze mode
    pub fn toggle_sort_freeze(&mut self) {
        self.sort_frozen = !self.sort_frozen;

        if self.sort_frozen {
            // Entering freeze mode - capture current process list as snapshot
            self.frozen_order.clear();
            for (index, process) in self.process_list.iter().enumerate() {
                self.frozen_order.insert(process.pid, index);
            }

            // Store complete snapshot of current process list
            self.frozen_process_snapshot = self.process_list.clone();
        } else {
            // Exiting freeze mode - clear frozen data
            self.frozen_order.clear();
            self.frozen_process_snapshot.clear();
        }
    }

    /// Apply interface filter to process list
    fn apply_process_filter(&self, mut processes: Vec<ProcessInfo>) -> Vec<ProcessInfo> {
        match &self.active_interface_filters {
            None => {
                // No filter - show all processes
                processes
            }
            Some(filters) if filters.is_empty() => {
                // Empty filter - show nothing
                vec![]
            }
            Some(filters) => {
                // Filter to processes using these interfaces
                processes.retain(|proc| {
                    // Keep process if it uses ANY of the filtered interfaces
                    proc.interface_stats
                        .keys()
                        .any(|iface_name| filters.contains(iface_name))
                });
                processes
            }
        }
    }

    /// Build backend items list for interactive backend modal
    pub fn build_backend_items(&mut self, backend_info: &BackendInfo) {
        // Save current selection if any (to preserve cursor position across rebuilds)
        let current_backend_name = self
            .backend_items
            .get(self.backend_selected_index)
            .and_then(|item| match item {
                BackendSelectorItem::Backend { name, .. } => Some(name.clone()),
                _ => None,
            });

        self.backend_items.clear();

        // Socket Mapper group
        if !backend_info.available_socket_mappers.is_empty() {
            self.backend_items
                .push(BackendSelectorItem::GroupHeader(BackendGroup::SocketMapper));
            for (name, priority, available) in &backend_info.available_socket_mappers {
                let is_current = backend_info.active_socket_mapper.as_ref() == Some(name);
                self.backend_items.push(BackendSelectorItem::Backend {
                    name: name.clone(),
                    group: BackendGroup::SocketMapper,
                    priority: *priority,
                    available: *available,
                    is_current_default: is_current,
                });
            }
        }

        // Upload group
        if !backend_info.available_upload.is_empty() {
            self.backend_items
                .push(BackendSelectorItem::GroupHeader(BackendGroup::Upload));
            for (name, priority, available) in &backend_info.available_upload {
                let is_current = backend_info.active_upload.as_ref() == Some(name);
                self.backend_items.push(BackendSelectorItem::Backend {
                    name: name.clone(),
                    group: BackendGroup::Upload,
                    priority: *priority,
                    available: *available,
                    is_current_default: is_current,
                });
            }
        }

        // Download group
        if !backend_info.available_download.is_empty() {
            self.backend_items
                .push(BackendSelectorItem::GroupHeader(BackendGroup::Download));
            for (name, priority, available) in &backend_info.available_download {
                let is_current = backend_info.active_download.as_ref() == Some(name);
                self.backend_items.push(BackendSelectorItem::Backend {
                    name: name.clone(),
                    group: BackendGroup::Download,
                    priority: *priority,
                    available: *available,
                    is_current_default: is_current,
                });
            }
        }

        // Try to restore previous selection (preserves cursor position)
        if let Some(name) = current_backend_name {
            if let Some(index) = self.backend_items.iter().position(
                |item| matches!(item, BackendSelectorItem::Backend { name: n, .. } if n == &name),
            ) {
                self.backend_selected_index = index;
                return; // Found it, keep cursor there!
            }
        }

        // Fallback: find first available backend item and select it
        self.backend_selected_index = self
            .backend_items
            .iter()
            .position(|item| {
                matches!(
                    item,
                    BackendSelectorItem::Backend {
                        available: true,
                        ..
                    }
                )
            })
            .unwrap_or(0);
    }

    pub fn select_next_backend(&mut self) {
        if self.backend_items.is_empty() {
            return;
        }

        // Move down, skip group headers and unavailable backends
        let start_index = self.backend_selected_index;
        loop {
            self.backend_selected_index =
                (self.backend_selected_index + 1) % self.backend_items.len();

            if let Some(BackendSelectorItem::Backend { available, .. }) =
                self.backend_items.get(self.backend_selected_index)
            {
                if *available {
                    break;
                }
            }

            // Prevent infinite loop if no available backends
            if self.backend_selected_index == start_index {
                break;
            }
        }
    }

    pub fn select_previous_backend(&mut self) {
        if self.backend_items.is_empty() {
            return;
        }

        // Move up, skip group headers and unavailable backends
        let start_index = self.backend_selected_index;
        loop {
            self.backend_selected_index = if self.backend_selected_index == 0 {
                self.backend_items.len() - 1
            } else {
                self.backend_selected_index - 1
            };

            if let Some(BackendSelectorItem::Backend { available, .. }) =
                self.backend_items.get(self.backend_selected_index)
            {
                if *available {
                    break;
                }
            }

            // Prevent infinite loop if no available backends
            if self.backend_selected_index == start_index {
                break;
            }
        }
    }

    /// Get the currently selected backend (for immediate apply)
    pub fn get_selected_backend(&self) -> Option<(&str, BackendGroup)> {
        if let Some(BackendSelectorItem::Backend {
            name,
            group,
            available,
            ..
        }) = self.backend_items.get(self.backend_selected_index)
        {
            if *available {
                return Some((name.as_str(), *group));
            }
        }
        None
    }

    // Process detail view navigation methods

    /// Enter process detail view for the selected process
    pub fn enter_process_detail(&mut self) {
        if let Some(process) = self.get_selected_process() {
            self.selected_process_detail_pid = Some(process.pid);
            self.detail_scroll_offset = 0;
            self.detail_tab = ProcessDetailTab::Overview;
            self.view_mode = ViewMode::ProcessDetail;
        }
    }

    /// Exit process detail view and return to process list
    pub fn exit_process_detail(&mut self) {
        self.view_mode = ViewMode::ProcessView;
        self.selected_process_detail_pid = None;
        self.detail_scroll_offset = 0;
    }

    /// Move to the next detail tab
    pub fn next_detail_tab(&mut self) {
        self.detail_tab = match self.detail_tab {
            ProcessDetailTab::Overview => ProcessDetailTab::Connections,
            ProcessDetailTab::Connections => ProcessDetailTab::Traffic,
            ProcessDetailTab::Traffic => ProcessDetailTab::System,
            ProcessDetailTab::System => ProcessDetailTab::Overview,
        };
        self.detail_scroll_offset = 0; // Reset scroll when changing tabs
    }

    /// Move to the previous detail tab
    pub fn previous_detail_tab(&mut self) {
        self.detail_tab = match self.detail_tab {
            ProcessDetailTab::Overview => ProcessDetailTab::System,
            ProcessDetailTab::Connections => ProcessDetailTab::Overview,
            ProcessDetailTab::Traffic => ProcessDetailTab::Connections,
            ProcessDetailTab::System => ProcessDetailTab::Traffic,
        };
        self.detail_scroll_offset = 0; // Reset scroll when changing tabs
    }

    /// Scroll detail view up
    pub fn scroll_detail_up(&mut self) {
        self.detail_scroll_offset = self.detail_scroll_offset.saturating_sub(1);
    }

    /// Scroll detail view down
    pub fn scroll_detail_down(&mut self) {
        self.detail_scroll_offset = self.detail_scroll_offset.saturating_add(1);
    }

    /// Get the process being detailed (if still exists in process list)
    pub fn get_detail_process(&self) -> Option<&ProcessInfo> {
        if let Some(pid) = self.selected_process_detail_pid {
            self.process_list.iter().find(|p| p.pid == pid)
        } else {
            None
        }
    }

    // Modal scroll methods

    /// Reset help overlay scroll when opened
    pub fn reset_help_scroll(&mut self) {
        self.help_scroll_offset = 0;
    }

    /// Scroll help overlay up
    pub fn scroll_help_up(&mut self) {
        self.help_scroll_offset = self.help_scroll_offset.saturating_sub(1);
    }

    /// Scroll help overlay down
    pub fn scroll_help_down(&mut self) {
        self.help_scroll_offset = self.help_scroll_offset.saturating_add(1);
    }

    /// Reset backend info scroll when opened
    pub fn reset_backend_info_scroll(&mut self) {
        self.backend_info_scroll_offset = 0;
    }

    /// Scroll backend info modal up
    pub fn scroll_backend_info_up(&mut self) {
        self.backend_info_scroll_offset = self.backend_info_scroll_offset.saturating_sub(1);
    }

    /// Scroll backend info modal down
    pub fn scroll_backend_info_down(&mut self) {
        self.backend_info_scroll_offset = self.backend_info_scroll_offset.saturating_add(1);
    }

    /// Reset interface modal scroll when opened
    pub fn reset_interface_modal_scroll(&mut self) {
        self.interface_modal_scroll_offset = 0;
    }

    /// Scroll interface modal up
    pub fn scroll_interface_modal_up(&mut self) {
        self.interface_modal_scroll_offset = self.interface_modal_scroll_offset.saturating_sub(1);
    }

    /// Scroll interface modal down
    pub fn scroll_interface_modal_down(&mut self) {
        self.interface_modal_scroll_offset = self.interface_modal_scroll_offset.saturating_add(1);
    }

    /// Reset backend compatibility dialog scroll when opened
    pub fn reset_backend_compat_scroll(&mut self) {
        self.backend_compat_scroll_offset = 0;
    }

    /// Scroll backend compatibility dialog up
    pub fn scroll_backend_compat_up(&mut self) {
        self.backend_compat_scroll_offset = self.backend_compat_scroll_offset.saturating_sub(1);
    }

    /// Scroll backend compatibility dialog down
    pub fn scroll_backend_compat_down(&mut self) {
        self.backend_compat_scroll_offset = self.backend_compat_scroll_offset.saturating_add(1);
    }

    /// Clamp scroll offset to content bounds
    /// content_lines: Total number of lines in content
    /// visible_height: Height of visible area (including borders)
    pub fn clamp_scroll(scroll_offset: usize, content_lines: usize, visible_height: u16) -> usize {
        // Account for borders (top + bottom = 2) and padding
        let usable_height = visible_height.saturating_sub(3) as usize;
        let max_scroll = content_lines.saturating_sub(usable_height);
        scroll_offset.min(max_scroll)
    }

    /// Adjust scroll offset to ensure a specific line is visible
    /// Returns the adjusted scroll offset
    pub fn scroll_to_line(current_scroll: usize, target_line: usize, visible_height: u16) -> usize {
        let usable_height = visible_height.saturating_sub(3) as usize;

        // If target is above visible area, scroll up to show it
        if target_line < current_scroll {
            return target_line;
        }

        // If target is below visible area, scroll down to show it
        let last_visible_line = current_scroll + usable_height.saturating_sub(1);
        if target_line > last_visible_line {
            return target_line.saturating_sub(usable_height.saturating_sub(1));
        }

        // Target is already visible
        current_scroll
    }
}

pub fn draw_ui(f: &mut PaintCx<'_, '_>, app: &mut AppState) {
    draw_ui_with_backend_info(f, app, None);
}

pub fn draw_ui_with_backend_info(
    f: &mut PaintCx<'_, '_>,
    app: &mut AppState,
    backend_info: Option<&BackendInfo>,
) {
    app.component_scene.borrow_mut().begin();
    // Clear clickable regions from previous frame
    app.clickable_regions.clear();

    let chunks = split_vertical(root_area(f), 3, 5);

    // Header (hide in ProcessDetail view to save space)
    if app.view_mode != ViewMode::ProcessDetail {
        draw_header(f, chunks[0], app);
    }

    // Main content area - render based on view mode
    match app.view_mode {
        ViewMode::ProcessView => {
            draw_process_list(f, chunks[1], app);
        }
        ViewMode::InterfaceList => {
            // Draw process list in background so we can see live updates
            draw_process_list(f, chunks[1], app);
            // Draw interface modal overlay on top
            draw_interface_modal(f, root_area(f), app);
        }
        ViewMode::InterfaceDetail => {
            draw_interface_detail(f, chunks[1], app);
        }
        ViewMode::ProcessDetail => {
            // Use full area including header space for detail view
            let detail_area = Rect {
                x: chunks[0].x,
                y: chunks[0].y,
                width: chunks[0].width,
                height: chunks[0].height + chunks[1].height,
            };
            draw_process_detail(f, detail_area, app);
        }
    }

    // Status bar
    draw_status_bar(f, chunks[2], app);

    // Help overlay
    if app.show_help {
        draw_help_overlay(f, root_area(f), app);
    }

    // Throttle dialog
    if app.show_throttle_dialog {
        draw_throttle_dialog(f, root_area(f), app);
    }

    // Bandwidth graph overlay
    if app.show_graph {
        draw_bandwidth_graph(f, root_area(f), app);
    }

    // Backend info modal (highest priority, renders on top)
    if app.show_backend_info {
        if let Some(info) = backend_info {
            draw_backend_info(f, root_area(f), app, info);
        }
    }

    // Backend compatibility dialog (highest priority - renders on top of everything)
    if app.show_backend_compatibility_dialog {
        if let Some(dialog) = app.backend_compatibility_dialog.clone() {
            draw_backend_compatibility_dialog(f, root_area(f), app, &dialog);
        }
    }
}

fn draw_header(f: &mut PaintCx<'_, '_>, area: Rect, app: &AppState) {
    let header = (
        render::lines("🔥 bproc v0.1.0 - Network Monitor & Throttler 🔥"),
        TextPresentation {
            style: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            chrome: Some(PanelChrome::default()),
            ..TextPresentation::default()
        },
    );

    paint_text(&app.component_scene, "draw_header", f, area, header);
}

fn draw_process_list(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    // Select which rates to display based on traffic view mode
    let get_rates = |proc: &ProcessInfo| -> (u64, u64, u64, u64) {
        match app.traffic_view_mode {
            TrafficViewMode::All => (
                proc.download_rate,
                proc.upload_rate,
                proc.total_download,
                proc.total_upload,
            ),
            TrafficViewMode::Internet => (
                proc.internet_download_rate,
                proc.internet_upload_rate,
                proc.internet_total_download,
                proc.internet_total_upload,
            ),
            TrafficViewMode::Local => (
                proc.local_download_rate,
                proc.local_upload_rate,
                proc.local_total_download,
                proc.local_total_upload,
            ),
        }
    };

    let items: Vec<bmux_tui_components::selectable_list::SelectableListItem> = app
        .process_list
        .iter()
        .enumerate()
        .map(|(index, proc)| {
            // Determine status indicator: throttled (⚡), terminated (💀), or nothing
            let status_indicator = if proc.is_throttled() {
                "⚡"
            } else if proc.is_terminated {
                "💀"
            } else {
                " "
            };

            // Manual selection indicator - always present for consistent alignment
            let selection_indicator = if Some(index) == app.list_state.selected() {
                "▶ "
            } else {
                "  "
            };

            // Use gray colors for terminated processes
            let terminated_color = Color::White;

            let name_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::BrightWhite
            };
            let dl_rate_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Green
            };
            let ul_rate_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Yellow
            };
            let dl_total_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Cyan
            };
            let ul_total_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Magenta
            };
            let status_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Red
            };

            // Get the appropriate rates based on traffic view mode
            let (download_rate, upload_rate, total_download, total_upload) = get_rates(proc);

            let content = line(vec![
                Span::styled(selection_indicator, Style::default().fg(Color::Yellow)),
                Span::raw(format!("{:7} ", proc.pid)),
                Span::styled(
                    format!(
                        "{:20} ",
                        if display_width(&proc.name) > 20 {
                            format!(
                                "{}...",
                                &bmux_tui_components::compact::truncate_width(&proc.name, 17)
                            )
                        } else {
                            proc.name.clone()
                        }
                    ),
                    Style::default().fg(name_color),
                ),
                Span::styled(
                    format!("↓{:>10} ", ProcessInfo::format_rate(download_rate)),
                    Style::default().fg(dl_rate_color),
                ),
                Span::styled(
                    format!("↑{:>10} ", ProcessInfo::format_rate(upload_rate)),
                    Style::default().fg(ul_rate_color),
                ),
                Span::styled(
                    format!("{:>10} ", ProcessInfo::format_bytes(total_download)),
                    Style::default().fg(dl_total_color),
                ),
                Span::styled(
                    format!("{:>10} ", ProcessInfo::format_bytes(total_upload)),
                    Style::default().fg(ul_total_color),
                ),
                Span::styled(
                    status_indicator,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);

            bmux_tui_components::selectable_list::SelectableListItem::rich(
                content.plain_text(),
                content,
            )
        })
        .collect();

    let header = line(vec![
        Span::styled("PID     ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            "Process              ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("DL Rate    ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("UL Rate    ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("Total DL   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("Total UL   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("Status", Style::default().add_modifier(Modifier::BOLD)),
    ]);

    // Split the area: header takes first row inside border, list gets the rest
    let header_area = Rect {
        x: area.x + 4, // +2 for border, +2 for manual selection indicator
        y: area.y + 1,
        width: area.width.saturating_sub(5),
        height: 1,
    };

    let list_area = Rect {
        x: area.x,
        y: area.y + 2, // Start below the header
        width: area.width,
        height: area.height.saturating_sub(2),
    };

    // Render the border and title separately
    let title = if app.sort_frozen {
        "Network Activity [FROZEN ❄️]"
    } else {
        "Network Activity"
    };

    let border = PanelChrome {
        title: Some(line(title)),
        ..PanelChrome::default()
    };
    paint_border(f, area, border);

    // Render header
    paint_text(
        &app.component_scene,
        "draw_process_list.header",
        f,
        header_area,
        (render::lines(header), TextPresentation::default()),
    );

    // Render list without its own border (since we drew it above)
    let list = (
        items,
        Style::default()
            .bg(Color::BrightBlack)
            .add_modifier(Modifier::BOLD),
    );

    // Adjust list_state rendering to account for the inner area (inside borders)
    let inner_list_area = Rect {
        x: list_area.x + 1,
        y: list_area.y,
        width: list_area.width.saturating_sub(2),
        height: list_area.height.saturating_sub(1),
    };

    paint_list(
        &app.component_scene,
        "draw_process_list",
        f,
        inner_list_area,
        &list.0,
        list.1,
        &mut app.list_state,
    );

    // Store clickable region for mouse selection
    let first_visible = app.list_state.offset();
    let visible_count = inner_list_area.height.saturating_sub(1) as usize; // -1 for potential scrollbar
    app.clickable_regions.push(ClickableRegion {
        area: inner_list_area,
        region_type: ClickableRegionType::ProcessList {
            first_visible_index: first_visible,
            visible_count,
        },
    });
}

/// Wrap spans into multiple lines based on available width
/// This ensures the status bar doesn't get truncated on narrow terminals
/// Uses lookahead to keep related spans (like [key] description) together
fn wrap_spans_to_lines(spans: Vec<Span>, max_width: u16) -> Vec<Line> {
    let mut lines = vec![];
    let mut current_line = vec![];
    let mut current_width = 0;
    let mut i = 0;

    while i < spans.len() {
        let span = &spans[i];
        let span_width = display_width(&span.content) as u16;

        // LOOKAHEAD: Check if this is a styled span (yellow key like "[b]")
        // If so, calculate combined width with the next span (description)
        let lookahead_width = if span.style.fg == Some(Color::Yellow) && i + 1 < spans.len() {
            // This is a yellow key span, check next span (likely the description)
            let next_span = &spans[i + 1];
            span_width + display_width(&next_span.content) as u16
        } else {
            // Not a key, just use this span's width
            span_width
        };

        // If adding this span (and its paired span if applicable) would overflow, wrap
        if current_width + lookahead_width > max_width && !current_line.is_empty() {
            lines.push(line(std::mem::take(&mut current_line)));
            current_width = 0;
        }

        // Add the current span
        current_width += span_width;
        current_line.push(span.clone());
        i += 1;
    }

    // Add remaining spans as the last line
    if !current_line.is_empty() {
        lines.push(line(current_line));
    }

    // Return at least one empty line if no content
    if lines.is_empty() {
        lines.push(line(""));
    }

    lines
}

fn draw_status_bar(f: &mut PaintCx<'_, '_>, area: Rect, app: &AppState) {
    // Auto-generate status bar from centralized keybindings
    let mut spans = vec![];

    for (i, (key, description)) in crate::keybindings::get_status_bar_keybindings()
        .iter()
        .enumerate()
    {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("[{}]", key),
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::raw(format!(" {}  ", description)));
    }

    spans.push(Span::raw("|  "));

    // Show traffic view mode
    let traffic_mode_text = match app.traffic_view_mode {
        TrafficViewMode::All => "All",
        TrafficViewMode::Internet => "Internet",
        TrafficViewMode::Local => "Local",
    };
    let traffic_mode_icon = match app.traffic_view_mode {
        TrafficViewMode::All => "🌐",
        TrafficViewMode::Internet => "🌐",
        TrafficViewMode::Local => "🏠",
    };
    spans.push(Span::styled(
        format!("{} {} ", traffic_mode_icon, traffic_mode_text),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("| "));

    // Show filter status
    match &app.active_interface_filters {
        None => {
            // No filter - show normal message
            spans.push(Span::styled(
                &app.status_message,
                Style::default().fg(Color::White),
            ));
        }
        Some(filters) if filters.is_empty() => {
            // Empty filter
            spans.push(Span::styled(
                "FILTER: None (showing 0 processes) | ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                &app.status_message,
                Style::default().fg(Color::White),
            ));
        }
        Some(filters) => {
            // Active filter
            let filter_text = if filters.len() <= 3 {
                format!("FILTER: {} | ", filters.join(", "))
            } else {
                format!(
                    "FILTER: {} +{} more | ",
                    filters[..2].join(", "),
                    filters.len() - 2
                )
            };
            spans.push(Span::styled(
                filter_text,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                &app.status_message,
                Style::default().fg(Color::White),
            ));
        }
    }

    // Wrap spans to multiple lines if they exceed terminal width
    let available_width = area.width.saturating_sub(2); // minus left/right borders
    let wrapped_lines = wrap_spans_to_lines(spans, available_width);
    let status = (
        render::lines(wrapped_lines),
        TextPresentation {
            chrome: Some(PanelChrome::default()),
            ..TextPresentation::default()
        },
    );

    paint_text(&app.component_scene, "draw_status_bar", f, area, status);
}

fn draw_help_overlay(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    // Auto-generate help text from centralized keybindings
    let mut help_text = vec![
        line(""),
        line(Span::styled(
            "bproc - Keyboard Shortcuts",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        line(""),
    ];

    // Get all keybindings and generate help lines
    for binding in crate::keybindings::get_all_keybindings() {
        help_text.push(line(format!(
            "  {:12} - {}",
            binding.key, binding.description
        )));
    }

    help_text.push(line(""));
    help_text.push(line("Use ↑↓ to scroll, any other key to close"));

    let help_area = centered_rect(60, 50, area);

    // Clamp scroll offset to content bounds
    let content_lines = help_text.len();
    let clamped_scroll =
        AppState::clamp_scroll(app.help_scroll_offset, content_lines, help_area.height);
    app.help_scroll_offset = clamped_scroll;

    let help = (
        render::lines(help_text),
        TextPresentation {
            style: Style::default().bg(Color::Black).fg(Color::BrightWhite),
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("Help")),
                style: Style::default().fg(Color::Cyan),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );

    clear(f, help_area);
    paint_text(&app.component_scene, "help", f, help_area, help);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    // Match the original three-percentage layout by rounding the cumulative
    // boundaries, not by flooring the modal size independently (24×30%=8).
    fn axis(length: u16, percent: u16) -> (u16, u16) {
        let side = (100 - percent.min(100)) / 2;
        let start = (u32::from(length) * u32::from(side) + 50) / 100;
        let end = (u32::from(length) * u32::from(side + percent.min(100)) + 50) / 100;
        (start as u16, end.saturating_sub(start) as u16)
    }
    let (x, width) = axis(r.width, percent_x);
    let (y, height) = axis(r.height, percent_y);
    Rect::new(r.x.saturating_add(x), r.y.saturating_add(y), width, height)
}

fn draw_throttle_dialog(f: &mut PaintCx<'_, '_>, area: Rect, app: &AppState) {
    let dialog = &app.throttle_dialog;

    let title = if let (Some(pid), Some(name)) = (dialog.target_pid, &dialog.target_name) {
        format!("Throttle: {} (PID {})", name, pid)
    } else {
        "Throttle Process".to_string()
    };

    let download_style = if dialog.selected_field == ThrottleField::Download {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::BrightWhite)
    };

    let upload_style = if dialog.selected_field == ThrottleField::Upload {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::BrightWhite)
    };

    let traffic_type = dialog.get_traffic_type();
    let traffic_type_display = match traffic_type {
        crate::process::TrafficType::All => "All Traffic",
        crate::process::TrafficType::Internet => "Internet Only",
        crate::process::TrafficType::Local => "Local Only",
    };

    let dialog_text = vec![
        line(""),
        line(vec![
            Span::styled("Download Limit (KB/s): ", download_style),
            Span::styled(
                if dialog.download_input.is_empty() {
                    "unlimited"
                } else {
                    &dialog.download_input
                },
                download_style,
            ),
        ]),
        line(""),
        line(vec![
            Span::styled("Upload Limit (KB/s):   ", upload_style),
            Span::styled(
                if dialog.upload_input.is_empty() {
                    "unlimited"
                } else {
                    &dialog.upload_input
                },
                upload_style,
            ),
        ]),
        line(""),
        line(vec![
            Span::styled(
                "Traffic Type:          ",
                Style::default().fg(Color::BrightWhite),
            ),
            Span::styled(
                traffic_type_display,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        line(""),
        line(Span::styled(
            "[Tab] Switch field  [t] Cycle traffic type  [Enter] Apply  [Esc] Cancel",
            Style::default().fg(Color::BrightBlack),
        )),
    ];

    let dialog_widget = (
        render::lines(dialog_text),
        TextPresentation {
            style: Style::default().bg(Color::Black).fg(Color::BrightWhite),
            chrome: Some(PanelChrome {
                title: Some(line(title)),
                style: Style::default().fg(Color::Cyan),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );

    let dialog_area = centered_rect(60, 30, area);
    clear(f, dialog_area);
    paint_text(
        &app.component_scene,
        "draw_throttle_dialog",
        f,
        dialog_area,
        dialog_widget,
    );
    let field_x = dialog_area.x.saturating_add(24);
    let field_width = dialog_area.width.saturating_sub(25);
    for (row, value, selected, style) in [
        (
            2u16,
            &dialog.download_input,
            dialog.selected_field == ThrottleField::Download,
            download_style,
        ),
        (
            4u16,
            &dialog.upload_input,
            dialog.selected_field == ThrottleField::Upload,
            upload_style,
        ),
    ] {
        if row < dialog_area.height.saturating_sub(1) {
            render::paint_input(
                &app.component_scene,
                if row == 2 {
                    "throttle.download"
                } else {
                    "throttle.upload"
                },
                f,
                Rect::new(field_x, dialog_area.y.saturating_add(row), field_width, 1),
                value,
                selected,
                style,
            );
        }
    }
}

fn draw_backend_compatibility_dialog(
    f: &mut PaintCx<'_, '_>,
    area: Rect,
    app: &mut AppState,
    dialog: &BackendCompatibilityDialog,
) {
    // Build option list
    let mut options = vec!["Cancel - don't apply throttle".to_string()];

    for backend in &dialog.compatible_backends {
        options.push(format!("Switch to '{}' for this throttle only", backend));
        options.push(format!("Switch to '{}' and make it default", backend));
    }

    options.push("Apply as 'All Traffic' instead".to_string());

    // Build styled lines with radio buttons
    let mut lines = vec![
        line(""),
        line(format!(
            "{} {} backend '{}' does not support '{:?}' traffic filtering.",
            if dialog.compatible_backends.is_empty() {
                "No available"
            } else {
                "Current"
            },
            if dialog.is_upload {
                "upload"
            } else {
                "download"
            },
            dialog.current_backend,
            dialog.traffic_type
        )),
        line(""),
        line(if dialog.compatible_backends.is_empty() {
            "No backends on this system support IP-based traffic filtering."
        } else {
            "Only 'All Traffic' throttling is supported by this backend."
        }),
        line(""),
        line(Span::styled(
            "What would you like to do?",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        line(""),
    ];

    for (i, option) in options.iter().enumerate() {
        let radio = if i == dialog.selected_action {
            "●"
        } else {
            "○"
        };
        let style = if i == dialog.selected_action {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::BrightWhite)
        };

        lines.push(line(vec![
            Span::raw("  "),
            Span::styled(radio, style),
            Span::raw(" "),
            Span::styled(option, style),
        ]));
    }

    lines.push(line(""));
    lines.push(line(""));
    lines.push(line(Span::styled(
        "[Enter] Confirm  [↑↓] Navigate  [Esc/q] Cancel",
        Style::default().fg(Color::BrightBlack),
    )));

    let dialog_area = centered_rect(80, 50, area);

    // Clamp scroll offset to content bounds
    let content_lines = lines.len();
    let clamped_scroll = AppState::clamp_scroll(
        app.backend_compat_scroll_offset,
        content_lines,
        dialog_area.height,
    );
    app.backend_compat_scroll_offset = clamped_scroll;

    // Render paragraph
    let paragraph = (
        render::lines(lines),
        TextPresentation {
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("Backend Incompatibility (↑↓ to scroll)")),
                style: Style::default().fg(Color::Red),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );

    clear(f, dialog_area);
    paint_text(
        &app.component_scene,
        "compatibility",
        f,
        dialog_area,
        paragraph,
    );
}

fn draw_bandwidth_graph(f: &mut PaintCx<'_, '_>, area: Rect, app: &AppState) {
    // Get selected process
    let selected_proc = app.get_selected_process();
    if selected_proc.is_none() {
        return;
    }

    let proc = selected_proc.unwrap();
    let history = app.history.get_history(proc.pid);

    if history.is_none() || history.unwrap().samples.is_empty() {
        // No history data available
        let no_data = (
            render::lines("No historical data available yet..."),
            TextPresentation {
                style: Style::default().bg(Color::Black).fg(Color::Yellow),
                chrome: Some(PanelChrome {
                    title: Some(line(format!(
                        "Bandwidth Graph: {} (PID {})",
                        proc.name, proc.pid
                    ))),
                    style: Style::default().fg(Color::Cyan),
                    ..PanelChrome::default()
                }),
                ..TextPresentation::default()
            },
        );

        let graph_area = centered_rect(80, 60, area);
        clear(f, graph_area);
        paint_text(
            &app.component_scene,
            "draw_bandwidth_graph",
            f,
            graph_area,
            no_data,
        );
        return;
    }

    let history = history.unwrap();
    let (download_data, upload_data) = history.get_graph_data();

    // Find max values for scaling
    let max_download = history.max_download_rate() as f64;
    let max_upload = history.max_upload_rate() as f64;
    let max_value = max_download.max(max_upload).max(1.0); // Avoid division by zero

    let graph_area = centered_rect(90, 70, area);
    clear(f, graph_area);
    render::paint_bandwidth_graph(
        f,
        graph_area,
        &download_data,
        &upload_data,
        max_value * 1.1,
        format!(
            "Bandwidth Graph: {} (PID {}) | Max: ↓{} ↑{} | Avg: ↓{} ↑{}",
            proc.name,
            proc.pid,
            ProcessInfo::format_rate(history.max_download_rate()),
            ProcessInfo::format_rate(history.max_upload_rate()),
            ProcessInfo::format_rate(history.avg_download_rate()),
            ProcessInfo::format_rate(history.avg_upload_rate())
        ),
    );

    // Draw instructions at bottom
    let inst_area = Rect {
        x: graph_area.x,
        y: graph_area.y + graph_area.height.saturating_sub(2),
        width: graph_area.width,
        height: 1,
    };
    let message = "Press 'g', 'q', or 'Esc' to close graph";
    let width = u16::try_from(display_width(message))
        .unwrap_or(u16::MAX)
        .min(inst_area.width);
    render::paint_label(
        f,
        Rect::new(
            inst_area.x.saturating_add((inst_area.width - width) / 2),
            inst_area.y,
            width,
            1,
        ),
        line(message),
        Style::new().fg(Color::BrightBlack),
    );
}

fn draw_backend_info(
    f: &mut PaintCx<'_, '_>,
    area: Rect,
    app: &mut AppState,
    backend_info: &BackendInfo,
) {
    let mut text = vec![line("")];

    text.push(line(Span::styled(
        "bproc - Backends",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    text.push(line(""));

    // Show currently active monitoring backend
    if let Some(monitoring_backend) = &backend_info.active_monitoring {
        text.push(line(vec![
            Span::styled(
                "📡 Monitoring: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                monitoring_backend,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" (packet capture)", Style::default().fg(Color::BrightBlack)),
        ]));
    }

    if let Some(socket_mapper) = &backend_info.active_socket_mapper {
        text.push(line(vec![
            Span::styled(
                "🔌 Socket Mapper: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                socket_mapper,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" (PID mapping)", Style::default().fg(Color::BrightBlack)),
        ]));
    }

    text.push(line(""));

    // Get backend stats from backend_info
    let backend_stats = &backend_info.backend_stats;

    // Track line numbers for auto-scroll and click handling
    let mut current_line = text.len();
    let mut selected_line: Option<usize> = None;
    let mut line_to_item: HashMap<usize, usize> = HashMap::new();

    // Render all items (group headers and backends) with radio buttons
    for (index, item) in app.backend_items.iter().enumerate() {
        match item {
            BackendSelectorItem::GroupHeader(group) => {
                // Add spacing before groups (except first)
                if index > 0 {
                    text.push(line(""));
                    current_line += 1;
                }

                let header = match group {
                    BackendGroup::SocketMapper => "Socket Mapper Backends:",
                    BackendGroup::Upload => "Upload Backends:",
                    BackendGroup::Download => "Download Backends:",
                };
                text.push(line(Span::styled(
                    header,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                current_line += 1;
            }
            BackendSelectorItem::Backend {
                name,
                group: _,
                priority,
                available,
                is_current_default,
            } => {
                let is_selected = index == app.backend_selected_index;

                // Map this line to the backend item index for click handling
                line_to_item.insert(current_line, index);

                // Track this line if it's selected
                if is_selected {
                    selected_line = Some(current_line);
                }

                // Radio button shows active backend (not pending, since Space applies immediately)
                let is_active = *is_current_default;
                let radio = if is_active { "◉" } else { "○" };
                let radio_style = if is_active {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::BrightBlack)
                };

                // Status indicator
                let (status_symbol, status_color) = if *is_current_default {
                    ("⭐", Color::Yellow)
                } else if *available {
                    ("✅", Color::Green)
                } else {
                    ("❌", Color::Red)
                };

                // Name style
                let name_style = if is_selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                        .bg(Color::BrightBlack)
                } else if !available {
                    Style::default().fg(Color::BrightWhite)
                } else {
                    Style::default().fg(Color::BrightWhite)
                };

                let priority_str = format!("{:?}", priority);
                let throttle_count = backend_stats.get(name).copied().unwrap_or(0);
                let throttle_info = if throttle_count > 0 {
                    format!(" ({} active)", throttle_count)
                } else {
                    String::new()
                };

                let mut line_spans = vec![
                    Span::raw("  "),
                    Span::styled(radio, radio_style),
                    Span::raw(" "),
                    Span::styled(format!("{:18}", name), name_style),
                    Span::styled(
                        format!(" [{:8}]", priority_str),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw("  "),
                    Span::styled(status_symbol, Style::default().fg(status_color)),
                ];

                // Add status text
                if *is_current_default {
                    line_spans.push(Span::styled(" ACTIVE", Style::default().fg(Color::Yellow)));
                } else if !available {
                    line_spans.push(Span::styled(
                        " (unavailable)",
                        Style::default().fg(Color::BrightWhite),
                    ));
                }

                // Add throttle info if available
                if !throttle_info.is_empty() {
                    line_spans.push(Span::styled(
                        throttle_info,
                        Style::default().fg(Color::BrightWhite),
                    ));
                }

                text.push(line(line_spans));
                current_line += 1;
            }
        }
    }

    text.push(line(""));
    text.push(line(""));

    // Upload Backends Section
    text.push(line(Span::styled(
        "Upload Backends:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    if backend_info.available_upload.is_empty() {
        text.push(line(Span::styled(
            "  ⚪ (none compiled)",
            Style::default().fg(Color::BrightWhite),
        )));
    } else {
        for (name, priority, available) in &backend_info.available_upload {
            let is_active = backend_info.active_upload.as_ref() == Some(name);
            let (symbol, color) = if is_active {
                ("⭐", Color::Yellow)
            } else if *available {
                ("✅", Color::Green)
            } else {
                ("❌", Color::Red)
            };

            let status = if is_active {
                "[DEFAULT]"
            } else if *available {
                "Available"
            } else {
                "Unavailable"
            };

            let priority_str = format!("{:?}", priority);
            let throttle_count = backend_info.backend_stats.get(name).copied().unwrap_or(0);
            let throttle_info = if throttle_count > 0 {
                format!(" ({} active)", throttle_count)
            } else {
                String::new()
            };

            text.push(line(vec![
                Span::raw("  "),
                Span::styled(symbol, Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(
                    format!("{:15}", name),
                    if is_active {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::BrightWhite)
                    },
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:12}", status),
                    Style::default().fg(if is_active {
                        Color::Yellow
                    } else {
                        Color::BrightWhite
                    }),
                ),
                Span::raw("  Priority: "),
                Span::styled(
                    format!("{:8}", priority_str),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(throttle_info, Style::default().fg(Color::BrightWhite)),
            ]));
        }
    }

    text.push(line(""));

    // Download Backends Section
    text.push(line(Span::styled(
        "Download Backends:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    if backend_info.available_download.is_empty() {
        text.push(line(Span::styled(
            "  ⚪ (none compiled)",
            Style::default().fg(Color::BrightWhite),
        )));
    } else {
        for (name, priority, available) in &backend_info.available_download {
            let is_active = backend_info.active_download.as_ref() == Some(name);
            let (symbol, color) = if is_active {
                ("⭐", Color::Yellow)
            } else if *available {
                ("✅", Color::Green)
            } else {
                ("❌", Color::Red)
            };

            let status = if is_active {
                "[DEFAULT]"
            } else if *available {
                "Available"
            } else {
                "Unavailable"
            };

            let priority_str = format!("{:?}", priority);
            let throttle_count = backend_info.backend_stats.get(name).copied().unwrap_or(0);
            let throttle_info = if throttle_count > 0 {
                format!(" ({} active)", throttle_count)
            } else {
                String::new()
            };

            text.push(line(vec![
                Span::raw("  "),
                Span::styled(symbol, Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(
                    format!("{:15}", name),
                    if is_active {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::BrightWhite)
                    },
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:12}", status),
                    Style::default().fg(if is_active {
                        Color::Yellow
                    } else {
                        Color::BrightWhite
                    }),
                ),
                Span::raw("  Priority: "),
                Span::styled(
                    format!("{:8}", priority_str),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(throttle_info, Style::default().fg(Color::BrightWhite)),
            ]));
        }
    }

    text.push(line(""));

    // Socket Mapper Backends Section
    text.push(line(Span::styled(
        "Socket Mapper Backends:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    if backend_info.available_socket_mappers.is_empty() {
        text.push(line(Span::styled(
            "  ⚪ (none available)",
            Style::default().fg(Color::BrightWhite),
        )));
    } else {
        for (name, priority, available) in &backend_info.available_socket_mappers {
            let is_active = backend_info.active_socket_mapper.as_ref() == Some(name);
            let (symbol, color) = if is_active {
                ("⭐", Color::Yellow)
            } else if *available {
                ("✅", Color::Green)
            } else {
                ("❌", Color::Red)
            };

            let status = if is_active {
                "[DEFAULT]"
            } else if *available {
                "Available"
            } else {
                "Unavailable"
            };

            let priority_str = format!("{:?}", priority);

            text.push(line(vec![
                Span::raw("  "),
                Span::styled(symbol, Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(
                    format!("{:15}", name),
                    if is_active {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::BrightWhite)
                    },
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:12}", status),
                    Style::default().fg(if is_active {
                        Color::Yellow
                    } else {
                        Color::BrightWhite
                    }),
                ),
                Span::raw("  Priority: "),
                Span::styled(
                    format!("{:8}", priority_str),
                    Style::default().fg(Color::Cyan),
                ),
            ]));
        }
    }

    text.push(line(""));

    // Configuration Section
    text.push(line(Span::styled(
        "Configuration:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    let preferred_upload_display = backend_info
        .preferred_upload
        .as_ref()
        .map(|s| s.as_str())
        .unwrap_or("Auto");
    let active_upload_display = backend_info
        .active_upload
        .as_ref()
        .map(|s| format!(" ({} selected)", s))
        .unwrap_or_else(|| " (none available)".to_string());

    text.push(line(vec![
        Span::raw("  Preferred Upload:     "),
        Span::styled(
            format!("{}{}", preferred_upload_display, active_upload_display),
            Style::default().fg(Color::BrightWhite),
        ),
    ]));

    let preferred_download_display = backend_info
        .preferred_download
        .as_ref()
        .map(|s| s.as_str())
        .unwrap_or("Auto");
    let active_download_display = backend_info
        .active_download
        .as_ref()
        .map(|s| format!(" ({} selected)", s))
        .unwrap_or_else(|| " (none available)".to_string());

    text.push(line(vec![
        Span::raw("  Preferred Download:   "),
        Span::styled(
            format!("{}{}", preferred_download_display, active_download_display),
            Style::default().fg(Color::BrightWhite),
        ),
    ]));

    let preferred_socket_mapper_display = backend_info
        .preferred_socket_mapper
        .as_ref()
        .map(|s| s.as_str())
        .unwrap_or("Auto");
    let active_socket_mapper_display = backend_info
        .active_socket_mapper
        .as_ref()
        .map(|s| format!(" ({} selected)", s))
        .unwrap_or_else(|| " (none available)".to_string());

    text.push(line(vec![
        Span::raw("  Preferred Socket Map: "),
        Span::styled(
            format!(
                "{}{}",
                preferred_socket_mapper_display, active_socket_mapper_display
            ),
            Style::default().fg(Color::BrightWhite),
        ),
    ]));

    text.push(line(vec![
        Span::raw("  Config File:          "),
        Span::styled(
            "~/.config/bproc/throttles.json",
            Style::default().fg(Color::BrightWhite),
        ),
    ]));

    text.push(line(""));

    // Capabilities Section (only if we have active backends)
    if backend_info.upload_capabilities.is_some() || backend_info.download_capabilities.is_some() {
        text.push(line(Span::styled(
            "Capabilities (Active Backends):",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        // Get capabilities - prefer upload, fall back to download
        let caps = backend_info
            .upload_capabilities
            .as_ref()
            .or(backend_info.download_capabilities.as_ref());

        if let Some(capabilities) = caps {
            text.push(line(vec![
                Span::raw("  IPv4:              "),
                Span::styled(
                    if capabilities.ipv4_support {
                        "✅"
                    } else {
                        "❌"
                    },
                    Style::default().fg(if capabilities.ipv4_support {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
                Span::raw("   IPv6:            "),
                Span::styled(
                    if capabilities.ipv6_support {
                        "✅"
                    } else {
                        "❌"
                    },
                    Style::default().fg(if capabilities.ipv6_support {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
            ]));

            text.push(line(vec![
                Span::raw("  Per-Process:       "),
                Span::styled(
                    if capabilities.per_process {
                        "✅"
                    } else {
                        "❌"
                    },
                    Style::default().fg(if capabilities.per_process {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
                Span::raw("   Per-Connection:  "),
                Span::styled(
                    if capabilities.per_connection {
                        "✅"
                    } else {
                        "❌"
                    },
                    Style::default().fg(if capabilities.per_connection {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
            ]));
        }

        text.push(line(""));
    }

    // Instructions
    text.push(line(Span::styled(
        "[↑↓] Navigate  [Space] Apply  [Enter/b/Esc] Close",
        Style::default().fg(Color::BrightBlack),
    )));

    let backend_area = centered_rect(80, 80, area);

    // Auto-scroll to keep selected backend visible
    if let Some(line) = selected_line {
        app.backend_info_scroll_offset =
            AppState::scroll_to_line(app.backend_info_scroll_offset, line, backend_area.height);
    }

    // Clamp scroll offset to content bounds
    let content_lines = text.len();
    let clamped_scroll = AppState::clamp_scroll(
        app.backend_info_scroll_offset,
        content_lines,
        backend_area.height,
    );
    app.backend_info_scroll_offset = clamped_scroll;

    let backend_widget = (
        render::lines(text),
        TextPresentation {
            style: Style::default().bg(Color::Black).fg(Color::BrightWhite),
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("Backends (↑↓ to scroll)")),
                style: Style::default().fg(Color::Cyan),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );

    clear(f, backend_area);
    paint_text(
        &app.component_scene,
        "backend",
        f,
        backend_area,
        backend_widget,
    );

    // Store clickable region for mouse selection
    app.clickable_regions.push(ClickableRegion {
        area: backend_area,
        region_type: ClickableRegionType::BackendModal { line_to_item },
    });
}

fn draw_interface_list(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    let items: Vec<bmux_tui_components::selectable_list::SelectableListItem> = app
        .interface_list
        .iter()
        .enumerate()
        .map(|(index, iface)| {
            // Selection indicator
            let selection_indicator = if Some(index) == app.interface_list_state.selected() {
                "▶ "
            } else {
                "  "
            };

            // Status indicator: up (✓), down (✗), loopback (⟲)
            let status_indicator = if iface.is_loopback {
                "⟲"
            } else if iface.is_up {
                "✓"
            } else {
                "✗"
            };

            let status_color = if iface.is_loopback {
                Color::Cyan
            } else if iface.is_up {
                Color::Green
            } else {
                Color::Red
            };

            // Calculate visible process count (filtered or total)
            let visible_count = if let Some(filters) = &app.active_interface_filters {
                if filters.is_empty() {
                    0 // Empty filter = show nothing
                } else if filters.contains(&iface.name) {
                    // Count processes that use this interface
                    app.process_list
                        .iter()
                        .filter(|p| p.interface_stats.contains_key(&iface.name))
                        .count()
                } else {
                    0 // Not in filter
                }
            } else {
                iface.process_count // No filter = show total
            };

            // Format IP addresses
            let ip_str = if iface.ip_addresses.is_empty() {
                "No IP".to_string()
            } else {
                iface
                    .ip_addresses
                    .iter()
                    .take(2) // Show up to 2 IPs
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            let content = line(vec![
                Span::styled(selection_indicator, Style::default().fg(Color::Yellow)),
                Span::styled(
                    status_indicator,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:12} ", iface.name),
                    Style::default()
                        .fg(Color::BrightWhite)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "{:30} ",
                        if ip_str.len() > 30 {
                            format!("{}...", &ip_str[..27])
                        } else {
                            ip_str
                        }
                    ),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!(
                        "↓{:>10} ",
                        ProcessInfo::format_rate(iface.total_download_rate)
                    ),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!(
                        "↑{:>10} ",
                        ProcessInfo::format_rate(iface.total_upload_rate)
                    ),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{} proc", visible_count),
                    Style::default().fg(Color::Magenta),
                ),
            ]);

            bmux_tui_components::selectable_list::SelectableListItem::rich(
                content.plain_text(),
                content,
            )
        })
        .collect();

    let header = line(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            "Interface    ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "IP Address                     ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("DL Rate    ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("UL Rate    ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("Processes", Style::default().add_modifier(Modifier::BOLD)),
    ]);

    // Split the area for header and list
    let header_area = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(5),
        height: 1,
    };

    let list_area = Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: area.height.saturating_sub(2),
    };

    // Render border and title
    let border = PanelChrome {
        title: Some(line("Network Interfaces")),
        ..PanelChrome::default()
    };
    paint_border(f, area, border);

    // Render header
    paint_text(
        &app.component_scene,
        "draw_interface_list.header",
        f,
        header_area,
        (render::lines(header), TextPresentation::default()),
    );

    // Render list
    let list = (
        items,
        Style::default()
            .bg(Color::BrightBlack)
            .add_modifier(Modifier::BOLD),
    );

    let inner_list_area = Rect {
        x: list_area.x + 1,
        y: list_area.y,
        width: list_area.width.saturating_sub(2),
        height: list_area.height.saturating_sub(1),
    };

    paint_list(
        &app.component_scene,
        "draw_interface_list",
        f,
        inner_list_area,
        &list.0,
        list.1,
        &mut app.interface_list_state,
    );
}

fn draw_interface_detail(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    // Get the selected interface name
    let interface_name = match &app.selected_interface_name {
        Some(name) => name.clone(),
        None => {
            // No interface selected, show error
            let error = (
                render::lines("No interface selected"),
                TextPresentation {
                    chrome: Some(PanelChrome {
                        title: Some(line("Error")),
                        ..PanelChrome::default()
                    }),
                    ..TextPresentation::default()
                },
            );
            paint_text(
                &app.component_scene,
                "draw_interface_detail",
                f,
                area,
                error,
            );
            return;
        }
    };

    // Filter processes to only show those using this interface
    // Use unfiltered_process_list to show ALL processes for this interface
    let filtered_processes: Vec<&ProcessInfo> = app
        .unfiltered_process_list
        .iter()
        .filter(|p| p.interface_stats.contains_key(&interface_name))
        .collect();

    let items: Vec<bmux_tui_components::selectable_list::SelectableListItem> = filtered_processes
        .iter()
        .map(|proc| {
            // Get interface-specific stats
            let iface_stats = proc.interface_stats.get(&interface_name);

            let (dl_rate, ul_rate, dl_total, ul_total) = if let Some(stats) = iface_stats {
                (
                    stats.download_rate,
                    stats.upload_rate,
                    stats.total_download,
                    stats.total_upload,
                )
            } else {
                (0, 0, 0, 0)
            };

            // Status indicator
            let status_indicator = if proc.is_throttled() {
                "⚡"
            } else if proc.is_terminated {
                "💀"
            } else {
                " "
            };

            let terminated_color = Color::White;
            let name_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::BrightWhite
            };
            let dl_rate_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Green
            };
            let ul_rate_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Yellow
            };
            let dl_total_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Cyan
            };
            let ul_total_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Magenta
            };
            let status_color = if proc.is_terminated {
                terminated_color
            } else {
                Color::Red
            };

            let content = line(vec![
                Span::raw("  "),
                Span::raw(format!("{:7} ", proc.pid)),
                Span::styled(
                    format!(
                        "{:20} ",
                        if display_width(&proc.name) > 20 {
                            format!(
                                "{}...",
                                &bmux_tui_components::compact::truncate_width(&proc.name, 17)
                            )
                        } else {
                            proc.name.clone()
                        }
                    ),
                    Style::default().fg(name_color),
                ),
                Span::styled(
                    format!("↓{:>10} ", ProcessInfo::format_rate(dl_rate)),
                    Style::default().fg(dl_rate_color),
                ),
                Span::styled(
                    format!("↑{:>10} ", ProcessInfo::format_rate(ul_rate)),
                    Style::default().fg(ul_rate_color),
                ),
                Span::styled(
                    format!("{:>10} ", ProcessInfo::format_bytes(dl_total)),
                    Style::default().fg(dl_total_color),
                ),
                Span::styled(
                    format!("{:>10} ", ProcessInfo::format_bytes(ul_total)),
                    Style::default().fg(ul_total_color),
                ),
                Span::styled(
                    status_indicator,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);

            bmux_tui_components::selectable_list::SelectableListItem::rich(
                content.plain_text(),
                content,
            )
        })
        .collect();

    let header = line(vec![
        Span::styled("PID     ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            "Process              ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("DL Rate    ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("UL Rate    ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("Total DL   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("Total UL   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("Status", Style::default().add_modifier(Modifier::BOLD)),
    ]);

    // Split the area
    let header_area = Rect {
        x: area.x + 4,
        y: area.y + 1,
        width: area.width.saturating_sub(5),
        height: 1,
    };

    let list_area = Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: area.height.saturating_sub(2),
    };

    // Render border and title
    let title = format!("Interface: {} [Press Esc to go back]", interface_name);
    let border = PanelChrome {
        title: Some(line(title)),
        ..PanelChrome::default()
    };
    paint_border(f, area, border);

    // Render header
    paint_text(
        &app.component_scene,
        "draw_interface_detail.2",
        f,
        header_area,
        (render::lines(header), TextPresentation::default()),
    );

    // Render list
    let list = (items, Style::default());
    let inner_list_area = Rect {
        x: list_area.x + 1,
        y: list_area.y,
        width: list_area.width.saturating_sub(2),
        height: list_area.height.saturating_sub(1),
    };

    paint_list(
        &app.component_scene,
        "draw_interface_detail",
        f,
        inner_list_area,
        &list.0,
        list.1,
        &mut ListState::default(),
    );
}

fn draw_interface_modal(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    let mut text = vec![line("")];

    // Title
    text.push(line(Span::styled(
        "Network Interfaces - Filter Selection",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    text.push(line(""));

    // Show current filter state
    let filter_state = match &app.active_interface_filters {
        None => "Current: All interfaces (no filter)",
        Some(filters) if filters.is_empty() => "Current: No interfaces (empty filter)",
        Some(filters) => {
            if filters.len() <= 3 {
                &format!("Current: {}", filters.join(", "))
            } else {
                "Current: Multiple interfaces"
            }
        }
    };
    text.push(line(Span::styled(
        filter_state,
        Style::default().fg(Color::Yellow),
    )));
    text.push(line(""));

    text.push(line("Select interfaces to show (updates live):"));
    text.push(line(""));

    // List interfaces with checkboxes
    for (index, iface) in app.interface_list.iter().enumerate() {
        let is_cursor = Some(index) == app.selected_interface_index;
        let is_filtered = app.is_interface_filtered(&iface.name);

        let checkbox = if is_filtered { "[✓]" } else { "[ ]" };
        let cursor = if is_cursor { "▶ " } else { "  " };

        let checkbox_style = if is_filtered {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::BrightBlack)
        };

        let name_style = if is_cursor {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
                .bg(Color::BrightBlack)
        } else {
            Style::default().fg(Color::BrightWhite)
        };

        // Calculate total count from unfiltered list (all processes using this interface)
        let total_count = app
            .unfiltered_process_list
            .iter()
            .filter(|p| p.interface_stats.contains_key(&iface.name))
            .count();

        // Calculate filtered count (how many are currently visible in the filtered view)
        let filtered_count = app
            .process_list
            .iter()
            .filter(|p| p.interface_stats.contains_key(&iface.name))
            .count();

        text.push(line(vec![
            Span::raw(cursor),
            Span::styled(checkbox, checkbox_style),
            Span::raw(" "),
            Span::styled(format!("{:12}", iface.name), name_style),
            Span::styled(
                format!(" ({}/{} processes)", filtered_count, total_count),
                Style::default().fg(Color::BrightWhite),
            ),
        ]));
    }

    text.push(line(""));
    text.push(line(""));

    // Instructions
    text.push(line(Span::styled(
        "[↑↓] Navigate  [Space] Toggle (applies live)  [A] Toggle All/None",
        Style::default().fg(Color::BrightBlack),
    )));
    text.push(line(Span::styled(
        "[Enter] View details  [Esc/i] Close and return to process view",
        Style::default().fg(Color::BrightBlack),
    )));

    let modal_area = centered_rect(70, 60, area);

    // Auto-scroll to keep selected interface visible
    // Header lines: blank + title + blank + filter state + blank + "Select interfaces..." + blank = 7 lines
    let header_lines = 7;
    if let Some(selected_idx) = app.selected_interface_index {
        let selected_line = header_lines + selected_idx;
        app.interface_modal_scroll_offset = AppState::scroll_to_line(
            app.interface_modal_scroll_offset,
            selected_line,
            modal_area.height,
        );
    }

    // Clamp scroll offset to content bounds
    let content_lines = text.len();
    let clamped_scroll = AppState::clamp_scroll(
        app.interface_modal_scroll_offset,
        content_lines,
        modal_area.height,
    );
    app.interface_modal_scroll_offset = clamped_scroll;

    let widget = (
        render::lines(text),
        TextPresentation {
            style: Style::default().bg(Color::Black).fg(Color::BrightWhite),
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("Interface Filter (↑↓ to scroll)")),
                style: Style::default().fg(Color::Cyan),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );

    clear(f, modal_area);
    paint_text(&app.component_scene, "interfaces", f, modal_area, widget);
    // Checked indicators are part of the rich BMUX text composition above.
    // Do not overlay another control: that changes glyphs and duplicates hit IDs.

    // Store clickable region for mouse selection
    app.clickable_regions.push(ClickableRegion {
        area: modal_area,
        region_type: ClickableRegionType::InterfaceModal {
            header_lines: 7, // Same as used in auto-scroll calculation
        },
    });
}
// Process Detail View Rendering

fn draw_process_detail(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    // Get the process being detailed (still alive in process list?)
    // Clone it to avoid borrow checker issues
    let process = match app.get_detail_process() {
        Some(p) => p.clone(),
        None => {
            // Process no longer exists - show message and return to process list
            let message = (
                render::lines("Process no longer exists (press Esc to return)"),
                TextPresentation {
                    style: Style::default().fg(Color::Red),
                    chrome: Some(PanelChrome {
                        title: Some(line("Process Not Found")),
                        ..PanelChrome::default()
                    }),
                    ..TextPresentation::default()
                },
            );
            paint_text(
                &app.component_scene,
                "draw_process_detail",
                f,
                area,
                message,
            );
            return;
        }
    };

    // Split into combined header and content areas (saves 6 lines!)
    let chunks = split_vertical(area, 3, 0);

    // Draw combined title with tabs
    draw_detail_header_with_tabs(f, chunks[0], &process, app.detail_tab, app);

    // Draw content based on active tab
    match app.detail_tab {
        ProcessDetailTab::Overview => draw_detail_overview(f, chunks[1], &process, app),
        ProcessDetailTab::Connections => draw_detail_connections(f, chunks[1], &process, app),
        ProcessDetailTab::Traffic => draw_detail_traffic(f, chunks[1], &process, app),
        ProcessDetailTab::System => draw_detail_system(f, chunks[1], &process, app),
    }
}

fn draw_detail_header_with_tabs(
    f: &mut PaintCx<'_, '_>,
    area: Rect,
    process: &ProcessInfo,
    current_tab: ProcessDetailTab,
    app: &mut AppState,
) {
    let prefix = line(vec![
        Span::styled(
            process.name.clone(),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" (PID {})  ", process.pid)),
    ]);
    let prefix_width = prefix.width().min(u16::MAX as usize) as u16;
    paint_border(
        f,
        area,
        PanelChrome {
            title: Some(line("Process Details")),
            ..Default::default()
        },
    );
    let inner = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    paint_text(
        &app.component_scene,
        "draw_detail_header_with_tabs",
        f,
        inner,
        (vec![prefix], TextPresentation::default()),
    );
    let tab_area = Rect::new(
        inner.x.saturating_add(prefix_width),
        inner.y,
        inner.width.saturating_sub(prefix_width),
        1.min(inner.height),
    );
    render::paint_detail_tabs(
        &app.component_scene,
        "detail.tabs",
        f,
        tab_area,
        match current_tab {
            ProcessDetailTab::Overview => 0,
            ProcessDetailTab::Connections => 1,
            ProcessDetailTab::Traffic => 2,
            ProcessDetailTab::System => 3,
        },
    );
    let mut col = tab_area.x;
    let tab_ranges = [
        ("Overview", ProcessDetailTab::Overview),
        ("Connections", ProcessDetailTab::Connections),
        ("Traffic", ProcessDetailTab::Traffic),
        ("System", ProcessDetailTab::System),
    ]
    .into_iter()
    .map(|(name, tab)| {
        let start = col;
        let end = start
            .saturating_add(display_width(name) as u16)
            .saturating_add(2);
        col = end.saturating_add(1);
        (start, end, tab)
    })
    .collect();

    // Store clickable region for mouse selection
    app.clickable_regions.push(ClickableRegion {
        area,
        region_type: ClickableRegionType::ProcessDetailTabs { tab_ranges },
    });
}

fn draw_detail_overview(
    f: &mut PaintCx<'_, '_>,
    area: Rect,
    process: &ProcessInfo,
    app: &mut AppState,
) {
    let history = &app.history;
    let mut text = vec![];

    // Basic Information
    text.push(line(vec![Span::styled(
        "Basic Information:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));
    text.push(line(format!("  PID:              {}", process.pid)));
    text.push(line(format!("  Name:             {}", process.name)));

    // Get process details
    let details = app.details_for(process);

    if let Some(ref cmdline) = details.cmdline {
        let cmd_str = cmdline.join(" ");
        let cmd_display = if display_width(&cmd_str) > 80 {
            format!(
                "{}...",
                &bmux_tui_components::compact::truncate_width(&cmd_str, 77)
            )
        } else {
            cmd_str
        };
        text.push(line(format!("  Command:          {}", cmd_display)));
    }

    if let Some(ref exe) = details.exe_path {
        text.push(line(format!("  Executable:       {}", exe)));
    }

    if let Some(ref cwd) = details.cwd {
        text.push(line(format!("  Working Dir:      {}", cwd)));
    }

    text.push(line(format!(
        "  State:            {}",
        details.state_description()
    )));

    if let Some(ppid) = details.ppid {
        text.push(line(format!("  Parent PID:       {}", ppid)));
    }

    text.push(line(""));

    // Network Statistics
    text.push(line(vec![Span::styled(
        "Network Statistics:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));
    text.push(line(vec![
        Span::raw("  Current Download: "),
        Span::styled(
            format!("↓ {:>10}", ProcessInfo::format_rate(process.download_rate)),
            Style::default().fg(Color::Green),
        ),
        Span::raw("    Upload: "),
        Span::styled(
            format!("↑ {:>10}", ProcessInfo::format_rate(process.upload_rate)),
            Style::default().fg(Color::Yellow),
        ),
    ]));

    text.push(line(vec![
        Span::raw("  Total Download:   "),
        Span::styled(
            format!("{:>10}", ProcessInfo::format_bytes(process.total_download)),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("      Upload: "),
        Span::styled(
            format!("{:>10}", ProcessInfo::format_bytes(process.total_upload)),
            Style::default().fg(Color::Magenta),
        ),
    ]));

    // Get history stats
    if let Some(hist) = history.get_history(process.pid) {
        text.push(line(vec![
            Span::raw("  Peak Download:    "),
            Span::styled(
                format!("{:>10}", ProcessInfo::format_rate(hist.max_download_rate())),
                Style::default().fg(Color::Green),
            ),
            Span::raw("      Upload: "),
            Span::styled(
                format!("{:>10}", ProcessInfo::format_rate(hist.max_upload_rate())),
                Style::default().fg(Color::Yellow),
            ),
        ]));

        text.push(line(vec![
            Span::raw("  Avg Download:     "),
            Span::styled(
                format!("{:>10}", ProcessInfo::format_rate(hist.avg_download_rate())),
                Style::default().fg(Color::Green),
            ),
            Span::raw("      Upload: "),
            Span::styled(
                format!("{:>10}", ProcessInfo::format_rate(hist.avg_upload_rate())),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }

    text.push(line(""));

    // Internet/Local breakdown
    text.push(line(vec![Span::styled(
        "Internet Traffic:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));

    let internet_pct = if process.download_rate > 0 {
        (process.internet_download_rate as f64 / process.download_rate as f64 * 100.0) as u32
    } else {
        0
    };

    text.push(line(vec![
        Span::raw("  Download:         "),
        Span::styled(
            format!(
                "↓ {:>10}",
                ProcessInfo::format_rate(process.internet_download_rate)
            ),
            Style::default().fg(Color::Green),
        ),
        Span::raw(format!(" ({}%)  Total: ", internet_pct)),
        Span::styled(
            ProcessInfo::format_bytes(process.internet_total_download),
            Style::default().fg(Color::Cyan),
        ),
    ]));

    let upload_pct = if process.upload_rate > 0 {
        (process.internet_upload_rate as f64 / process.upload_rate as f64 * 100.0) as u32
    } else {
        0
    };

    text.push(line(vec![
        Span::raw("  Upload:           "),
        Span::styled(
            format!(
                "↑ {:>10}",
                ProcessInfo::format_rate(process.internet_upload_rate)
            ),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(format!(" ({}%)  Total: ", upload_pct)),
        Span::styled(
            ProcessInfo::format_bytes(process.internet_total_upload),
            Style::default().fg(Color::Magenta),
        ),
    ]));

    text.push(line(""));

    // Local traffic
    text.push(line(vec![Span::styled(
        "Local Traffic:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));

    let local_dl_pct = if process.download_rate > 0 {
        (process.local_download_rate as f64 / process.download_rate as f64 * 100.0) as u32
    } else {
        0
    };

    text.push(line(vec![
        Span::raw("  Download:         "),
        Span::styled(
            format!(
                "↓ {:>10}",
                ProcessInfo::format_rate(process.local_download_rate)
            ),
            Style::default().fg(Color::Green),
        ),
        Span::raw(format!(" ({}%)   Total: ", local_dl_pct)),
        Span::styled(
            ProcessInfo::format_bytes(process.local_total_download),
            Style::default().fg(Color::Cyan),
        ),
    ]));

    let local_ul_pct = if process.upload_rate > 0 {
        (process.local_upload_rate as f64 / process.upload_rate as f64 * 100.0) as u32
    } else {
        0
    };

    text.push(line(vec![
        Span::raw("  Upload:           "),
        Span::styled(
            format!(
                "↑ {:>10}",
                ProcessInfo::format_rate(process.local_upload_rate)
            ),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(format!(" ({}%)   Total: ", local_ul_pct)),
        Span::styled(
            ProcessInfo::format_bytes(process.local_total_upload),
            Style::default().fg(Color::Magenta),
        ),
    ]));

    text.push(line(""));

    // Throttle status
    text.push(line(vec![Span::styled(
        "Throttle Status:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));

    if let Some(ref throttle) = process.throttle_limit {
        let dl_text = if let Some(limit) = throttle.download_limit {
            ProcessInfo::format_rate(limit)
        } else {
            "Unlimited".to_string()
        };

        let ul_text = if let Some(limit) = throttle.upload_limit {
            ProcessInfo::format_rate(limit)
        } else {
            "Unlimited".to_string()
        };

        let traffic_type_text = match throttle.traffic_type {
            crate::process::TrafficType::All => "All Traffic",
            crate::process::TrafficType::Internet => "Internet Only",
            crate::process::TrafficType::Local => "Local Only",
        };

        text.push(line(vec![
            Span::raw("  Download Limit:   "),
            Span::styled(
                dl_text,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" ({})", traffic_type_text)),
            Span::styled(" ⚡", Style::default().fg(Color::Yellow)),
        ]));

        text.push(line(vec![
            Span::raw("  Upload Limit:     "),
            Span::styled(
                ul_text,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        text.push(line("  Not throttled"));
    }

    text.push(line(""));

    // System resources
    if details.memory_rss.is_some() || details.threads.is_some() {
        text.push(line(vec![Span::styled(
            "System Resources:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        text.push(line(""));

        if let Some(rss) = details.memory_rss {
            text.push(line(format!(
                "  Memory (RSS):     {}",
                crate::process::ProcessDetails::format_memory(rss)
            )));
        }

        if let Some(vms) = details.memory_vms {
            text.push(line(format!(
                "  Memory (VMS):     {}",
                crate::process::ProcessDetails::format_memory(vms)
            )));
        }

        if let Some(threads) = details.threads {
            text.push(line(format!("  Threads:          {}", threads)));
        }
    }

    text.push(line(""));
    text.push(line(Span::styled(
        "[↑↓] Scroll  [Tab] Switch tab  [t] Throttle  [g] Graph  [Esc] Back",
        Style::default().fg(Color::BrightBlack),
    )));

    // Clamp scroll offset to content bounds
    let content_lines = text.len();
    let clamped_scroll =
        AppState::clamp_scroll(app.detail_scroll_offset, content_lines, area.height);
    app.detail_scroll_offset = clamped_scroll;

    let paragraph = (
        render::lines(text),
        TextPresentation {
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("Overview")),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );
    paint_text(&app.component_scene, "detail.overview", f, area, paragraph);
}

fn draw_detail_connections(
    f: &mut PaintCx<'_, '_>,
    area: Rect,
    process: &ProcessInfo,
    app: &mut AppState,
) {
    let mut text = vec![];

    text.push(line(""));
    text.push(line(vec![Span::styled(
        format!("Active Network Connections ({})", process.connections.len()),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));

    if process.connections.is_empty() {
        text.push(line("  No active connections"));
    } else {
        // Header
        text.push(line(vec![
            Span::styled(
                "  Protocol  ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Local Address            ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Remote Address           ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("State     ", Style::default().add_modifier(Modifier::BOLD)),
        ]));
        text.push(line(
            "  ──────────────────────────────────────────────────────────────────────",
        ));

        // Sort connections: ESTABLISHED first, then LISTEN, then others
        let mut sorted_conns = process.connections.clone();
        sorted_conns.sort_by(|a, b| {
            let order_a = match a.state.as_str() {
                "Established" => 0,
                "Listen" => 1,
                _ => 2,
            };
            let order_b = match b.state.as_str() {
                "Established" => 0,
                "Listen" => 1,
                _ => 2,
            };
            order_a.cmp(&order_b)
        });

        // Render all connections (scrolling handled by Paragraph widget)
        for conn in &sorted_conns {
            let local = format!("{}:{}", format_ip_addr(&conn.local_addr), conn.local_port);
            let remote = if conn.remote_port == 0 {
                "*:*".to_string()
            } else {
                format!("{}:{}", format_ip_addr(&conn.remote_addr), conn.remote_port)
            };

            let state_display = match conn.state.as_str() {
                "Established" => "ESTAB",
                "Listen" => "LISTEN",
                "TimeWait" => "TIMEWT",
                "CloseWait" => "CLOSWT",
                "FinWait1" => "FIN1",
                "FinWait2" => "FIN2",
                "Closing" => "CLOSNG",
                "SynSent" => "SYNSNT",
                "SynRecv" => "SYNRCV",
                s => s,
            };

            let proto_style = match conn.protocol.as_str() {
                "TCP" | "TCP6" => Style::default().fg(Color::Green),
                "UDP" | "UDP6" => Style::default().fg(Color::Yellow),
                _ => Style::default(),
            };

            let state_style = match conn.state.as_str() {
                "Established" => Style::default().fg(Color::Green),
                "Listen" => Style::default().fg(Color::Cyan),
                _ => Style::default().fg(Color::BrightWhite),
            };

            text.push(line(vec![
                Span::raw("  "),
                Span::styled(format!("{:8}  ", conn.protocol), proto_style),
                Span::raw(format!("{:24} ", local)),
                Span::raw(format!("{:24} ", remote)),
                Span::styled(format!("{:8}", state_display), state_style),
            ]));
        }
    }

    text.push(line(""));
    text.push(line(""));
    text.push(line(Span::styled(
        "[↑↓] Scroll  [Tab] Switch tab  [Esc] Back",
        Style::default().fg(Color::BrightBlack),
    )));

    // Clamp scroll offset to content bounds
    let content_lines = text.len();
    let clamped_scroll =
        AppState::clamp_scroll(app.detail_scroll_offset, content_lines, area.height);
    app.detail_scroll_offset = clamped_scroll;

    let paragraph = (
        render::lines(text),
        TextPresentation {
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("Connections")),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );
    paint_text(
        &app.component_scene,
        "detail.connections",
        f,
        area,
        paragraph,
    );
}

fn format_ip_addr(addr: &IpAddr) -> String {
    match addr {
        IpAddr::V4(ipv4) if ipv4.is_unspecified() => "0.0.0.0".to_string(),
        IpAddr::V6(ipv6) if ipv6.is_unspecified() => "::".to_string(),
        _ => addr.to_string(),
    }
}

fn draw_detail_traffic(
    f: &mut PaintCx<'_, '_>,
    area: Rect,
    process: &ProcessInfo,
    app: &mut AppState,
) {
    let mut text = vec![];

    text.push(line(""));
    text.push(line(vec![Span::styled(
        "Traffic Breakdown by Interface:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));

    if process.interface_stats.is_empty() {
        text.push(line("  No interface data available"));
    } else {
        // Header
        text.push(line(vec![
            Span::styled(
                "  Interface    ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Download Rate    ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Upload Rate    ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Total DL     ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("Total UL", Style::default().add_modifier(Modifier::BOLD)),
        ]));

        text.push(line(
            "  ─────────────────────────────────────────────────────────────────────",
        ));

        // Sort interfaces by download rate
        let mut ifaces: Vec<_> = process.interface_stats.iter().collect();
        ifaces.sort_by(|a, b| b.1.download_rate.cmp(&a.1.download_rate));

        for (iface_name, stats) in ifaces.iter().take(10) {
            // Show top 10
            text.push(line(vec![
                Span::raw(format!("  {:12} ", iface_name)),
                Span::styled(
                    format!(
                        "↓ {:>10}     ",
                        ProcessInfo::format_rate(stats.download_rate)
                    ),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("↑ {:>10}   ", ProcessInfo::format_rate(stats.upload_rate)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{:>10}   ", ProcessInfo::format_bytes(stats.total_download)),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("{:>10}", ProcessInfo::format_bytes(stats.total_upload)),
                    Style::default().fg(Color::Magenta),
                ),
            ]));
        }
    }

    text.push(line(""));
    text.push(line(""));

    // Traffic by type
    text.push(line(vec![Span::styled(
        "Traffic by Type:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));

    let total_dl = process.download_rate;
    let total_ul = process.upload_rate;

    if total_dl > 0 || total_ul > 0 {
        let internet_pct = if total_dl > 0 {
            (process.internet_download_rate as f64 / total_dl as f64 * 100.0) as u32
        } else {
            0
        };

        text.push(line(vec![
            Span::raw("  🌐 Internet:  "),
            Span::styled(
                format!("{}%", internet_pct),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  ("),
            Span::styled(
                format!(
                    "↓ {}",
                    ProcessInfo::format_rate(process.internet_download_rate)
                ),
                Style::default().fg(Color::Green),
            ),
            Span::raw(", "),
            Span::styled(
                format!(
                    "↑ {})",
                    ProcessInfo::format_rate(process.internet_upload_rate)
                ),
                Style::default().fg(Color::Yellow),
            ),
        ]));

        let local_pct = if total_dl > 0 {
            (process.local_download_rate as f64 / total_dl as f64 * 100.0) as u32
        } else {
            0
        };

        text.push(line(vec![
            Span::raw("  🏠 Local:     "),
            Span::styled(
                format!("{}%", local_pct),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  ("),
            Span::styled(
                format!(
                    "↓ {}",
                    ProcessInfo::format_rate(process.local_download_rate)
                ),
                Style::default().fg(Color::Green),
            ),
            Span::raw(", "),
            Span::styled(
                format!("↑ {})", ProcessInfo::format_rate(process.local_upload_rate)),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    } else {
        text.push(line("  No traffic"));
    }

    text.push(line(""));
    text.push(line(""));
    text.push(line(Span::styled(
        "[↑↓] Scroll  [Tab] Switch tab  [Esc] Back",
        Style::default().fg(Color::BrightBlack),
    )));

    // Clamp scroll offset to content bounds
    let content_lines = text.len();
    let clamped_scroll =
        AppState::clamp_scroll(app.detail_scroll_offset, content_lines, area.height);
    app.detail_scroll_offset = clamped_scroll;

    let paragraph = (
        render::lines(text),
        TextPresentation {
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("Traffic Details")),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );
    paint_text(&app.component_scene, "detail.traffic", f, area, paragraph);
}

fn draw_detail_system(
    f: &mut PaintCx<'_, '_>,
    area: Rect,
    process: &ProcessInfo,
    app: &mut AppState,
) {
    let details = app.details_for(process);

    let mut text = vec![];

    text.push(line(""));
    text.push(line(vec![Span::styled(
        "Process Information:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));

    text.push(line(format!("  PID:                {}", details.pid)));
    if let Some(ppid) = details.ppid {
        text.push(line(format!("  Parent PID:         {}", ppid)));
    }
    text.push(line(format!(
        "  State:              {}",
        details.state_description()
    )));

    text.push(line(""));

    // User/Group
    if details.uid.is_some() || details.gid.is_some() {
        text.push(line(vec![Span::styled(
            "User/Group:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        text.push(line(""));

        if let Some(uid) = details.uid {
            text.push(line(format!("  UID:                {}", uid)));
        }
        if let Some(gid) = details.gid {
            text.push(line(format!("  GID:                {}", gid)));
        }

        text.push(line(""));
    }

    // Memory
    if details.memory_rss.is_some() || details.memory_vms.is_some() {
        text.push(line(vec![Span::styled(
            "Memory:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        text.push(line(""));

        if let Some(vms) = details.memory_vms {
            text.push(line(format!(
                "  Virtual Memory:     {}",
                crate::process::ProcessDetails::format_memory(vms)
            )));
        }
        if let Some(rss) = details.memory_rss {
            text.push(line(format!(
                "  Resident Memory:    {}",
                crate::process::ProcessDetails::format_memory(rss)
            )));
        }

        text.push(line(""));
    }

    // Threads
    if let Some(threads) = details.threads {
        text.push(line(vec![Span::styled(
            "Threads:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        text.push(line(""));
        text.push(line(format!("  Thread Count:       {}", threads)));
        text.push(line(""));
    }

    // Executable
    text.push(line(vec![Span::styled(
        "Executable:",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    text.push(line(""));

    if let Some(ref exe) = details.exe_path {
        text.push(line(format!("  Path:               {}", exe)));
    } else {
        text.push(line("  Path:               (not available)"));
    }

    if let Some(ref cwd) = details.cwd {
        text.push(line(format!("  Working Directory:  {}", cwd)));
    }

    text.push(line(""));

    // Command line
    if let Some(ref cmdline) = details.cmdline {
        text.push(line(vec![Span::styled(
            "Command Line:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        text.push(line(""));

        let cmd_str = cmdline.join(" ");
        // Wrap long command lines at word/path boundaries
        // Calculate available width: area width - borders (2) - indentation (2)
        let max_width = usize::from(area.width.saturating_sub(4)).max(1);
        for row in Line::raw(cmd_str).wrap_word(max_width) {
            text.push(line(format!("  {}", row.plain_text())));
        }
    }

    text.push(line(""));
    text.push(line(""));
    text.push(line(Span::styled(
        "[↑↓] Scroll  [Tab] Switch tab  [Esc] Back",
        Style::default().fg(Color::BrightBlack),
    )));

    // Clamp scroll offset to content bounds
    let content_lines = text.len();
    let clamped_scroll =
        AppState::clamp_scroll(app.detail_scroll_offset, content_lines, area.height);
    app.detail_scroll_offset = clamped_scroll;

    let paragraph = (
        render::lines(text),
        TextPresentation {
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("System Information")),
                ..PanelChrome::default()
            }),
            ..TextPresentation::default()
        },
    );
    paint_text(&app.component_scene, "detail.system", f, area, paragraph);
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod parity;

#[cfg(test)]
mod regressions;
