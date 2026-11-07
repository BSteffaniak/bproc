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
fn save_interface_filters(app: &AppState, config: &mut config::Config, effects: &mut Vec<Effect>) {
    config.filtered_interfaces = app.active_interface_filters.clone();
    if let Err(e) = queue_preferences(config, effects) {
        log::error!("Failed to save filter config: {}", e);
    }
}
/// Rows in the interface detail list (processes using the selected interface).
fn interface_detail_rows(app: &AppState) -> usize {
    let Some(name) = &app.selected_interface_name else {
        return 0;
    };
    app.unfiltered_process_list
        .iter()
        .filter(|p| p.interface_stats.contains_key(name))
        .count()
}
/// Move the interface detail selection by `delta` rows (clamped, no wrap).
fn move_interface_detail(app: &mut AppState, delta: isize) {
    let rows = interface_detail_rows(app);
    if rows == 0 {
        app.interface_detail_state.select(None);
        return;
    }
    let next = match app.interface_detail_state.selected() {
        None => 0,
        Some(index) => index.saturating_add_signed(delta).min(rows - 1),
    };
    app.interface_detail_state.select(Some(next));
}
/// Left click while a legacy surface owns input. Regions come from the last
/// committed frame; clicks outside the topmost panel dismiss it.
fn click(
    app: &mut AppState,
    x: u16,
    y: u16,
    config: &mut config::Config,
    committed_regions: &[ui::ClickableRegion],
    effects: &mut Vec<Effect>,
) {
    let Some(layer) = app.top_layer() else {
        return;
    };
    let contains = |area: &ui::Rect| {
        x >= area.x
            && y >= area.y
            && u32::from(x) < u32::from(area.x) + u32::from(area.width)
            && u32::from(y) < u32::from(area.y) + u32::from(area.height)
    };
    let mut painted = false;
    let mut target = None;
    for region in committed_regions.iter().rev() {
        if region.region_type.layer() != layer {
            continue;
        }
        painted = true;
        if contains(&region.area) {
            target = Some(region);
            break;
        }
    }
    let Some(region) = target else {
        if painted && layer.dismissible() {
            app.dismiss(layer);
        }
        return;
    };
    match &region.region_type {
        ui::ClickableRegionType::InterfaceModal { header_lines } => {
            if let Some((index, on_box)) = ui::interface_click(
                region.area,
                *header_lines,
                app.interface_modal_scroll_offset,
                app.interface_list.len(),
                x,
                y,
            ) {
                app.selected_interface_index = Some(index);
                app.interface_list_state.select(Some(index));
                if on_box {
                    let name = app.interface_list[index].name.clone();
                    app.toggle_interface_filter(name);
                    save_interface_filters(app, config, effects);
                }
            }
        }
        ui::ClickableRegionType::BackendModal { line_to_item } => {
            let relative_y = y.saturating_sub(region.area.y.saturating_add(1));
            let visual_line = usize::from(relative_y) + app.backend_info_scroll_offset;
            if let Some(&item_index) = line_to_item.get(&visual_line)
                && matches!(
                    app.backend_items.get(item_index),
                    Some(ui::BackendSelectorItem::Backend { .. })
                )
            {
                app.backend_selected_index = item_index;
            }
        }
        ui::ClickableRegionType::InterfaceBack => app.dismiss(ui::Layer::InterfaceDetail),
        ui::ClickableRegionType::ThrottleTraffic => app.throttle_dialog.cycle_traffic_type(),
        ui::ClickableRegionType::Panel(_) => {}
    }
}
/// Input for the legacy network surfaces (modals and interface views). The
/// explorer handles everything else. Returns true to quit.
fn input(
    app: &mut AppState,
    event: Event,
    config: &mut config::Config,
    backend_info: Option<&BackendInfo>,
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
                        for _ in 0..10 {
                            app.scroll_backend_compat_up();
                        }
                    }
                    KeyCode::PageDown => {
                        for _ in 0..10 {
                            app.scroll_backend_compat_down();
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(dialog) = &app.backend_compatibility_dialog {
                            let action = dialog.get_action();
                            if !matches!(action, ui::BackendCompatibilityAction::Cancel)
                                && let Some(request) = throttle_request(app)
                            {
                                effects.push(Effect::Resolve {
                                    request,
                                    is_upload: dialog.is_upload,
                                    action,
                                });
                                app.status_message = "Applying throttle...".into();
                            }
                            app.show_backend_compatibility_dialog = false;
                            app.backend_compatibility_dialog = None;
                        }
                    }
                    KeyCode::Escape | KeyCode::Char('q') => {
                        // Cancel - close modal, keep throttle dialog open
                        app.dismiss(ui::Layer::Compatibility);
                    }
                    _ => {}
                }
                return false;
            }

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
                    KeyCode::Up | KeyCode::Char('k') => app.select_previous_backend(),
                    KeyCode::Down | KeyCode::Char('j') => app.select_next_backend(),
                    KeyCode::PageUp => {
                        for _ in 0..10 {
                            app.scroll_backend_info_up();
                        }
                    }
                    KeyCode::PageDown => {
                        for _ in 0..10 {
                            app.scroll_backend_info_down();
                        }
                    }
                    KeyCode::Enter | KeyCode::Char('b') | KeyCode::Char('q') | KeyCode::Escape => {
                        app.dismiss(ui::Layer::Backends);
                    }
                    _ => {}
                }
                return false;
            }

            if app.show_graph {
                if matches!(
                    key.key,
                    KeyCode::Char('g') | KeyCode::Char('q') | KeyCode::Escape
                ) {
                    app.dismiss(ui::Layer::Graph);
                }
                return false;
            }

            if app.show_throttle_dialog {
                match key.key {
                    KeyCode::Escape | KeyCode::Char('q') => app.dismiss(ui::Layer::Throttle),
                    KeyCode::Tab => app.throttle_dialog.toggle_field(),
                    KeyCode::Char('t') => app.throttle_dialog.cycle_traffic_type(),
                    KeyCode::Char(c) if c.is_ascii_digit() => app.throttle_dialog.handle_char(c),
                    KeyCode::Backspace => app.throttle_dialog.handle_backspace(),
                    KeyCode::Enter => {
                        if let Some(request) = throttle_request(app) {
                            effects.push(Effect::Apply(request));
                            app.status_message = "Checking throttle compatibility...".into();
                        } else {
                            app.dismiss(ui::Layer::Throttle);
                        }
                    }
                    _ => {}
                }
                return false;
            }

            if app.show_help {
                match key.key {
                    KeyCode::Up | KeyCode::Char('k') => app.scroll_help_up(),
                    KeyCode::Down | KeyCode::Char('j') => app.scroll_help_down(),
                    KeyCode::PageUp => {
                        for _ in 0..10 {
                            app.scroll_help_up();
                        }
                    }
                    KeyCode::PageDown => {
                        for _ in 0..10 {
                            app.scroll_help_down();
                        }
                    }
                    _ => app.dismiss(ui::Layer::Help),
                }
                return false;
            }

            // Interface views. The process view itself belongs to the explorer.
            let view = app.view_mode;
            if view == ui::ViewMode::ProcessView {
                return false;
            }
            if view == ui::ViewMode::InterfaceList {
                app.ensure_interface_cursor();
            }
            match key.key {
                KeyCode::Char('q') | KeyCode::Escape => match view {
                    ui::ViewMode::InterfaceDetail => app.dismiss(ui::Layer::InterfaceDetail),
                    _ => app.dismiss(ui::Layer::InterfaceList),
                },
                KeyCode::Char('h') | KeyCode::Char('?') => {
                    app.reset_help_scroll();
                    app.show_help = true;
                }
                KeyCode::Char('b') => {
                    if let Some(backend_info) = backend_info {
                        app.build_backend_items(backend_info);
                        app.reset_backend_info_scroll();
                        app.show_backend_info = true;
                    } else {
                        app.status_message = "Backends are still loading...".into();
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => match view {
                    ui::ViewMode::InterfaceList => app.select_next_interface(),
                    _ => move_interface_detail(app, 1),
                },
                KeyCode::Up | KeyCode::Char('k') => match view {
                    ui::ViewMode::InterfaceList => app.select_previous_interface(),
                    _ => move_interface_detail(app, -1),
                },
                KeyCode::PageUp => match view {
                    ui::ViewMode::InterfaceList => {
                        for _ in 0..10 {
                            app.scroll_interface_modal_up();
                        }
                    }
                    _ => move_interface_detail(app, -10),
                },
                KeyCode::PageDown => match view {
                    ui::ViewMode::InterfaceList => {
                        for _ in 0..10 {
                            app.scroll_interface_modal_down();
                        }
                    }
                    _ => move_interface_detail(app, 10),
                },
                KeyCode::Char('i') => {
                    app.toggle_view_mode();
                    app.status_message = "Closed interface views".to_string();
                }
                KeyCode::Char('l') => {
                    app.toggle_traffic_view_mode();
                    config.traffic_view_mode = Some(match app.traffic_view_mode {
                        ui::TrafficViewMode::All => crate::process::TrafficType::All,
                        ui::TrafficViewMode::Internet => crate::process::TrafficType::Internet,
                        ui::TrafficViewMode::Local => crate::process::TrafficType::Local,
                    });
                    if let Err(e) = queue_preferences(config, effects) {
                        log::warn!("Failed to save traffic view mode to config: {}", e);
                    }
                }
                KeyCode::Enter if view == ui::ViewMode::InterfaceList => {
                    app.enter_interface_detail();
                    if let Some(iface) = app.get_selected_interface() {
                        app.status_message =
                            format!("Viewing processes on interface: {}", iface.name);
                    }
                }
                KeyCode::Space | KeyCode::Char(' ') if view == ui::ViewMode::InterfaceList => {
                    if let Some(iface) = app.get_selected_interface() {
                        let iface_name = iface.name.clone();
                        app.toggle_interface_filter(iface_name);
                        save_interface_filters(app, config, effects);
                    }
                }
                KeyCode::Char('a') | KeyCode::Char('A') if view == ui::ViewMode::InterfaceList => {
                    app.toggle_all_interface_filters();
                    save_interface_filters(app, config, effects);
                }
                _ => {}
            }
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                match app.top_layer() {
                    Some(ui::Layer::Compatibility) if up => app.scroll_backend_compat_up(),
                    Some(ui::Layer::Compatibility) => app.scroll_backend_compat_down(),
                    Some(ui::Layer::Backends) if up => app.scroll_backend_info_up(),
                    Some(ui::Layer::Backends) => app.scroll_backend_info_down(),
                    Some(ui::Layer::Help) if up => app.scroll_help_up(),
                    Some(ui::Layer::Help) => app.scroll_help_down(),
                    Some(ui::Layer::InterfaceList) if up => app.scroll_interface_modal_up(),
                    Some(ui::Layer::InterfaceList) => app.scroll_interface_modal_down(),
                    Some(ui::Layer::InterfaceDetail) => {
                        move_interface_detail(app, if up { -3 } else { 3 })
                    }
                    Some(ui::Layer::Graph | ui::Layer::Throttle) | None => {}
                }
            }
            MouseEventKind::Down(MouseButton::Left) => click(
                app,
                mouse.position.x,
                mouse.position.y,
                config,
                committed_regions,
                effects,
            ),
            _ => {}
        },
        _ => {}
    }
    false
}

enum Message {
    Start,
    InputFailed(String),
    Monitor(MonitorUpdate),
    MonitorClosed,
    Effect(Box<ServiceResult>, u64, Option<i32>),
    EffectFailed(String),
    Procs(crate::procs::ProcSnapshot),
    DeepDetail(Box<crate::procs::detail::DeepDetail>),
    Report(crate::procs::actions::ActionReport),
    Exported(std::result::Result<std::path::PathBuf, String>),
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
    procs: Option<tokio::sync::mpsc::Receiver<crate::procs::ProcSnapshot>>,
    collector: Option<std::sync::mpsc::Sender<crate::procs::collector::CollectorCommand>>,
}
impl Application {
    /// True while a legacy (network) modal or interface view owns input.
    fn legacy_active(&self) -> bool {
        let app = &self.app;
        app.show_backend_compatibility_dialog
            || app.show_backend_info
            || app.show_graph
            || app.show_throttle_dialog
            || app.show_help
            || matches!(
                app.view_mode,
                ui::ViewMode::InterfaceList | ui::ViewMode::InterfaceDetail
            )
    }
    fn collector(&self, command: crate::procs::collector::CollectorCommand) {
        if let Some(tx) = &self.collector {
            let _ = tx.send(command);
        }
    }
    /// Execute side effects queued by the explorer.
    fn drain_explorer(&mut self, update: &mut Update<Message>) -> Result<()> {
        use crate::procs::collector::CollectorCommand;
        use ui::explorer::{LegacyRequest, UiRequest};
        for request in self.app.explorer.take_requests() {
            match request {
                UiRequest::Action(action) => {
                    let names: std::collections::HashMap<_, _> = action
                        .targets()
                        .iter()
                        .map(|k| (*k, self.app.explorer.name_of(k)))
                        .collect();
                    update.commands.push(Command::concurrent(async move {
                        let report = tokio::task::spawn_blocking(move || {
                            crate::procs::actions::execute(&action, &|k| {
                                names
                                    .get(k)
                                    .cloned()
                                    .unwrap_or_else(|| format!("PID {}", k.pid))
                            })
                        })
                        .await
                        .ok()?;
                        Some(Message::Report(report))
                    }));
                }
                UiRequest::LoadDetail(key) => {
                    let previous = self
                        .app
                        .explorer
                        .detail
                        .detail()
                        .filter(|d| d.key == key)
                        .cloned();
                    update.commands.push(Command::replace(
                        bmux_tui_runtime::CommandKey::new("deep-detail"),
                        async move {
                            let detail = tokio::task::spawn_blocking(move || {
                                crate::procs::detail::collect(key, previous.as_ref())
                            })
                            .await
                            .ok()?;
                            Some(Message::DeepDetail(Box::new(detail)))
                        },
                    ));
                }
                UiRequest::SetRefresh(interval) => {
                    self.collector(CollectorCommand::SetInterval(interval))
                }
                UiRequest::Pause(paused) => self.collector(CollectorCommand::Pause(paused)),
                UiRequest::RefreshNow => self.collector(CollectorCommand::RefreshNow),
                UiRequest::SaveSettings => {
                    self.config.explorer = self.app.explorer.settings.clone();
                    self.config.watch_rules = self.app.explorer.watch.rules.clone();
                    self.effect(
                        Effect::Explorer {
                            settings: self.config.explorer.clone(),
                            rules: self.config.watch_rules.clone(),
                        },
                        update,
                    )?;
                }
                UiRequest::Export(format) => {
                    let explorer = &self.app.explorer;
                    let ctx = explorer.table.column_ctx(&explorer.store, explorer.traffic);
                    let contents = crate::procs::export::render(
                        &explorer.store,
                        explorer.table.rows(),
                        explorer.table.columns(),
                        &ctx,
                        format,
                    );
                    update.commands.push(Command::concurrent(async move {
                        let written = tokio::task::spawn_blocking(move || {
                            crate::procs::export::write(&contents, format, None)
                                .map_err(|e| e.to_string())
                        })
                        .await
                        .ok()?;
                        Some(Message::Exported(written))
                    }));
                }
                UiRequest::Copy(text) => copy_to_clipboard(&text),
                UiRequest::Quit => update.lifecycle = Lifecycle::Exit,
                UiRequest::Legacy(legacy) => match legacy {
                    LegacyRequest::OpenThrottle { pid, name } => {
                        self.app.throttle_dialog.reset();
                        self.app.throttle_dialog.target_pid = Some(pid);
                        self.app.throttle_dialog.target_name = Some(name);
                        self.app.show_throttle_dialog = true;
                        self.dialog_generation += 1;
                    }
                    LegacyRequest::RemoveThrottle { pid, name } => {
                        if self.throttles.contains_key(&pid) {
                            self.app.status_message = "Removing throttle...".into();
                            self.effect(Effect::Remove { pid, name }, update)?;
                        } else {
                            self.app
                                .explorer
                                .flash(format!("{name} has no bandwidth limit"));
                        }
                    }
                    LegacyRequest::Interfaces => {
                        self.app.view_mode = ui::ViewMode::InterfaceList;
                    }
                    LegacyRequest::TrafficMode => {
                        self.app.toggle_traffic_view_mode();
                        self.sync_traffic();
                        self.config.traffic_view_mode = Some(self.app.explorer.traffic);
                        let mut effects = Vec::new();
                        queue_preferences(&self.config, &mut effects)?;
                        for effect in effects {
                            self.effect(effect, update)?;
                        }
                    }
                    LegacyRequest::Graph => {
                        let pid = self.app.explorer.table.selected().map(|k| k.pid);
                        match pid
                            .and_then(|pid| self.app.process_list.iter().position(|p| p.pid == pid))
                        {
                            Some(index) => {
                                self.app.selected_index = Some(index);
                                self.app.list_state.select(Some(index));
                                self.app.show_graph = true;
                            }
                            None => self
                                .app
                                .explorer
                                .flash("No network history for this process yet"),
                        }
                    }
                    LegacyRequest::Backends => {
                        if let Some(backend) = &self.backend {
                            let backend = backend.clone();
                            self.app.build_backend_items(&backend);
                            self.app.reset_backend_info_scroll();
                            self.app.show_backend_info = true;
                        }
                    }
                },
            }
        }
        Ok(())
    }
    fn sync_traffic(&mut self) {
        self.app.explorer.traffic = match self.app.traffic_view_mode {
            ui::TrafficViewMode::All => crate::process::TrafficType::All,
            ui::TrafficViewMode::Internet => crate::process::TrafficType::Internet,
            ui::TrafficViewMode::Local => crate::process::TrafficType::Local,
        };
        self.app.explorer.table.mark_dirty();
    }
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
                Ok(result) => Message::Effect(Box::new(result), dialog_generation, dialog_pid),
                Err(error) => Message::EffectFailed(error.to_string()),
            })
        }));
        Ok(())
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
                if let Some(mut procs) = self.procs.take() {
                    update.subscriptions.push(Subscription::new(
                        SubscriptionKey::new("procs"),
                        move |sender| async move {
                            while let Some(snapshot) = procs.recv().await {
                                if sender.send(Message::Procs(snapshot)).await.is_err() {
                                    return;
                                }
                            }
                        },
                    ));
                }
                self.sync_traffic();
                self.effect(Effect::Refresh, &mut update)?;
            }
            RuntimeEvent::Terminal(event) => {
                let force_quit = matches!(&event, Event::Key(key) if key.key == KeyCode::Char('c') && key.modifiers.ctrl);
                if force_quit {
                    update.lifecycle = Lifecycle::Exit;
                    return Ok(update);
                }
                if !self.legacy_active() {
                    self.app.explorer.handle_event(&event);
                    let identity = self.dialog_identity();
                    self.drain_explorer(&mut update)?;
                    if identity != self.dialog_identity() {
                        self.dialog_generation += 1;
                    }
                    return Ok(update);
                }
                if self.semantic_tab(&event) {
                    return Ok(update);
                }
                let identity = self.dialog_identity();
                let route = self.router.route(event.clone());
                if self.app.handle_component_event(&route) {
                    if identity != self.dialog_identity() {
                        self.dialog_generation += 1;
                    }
                    return Ok(update);
                }
                let traffic = self.app.traffic_view_mode;
                let mut effects = Vec::new();
                if input(
                    &mut self.app,
                    event,
                    &mut self.config,
                    self.backend.as_ref(),
                    &self.committed_regions,
                    &mut effects,
                ) {
                    update.lifecycle = Lifecycle::Exit;
                }
                if traffic != self.app.traffic_view_mode {
                    self.sync_traffic();
                }
                if identity != self.dialog_identity() {
                    self.dialog_generation += 1;
                }
                for effect in effects {
                    self.effect(effect, &mut update)?;
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
                self.app.explorer.apply_network(&data.process_map);
                self.app.update_processes(data.process_map);
                self.app.update_interfaces(data.interface_map);
                // The explorer status line has its own counts; only the legacy
                // interface detail view still shows this summary.
                if self.app.view_mode == ui::ViewMode::InterfaceDetail
                    && !self.app.status_message.starts_with("Throttle")
                    && !self.app.status_message.starts_with("Failed")
                {
                    self.app.status_message = format!(
                        "Monitoring {} process(es) on {} interface(s)",
                        self.app.process_list.len(),
                        self.app.interface_list.len()
                    );
                }
            }
            RuntimeEvent::Message(Message::Effect(result, generation, dialog_pid)) => {
                self.in_flight_effects = self.in_flight_effects.saturating_sub(1);
                self.receipts
                    .insert(result.sequence, (*result, generation, dialog_pid));
                while let Some((result, generation, dialog_pid)) = self.receipts.pop() {
                    self.throttles = result.throttles;
                    self.backend = Some(result.backend);
                    if let Some(status) = result.status {
                        self.app.status_message = status;
                    }
                    if generation == self.dialog_generation
                        && dialog_pid == self.app.throttle_dialog.target_pid
                    {
                        if let Some(dialog) = result.dialog
                            && self.app.show_throttle_dialog
                        {
                            self.app.backend_compatibility_dialog = Some(dialog);
                            self.app.show_backend_compatibility_dialog = true;
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
            RuntimeEvent::Message(Message::Procs(snapshot)) => {
                self.app.explorer.apply_snapshot(snapshot);
                self.drain_explorer(&mut update)?;
            }
            RuntimeEvent::Message(Message::DeepDetail(detail)) => {
                self.app.explorer.apply_detail(*detail);
            }
            RuntimeEvent::Message(Message::Report(report)) => {
                self.app.explorer.apply_report(report);
                self.drain_explorer(&mut update)?;
            }
            RuntimeEvent::Message(Message::Exported(result)) => {
                use ui::explorer::toasts::Severity;
                match result {
                    Ok(path) => self
                        .app
                        .explorer
                        .toasts
                        .push(Severity::Success, format!("Exported to {}", path.display())),
                    Err(error) => self
                        .app
                        .explorer
                        .toasts
                        .push(Severity::Error, format!("Export failed: {error}")),
                }
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

/// Resources transferred into the interactive runtime as one lifecycle unit.
pub(super) struct Startup {
    pub app: AppState,
    pub manager: ThrottleManager,
    pub config: config::Config,
    pub monitor: tokio::sync::mpsc::Receiver<MonitorUpdate>,
    pub monitor_commands: tokio::sync::mpsc::UnboundedSender<MonitorCommand>,
    pub monitor_thread: std::thread::JoinHandle<()>,
    pub save: bool,
    pub socket_mapper: Option<String>,
    pub collector: crate::procs::collector::CollectorHandle,
    pub procs: tokio::sync::mpsc::Receiver<crate::procs::ProcSnapshot>,
}

pub(super) async fn run(startup: Startup) -> Result<()> {
    let Startup {
        app,
        manager,
        config,
        monitor,
        monitor_commands,
        monitor_thread,
        save,
        socket_mapper,
        collector,
        procs,
    } = startup;
    let setup = (|| -> Result<_> {
        let size = bmux_tui::crossterm::terminal_size()?;
        let guard = bmux_tui::crossterm::CrosstermTerminalGuard::enter(std::io::stdout())?;
        Ok((guard, size))
    })();
    let (guard, size) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            drop(monitor);
            drop(procs);
            collector.shutdown();
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
        procs: Some(procs),
        collector: Some(collector.commands.clone()),
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
            program.app.explorer.commit();
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
            *output,
            Some(anyhow::anyhow!(
                "runtime: {error:#}; restoration: {cleanup_error:?}"
            )),
        ),
        Err(RuntimeError::Presenter {
            error,
            cleanup_error,
            output,
        }) => (
            *output,
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
    drop(output.program.procs.take());
    let collector_shutdown = tokio::task::spawn_blocking(move || collector.shutdown());
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
    let _ = collector_shutdown.await;
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

/// Copy via OSC 52 so it works over SSH and inside multiplexers.
fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                encoded.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{encoded}\x07");
    let _ = out.flush();
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
            procs: None,
            collector: None,
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
    fn shifted(code: KeyCode) -> RuntimeEvent<Message> {
        let mut modifiers = bmux_keyboard::Modifiers::NONE;
        modifiers.shift = true;
        RuntimeEvent::Terminal(Event::Key(bmux_keyboard::KeyStroke::with_modifiers(
            code, modifiers,
        )))
    }
    fn ctrl_c() -> RuntimeEvent<Message> {
        let mut modifiers = bmux_keyboard::Modifiers::NONE;
        modifiers.ctrl = true;
        RuntimeEvent::Terminal(Event::Key(bmux_keyboard::KeyStroke::with_modifiers(
            KeyCode::Char('c'),
            modifiers,
        )))
    }
    fn left_click(x: u16, y: u16) -> RuntimeEvent<Message> {
        RuntimeEvent::Terminal(Event::Mouse(bmux_tui::event::MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            bmux_tui::geometry::Point::new(x, y),
        )))
    }
    fn wheel(down: bool) -> RuntimeEvent<Message> {
        RuntimeEvent::Terminal(Event::Mouse(bmux_tui::event::MouseEvent::new(
            if down {
                MouseEventKind::ScrollDown
            } else {
                MouseEventKind::ScrollUp
            },
            bmux_tui::geometry::Point::new(0, 0),
        )))
    }
    fn with_interfaces(program: &mut Application) {
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
    }
    fn on_en0(pid: i32) -> crate::process::ProcessInfo {
        let mut process = crate::process::ProcessInfo::new(pid, format!("net-{pid}"));
        process.download_rate = pid as u64;
        process
            .interface_stats
            .insert("en0".into(), Default::default());
        process
    }
    fn region(
        program: &Application,
        matches: impl Fn(&ui::ClickableRegionType) -> bool,
    ) -> ui::Rect {
        program
            .committed_regions
            .iter()
            .find(|r| matches(&r.region_type))
            .map(|r| r.area)
            .expect("region painted and committed")
    }
    #[test]
    fn committed_interface_space_toggles_selected_filter_for_both_backends() {
        for space in [KeyCode::Space, KeyCode::Char(' ')] {
            let mut program = application();
            with_interfaces(&mut program);
            // 'i' is an explorer action that opens the legacy interface modal.
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
            assert!(program.app.explorer.take_requests().is_empty());
        }
    }
    #[test]
    fn explorer_owns_tab_and_shift_tab_when_no_legacy_surface_is_open() {
        use crate::procs::columns::Preset;
        let mut program = application();
        let start = program.app.explorer.settings.preset;
        let index = |p: Preset| Preset::ALL.iter().position(|q| *q == p).unwrap();
        program.update(key(KeyCode::Tab)).unwrap();
        let next = program.app.explorer.settings.preset;
        assert_eq!(index(next), (index(start) + 1) % Preset::ALL.len());
        program.update(shifted(KeyCode::Tab)).unwrap();
        assert_eq!(program.app.explorer.settings.preset, start);
        assert_eq!(program.app.view_mode, ui::ViewMode::ProcessView);
    }
    #[test]
    fn modal_escape_closes_top_scope_before_quitting() {
        let mut program = application();
        // Explorer help overlay.
        program.update(key(KeyCode::Char('h'))).unwrap();
        assert!(matches!(
            program.app.explorer.overlays.last(),
            Some(ui::explorer::overlays::Overlay::Help(_))
        ));
        assert_eq!(
            program.update(key(KeyCode::Escape)).unwrap().lifecycle,
            Lifecycle::Continue
        );
        assert!(program.app.explorer.overlays.is_empty());
        // Legacy interface modal and its help.
        program.update(key(KeyCode::Char('i'))).unwrap();
        program.update(key(KeyCode::Char('?'))).unwrap();
        assert!(program.app.show_help);
        for _ in 0..2 {
            assert_eq!(
                program.update(key(KeyCode::Escape)).unwrap().lifecycle,
                Lifecycle::Continue
            );
        }
        assert!(!program.app.show_help);
        assert_eq!(program.app.view_mode, ui::ViewMode::ProcessView);
        assert_eq!(
            program.update(key(KeyCode::Char('q'))).unwrap().lifecycle,
            Lifecycle::Exit
        );
    }
    #[test]
    fn ctrl_c_precedes_every_modal() {
        let open: [fn(&mut Application); 6] = [
            |p| p.app.show_help = true,
            |p| p.app.show_throttle_dialog = true,
            |p| p.app.show_graph = true,
            |p| p.app.show_backend_info = true,
            |p| p.app.view_mode = ui::ViewMode::InterfaceDetail,
            |p| p.app.explorer.run(ui::explorer::actions::Action::Settings),
        ];
        for open in open {
            let mut program = application();
            open(&mut program);
            assert_eq!(program.update(ctrl_c()).unwrap().lifecycle, Lifecycle::Exit);
        }
        let mut program = application();
        program.backend = None;
        program.app.view_mode = ui::ViewMode::InterfaceList;
        // Before the first service snapshot, 'q' closes the modal instead of quitting.
        assert_eq!(
            program.update(key(KeyCode::Char('q'))).unwrap().lifecycle,
            Lifecycle::Continue
        );
        assert_eq!(program.app.view_mode, ui::ViewMode::ProcessView);
    }
    #[test]
    fn throttle_dialog_owns_input_while_open() {
        let mut program = application();
        program.app.show_throttle_dialog = true;
        program.app.throttle_dialog.target_pid = Some(10);
        program.app.throttle_dialog.target_name = Some("net-10".into());
        paint_commit(&mut program);
        for code in [KeyCode::Char('4'), KeyCode::Char('2')] {
            program.update(key(code)).unwrap();
        }
        assert_eq!(program.app.throttle_dialog.download_input, "42");
        program.update(key(KeyCode::Char('t'))).unwrap();
        assert_eq!(program.app.throttle_dialog.traffic_type_index, 1);
        // Keys the explorer would act on are swallowed by the dialog.
        for code in [KeyCode::Char('i'), KeyCode::Char('h'), KeyCode::Char('x')] {
            program.update(key(code)).unwrap();
        }
        assert_eq!(program.app.view_mode, ui::ViewMode::ProcessView);
        assert!(program.app.explorer.overlays.is_empty());
        // The traffic row is clickable; a click outside never discards input.
        paint_commit(&mut program);
        let row = region(&program, |r| {
            matches!(r, ui::ClickableRegionType::ThrottleTraffic)
        });
        program.update(left_click(row.x + 2, row.y)).unwrap();
        assert_eq!(program.app.throttle_dialog.traffic_type_index, 2);
        program.update(left_click(0, 0)).unwrap();
        assert!(program.app.show_throttle_dialog);
        assert_eq!(
            program.update(key(KeyCode::Char('q'))).unwrap().lifecycle,
            Lifecycle::Continue
        );
        assert!(!program.app.show_throttle_dialog);
        assert_eq!(program.app.throttle_dialog.download_input, "");
    }
    #[test]
    fn backends_modal_routes_navigation_apply_and_dismissal() {
        let mut program = application();
        let mut info = backend();
        info.available_upload = vec![
            ("a".into(), crate::backends::BackendPriority::Good, true),
            ("b".into(), crate::backends::BackendPriority::Fallback, true),
        ];
        info.active_upload = Some("a".into());
        program.backend = Some(info);
        program.update(key(KeyCode::Char('b'))).unwrap();
        assert!(program.app.show_backend_info);
        let first = program.app.backend_selected_index;
        program.update(key(KeyCode::Down)).unwrap();
        assert_ne!(program.app.backend_selected_index, first);
        assert_eq!(
            program.app.get_selected_backend(),
            Some(("b", ui::BackendGroup::Upload))
        );
        program.update(key(KeyCode::Space)).unwrap();
        // The disconnected test service rejects the queued SelectBackend effect.
        assert!(program.app.status_message.starts_with("Failed to queue"));
        // Clicking a row selects it; clicking outside the panel closes it.
        paint_commit(&mut program);
        let (area, line) = program
            .committed_regions
            .iter()
            .find_map(|r| match &r.region_type {
                ui::ClickableRegionType::BackendModal { line_to_item } => line_to_item
                    .iter()
                    .find(|(_, item)| **item == first)
                    .map(|(line, _)| (r.area, *line)),
                _ => None,
            })
            .unwrap();
        let y = area.y + 1 + (line - program.app.backend_info_scroll_offset) as u16;
        program.update(left_click(area.x + 5, y)).unwrap();
        assert_eq!(program.app.backend_selected_index, first);
        assert!(program.app.show_backend_info);
        program.update(left_click(0, 0)).unwrap();
        assert!(!program.app.show_backend_info);
        assert_eq!(program.app.top_layer(), None);
    }
    #[test]
    fn graph_routes_close_keys_and_clicks_without_quitting() {
        let mut program = application();
        program.app.update_processes([(10, on_en0(10))].into());
        program.app.selected_index = Some(0);
        for close in [
            key(KeyCode::Char('g')),
            key(KeyCode::Char('q')),
            key(KeyCode::Escape),
        ] {
            program.app.show_graph = true;
            program.update(key(KeyCode::Char('i'))).unwrap();
            assert_eq!(program.app.view_mode, ui::ViewMode::ProcessView);
            assert_eq!(
                program.update(close).unwrap().lifecycle,
                Lifecycle::Continue
            );
            assert!(!program.app.show_graph);
        }
        program.app.show_graph = true;
        paint_commit(&mut program);
        let panel = region(&program, |r| {
            matches!(r, ui::ClickableRegionType::Panel(ui::Layer::Graph))
        });
        program
            .update(left_click(panel.x + 3, panel.y + 3))
            .unwrap();
        assert!(program.app.show_graph);
        program.update(wheel(true)).unwrap();
        assert!(program.app.show_graph);
        program.update(left_click(0, 0)).unwrap();
        assert!(!program.app.show_graph);
    }
    #[test]
    fn interface_modal_mouse_selects_toggles_and_dismisses() {
        let mut program = application();
        with_interfaces(&mut program);
        program.update(key(KeyCode::Char('i'))).unwrap();
        paint_commit(&mut program);
        let (area, header) = program
            .committed_regions
            .iter()
            .find_map(|r| match r.region_type {
                ui::ClickableRegionType::InterfaceModal { header_lines } => {
                    Some((r.area, header_lines))
                }
                _ => None,
            })
            .unwrap();
        let lo0 = area.y + 1 + header as u16 + 1 - program.app.interface_modal_scroll_offset as u16;
        program.update(left_click(area.x + 10, lo0)).unwrap();
        assert_eq!(program.app.selected_interface_index, Some(1));
        assert_eq!(program.app.active_interface_filters, None);
        program.update(left_click(area.x + 4, lo0)).unwrap();
        assert!(!program.app.is_interface_filtered("lo0"));
        assert_eq!(
            program.config.filtered_interfaces,
            Some(vec!["en0".to_string()])
        );
        paint_commit(&mut program);
        program.update(left_click(0, 0)).unwrap();
        assert_eq!(program.app.view_mode, ui::ViewMode::ProcessView);
        assert_eq!(
            program.app.active_interface_filters,
            Some(vec!["en0".into()])
        );
    }
    #[test]
    fn interface_detail_scrolls_and_back_button_returns_to_the_list() {
        let mut program = application();
        with_interfaces(&mut program);
        program
            .app
            .update_processes((1..=30).map(|pid| (pid, on_en0(pid))).collect());
        program.update(key(KeyCode::Char('i'))).unwrap();
        program.update(key(KeyCode::Enter)).unwrap();
        assert_eq!(program.app.view_mode, ui::ViewMode::InterfaceDetail);
        assert_eq!(program.app.selected_interface_name.as_deref(), Some("en0"));
        program.update(key(KeyCode::Down)).unwrap();
        assert_eq!(program.app.interface_detail_state.selected(), Some(0));
        program.update(key(KeyCode::PageDown)).unwrap();
        assert_eq!(program.app.interface_detail_state.selected(), Some(10));
        program.update(wheel(true)).unwrap();
        assert_eq!(program.app.interface_detail_state.selected(), Some(13));
        for _ in 0..5 {
            program.update(key(KeyCode::PageDown)).unwrap();
        }
        assert_eq!(program.app.interface_detail_state.selected(), Some(29));
        paint_commit(&mut program);
        let back = region(&program, |r| {
            matches!(r, ui::ClickableRegionType::InterfaceBack)
        });
        program.update(left_click(back.x, back.y)).unwrap();
        assert_eq!(program.app.view_mode, ui::ViewMode::InterfaceList);
        program.update(key(KeyCode::Char('i'))).unwrap();
        assert_eq!(program.app.view_mode, ui::ViewMode::ProcessView);
    }
    #[test]
    fn traffic_mode_from_interface_view_keeps_the_explorer_in_sync() {
        let mut program = application();
        program.app.view_mode = ui::ViewMode::InterfaceList;
        program.update(key(KeyCode::Char('l'))).unwrap();
        assert_eq!(program.app.traffic_view_mode, ui::TrafficViewMode::Internet);
        assert_eq!(
            program.app.explorer.traffic,
            crate::process::TrafficType::Internet
        );
        assert_eq!(
            program.config.traffic_view_mode,
            Some(crate::process::TrafficType::Internet)
        );
    }
    #[test]
    fn clicks_before_the_first_commit_never_dismiss() {
        let mut program = application();
        program.app.show_help = true;
        program.update(left_click(0, 0)).unwrap();
        assert!(program.app.show_help);
        paint_commit(&mut program);
        program.update(left_click(0, 0)).unwrap();
        assert!(!program.app.show_help);
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
                Box::new(ServiceResult {
                    sequence: 1,
                    status: None,
                    dialog: None,
                    close_throttle: true,
                    backend: backend(),
                    throttles: Default::default(),
                }),
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
                Box::new(ServiceResult {
                    sequence: 1,
                    status: None,
                    dialog: None,
                    close_throttle: true,
                    backend: backend(),
                    throttles: Default::default(),
                }),
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
                Box::new(ServiceResult {
                    sequence: 1,
                    status: Some("Failed old action".into()),
                    dialog: Some(dialog),
                    close_throttle: true,
                    backend: backend(),
                    throttles: Default::default(),
                }),
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
