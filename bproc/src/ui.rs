use crate::backends::BackendPriority;
use crate::backends::throttle::BackendInfo;
use crate::history::HistoryTracker;
use crate::process::{InterfaceInfo, InterfaceMap, ProcessInfo, ProcessMap};
pub use bmux_tui::geometry::Rect;
use bmux_tui::{
    paint::PaintCx,
    style::{Color, Modifier, Style},
    text::Span,
};
mod events;
pub mod explorer;
mod render;
mod scene;
use bmux_tui::text_width::display_width;
pub use render::ListState;
use render::{PanelChrome, TextPresentation, clear, line, paint_list, paint_text, root_area};
use std::collections::HashMap;

pub struct AppState {
    /// Process explorer (main screen) state.
    pub explorer: explorer::ExplorerState,
    component_scene: std::cell::RefCell<scene::ComponentScene>,
    /// Network view of processes after the interface filter, in stable order.
    pub process_list: Vec<ProcessInfo>,
    /// Full network list before interface filtering (interface detail view).
    pub unfiltered_process_list: Vec<ProcessInfo>,
    /// Index into `process_list` of the process shown by the bandwidth graph.
    pub selected_index: Option<usize>,
    pub list_state: ListState,
    pub show_help: bool,
    pub show_throttle_dialog: bool,
    pub show_backend_info: bool,
    pub throttle_dialog: ThrottleDialog,
    pub status_message: String,
    pub history: HistoryTracker,
    pub show_graph: bool,
    // Interface view state
    pub view_mode: ViewMode,
    pub interface_list: Vec<InterfaceInfo>,
    pub interface_list_state: ListState,
    pub selected_interface_index: Option<usize>,
    pub selected_interface_name: Option<String>,
    /// Selection/scroll of the process list in the interface detail view.
    pub interface_detail_state: ListState,
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
    // Modal scroll offsets
    pub help_scroll_offset: usize,         // For help overlay scrolling
    pub backend_info_scroll_offset: usize, // For backend info modal scrolling
    pub interface_modal_scroll_offset: usize, // For interface filter modal scrolling
    pub backend_compat_scroll_offset: usize, // For backend compatibility dialog scrolling
    /// Mouse targets painted this frame (promoted by the presenter commit).
    pub clickable_regions: Vec<ClickableRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    /// The process explorer owns the screen.
    ProcessView,
    /// Interface filter modal over the explorer.
    InterfaceList,
    /// Processes on the selected interface.
    InterfaceDetail,
}

/// Legacy (network) surfaces that can own input, topmost first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Compatibility,
    Backends,
    Graph,
    Throttle,
    Help,
    InterfaceList,
    InterfaceDetail,
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
    InterfaceModal {
        header_lines: usize, // Number of header lines before interface list starts
    },
    BackendModal {
        // Maps visual line (accounting for scroll) to backend item index
        line_to_item: HashMap<usize, usize>,
    },
    /// Body of a legacy modal: swallows clicks; clicks outside the topmost
    /// panel dismiss it (see `Layer::dismissible`).
    Panel(Layer),
    /// "Back" button of the interface detail view.
    InterfaceBack,
    /// Traffic type row of the throttle dialog (click cycles it).
    ThrottleTraffic,
}

impl ClickableRegionType {
    /// The surface this region belongs to.
    pub fn layer(&self) -> Layer {
        match self {
            Self::InterfaceModal { .. } => Layer::InterfaceList,
            Self::BackendModal { .. } => Layer::Backends,
            Self::Panel(layer) => *layer,
            Self::InterfaceBack => Layer::InterfaceDetail,
            Self::ThrottleTraffic => Layer::Throttle,
        }
    }
}

impl Layer {
    /// Whether a click outside the panel closes it. Dialogs holding typed
    /// input or a pending decision are never dismissed by a stray click.
    pub fn dismissible(self) -> bool {
        !matches!(self, Layer::Throttle | Layer::Compatibility)
    }
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
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(None); // Nothing selected initially

        let mut interface_list_state = ListState::default();
        interface_list_state.select(None);

        Self {
            explorer: explorer::ExplorerState::default(),
            component_scene: Default::default(),
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
            status_message: String::new(),
            view_mode: ViewMode::ProcessView,
            interface_list: Vec::new(),
            interface_list_state,
            selected_interface_index: None,
            selected_interface_name: None,
            interface_detail_state: ListState::default(),
            active_interface_filters: None, // Show all by default
            traffic_view_mode: TrafficViewMode::All, // Show all traffic by default
            show_backend_compatibility_dialog: false,
            backend_compatibility_dialog: None,
            backend_items: Vec::new(),
            backend_selected_index: 0,
            help_scroll_offset: 0,
            backend_info_scroll_offset: 0,
            interface_modal_scroll_offset: 0,
            backend_compat_scroll_offset: 0,
            clickable_regions: Vec::new(),
        }
    }

    pub fn update_interfaces(&mut self, interface_map: InterfaceMap) {
        let mut interfaces: Vec<InterfaceInfo> = interface_map.into_values().collect();

        // Sort by name for consistent display
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));

        self.interface_list = interfaces;

        // Adjust selection if out of bounds
        if let Some(index) = self.selected_interface_index
            && index >= self.interface_list.len()
            && !self.interface_list.is_empty()
        {
            self.selected_interface_index = Some(self.interface_list.len() - 1);
            self.interface_list_state
                .select(Some(self.interface_list.len() - 1));
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
            ViewMode::InterfaceList | ViewMode::InterfaceDetail => ViewMode::ProcessView,
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
            self.interface_detail_state = ListState::default();
            self.view_mode = ViewMode::InterfaceDetail;
        }
    }

    /// Keep the interface cursor on a real row whenever interfaces exist
    /// (the modal may be opened before the first interface snapshot).
    pub fn ensure_interface_cursor(&mut self) {
        let len = self.interface_list.len();
        let index = match self.selected_interface_index {
            _ if len == 0 => None,
            Some(index) => Some(index.min(len - 1)),
            None => Some(0),
        };
        self.selected_interface_index = index;
        self.interface_list_state.select(index);
    }

    /// The legacy surface that currently owns input, if any.
    pub fn top_layer(&self) -> Option<Layer> {
        if self.show_backend_compatibility_dialog {
            Some(Layer::Compatibility)
        } else if self.show_backend_info {
            Some(Layer::Backends)
        } else if self.show_graph {
            Some(Layer::Graph)
        } else if self.show_throttle_dialog {
            Some(Layer::Throttle)
        } else if self.show_help {
            Some(Layer::Help)
        } else {
            match self.view_mode {
                ViewMode::InterfaceList => Some(Layer::InterfaceList),
                ViewMode::InterfaceDetail => Some(Layer::InterfaceDetail),
                ViewMode::ProcessView => None,
            }
        }
    }

    /// Close `layer` as Esc would (used for clicks outside its panel).
    pub fn dismiss(&mut self, layer: Layer) {
        match layer {
            Layer::Backends => {
                self.show_backend_info = false;
                self.reset_backend_info_scroll();
            }
            Layer::Graph => self.show_graph = false,
            Layer::Help => {
                self.show_help = false;
                self.reset_help_scroll();
            }
            Layer::InterfaceList => {
                self.view_mode = ViewMode::ProcessView;
                self.status_message = "Closed interface filter - filter still active".into();
            }
            Layer::InterfaceDetail => {
                self.exit_interface_detail();
                self.status_message = "Back to interface list".into();
            }
            Layer::Throttle => {
                self.show_throttle_dialog = false;
                self.throttle_dialog.reset();
            }
            Layer::Compatibility => {
                self.show_backend_compatibility_dialog = false;
                self.backend_compatibility_dialog = None;
            }
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
        let mut processes: Vec<ProcessInfo> = process_map.into_values().collect();
        // Deterministic multi-level order so the interface views never jump:
        // terminated last, then DL rate, total DL, UL rate, total UL,
        // throttled first, name, PID.
        processes.sort_by(|a, b| {
            let (a_dl_rate, a_dl_total, a_ul_rate, a_ul_total) = self.get_sort_rates(a);
            let (b_dl_rate, b_dl_total, b_ul_rate, b_ul_total) = self.get_sort_rates(b);
            a.is_terminated
                .cmp(&b.is_terminated)
                .then(b_dl_rate.cmp(&a_dl_rate))
                .then(b_dl_total.cmp(&a_dl_total))
                .then(b_ul_rate.cmp(&a_ul_rate))
                .then(b_ul_total.cmp(&a_ul_total))
                .then(b.is_throttled().cmp(&a.is_throttled()))
                .then_with(|| a.name.cmp(&b.name))
                .then(a.pid.cmp(&b.pid))
        });

        // Keep following the same process (the graph) across re-sorts.
        let selected_pid = self.get_selected_process().map(|p| p.pid);
        self.unfiltered_process_list = processes.clone();
        self.process_list = self.apply_process_filter(processes);
        self.selected_index =
            selected_pid.and_then(|pid| self.process_list.iter().position(|p| p.pid == pid));
        self.list_state.select(self.selected_index);
    }

    pub fn get_selected_process(&self) -> Option<&ProcessInfo> {
        self.selected_index
            .and_then(|idx| self.process_list.get(idx))
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
        if let Some(name) = current_backend_name
            && let Some(index) = self.backend_items.iter().position(
                |item| matches!(item, BackendSelectorItem::Backend { name: n, .. } if n == &name),
            )
        {
            self.backend_selected_index = index;
            return; // Found it, keep cursor there!
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
                && *available
            {
                break;
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
                && *available
            {
                break;
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
            && *available
        {
            return Some((name.as_str(), *group));
        }
        None
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

#[cfg(test)]
pub fn draw_ui(f: &mut PaintCx<'_, '_>, app: &mut AppState) {
    draw_ui_with_backend_info(f, app, None);
}

pub fn draw_ui_with_backend_info(
    f: &mut PaintCx<'_, '_>,
    app: &mut AppState,
    backend_info: Option<&BackendInfo>,
) {
    app.component_scene.borrow_mut().begin();
    app.clickable_regions.clear();

    let root = root_area(f);
    // The explorer always stays live underneath the legacy network surfaces.
    draw_explorer(f, root, app, backend_info);
    match app.view_mode {
        ViewMode::ProcessView => {}
        ViewMode::InterfaceList => draw_interface_modal(f, root, app),
        ViewMode::InterfaceDetail => draw_interface_detail(f, root, app),
    }
    if app.show_help {
        draw_help_overlay(f, root, app);
    }
    if app.show_throttle_dialog {
        draw_throttle_dialog(f, root, app);
    }
    if app.show_graph {
        draw_bandwidth_graph(f, root, app);
    }
    if app.show_backend_info
        && let Some(info) = backend_info
    {
        draw_backend_info(f, root, app, info);
    }
    if app.show_backend_compatibility_dialog
        && let Some(dialog) = app.backend_compatibility_dialog.clone()
    {
        draw_backend_compatibility_dialog(f, root, app, &dialog);
    }
}

/// Record a legacy modal body for click routing.
fn push_panel(app: &mut AppState, area: Rect, layer: Layer) {
    app.clickable_regions.push(ClickableRegion {
        area,
        region_type: ClickableRegionType::Panel(layer),
    });
}

fn draw_explorer(
    f: &mut PaintCx<'_, '_>,
    area: Rect,
    app: &mut AppState,
    backend_info: Option<&BackendInfo>,
) {
    let interface_filter = app.active_interface_filters.as_ref().map(|filters| {
        if filters.is_empty() {
            "no interfaces".to_string()
        } else {
            filters.join(", ")
        }
    });
    let status = app.status_message.clone();
    let env = explorer::DrawEnv {
        status: &status,
        interface_filter,
        socket_mapper: backend_info.and_then(|b| b.active_socket_mapper.clone()),
    };
    app.explorer.draw(f, area, &env);
}

/// Help for the interface views (the explorer has its own F1 help).
fn draw_help_overlay(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    let mut help_text = vec![
        line(""),
        line(Span::styled(
            "Interfaces - keyboard & mouse",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        line(""),
    ];
    for binding in crate::keybindings::get_all_keybindings() {
        help_text.push(line(vec![
            Span::styled(
                format!("  {:12}", binding.key),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(format!(" {}", binding.description)),
        ]));
    }
    help_text.push(line(""));
    help_text.push(line(Span::styled(
        "Process keys live in the main help: close this view, then F1 or ?",
        Style::default().fg(Color::BrightBlack),
    )));
    help_text.push(line(Span::styled(
        "↑↓ scroll · any other key or a click outside closes",
        Style::default().fg(Color::BrightBlack),
    )));

    // Larger than the interface modal so it fully covers it.
    let help_area = centered_rect(80, 70, area);
    let clamped_scroll =
        AppState::clamp_scroll(app.help_scroll_offset, help_text.len(), help_area.height);
    app.help_scroll_offset = clamped_scroll;

    let help = (
        render::lines(help_text),
        TextPresentation {
            style: Style::default().bg(Color::Black).fg(Color::BrightWhite),
            scroll: (clamped_scroll as u16, 0),
            chrome: Some(PanelChrome {
                title: Some(line("Help")),
                style: Style::default().fg(Color::Cyan),
            }),
            ..TextPresentation::default()
        },
    );

    clear(f, help_area);
    paint_text(&app.component_scene, "help", f, help_area, help);
    push_panel(app, help_area, Layer::Help);
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

/// A `width` x `height` rectangle centred in `r`, shrunk to fit.
fn sized_rect(width: u16, height: u16, r: Rect) -> Rect {
    let width = width.min(r.width);
    let height = height.min(r.height);
    Rect::new(
        r.x + (r.width - width) / 2,
        r.y + (r.height - height) / 2,
        width,
        height,
    )
}

fn draw_throttle_dialog(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
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
            Span::styled(
                "  (click to change)",
                Style::default().fg(Color::BrightBlack),
            ),
        ]),
        line(""),
        line(Span::styled(
            "Tab field · t traffic type · Enter apply · Esc cancel",
            Style::default().fg(Color::BrightBlack),
        )),
    ];

    let dialog_rows = dialog_text.len() as u16;
    let dialog_widget = (
        render::lines(dialog_text),
        TextPresentation {
            style: Style::default().bg(Color::Black).fg(Color::BrightWhite),
            chrome: Some(PanelChrome {
                title: Some(line(title)),
                style: Style::default().fg(Color::Cyan),
            }),
            ..TextPresentation::default()
        },
    );

    // Tall enough for every row and hint; at least 62 columns wide when possible.
    let dialog_area = sized_rect(
        centered_rect(60, 30, area).width.max(62),
        dialog_rows + 2,
        area,
    );
    app.clickable_regions.push(ClickableRegion {
        area: dialog_area,
        region_type: ClickableRegionType::Panel(Layer::Throttle),
    });
    clear(f, dialog_area);
    paint_text(
        &app.component_scene,
        "draw_throttle_dialog",
        f,
        dialog_area,
        dialog_widget,
    );
    if dialog_area.height > 7 && dialog_area.width > 2 {
        app.clickable_regions.push(ClickableRegion {
            area: Rect::new(
                dialog_area.x + 1,
                dialog_area.y + 6,
                dialog_area.width - 2,
                1,
            ),
            region_type: ClickableRegionType::ThrottleTraffic,
        });
    }
    let dialog = &app.throttle_dialog;
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
            }),
            ..TextPresentation::default()
        },
    );

    clear(f, dialog_area);
    push_panel(app, dialog_area, Layer::Compatibility);
    paint_text(
        &app.component_scene,
        "compatibility",
        f,
        dialog_area,
        paragraph,
    );
}

fn draw_bandwidth_graph(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    let selected = app.get_selected_process().map(|p| {
        (
            p.pid,
            p.name.clone(),
            app.history.get_history(p.pid).cloned(),
        )
    });
    let Some((pid, name, Some(history))) =
        selected.filter(|(_, _, h)| h.as_ref().is_some_and(|h| !h.samples.is_empty()))
    else {
        let (title, message) = match app.get_selected_process() {
            Some(p) => (
                format!("Bandwidth Graph: {} (PID {})", p.name, p.pid),
                "No historical data available yet...",
            ),
            None => (
                "Bandwidth Graph".to_string(),
                "The process is no longer reporting network activity.",
            ),
        };
        let no_data = (
            render::lines(vec![
                line(message),
                line(""),
                line(Span::styled(
                    "Press 'g', 'q' or Esc (or click outside) to close",
                    Style::default().fg(Color::BrightBlack),
                )),
            ]),
            TextPresentation {
                style: Style::default().bg(Color::Black).fg(Color::Yellow),
                chrome: Some(PanelChrome {
                    title: Some(line(title)),
                    style: Style::default().fg(Color::Cyan),
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
        push_panel(app, graph_area, Layer::Graph);
        return;
    };
    let proc_name = name;
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
            proc_name,
            pid,
            ProcessInfo::format_rate(history.max_download_rate()),
            ProcessInfo::format_rate(history.max_upload_rate()),
            ProcessInfo::format_rate(history.avg_download_rate()),
            ProcessInfo::format_rate(history.avg_upload_rate())
        ),
    );

    push_panel(app, graph_area, Layer::Graph);
    // Instructions on the last plot row, left of the "Time (samples)" axis title.
    let inst_area = Rect {
        x: graph_area.x.saturating_add(1),
        y: graph_area.y + graph_area.height.saturating_sub(2),
        width: graph_area.width.saturating_sub(2 + 15),
        height: 1,
    };
    let Some(message) = ["g/q/Esc or click outside to close", "Esc closes"]
        .into_iter()
        .find(|m| display_width(m) <= usize::from(inst_area.width))
    else {
        return;
    };
    let width = display_width(message) as u16;
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

    let preferred_upload_display = backend_info.preferred_upload.as_deref().unwrap_or("Auto");
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

    let preferred_download_display = backend_info.preferred_download.as_deref().unwrap_or("Auto");
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
        .as_deref()
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

/// Rates for one interface in the active traffic view.
fn interface_rates(app: &AppState, process: &ProcessInfo, interface: &str) -> (u64, u64, u64, u64) {
    let Some(stats) = process.interface_stats.get(interface) else {
        return (0, 0, 0, 0);
    };
    let (dl, ul) = match app.traffic_view_mode {
        TrafficViewMode::All => (stats.download_rate, stats.upload_rate),
        TrafficViewMode::Internet => (stats.internet_download_rate, stats.internet_upload_rate),
        TrafficViewMode::Local => (stats.local_download_rate, stats.local_upload_rate),
    };
    (dl, ul, stats.total_download, stats.total_upload)
}

fn traffic_label(mode: TrafficViewMode) -> &'static str {
    match mode {
        TrafficViewMode::All => "All",
        TrafficViewMode::Internet => "Internet",
        TrafficViewMode::Local => "Local",
    }
}

/// Processes on the selected interface, as a large panel over the explorer.
fn draw_interface_detail(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    let panel = centered_rect(90, 80, area);
    let name = app.selected_interface_name.clone().unwrap_or_default();
    let processes: Vec<&ProcessInfo> = app
        .unfiltered_process_list
        .iter()
        .filter(|p| p.interface_stats.contains_key(&name))
        .collect();
    let title = format!(
        "Interface: {} · {} process{} · {} traffic",
        if name.is_empty() { "?" } else { &name },
        processes.len(),
        if processes.len() == 1 { "" } else { "es" },
        traffic_label(app.traffic_view_mode),
    );
    // Totals are dropped (upload first) when the panel is too narrow.
    let inner_width = panel.width.saturating_sub(2);
    let columns = usize::from(inner_width >= 66) + usize::from(inner_width >= 77);
    let items: Vec<bmux_tui_components::selectable_list::SelectableListItem> = processes
        .iter()
        .map(|proc| {
            let (dl_rate, ul_rate, dl_total, ul_total) = interface_rates(app, proc, &name);
            let (status, dim) = if proc.is_throttled() {
                ("⚡", false)
            } else if proc.is_terminated {
                ("✝", true)
            } else {
                (" ", false)
            };
            let color = |c: Color| Style::default().fg(if dim { Color::White } else { c });
            let mut spans = vec![
                Span::styled(
                    explorer::paint::fit(status, 2, false),
                    color(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{:>7} ", proc.pid)),
                Span::styled(
                    format!("{} ", explorer::paint::fit(&proc.name, 20, false)),
                    color(Color::BrightWhite),
                ),
                Span::styled(
                    format!("↓{:>10} ", ProcessInfo::format_rate(dl_rate)),
                    color(Color::Green),
                ),
                Span::styled(
                    format!("↑{:>10} ", ProcessInfo::format_rate(ul_rate)),
                    color(Color::Yellow),
                ),
            ];
            if columns > 0 {
                spans.push(Span::styled(
                    format!("{:>10} ", ProcessInfo::format_bytes(dl_total)),
                    color(Color::Cyan),
                ));
            }
            if columns > 1 {
                spans.push(Span::styled(
                    format!("{:>10} ", ProcessInfo::format_bytes(ul_total)),
                    color(Color::Magenta),
                ));
            }
            let content = line(spans);
            bmux_tui_components::selectable_list::SelectableListItem::rich(
                content.plain_text(),
                content,
            )
        })
        .collect();

    clear(f, panel);
    paint_text(
        &app.component_scene,
        "draw_interface_detail.panel",
        f,
        panel,
        (
            Vec::new(),
            TextPresentation {
                style: Style::default().bg(Color::Black).fg(Color::BrightWhite),
                chrome: Some(PanelChrome {
                    title: Some(line(title)),
                    style: Style::default().fg(Color::Cyan),
                }),
                ..TextPresentation::default()
            },
        ),
    );
    push_panel(app, panel, Layer::InterfaceDetail);
    let inner = Rect::new(
        panel.x.saturating_add(1),
        panel.y.saturating_add(1),
        panel.width.saturating_sub(2),
        panel.height.saturating_sub(2),
    );
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let base = Style::default().bg(Color::Black);
    let bold = base.fg(Color::BrightWhite).add_modifier(Modifier::BOLD);
    let header = [
        "      PID ",
        "Process              ",
        "    DL Rate ",
        "    UL Rate ",
    ]
    .into_iter()
    .chain(["  Total DL ", "  Total UL "].into_iter().take(columns))
    .map(|title| Span::styled(title, bold))
    .collect::<Vec<_>>();
    render::paint_label(
        f,
        Rect::new(inner.x, inner.y, inner.width, 1),
        line(header),
        base,
    );
    let list_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(2),
    );
    if items.is_empty() {
        if list_area.height > 0 {
            render::paint_label(
                f,
                Rect::new(list_area.x, list_area.y, list_area.width, 1),
                line(" No processes have used this interface yet."),
                base.fg(Color::BrightBlack),
            );
        }
    } else if list_area.height > 0 {
        let mut state = std::mem::take(&mut app.interface_detail_state);
        paint_list(
            &app.component_scene,
            "draw_interface_detail",
            f,
            list_area,
            &items,
            Style::default()
                .bg(Color::BrightBlack)
                .add_modifier(Modifier::BOLD),
            &mut state,
        );
        app.interface_detail_state = state;
    }
    if inner.height < 2 {
        return;
    }
    let footer_y = inner.y + inner.height - 1;
    let back = "[◀ Back]";
    let back_width = u16::try_from(display_width(back))
        .unwrap_or(u16::MAX)
        .min(inner.width);
    let back_area = Rect::new(inner.x, footer_y, back_width, 1);
    render::paint_label(
        f,
        back_area,
        line(back),
        base.fg(Color::Yellow).add_modifier(Modifier::BOLD),
    );
    app.clickable_regions.push(ClickableRegion {
        area: back_area,
        region_type: ClickableRegionType::InterfaceBack,
    });
    render::paint_label(
        f,
        Rect::new(inner.x + back_width, footer_y, inner.width - back_width, 1),
        line(" Esc back · ↑↓ scroll · i close · l traffic · b backends · h help"),
        base.fg(Color::BrightBlack),
    );
}

/// Rows above the first interface in the filter modal (inside the border).
pub const INTERFACE_HEADER_LINES: usize = 7;
/// Checkbox columns relative to the modal's left border (after "▶ ").
const INTERFACE_CHECKBOX_COLUMNS: std::ops::Range<u16> = 3..6;

fn draw_interface_modal(f: &mut PaintCx<'_, '_>, area: Rect, app: &mut AppState) {
    app.ensure_interface_cursor();
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
    if app.interface_list.is_empty() {
        text.push(line(Span::styled(
            "  No interfaces detected yet (requires network monitoring).",
            Style::default().fg(Color::BrightBlack),
        )));
    }

    text.push(line(""));
    text.push(line(""));

    text.push(line(Span::styled(
        "↑↓ select · Space/click box toggle · A all/none",
        Style::default().fg(Color::BrightBlack),
    )));
    text.push(line(Span::styled(
        "Enter processes · Esc/i/click outside close · h help",
        Style::default().fg(Color::BrightBlack),
    )));

    let modal_area = centered_rect(70, 60, area);

    // Auto-scroll to keep the selected interface visible.
    if let Some(selected_idx) = app.selected_interface_index {
        let selected_line = INTERFACE_HEADER_LINES + selected_idx;
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
            }),
            ..TextPresentation::default()
        },
    );

    clear(f, modal_area);
    paint_text(&app.component_scene, "interfaces", f, modal_area, widget);
    // Checked indicators are part of the rich BMUX text composition above.
    // Do not overlay another control: that changes glyphs and duplicates hit IDs.
    app.clickable_regions.push(ClickableRegion {
        area: modal_area,
        region_type: ClickableRegionType::InterfaceModal {
            header_lines: INTERFACE_HEADER_LINES,
        },
    });
}

/// What a click at (`x`, `y`) inside the interface modal `area` targets:
/// the interface index and whether it landed on the checkbox.
pub fn interface_click(
    area: Rect,
    header_lines: usize,
    scroll: usize,
    rows: usize,
    x: u16,
    y: u16,
) -> Option<(usize, bool)> {
    let inner_bottom = area.y.saturating_add(area.height).saturating_sub(1);
    if y <= area.y || y >= inner_bottom {
        return None;
    }
    let line = usize::from(y - area.y - 1) + scroll;
    let index = line.checked_sub(header_lines).filter(|i| *i < rows)?;
    let column = x.saturating_sub(area.x);
    Some((index, INTERFACE_CHECKBOX_COLUMNS.contains(&column)))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod regressions;
