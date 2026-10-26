//! BMUX owns scheduling, terminal ingress, and committed presentation. All kernel/config
//! effects are serialized by `services`; update only performs local state transitions.
mod services;
use crate::backends::throttle::{BackendInfo, ThrottleManager};
use crate::{
    config,
    monitor::{MonitorCommand, MonitorUpdate},
    ui::{self, AppState},
};
use anyhow::Result;
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui_runtime::{
    Command, Lifecycle, Program, Runtime, RuntimeConfig, RuntimeError, RuntimeEvent, Subscription,
    SubscriptionKey, TerminalInput, TerminalPresenter, Update,
};
use services::{Effect, Service, ServiceResult, ThrottleRequest};

fn throttle_request(app: &AppState) -> Option<ThrottleRequest> {
    let (download_limit, upload_limit) = app.throttle_dialog.parse_limits()?;
    Some(ThrottleRequest {
        pid: app.throttle_dialog.target_pid?,
        name: app.throttle_dialog.target_name.clone().unwrap_or_default(),
        limit: crate::process::ThrottleLimit {
            download_limit,
            upload_limit,
            traffic_type: app.throttle_dialog.get_traffic_type(),
        },
    })
}
fn queue_preferences(config: &config::Config, effects: &mut Vec<Effect>) -> Result<()> {
    effects.push(Effect::Preferences {
        filters: config.filtered_interfaces.clone(),
        traffic: config.traffic_view_mode,
    });
    Ok(())
}
fn region_allowed(app: &AppState, region: &ui::ClickableRegionType) -> bool {
    if app.show_backend_compatibility_dialog {
        return false;
    }
    if app.show_backend_info {
        return matches!(region, ui::ClickableRegionType::BackendModal { .. });
    }
    if app.show_help || app.show_throttle_dialog || app.show_graph {
        return false;
    }
    if app.view_mode == ui::ViewMode::InterfaceList {
        return matches!(region, ui::ClickableRegionType::InterfaceModal { .. });
    }
    true
}
fn input(
    app: &mut AppState,
    event: Event,
    config: &mut config::Config,
    backend_info: &BackendInfo,
    committed_regions: &[ui::ClickableRegion],
    effects: &mut Vec<Effect>,
) -> bool {
    match event {
        Event::Key(key) => {
            // ALWAYS check Ctrl+C first - force quit regardless of modal state
            if key.modifiers.ctrl && key.key == KeyCode::Char('c') {
                return true;
            }

            // Handle backend compatibility dialog (highest priority)
            if app.show_backend_compatibility_dialog {
                match key.key {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(dialog) = &mut app.backend_compatibility_dialog {
                            dialog.select_previous();
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(dialog) = &mut app.backend_compatibility_dialog {
                            dialog.select_next();
                        }
                    }
                    KeyCode::PageUp => {
                        // Scroll up by 10 lines
                        for _ in 0..10 {
                            app.scroll_backend_compat_up();
                        }
                    }
                    KeyCode::PageDown => {
                        // Scroll down by 10 lines
                        for _ in 0..10 {
                            app.scroll_backend_compat_down();
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(dialog) = &app.backend_compatibility_dialog {
                            let action = dialog.get_action();
                            if !matches!(action, ui::BackendCompatibilityAction::Cancel) {
                                if let Some(request) = throttle_request(app) {
                                    effects.push(Effect::Resolve {
                                        request,
                                        is_upload: dialog.is_upload,
                                        action,
                                    });
                                    app.status_message = "Applying throttle...".into();
                                }
                            }
                            app.show_backend_compatibility_dialog = false;
                            app.backend_compatibility_dialog = None;
                        }
                    }
                    KeyCode::Escape | KeyCode::Char('q') => {
                        // Cancel - close modal, keep throttle dialog open
                        app.show_backend_compatibility_dialog = false;
                        app.backend_compatibility_dialog = None;
                    }
                    _ => {}
                }
                return false;
            }

            // If backend info is shown, Enter switches to selector, Esc/q closes it
            if app.show_backend_info {
                match key.key {
                    KeyCode::Space | KeyCode::Char(' ') => {
                        if let Some((name, group)) = app.get_selected_backend() {
                            effects.push(Effect::SelectBackend {
                                group,
                                name: name.to_string(),
                            });
                            app.status_message = format!("Switching to {}...", name);
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.select_previous_backend();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.select_next_backend();
                    }
                    KeyCode::PageUp => {
                        // Scroll up by 10 lines
                        for _ in 0..10 {
                            app.scroll_backend_info_up();
                        }
                    }
                    KeyCode::PageDown => {
                        // Scroll down by 10 lines
                        for _ in 0..10 {
                            app.scroll_backend_info_down();
                        }
                    }
                    KeyCode::Enter | KeyCode::Char('b') | KeyCode::Char('q') | KeyCode::Escape => {
                        app.show_backend_info = false;
                        app.reset_backend_info_scroll();
                    }
                    _ => {}
                }
                return false;
            }

            // If graph is shown, g/Esc/q closes it
            if app.show_graph {
                match key.key {
                    KeyCode::Char('g') | KeyCode::Char('q') | KeyCode::Escape => {
                        app.show_graph = false;
                    }
                    _ => {}
                }
                return false;
            }

            // Handle throttle dialog input
            if app.show_throttle_dialog {
                match key.key {
                    KeyCode::Escape | KeyCode::Char('q') => {
                        app.show_throttle_dialog = false;
                        app.throttle_dialog.reset();
                    }
                    KeyCode::Tab => {
                        app.throttle_dialog.toggle_field();
                    }
                    KeyCode::Char('t') => {
                        app.throttle_dialog.cycle_traffic_type();
                    }
                    KeyCode::Char(c) if c.is_numeric() => {
                        app.throttle_dialog.handle_char(c);
                    }
                    KeyCode::Backspace => {
                        app.throttle_dialog.handle_backspace();
                    }
                    KeyCode::Enter => {
                        if let Some(request) = throttle_request(app) {
                            effects.push(Effect::Apply(request));
                            app.status_message = "Checking throttle compatibility...".into();
                        } else {
                            app.show_throttle_dialog = false;
                            app.throttle_dialog.reset();
                        }
                    }
                    _ => {}
                }
                return false;
            }

            // If help is shown, handle scroll or close it
            if app.show_help {
                match key.key {
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.scroll_help_up();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.scroll_help_down();
                    }
                    KeyCode::PageUp => {
                        // Scroll up by 10 lines
                        for _ in 0..10 {
                            app.scroll_help_up();
                        }
                    }
                    KeyCode::PageDown => {
                        // Scroll down by 10 lines
                        for _ in 0..10 {
                            app.scroll_help_down();
                        }
                    }
                    _ => {
                        app.show_help = false;
                        app.reset_help_scroll();
                    }
                }
                return false;
            }

            match key.key {
                KeyCode::Char('q') | KeyCode::Escape => {
                    // Special handling for interface views and process detail
                    match app.view_mode {
                        ui::ViewMode::InterfaceDetail => {
                            app.exit_interface_detail();
                            app.status_message = "Back to interface list".to_string();
                        }
                        ui::ViewMode::InterfaceList => {
                            app.view_mode = ui::ViewMode::ProcessView;
                            app.status_message =
                                "Closed interface modal - filter still active".to_string();
                        }
                        ui::ViewMode::ProcessDetail => {
                            app.exit_process_detail();
                            app.status_message = "Back to process list".to_string();
                        }
                        ui::ViewMode::ProcessView => {
                            return true;
                        }
                    }
                }
                KeyCode::Char('h') | KeyCode::Char('?') => {
                    app.reset_help_scroll();
                    app.show_help = true;
                }
                KeyCode::Char('b') => {
                    if !app.show_backend_info {
                        // Build backend items when opening modal
                        let backend_info = backend_info.clone();
                        app.build_backend_items(&backend_info);
                        app.reset_backend_info_scroll();
                        app.show_backend_info = true;
                    } else {
                        app.show_backend_info = false;
                        app.reset_backend_info_scroll();
                    }
                }
                KeyCode::Char('f') => {
                    app.toggle_sort_freeze();
                    app.status_message = if app.sort_frozen {
                        "Sort order frozen ❄️ - Stats continue updating, order preserved"
                            .to_string()
                    } else {
                        "Sort order unfrozen - Dynamic sorting re-enabled".to_string()
                    };
                }
                KeyCode::Char('g') => {
                    app.show_graph = !app.show_graph;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    match app.view_mode {
                        ui::ViewMode::ProcessView => app.select_next(),
                        ui::ViewMode::InterfaceList => app.select_next_interface(),
                        ui::ViewMode::InterfaceDetail => {} // No selection in detail view
                        ui::ViewMode::ProcessDetail => app.scroll_detail_down(),
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    match app.view_mode {
                        ui::ViewMode::ProcessView => app.select_previous(),
                        ui::ViewMode::InterfaceList => app.select_previous_interface(),
                        ui::ViewMode::InterfaceDetail => {} // No selection in detail view
                        ui::ViewMode::ProcessDetail => app.scroll_detail_up(),
                    }
                }
                KeyCode::PageUp => {
                    match app.view_mode {
                        ui::ViewMode::ProcessView => {
                            // Select previous by 10
                            for _ in 0..10 {
                                app.select_previous();
                            }
                        }
                        ui::ViewMode::InterfaceList => {
                            // Scroll interface modal up
                            for _ in 0..10 {
                                app.scroll_interface_modal_up();
                            }
                        }
                        ui::ViewMode::InterfaceDetail => {}
                        ui::ViewMode::ProcessDetail => {
                            // Scroll detail view up by 10 lines
                            for _ in 0..10 {
                                app.scroll_detail_up();
                            }
                        }
                    }
                }
                KeyCode::PageDown => {
                    match app.view_mode {
                        ui::ViewMode::ProcessView => {
                            // Select next by 10
                            for _ in 0..10 {
                                app.select_next();
                            }
                        }
                        ui::ViewMode::InterfaceList => {
                            // Scroll interface modal down
                            for _ in 0..10 {
                                app.scroll_interface_modal_down();
                            }
                        }
                        ui::ViewMode::InterfaceDetail => {}
                        ui::ViewMode::ProcessDetail => {
                            // Scroll detail view down by 10 lines
                            for _ in 0..10 {
                                app.scroll_detail_down();
                            }
                        }
                    }
                }
                KeyCode::Char('i') => {
                    app.toggle_view_mode();
                    app.status_message = match app.view_mode {
                        ui::ViewMode::ProcessView => "Switched to process view".to_string(),
                        ui::ViewMode::InterfaceList => format!(
                            "Switched to interface view ({} interfaces)",
                            app.interface_list.len()
                        ),
                        ui::ViewMode::InterfaceDetail => "Viewing interface details".to_string(),
                        ui::ViewMode::ProcessDetail => "Back to process list".to_string(),
                    };
                }
                KeyCode::Char('l') => {
                    app.toggle_traffic_view_mode();
                    // status_message is set by toggle_traffic_view_mode()

                    // Save traffic view mode to config
                    config.traffic_view_mode = Some(match app.traffic_view_mode {
                        ui::TrafficViewMode::All => crate::process::TrafficType::All,
                        ui::TrafficViewMode::Internet => crate::process::TrafficType::Internet,
                        ui::TrafficViewMode::Local => crate::process::TrafficType::Local,
                    });
                    if let Err(e) = queue_preferences(config, effects) {
                        log::warn!("Failed to save traffic view mode to config: {}", e);
                    }
                }
                KeyCode::Enter => match app.view_mode {
                    ui::ViewMode::InterfaceList => {
                        app.enter_interface_detail();
                        if let Some(iface) = app.get_selected_interface() {
                            app.status_message =
                                format!("Viewing processes on interface: {}", iface.name);
                        }
                    }
                    ui::ViewMode::ProcessView => {
                        if app.get_selected_process().is_some() {
                            app.enter_process_detail();
                            app.status_message = "Viewing process details".to_string();
                        }
                    }
                    _ => {}
                },
                KeyCode::Tab if key.modifiers.shift => {
                    // Shift+Tab switches tabs backwards in process detail view
                    if app.view_mode == ui::ViewMode::ProcessDetail {
                        app.previous_detail_tab();
                        let tab_name = match app.detail_tab {
                            ui::ProcessDetailTab::Overview => "Overview",
                            ui::ProcessDetailTab::Connections => "Connections",
                            ui::ProcessDetailTab::Traffic => "Traffic",
                            ui::ProcessDetailTab::System => "System",
                        };
                        app.status_message = format!("Switched to {} tab", tab_name);
                    }
                }
                KeyCode::Tab => {
                    // Tab switches tabs in process detail view
                    if app.view_mode == ui::ViewMode::ProcessDetail {
                        app.next_detail_tab();
                        let tab_name = match app.detail_tab {
                            ui::ProcessDetailTab::Overview => "Overview",
                            ui::ProcessDetailTab::Connections => "Connections",
                            ui::ProcessDetailTab::Traffic => "Traffic",
                            ui::ProcessDetailTab::System => "System",
                        };
                        app.status_message = format!("Switched to {} tab", tab_name);
                    }
                }
                KeyCode::Space | KeyCode::Char(' ') => {
                    // Space bar toggles filter in interface list view
                    if app.view_mode == ui::ViewMode::InterfaceList {
                        if let Some(iface) = app.get_selected_interface() {
                            let iface_name = iface.name.clone();
                            app.toggle_interface_filter(iface_name);

                            // Save to config immediately
                            config.filtered_interfaces = app.active_interface_filters.clone();
                            if let Err(e) = queue_preferences(config, effects) {
                                log::error!("Failed to save filter config: {}", e);
                            }
                        }
                    }
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    // 'A' - Toggle all/none in interface list view
                    if app.view_mode == ui::ViewMode::InterfaceList {
                        app.toggle_all_interface_filters();

                        // Save to config immediately
                        config.filtered_interfaces = app.active_interface_filters.clone();
                        if let Err(e) = queue_preferences(config, effects) {
                            log::error!("Failed to save filter config: {}", e);
                        }
                    }
                }
                KeyCode::Char('t') => {
                    if let Some(process) = app.get_selected_process() {
                        // Clone the values we need
                        let pid = process.pid;
                        let name = process.name.clone();

                        // Open throttle dialog
                        app.throttle_dialog.target_pid = Some(pid);
                        app.throttle_dialog.target_name = Some(name);
                        app.show_throttle_dialog = true;
                    } else {
                        app.status_message = "No process selected".to_string();
                    }
                }
                KeyCode::Char('r') => {
                    if let Some(process) = app.get_selected_process() {
                        effects.push(Effect::Remove {
                            pid: process.pid,
                            name: process.name.clone(),
                        });
                        app.status_message = "Removing throttle...".into();
                    }
                }
                _ => {}
            }
        }
        Event::Mouse(mouse) => {
            // Handle mouse scroll events
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    // Handle scroll up based on current state (priority: modals first, then view modes)
                    if app.show_backend_compatibility_dialog {
                        app.scroll_backend_compat_up();
                    } else if app.show_backend_info {
                        app.scroll_backend_info_up();
                    } else if app.show_graph || app.show_throttle_dialog {
                        return false;
                    } else if app.show_help {
                        app.scroll_help_up();
                    } else if app.show_throttle_dialog {
                        return false;
                    } else {
                        // Handle based on view mode
                        match app.view_mode {
                            ui::ViewMode::ProcessView => {
                                // Don't scroll lists with mouse - would change selection
                                // Use arrow keys or PageUp/PageDown instead
                            }
                            ui::ViewMode::InterfaceList => {
                                // Interface modal uses Paragraph widget, so scroll it
                                // (auto-scroll keeps selection visible)
                                app.scroll_interface_modal_up();
                            }
                            ui::ViewMode::ProcessDetail => {
                                // Scroll detail view up (3 lines)
                                for _ in 0..3 {
                                    app.scroll_detail_up();
                                }
                            }
                            ui::ViewMode::InterfaceDetail => {
                                // No scrolling needed for interface detail
                            }
                        }
                    }
                }
                MouseEventKind::ScrollDown => {
                    // Handle scroll down based on current state (priority: modals first, then view modes)
                    if app.show_backend_compatibility_dialog {
                        app.scroll_backend_compat_down();
                    } else if app.show_backend_info {
                        app.scroll_backend_info_down();
                    } else if app.show_graph || app.show_throttle_dialog {
                        return false;
                    } else if app.show_help {
                        app.scroll_help_down();
                    } else if app.show_throttle_dialog {
                        return false;
                    } else {
                        // Handle based on view mode
                        match app.view_mode {
                            ui::ViewMode::ProcessView => {
                                // Don't scroll lists with mouse - would change selection
                                // Use arrow keys or PageUp/PageDown instead
                            }
                            ui::ViewMode::InterfaceList => {
                                // Interface modal uses Paragraph widget, so scroll it
                                // (auto-scroll keeps selection visible)
                                app.scroll_interface_modal_down();
                            }
                            ui::ViewMode::ProcessDetail => {
                                // Scroll detail view down (3 lines)
                                for _ in 0..3 {
                                    app.scroll_detail_down();
                                }
                            }
                            ui::ViewMode::InterfaceDetail => {
                                // No scrolling needed for interface detail
                            }
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // Handle left mouse click for selection
                    let click_x = mouse.position.x;
                    let click_y = mouse.position.y;

                    // Check each clickable region
                    for region in committed_regions.iter().rev() {
                        if !region_allowed(app, &region.region_type) {
                            continue;
                        }
                        // Check if click is within region bounds
                        if click_x >= region.area.x
                            && click_x < region.area.x + region.area.width
                            && click_y >= region.area.y
                            && click_y < region.area.y + region.area.height
                        {
                            match &region.region_type {
                                ui::ClickableRegionType::ProcessList {
                                    first_visible_index,
                                    visible_count: _,
                                } => {
                                    // Only handle process list clicks when it's actually visible
                                    // (not covered by any overlays/modals)
                                    if app.view_mode != ui::ViewMode::ProcessView
                                        || app.show_backend_info
                                        || app.show_help
                                        || app.show_throttle_dialog
                                        || app.show_graph
                                        || app.show_backend_compatibility_dialog
                                    {
                                        return false; // Skip this region, check next one
                                    }

                                    // Calculate which list item was clicked
                                    // inner_list_area.y doesn't include border, but List widget does
                                    // Remove the +1 offset that was causing off-by-one error
                                    let relative_y = click_y.saturating_sub(region.area.y);
                                    let clicked_index = first_visible_index + relative_y as usize;

                                    // Validate index is within bounds
                                    if clicked_index < app.process_list.len() {
                                        app.selected_index = Some(clicked_index);
                                        app.list_state.select(Some(clicked_index));
                                    }
                                }
                                ui::ClickableRegionType::ProcessDetailTabs { tab_ranges } => {
                                    // Only handle tab clicks in ProcessDetail mode
                                    if app.view_mode != ui::ViewMode::ProcessDetail {
                                        return false;
                                    }

                                    // Find which tab was clicked based on column
                                    for (start_col, end_col, tab) in tab_ranges {
                                        if click_x >= *start_col && click_x < *end_col {
                                            app.detail_tab = *tab;
                                            app.detail_scroll_offset = 0; // Reset scroll when switching tabs
                                            break;
                                        }
                                    }
                                }
                                ui::ClickableRegionType::InterfaceModal { header_lines } => {
                                    // Only handle interface modal clicks when modal is open
                                    if app.view_mode != ui::ViewMode::InterfaceList {
                                        return false;
                                    }

                                    // Calculate which interface was clicked
                                    // Account for border and header lines
                                    let relative_y = click_y.saturating_sub(region.area.y + 1);

                                    if relative_y >= *header_lines as u16 {
                                        let clicked_index =
                                            (relative_y - *header_lines as u16) as usize;

                                        // Validate index is within bounds
                                        if clicked_index < app.interface_list.len() {
                                            app.selected_interface_index = Some(clicked_index);
                                            // Don't update last_interface_selection here
                                            // Let the auto-scroll code detect the change on next frame
                                        }
                                    }
                                }
                                ui::ClickableRegionType::BackendModal { line_to_item } => {
                                    // Only handle backend modal clicks when modal is open
                                    if !app.show_backend_info {
                                        return false;
                                    }

                                    // Calculate which line was clicked (accounting for scroll)
                                    let relative_y = click_y.saturating_sub(region.area.y + 1);
                                    let visual_line =
                                        relative_y as usize + app.backend_info_scroll_offset;

                                    // Look up which backend item this line corresponds to
                                    if let Some(&item_index) = line_to_item.get(&visual_line) {
                                        // Make sure it's a backend (not a group header)
                                        if matches!(
                                            app.backend_items.get(item_index),
                                            Some(ui::BackendSelectorItem::Backend { .. })
                                        ) {
                                            app.backend_selected_index = item_index;
                                            // Don't update last_backend_selection here
                                            // Let the auto-scroll code detect the change on next frame
                                        }
                                    }
                                }
                            }

                            // Found matching region, stop searching
                            break;
                        }
                    }
                }
                _ => {
                    // Ignore other mouse events (moves, etc.)
                }
            }
        }
        _ => {
            // Ignore other event types
        }
    }
    false
}

enum Message {
    Start,
    InputFailed(String),
    Monitor(MonitorUpdate),
    MonitorClosed,
    Effect(ServiceResult, u64, Option<i32>),
    EffectFailed(String),
    Details {
        generation: u64,
        pid: i32,
        details: crate::process::ProcessDetails,
    },
}
struct Application {
    app: AppState,
    config: config::Config,
    backend: Option<BackendInfo>,
    service: Service,
    monitor: Option<tokio::sync::mpsc::Receiver<MonitorUpdate>>,
    committed_regions: Vec<ui::ClickableRegion>,
    router: bmux_tui::interaction::InteractionRouter,
    throttles: std::collections::HashMap<i32, crate::backends::ActiveThrottle>,
    receipts: OrderedReceipts<(ServiceResult, u64, Option<i32>)>,
    dialog_generation: u64,
    in_flight_effects: usize,
    detail_generation: u64,
    detail_pid: Option<i32>,
}
impl Application {
    fn dialog_identity(&self) -> (bool, Option<i32>, Option<String>, String, String, usize) {
        let dialog = &self.app.throttle_dialog;
        (
            self.app.show_throttle_dialog,
            dialog.target_pid,
            dialog.target_name.clone(),
            dialog.download_input.clone(),
            dialog.upload_input.clone(),
            dialog.traffic_type_index,
        )
    }
    fn semantic_tab(&mut self, event: &Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };
        if key.key != KeyCode::Tab
            || self.app.show_backend_compatibility_dialog
            || self.app.show_backend_info
            || self.app.show_graph
        {
            return false;
        }
        if self.app.show_throttle_dialog {
            self.app.throttle_dialog.toggle_field();
            let id = match self.app.throttle_dialog.selected_field {
                ui::ThrottleField::Download => "throttle.download.input",
                ui::ThrottleField::Upload => "throttle.upload.input",
            };
            self.router.set_focused(&bmux_tui::hit::HitId::new(id));
            return true;
        }
        if self.app.show_help {
            return false;
        }
        if self.app.view_mode == ui::ViewMode::ProcessDetail {
            if key.modifiers.shift {
                self.app.previous_detail_tab();
            } else {
                self.app.next_detail_tab();
            }
            return true;
        }
        false
    }
    fn effect(&mut self, effect: Effect, update: &mut Update<Message>) -> Result<()> {
        // Bound accepted-but-unconsumed receipts as well as the service's FIFO.
        if self.in_flight_effects >= 32 {
            self.app.status_message =
                "Failed to queue action: service busy; retry after pending work completes".into();
            return Ok(());
        }
        // Admission happens now, not when command futures happen to be polled.
        let reply = match self.service.request(effect) {
            Ok(reply) => reply,
            Err(error) => {
                self.app.status_message = format!("Failed to queue action: {error:#}");
                return Ok(());
            }
        };
        self.in_flight_effects += 1;
        let dialog_generation = self.dialog_generation;
        let dialog_pid = self.app.throttle_dialog.target_pid;
        update.commands.push(Command::concurrent(async move {
            Some(match reply.await {
                Ok(result) => Message::Effect(result, dialog_generation, dialog_pid),
                Err(error) => Message::EffectFailed(error.to_string()),
            })
        }));
        Ok(())
    }
    fn refresh_details(&mut self, update: &mut Update<Message>, force: bool) {
        let pid = self.app.selected_process_detail_pid;
        if !force && pid == self.detail_pid {
            return;
        }
        if pid != self.detail_pid {
            self.app.update_process_details(None);
        }
        self.detail_pid = pid;
        self.detail_generation += 1;
        if let Some(pid) = pid {
            let generation = self.detail_generation;
            update.commands.push(Command::replace(
                bmux_tui_runtime::CommandKey::new("process-details"),
                async move {
                    let details = tokio::task::spawn_blocking(move || {
                        crate::process::ProcessDetails::from_pid(pid)
                    })
                    .await
                    .ok()?;
                    Some(Message::Details {
                        generation,
                        pid,
                        details,
                    })
                },
            ));
        }
    }
}
impl Program for Application {
    type Message = Message;
    type Error = anyhow::Error;
    fn update(&mut self, event: RuntimeEvent<Message>) -> Result<Update<Message>> {
        let mut update = Update::redraw();
        match event {
            RuntimeEvent::Message(Message::Start) => {
                let mut receiver = self.monitor.take().expect("start once");
                update.subscriptions.push(Subscription::new(
                    SubscriptionKey::new("monitor"),
                    move |sender| async move {
                        while let Some(update) = receiver.recv().await {
                            // Reliable FIFO: never coalesce away a bandwidth-history sample.
                            if sender.send(Message::Monitor(update)).await.is_err() {
                                return;
                            }
                        }
                        let _ = sender.send(Message::MonitorClosed).await;
                    },
                ));
                self.effect(Effect::Refresh, &mut update)?;
            }
            RuntimeEvent::Terminal(event) => {
                if self.semantic_tab(&event) {
                    return Ok(update);
                }
                let identity = self.dialog_identity();
                let route = self.router.route(event.clone());
                let force_quit = matches!(&event, Event::Key(key) if key.key == KeyCode::Char('c') && key.modifiers.ctrl);
                if !force_quit && self.app.handle_component_event(&route) {
                    if identity != self.dialog_identity() {
                        self.dialog_generation += 1;
                    }
                    self.refresh_details(&mut update, false);
                    return Ok(update);
                }
                {
                    let mut effects = Vec::new();
                    // Input before the first service snapshot still supports quit/resize.
                    if let Some(backend) = &self.backend {
                        if input(
                            &mut self.app,
                            event,
                            &mut self.config,
                            backend,
                            &self.committed_regions,
                            &mut effects,
                        ) {
                            update.lifecycle = Lifecycle::Exit;
                        }
                    } else if matches!(event, Event::Key(key) if key.key == KeyCode::Char('q') || (key.key == KeyCode::Char('c') && key.modifiers.ctrl))
                    {
                        update.lifecycle = Lifecycle::Exit;
                    }
                    if identity != self.dialog_identity() {
                        self.dialog_generation += 1;
                    }
                    for effect in effects {
                        self.effect(effect, &mut update)?;
                    }
                    self.refresh_details(&mut update, false);
                }
            }
            RuntimeEvent::Message(Message::Monitor(mut data)) => {
                for (pid, process) in &mut data.process_map {
                    if let Some(throttle) = self.throttles.get(pid) {
                        process.throttle_limit = Some(crate::process::ThrottleLimit {
                            download_limit: throttle.download_limit,
                            upload_limit: throttle.upload_limit,
                            traffic_type: crate::process::TrafficType::All,
                        });
                    }
                    self.app.history.update_at(
                        data.acquired_at,
                        *pid,
                        process.name.clone(),
                        process.download_rate,
                        process.upload_rate,
                    );
                }
                if let Some(backend) = &mut self.backend {
                    backend.active_socket_mapper = Some(data.socket_mapper_name);
                    backend.socket_mapper_capabilities = Some(data.socket_mapper_capabilities);
                }
                self.app.update_processes(data.process_map);
                self.app.update_interfaces(data.interface_map);
                if !self.app.status_message.starts_with("Throttle")
                    && !self.app.status_message.starts_with("Failed")
                {
                    self.app.status_message = format!(
                        "Monitoring {} process(es) on {} interface(s)",
                        self.app.process_list.len(),
                        self.app.interface_list.len()
                    );
                }
                self.refresh_details(&mut update, true);
            }
            RuntimeEvent::Message(Message::Effect(result, generation, dialog_pid)) => {
                self.in_flight_effects = self.in_flight_effects.saturating_sub(1);
                self.receipts
                    .insert(result.sequence, (result, generation, dialog_pid));
                while let Some((result, generation, dialog_pid)) = self.receipts.pop() {
                    self.throttles = result.throttles;
                    self.backend = Some(result.backend);
                    if let Some(status) = result.status {
                        self.app.status_message = status;
                    }
                    if generation == self.dialog_generation
                        && dialog_pid == self.app.throttle_dialog.target_pid
                    {
                        if let Some(dialog) = result.dialog {
                            if self.app.show_throttle_dialog {
                                self.app.backend_compatibility_dialog = Some(dialog);
                                self.app.show_backend_compatibility_dialog = true;
                            }
                        }
                        if result.close_throttle {
                            self.app.show_throttle_dialog = false;
                            self.app.throttle_dialog.reset();
                        }
                    }
                    if self.app.show_backend_info {
                        self.app.build_backend_items(self.backend.as_ref().unwrap());
                    }
                }
            }
            RuntimeEvent::Message(Message::Details {
                generation,
                pid,
                details,
            }) => {
                if generation != self.detail_generation
                    || Some(pid) != self.app.selected_process_detail_pid
                {
                    return Ok(Update::none());
                }
                self.app.update_process_details(Some(details));
            }
            RuntimeEvent::Message(Message::MonitorClosed) => {
                anyhow::bail!("monitor update channel closed")
            }
            RuntimeEvent::Message(Message::InputFailed(error) | Message::EffectFailed(error)) => {
                anyhow::bail!("{error}")
            }
            RuntimeEvent::Timer(_) => return Ok(Update::none()),
        }
        Ok(update)
    }
}

pub(super) async fn run(
    app: AppState,
    manager: ThrottleManager,
    config: config::Config,
    monitor: tokio::sync::mpsc::Receiver<MonitorUpdate>,
    monitor_commands: tokio::sync::mpsc::UnboundedSender<MonitorCommand>,
    monitor_thread: std::thread::JoinHandle<()>,
    save: bool,
    socket_mapper: Option<String>,
) -> Result<()> {
    let setup = (|| -> Result<_> {
        let size = bmux_tui::crossterm::terminal_size()?;
        let guard = bmux_tui::crossterm::CrosstermTerminalGuard::enter(std::io::stdout())?;
        Ok((guard, size))
    })();
    let (guard, size) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            drop(monitor);
            let _ = monitor_commands.send(MonitorCommand::Shutdown);
            let cleanup = tokio::task::spawn_blocking(move || {
                let mut manager = manager;
                let cleanup = manager.cleanup();
                let joined = monitor_thread
                    .join()
                    .map_err(|_| anyhow::anyhow!("monitor thread panicked during startup cleanup"));
                cleanup.and(joined)
            })
            .await;
            return Err(error.context(format!("startup cleanup: {cleanup:?}")));
        }
    };
    let service = Service::start(
        manager,
        config.clone(),
        monitor_commands.clone(),
        save,
        socket_mapper,
    );
    let program = Application {
        app,
        config,
        backend: None,
        service,
        monitor: Some(monitor),
        committed_regions: Vec::new(),
        router: bmux_tui::interaction::InteractionRouter::new(),
        throttles: Default::default(),
        receipts: OrderedReceipts::default(),
        dialog_generation: 0,
        in_flight_effects: 0,
        detail_generation: 0,
        detail_pid: None,
    };
    let terminal = bmux_tui::terminal::Terminal::new(
        std::io::stdout(),
        bmux_tui::geometry::Rect::new(0, 0, size.width, size.height),
    );
    let presenter = TerminalPresenter::with_commit(
        terminal,
        |program: &mut Application, cx: &mut bmux_tui::paint::PaintCx<'_, '_>| {
            ui::draw_ui_with_backend_info(cx, &mut program.app, program.backend.as_ref());
        },
        |program: &mut Application,
         hits: &bmux_tui::hit::HitMap,
         focus: &bmux_tui::focus::FocusTrap| {
            program.app.commit_component_scene();
            program
                .router
                .commit_scene(hits.clone(), focus.active_scope().cloned());
            program.committed_regions = program.app.clickable_regions.clone();
        },
    );
    let (runtime, handle) = Runtime::new(program, presenter, RuntimeConfig::default());
    let mut input = TerminalInput::start::<Application>(handle.clone(), |error| {
        Message::InputFailed(error.to_string())
    });
    let _ = handle.send(Message::Start).await;
    let result = runtime.run().await;
    // Never restore the terminal while a managed reader still owns stdin.
    input.shutdown().await;
    let (mut output, runtime_error) = match result {
        Ok(output) => (output, None),
        Err(RuntimeError::Program {
            error,
            cleanup_error,
            output,
        }) => (
            output,
            Some(anyhow::anyhow!(
                "runtime: {error:#}; restoration: {cleanup_error:?}"
            )),
        ),
        Err(RuntimeError::Presenter {
            error,
            cleanup_error,
            output,
        }) => (
            output,
            Some(anyhow::anyhow!(
                "presentation: {error}; restoration: {cleanup_error:?}"
            )),
        ),
    };
    // Start may never have run (for example, initial presentation failed). The
    // runtime has already stopped/joined subscriptions; drop any receiver still
    // owned by the program too, before effects or monitor joins can wait on a
    // producer blocked in blocking_send against a full bounded queue.
    drop(output.program.monitor.take());
    let shutdown = output
        .program
        .service
        .finish(Effect::Shutdown {
            filters: output.program.app.active_interface_filters.clone(),
            traffic: output.program.config.traffic_view_mode,
        })
        .await;
    let save_error = match shutdown {
        Ok(result) => result
            .status
            .filter(|s| s.starts_with("Failed"))
            .map(anyhow::Error::msg),
        Err(error) => Some(error),
    };
    let service_result = output.program.service.join();
    let _ = monitor_commands.send(MonitorCommand::Shutdown);
    let monitor_result = match tokio::task::spawn_blocking(move || monitor_thread.join()).await {
        Ok(result) => result.map_err(|_| anyhow::anyhow!("monitor thread panicked")),
        Err(error) => Err(anyhow::anyhow!("monitor join task: {error}")),
    };
    drop(output);
    let restoration = guard.leave();
    let mut failures = Vec::new();
    if let Some(error) = runtime_error {
        failures.push(format!("{error:#}"));
    }
    if let Err(error) = restoration {
        failures.push(format!("terminal restoration: {error}"));
    }
    if let Some(error) = save_error {
        failures.push(format!("save: {error:#}"));
    }
    if let Err(error) = service_result {
        failures.push(format!("service: {error:#}"));
    }
    if let Err(error) = monitor_result {
        failures.push(format!("monitor: {error:#}"));
    }
    if !failures.is_empty() {
        anyhow::bail!("{}", failures.join("; "));
    }
    Ok(())
}

/// Mutation receipts are delivered reliably, but concurrent command futures may complete
/// in a different order. Retain every semantic result, not just the newest snapshot.
struct OrderedReceipts<T> {
    next: u64,
    pending: std::collections::BTreeMap<u64, T>,
}
impl<T> Default for OrderedReceipts<T> {
    fn default() -> Self {
        Self {
            next: 1,
            pending: Default::default(),
        }
    }
}
impl<T> OrderedReceipts<T> {
    fn insert(&mut self, sequence: u64, result: T) {
        if sequence >= self.next {
            self.pending.entry(sequence).or_insert(result);
        }
    }
    fn pop(&mut self) -> Option<T> {
        let result = self.pending.remove(&self.next)?;
        self.next += 1;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn backend() -> BackendInfo {
        BackendInfo {
            active_upload: None,
            active_download: None,
            active_monitoring: None,
            active_socket_mapper: None,
            available_upload: vec![],
            available_download: vec![],
            available_socket_mappers: vec![],
            preferred_upload: None,
            preferred_download: None,
            preferred_socket_mapper: None,
            upload_capabilities: None,
            download_capabilities: None,
            socket_mapper_capabilities: None,
            backend_stats: Default::default(),
        }
    }
    fn application() -> Application {
        Application {
            app: AppState::new(),
            config: config::Config::default(),
            backend: Some(backend()),
            service: Service::disconnected_for_test(),
            monitor: None,
            committed_regions: vec![],
            router: Default::default(),
            throttles: Default::default(),
            receipts: Default::default(),
            dialog_generation: 0,
            in_flight_effects: 0,
            detail_generation: 0,
            detail_pid: None,
        }
    }
    fn key(key: KeyCode) -> RuntimeEvent<Message> {
        RuntimeEvent::Terminal(Event::Key(bmux_keyboard::KeyStroke::simple(key)))
    }
    fn paint_commit(program: &mut Application) {
        let mut terminal = bmux_tui::terminal::Terminal::new(
            Vec::<u8>::new(),
            bmux_tui::geometry::Rect::new(0, 0, 120, 40),
        );
        terminal
            .draw(|cx| {
                ui::draw_ui_with_backend_info(cx, &mut program.app, program.backend.as_ref())
            })
            .unwrap();
        program.app.commit_component_scene();
        program.router.commit_scene(
            terminal.hits().clone(),
            terminal.focus().active_scope().cloned(),
        );
        program.committed_regions = program.app.clickable_regions.clone();
    }
    #[test]
    fn committed_interface_space_toggles_selected_filter_for_both_backends() {
        for space in [KeyCode::Space, KeyCode::Char(' ')] {
            let mut program = application();
            for name in ["en0", "lo0"] {
                program
                    .app
                    .interface_list
                    .push(crate::process::InterfaceInfo {
                        name: name.into(),
                        mac_address: None,
                        ip_addresses: vec![],
                        is_up: true,
                        is_loopback: name == "lo0",
                        total_download_rate: 0,
                        total_upload_rate: 0,
                        process_count: 0,
                    });
            }
            program.update(key(KeyCode::Char('i'))).unwrap();
            assert_eq!(program.app.view_mode, ui::ViewMode::InterfaceList);
            paint_commit(&mut program);

            program.update(key(space)).unwrap();
            assert!(!program.app.is_interface_filtered("en0"), "{space:?}");
            assert!(program.app.is_interface_filtered("lo0"), "{space:?}");
            assert_eq!(
                program.config.filtered_interfaces,
                program.app.active_interface_filters
            );

            paint_commit(&mut program);
            program.update(key(space)).unwrap();
            assert_eq!(program.app.active_interface_filters, None, "{space:?}");
            program.update(key(KeyCode::Char('A'))).unwrap();
            assert_eq!(program.app.active_interface_filters, Some(vec![]));
            paint_commit(&mut program);
            program.update(key(space)).unwrap();
            assert!(program.app.is_interface_filtered("en0"), "{space:?}");
            assert!(!program.app.is_interface_filtered("lo0"), "{space:?}");
        }
    }

    #[test]
    fn committed_detail_scene_preserves_semantic_tab_and_shift_tab() {
        let mut program = application();
        program.app.view_mode = ui::ViewMode::ProcessDetail;
        program.app.detail_tab = ui::ProcessDetailTab::Overview;
        paint_commit(&mut program);
        program.update(key(KeyCode::Tab)).unwrap();
        assert_eq!(program.app.detail_tab, ui::ProcessDetailTab::Connections);
        paint_commit(&mut program);
        let mut modifiers = bmux_keyboard::Modifiers::NONE;
        modifiers.shift = true;
        program
            .update(RuntimeEvent::Terminal(Event::Key(
                bmux_keyboard::KeyStroke::with_modifiers(KeyCode::Tab, modifiers),
            )))
            .unwrap();
        assert_eq!(program.app.detail_tab, ui::ProcessDetailTab::Overview);
    }
    #[test]
    fn committed_throttle_tab_syncs_field_focus_without_invalidating_pending_receipt() {
        let mut program = application();
        program.app.show_throttle_dialog = true;
        program.app.throttle_dialog.target_pid = Some(10);
        paint_commit(&mut program);
        program.update(key(KeyCode::Tab)).unwrap();
        assert_eq!(
            program.app.throttle_dialog.selected_field,
            ui::ThrottleField::Upload
        );
        assert_eq!(
            program.router.focused(),
            Some(&bmux_tui::hit::HitId::new("throttle.upload.input"))
        );
        assert_eq!(program.dialog_generation, 0);
        paint_commit(&mut program);
        program.update(key(KeyCode::Left)).unwrap();
        assert_eq!(program.dialog_generation, 0);
        program
            .update(RuntimeEvent::Message(Message::Effect(
                ServiceResult {
                    sequence: 1,
                    status: None,
                    dialog: None,
                    close_throttle: true,
                    backend: backend(),
                    throttles: Default::default(),
                },
                0,
                Some(10),
            )))
            .unwrap();
        assert!(!program.app.show_throttle_dialog);
    }
    #[test]
    fn committed_throttle_tab_then_digit_without_repaint_edits_new_field() {
        let mut program = application();
        program.app.show_throttle_dialog = true;
        program.app.throttle_dialog.target_pid = Some(10);
        program.app.throttle_dialog.selected_field = ui::ThrottleField::Download;
        paint_commit(&mut program);

        program.update(key(KeyCode::Tab)).unwrap();
        assert_eq!(
            program.router.focused(),
            Some(&bmux_tui::hit::HitId::new("throttle.upload.input"))
        );
        // Deliberately do not paint/commit between these two admitted events:
        // the committed layout still describes Download as selected.
        program.update(key(KeyCode::Char('7'))).unwrap();
        assert_eq!(program.app.throttle_dialog.download_input, "");
        assert_eq!(program.app.throttle_dialog.upload_input, "7");
        assert_eq!(
            program.app.throttle_dialog.selected_field,
            ui::ThrottleField::Upload
        );
        assert_eq!(program.dialog_generation, 1);
    }
    #[test]
    fn committed_throttle_edit_invalidates_pending_receipt_but_cursor_navigation_does_not() {
        let mut program = application();
        program.app.show_throttle_dialog = true;
        program.app.throttle_dialog.target_pid = Some(10);
        paint_commit(&mut program);
        program.update(key(KeyCode::Char('1'))).unwrap();
        assert_eq!(program.dialog_generation, 1);
        program
            .update(RuntimeEvent::Message(Message::Effect(
                ServiceResult {
                    sequence: 1,
                    status: None,
                    dialog: None,
                    close_throttle: true,
                    backend: backend(),
                    throttles: Default::default(),
                },
                0,
                Some(10),
            )))
            .unwrap();
        assert!(program.app.show_throttle_dialog);
    }
    #[test]
    fn receipts_keep_earlier_errors_and_dialogs_before_later_snapshots() {
        let mut receipts = OrderedReceipts::default();
        receipts.insert(3, "refresh");
        receipts.insert(2, "dialog");
        assert_eq!(receipts.pop(), None);
        receipts.insert(1, "error");
        assert_eq!(receipts.pop(), Some("error"));
        assert_eq!(receipts.pop(), Some("dialog"));
        assert_eq!(receipts.pop(), Some("refresh"));
        receipts.insert(1, "stale");
        assert_eq!(receipts.pop(), None);
    }
    #[test]
    fn modal_escape_closes_top_scope_before_quitting() {
        let mut program = application();
        program.app.show_help = true;
        assert_eq!(
            program.update(key(KeyCode::Escape)).unwrap().lifecycle,
            Lifecycle::Continue
        );
        assert!(!program.app.show_help);
        assert_eq!(
            program.update(key(KeyCode::Escape)).unwrap().lifecycle,
            Lifecycle::Exit
        );
    }
    #[test]
    fn shift_tab_precedes_tab_and_ctrl_c_precedes_modals() {
        let mut program = application();
        program.app.view_mode = ui::ViewMode::ProcessDetail;
        program.app.detail_tab = ui::ProcessDetailTab::Overview;
        let mut modifiers = bmux_keyboard::Modifiers::NONE;
        modifiers.shift = true;
        program
            .update(RuntimeEvent::Terminal(Event::Key(
                bmux_keyboard::KeyStroke::with_modifiers(KeyCode::Tab, modifiers),
            )))
            .unwrap();
        assert_eq!(program.app.detail_tab, ui::ProcessDetailTab::System);
        program.app.show_help = true;
        modifiers.shift = false;
        modifiers.ctrl = true;
        let update = program
            .update(RuntimeEvent::Terminal(Event::Key(
                bmux_keyboard::KeyStroke::with_modifiers(KeyCode::Char('c'), modifiers),
            )))
            .unwrap();
        assert_eq!(update.lifecycle, Lifecycle::Exit);
    }
    #[test]
    fn stale_process_details_cannot_replace_new_selection() {
        let mut program = application();
        program.detail_generation = 2;
        program.app.selected_process_detail_pid = Some(20);
        let details = crate::process::ProcessDetails::from_pid(-1);
        let update = program
            .update(RuntimeEvent::Message(Message::Details {
                generation: 1,
                pid: 10,
                details,
            }))
            .unwrap();
        assert_eq!(update.invalidation, bmux_tui_runtime::Invalidation::None);
        assert!(program.app.cached_process_details.is_none());
    }
    #[test]
    fn old_throttle_receipt_does_not_touch_reopened_dialog() {
        let mut program = application();
        program.app.show_throttle_dialog = true;
        program.app.throttle_dialog.target_pid = Some(10);
        program.update(key(KeyCode::Escape)).unwrap();
        program.app.show_throttle_dialog = true;
        program.app.throttle_dialog.target_pid = Some(20);
        let dialog = ui::BackendCompatibilityDialog::new(
            "old".into(),
            crate::process::TrafficType::Local,
            vec![],
            true,
        );
        program
            .update(RuntimeEvent::Message(Message::Effect(
                ServiceResult {
                    sequence: 1,
                    status: Some("Failed old action".into()),
                    dialog: Some(dialog),
                    close_throttle: true,
                    backend: backend(),
                    throttles: Default::default(),
                },
                0,
                Some(10),
            )))
            .unwrap();
        assert!(program.app.show_throttle_dialog);
        assert_eq!(program.app.throttle_dialog.target_pid, Some(20));
        assert!(!program.app.show_backend_compatibility_dialog);
        assert_eq!(program.app.status_message, "Failed old action");
    }
    #[test]
    fn same_pid_refresh_keeps_cached_details_until_receipt() {
        let mut program = application();
        program.detail_pid = Some(10);
        program.app.selected_process_detail_pid = Some(10);
        let mut details = crate::process::ProcessDetails::from_pid(-1);
        details.pid = 10;
        program.app.update_process_details(Some(details));
        let mut update = Update::none();
        program.refresh_details(&mut update, true);
        assert_eq!(program.app.cached_process_details.as_ref().unwrap().pid, 10);
        program.app.selected_process_detail_pid = Some(20);
        program.refresh_details(&mut update, false);
        assert!(program.app.cached_process_details.is_none());
    }
    #[test]
    fn queued_monitor_samples_keep_acquisition_timestamps_and_all_history_updates() {
        let mut program = application();
        for (timestamp, rate) in [(100, 10), (101, 20), (102, 30)] {
            let mut process = crate::process::ProcessInfo::new(7, "sample".into());
            process.download_rate = rate;
            program
                .update(RuntimeEvent::Message(Message::Monitor(
                    crate::monitor::MonitorUpdateData {
                        acquired_at: timestamp,
                        process_map: [(7, process)].into_iter().collect(),
                        interface_map: Default::default(),
                        socket_mapper_name: "test".into(),
                        socket_mapper_capabilities: crate::backends::BackendCapabilities {
                            ipv4_support: true,
                            ipv6_support: true,
                            per_process: true,
                            per_connection: false,
                        },
                    },
                )))
                .unwrap();
        }
        let samples = &program.app.history.get_history(7).unwrap().samples;
        assert_eq!(
            samples
                .iter()
                .map(|sample| (sample.timestamp, sample.download_rate))
                .collect::<Vec<_>>(),
            vec![(100, 10), (101, 20), (102, 30)]
        );
    }
    #[test]
    fn rejected_service_admission_does_not_exit_runtime() {
        let mut program = application();
        let mut update = Update::none();
        program.effect(Effect::Refresh, &mut update).unwrap();
        assert!(update.commands.is_empty());
        assert_eq!(update.lifecycle, Lifecycle::Continue);
        assert!(program.app.status_message.starts_with("Failed to queue"));
    }
}
