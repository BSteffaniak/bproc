//! Deterministic render tests for the legacy network surfaces drawn over the
//! process explorer, plus their pure state helpers.
use super::*;
use crate::process::InterfaceStats;
use bmux_tui::{buffer::Buffer, frame::Frame};

const SIZES: [(u16, u16); 5] = [(120, 40), (80, 24), (40, 12), (1, 1), (0, 0)];

fn worker(pid: i32, name: &str, rate: u64) -> ProcessInfo {
    let mut process = ProcessInfo::new(pid, name.into());
    process.download_rate = rate;
    process.upload_rate = rate / 2;
    process.interface_stats.insert(
        "en0".into(),
        InterfaceStats {
            download_rate: rate,
            upload_rate: rate / 2,
            total_download: rate * 10,
            total_upload: rate * 5,
            internet_download_rate: rate,
            internet_upload_rate: 0,
            local_download_rate: 0,
            local_upload_rate: rate / 2,
        },
    );
    process
}

fn interface(name: &str) -> InterfaceInfo {
    InterfaceInfo {
        name: name.into(),
        mac_address: Some("00:11:22:33:44:55".into()),
        ip_addresses: vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()],
        is_up: true,
        is_loopback: name == "lo0",
        total_download_rate: 4096,
        total_upload_rate: 2048,
        process_count: 1,
    }
}

fn fixture() -> AppState {
    let mut app = AppState::new();
    app.update_processes(
        [
            (4242, worker(4242, "worker", 4096)),
            (7, worker(7, "界面🦀 café e\u{301} long process name", 1024)),
        ]
        .into(),
    );
    app.update_interfaces(
        ["en0", "lo0"]
            .into_iter()
            .map(|n| (n.to_string(), interface(n)))
            .collect(),
    );
    app.selected_index = Some(0);
    app.list_state.select(Some(0));
    app.selected_interface_index = Some(0);
    app.selected_interface_name = Some("en0".into());
    app
}

fn backend() -> BackendInfo {
    BackendInfo {
        active_upload: Some("userspace".into()),
        active_download: Some("userspace".into()),
        active_monitoring: Some("pnet".into()),
        active_socket_mapper: Some("libproc".into()),
        available_upload: vec![("userspace".into(), BackendPriority::Good, true)],
        available_download: vec![
            ("userspace".into(), BackendPriority::Good, true),
            ("tc".into(), BackendPriority::Best, false),
        ],
        available_socket_mappers: vec![("libproc".into(), BackendPriority::Best, true)],
        preferred_upload: None,
        preferred_download: None,
        preferred_socket_mapper: None,
        upload_capabilities: None,
        download_capabilities: None,
        socket_mapper_capabilities: None,
        backend_stats: HashMap::new(),
    }
}

/// Every legacy surface that can sit on top of the explorer.
#[derive(Debug, Clone, Copy)]
enum Surface {
    Interfaces,
    InterfacesEmpty,
    InterfaceDetail,
    InterfaceDetailEmpty,
    Help,
    Throttle,
    Backends,
    Graph,
    GraphNoHistory,
    GraphNoProcess,
    Compatibility,
}

const SURFACES: [Surface; 11] = [
    Surface::Interfaces,
    Surface::InterfacesEmpty,
    Surface::InterfaceDetail,
    Surface::InterfaceDetailEmpty,
    Surface::Help,
    Surface::Throttle,
    Surface::Backends,
    Surface::Graph,
    Surface::GraphNoHistory,
    Surface::GraphNoProcess,
    Surface::Compatibility,
];

impl Surface {
    fn open(self, app: &mut AppState) {
        match self {
            Surface::Interfaces => app.view_mode = ViewMode::InterfaceList,
            Surface::InterfacesEmpty => {
                app.interface_list.clear();
                app.view_mode = ViewMode::InterfaceList;
            }
            Surface::InterfaceDetail => app.view_mode = ViewMode::InterfaceDetail,
            Surface::InterfaceDetailEmpty => {
                app.selected_interface_name = Some("lo0".into());
                app.view_mode = ViewMode::InterfaceDetail;
            }
            Surface::Help => {
                app.view_mode = ViewMode::InterfaceList;
                app.show_help = true;
            }
            Surface::Throttle => {
                app.show_throttle_dialog = true;
                app.throttle_dialog.target_pid = Some(4242);
                app.throttle_dialog.target_name = Some("worker".into());
                app.throttle_dialog.download_input = "1024".into();
            }
            Surface::Backends => {
                app.build_backend_items(&backend());
                app.show_backend_info = true;
            }
            Surface::Graph => {
                app.show_graph = true;
                app.history.update_at(1, 4242, "worker".into(), 4096, 2048);
                app.history.update_at(2, 4242, "worker".into(), 8192, 1024);
            }
            Surface::GraphNoHistory => app.show_graph = true,
            Surface::GraphNoProcess => {
                app.show_graph = true;
                app.selected_index = None;
            }
            Surface::Compatibility => {
                app.show_throttle_dialog = true;
                app.throttle_dialog.target_pid = Some(4242);
                app.show_backend_compatibility_dialog = true;
                app.backend_compatibility_dialog = Some(BackendCompatibilityDialog::new(
                    "userspace".into(),
                    crate::process::TrafficType::Internet,
                    vec!["界backend".into()],
                    true,
                ));
            }
        }
    }

    fn layer(self) -> Layer {
        match self {
            Surface::Interfaces | Surface::InterfacesEmpty => Layer::InterfaceList,
            Surface::InterfaceDetail | Surface::InterfaceDetailEmpty => Layer::InterfaceDetail,
            Surface::Help => Layer::Help,
            Surface::Throttle => Layer::Throttle,
            Surface::Backends => Layer::Backends,
            Surface::Graph | Surface::GraphNoHistory | Surface::GraphNoProcess => Layer::Graph,
            Surface::Compatibility => Layer::Compatibility,
        }
    }

    /// Text that must be visible at 120x40 (the first entry also at 80x24).
    fn expected(self) -> &'static [&'static str] {
        match self {
            Surface::Interfaces => &["Interface Filter", "[✓]", "en0", "lo0", "click box"],
            Surface::InterfacesEmpty => &["Interface Filter", "No interfaces detected"],
            Surface::InterfaceDetail => &[
                "Interface: en0",
                "2 processes",
                "worker",
                "[◀ Back]",
                "Esc back",
                "DL Rate",
            ],
            Surface::InterfaceDetailEmpty => &["Interface: lo0", "No processes have used"],
            Surface::Help => &["Help", "Interfaces - keyboard & mouse", "Toggle all / no"],
            Surface::Throttle => &[
                "Throttle: worker (PID 4242)",
                "Download Limit",
                "click to change",
            ],
            Surface::Backends => &["Backends", "Socket Mapper Backends:", "libproc"],
            Surface::Graph => &["Bandwidth Graph: worker (PID 4242)", "Download", "close"],
            Surface::GraphNoHistory => &["Bandwidth Graph: worker", "No historical data"],
            Surface::GraphNoProcess => &["Bandwidth Graph", "no longer reporting"],
            Surface::Compatibility => &["Backend Incompatibility", "Cancel - don't apply"],
        }
    }
}

fn render(app: &mut AppState, width: u16, height: u16) -> Buffer {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    let mut frame = Frame::new(&mut buffer);
    draw_ui_with_backend_info(&mut PaintCx::new(&mut frame), app, Some(&backend()));
    buffer
}

fn rows(buffer: &Buffer, width: u16) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    buffer
        .cells()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol.as_str()).collect())
        .collect()
}

#[test]
fn every_legacy_surface_renders_over_the_explorer_at_every_size() {
    for surface in SURFACES {
        for (width, height) in SIZES {
            let mut app = fixture();
            surface.open(&mut app);
            let first = render(&mut app, width, height);
            let second = render(&mut app, width, height);
            assert_eq!(first, second, "{surface:?} at {width}x{height}");
            let screen = rows(&first, width).join("\n");
            let expected = surface.expected();
            let checked = match (width, height) {
                (120, 40) => expected.len(),
                (80, 24) => 1,
                _ => 0,
            };
            for text in &expected[..checked] {
                assert!(
                    screen.contains(text),
                    "{surface:?} at {width}x{height} lacks {text:?}:\n{screen}"
                );
            }
        }
    }
}

#[test]
fn legacy_click_regions_stay_on_screen_and_belong_to_the_open_surface() {
    for surface in SURFACES {
        for (width, height) in SIZES {
            let mut app = fixture();
            surface.open(&mut app);
            render(&mut app, width, height);
            for region in &app.clickable_regions {
                let area = region.area;
                assert!(
                    u32::from(area.x) + u32::from(area.width) <= u32::from(width)
                        && u32::from(area.y) + u32::from(area.height) <= u32::from(height),
                    "{surface:?} {width}x{height}: {region:?} off screen"
                );
            }
            if width >= 40 {
                let layer = surface.layer();
                assert!(
                    app.clickable_regions
                        .iter()
                        .any(|r| r.region_type.layer() == layer),
                    "{surface:?} {width}x{height} registered no {layer:?} panel"
                );
            }
        }
    }
}

#[test]
fn process_view_paints_only_the_explorer() {
    let mut app = fixture();
    for (width, height) in SIZES {
        render(&mut app, width, height);
        assert!(app.clickable_regions.is_empty(), "{width}x{height}");
        assert_eq!(app.top_layer(), None);
    }
}

#[test]
fn interface_detail_lists_only_processes_on_that_interface() {
    let mut app = fixture();
    let mut offline = ProcessInfo::new(99, "offline-daemon".into());
    offline.download_rate = 1 << 20;
    app.update_processes([(4242, worker(4242, "worker", 4096)), (99, offline)].into());
    app.view_mode = ViewMode::InterfaceDetail;
    let screen = rows(&render(&mut app, 120, 40), 120).join("\n");
    assert!(screen.contains("worker"));
    assert!(screen.contains("1 process ·"));
    assert!(!screen.contains("offline-daemon"));
}

#[test]
fn interface_detail_selection_scrolls_long_lists() {
    let mut app = fixture();
    app.update_processes(
        (1..=80)
            .map(|pid| {
                (
                    pid,
                    worker(pid, &format!("proc-{pid:02}"), pid as u64 * 100),
                )
            })
            .collect(),
    );
    app.enter_interface_detail();
    app.interface_detail_state.select(Some(79));
    let screen = rows(&render(&mut app, 80, 24), 80).join("\n");
    // Highest rate sorts first, so the last row is the slowest process.
    assert!(screen.contains("proc-01"), "{screen}");
    assert!(!screen.contains("proc-80"));
    assert!(*app.interface_detail_state.offset_mut() > 0);
}

#[test]
fn interface_click_maps_rows_through_scroll_and_detects_the_checkbox() {
    let area = Rect::new(10, 5, 50, 20);
    let header = INTERFACE_HEADER_LINES;
    let first_row_y = 5 + 1 + header as u16;
    assert_eq!(
        interface_click(area, header, 0, 3, 20, first_row_y),
        Some((0, false))
    );
    assert_eq!(
        interface_click(area, header, 0, 3, 13, first_row_y + 2),
        Some((2, true))
    );
    assert_eq!(
        interface_click(area, header, 0, 3, 15, first_row_y),
        Some((0, true))
    );
    assert_eq!(
        interface_click(area, header, 0, 3, 16, first_row_y),
        Some((0, false))
    );
    // Scrolled by two lines: the same screen row now shows interface 2.
    assert_eq!(
        interface_click(area, header, 2, 3, 20, first_row_y),
        Some((2, false))
    );
    // Header, rows past the end, borders and degenerate areas miss.
    assert_eq!(
        interface_click(area, header, 0, 3, 20, first_row_y - 1),
        None
    );
    assert_eq!(
        interface_click(area, header, 0, 3, 20, first_row_y + 3),
        None
    );
    assert_eq!(interface_click(area, header, 0, 3, 20, 5), None);
    assert_eq!(interface_click(area, header, 0, 3, 20, 24), None);
    assert_eq!(
        interface_click(Rect::new(0, 0, 1, 1), header, 0, 3, 0, 0),
        None
    );
    assert_eq!(interface_click(Rect::default(), header, 0, 3, 0, 0), None);
}

#[test]
fn interface_cursor_is_normalized_for_the_modal() {
    let mut app = fixture();
    app.selected_interface_index = None;
    app.ensure_interface_cursor();
    assert_eq!(app.selected_interface_index, Some(0));
    app.selected_interface_index = Some(9);
    app.ensure_interface_cursor();
    assert_eq!(app.selected_interface_index, Some(1));
    app.interface_list.clear();
    app.ensure_interface_cursor();
    assert_eq!(app.selected_interface_index, None);
}

#[test]
fn top_layer_follows_input_priority_and_dismiss_closes_it() {
    let mut app = fixture();
    app.view_mode = ViewMode::InterfaceDetail;
    app.show_help = true;
    app.show_throttle_dialog = true;
    app.show_graph = true;
    app.show_backend_info = true;
    app.show_backend_compatibility_dialog = true;
    let mut order = Vec::new();
    while let Some(layer) = app.top_layer() {
        order.push(layer);
        app.dismiss(layer);
    }
    assert_eq!(
        order,
        [
            Layer::Compatibility,
            Layer::Backends,
            Layer::Graph,
            Layer::Throttle,
            Layer::Help,
            Layer::InterfaceDetail,
            Layer::InterfaceList,
        ]
    );
    assert_eq!(app.view_mode, ViewMode::ProcessView);
    assert!(!Layer::Throttle.dismissible() && !Layer::Compatibility.dismissible());
    assert!(Layer::Graph.dismissible() && Layer::InterfaceList.dismissible());
}

#[test]
fn graph_keeps_following_its_process_across_resorts() {
    let mut app = fixture();
    let pid = app.get_selected_process().unwrap().pid;
    assert_eq!(pid, 4242);
    // A burst on the other process re-sorts the list underneath the graph.
    app.update_processes(
        [
            (4242, worker(4242, "worker", 10)),
            (7, worker(7, "busy", 1 << 30)),
        ]
        .into(),
    );
    assert_eq!(app.process_list[0].pid, 7);
    assert_eq!(app.get_selected_process().unwrap().pid, 4242);
    assert_eq!(app.list_state.selected(), app.selected_index);
    app.update_processes([(7, worker(7, "busy", 1))].into());
    assert_eq!(app.selected_index, None);
}

#[test]
fn network_order_is_deterministic() {
    let mut app = AppState::new();
    let mut throttled = worker(3, "b", 0);
    throttled.throttle_limit = Some(crate::process::ThrottleLimit {
        download_limit: Some(1),
        upload_limit: None,
        traffic_type: crate::process::TrafficType::All,
    });
    let mut dead = worker(1, "z", 1 << 20);
    dead.is_terminated = true;
    app.update_processes(
        [
            (1, dead),
            (2, worker(2, "a", 0)),
            (3, throttled),
            (4, worker(4, "fast", 9000)),
            (5, worker(5, "a", 0)),
        ]
        .into(),
    );
    let order: Vec<i32> = app.process_list.iter().map(|p| p.pid).collect();
    assert_eq!(order, [4, 3, 2, 5, 1]);
}

#[test]
fn interface_filter_hides_processes_from_the_network_list() {
    let mut app = fixture();
    app.toggle_interface_filter("en0".into());
    assert_eq!(app.active_interface_filters, Some(vec!["lo0".to_string()]));
    app.update_processes([(4242, worker(4242, "worker", 1))].into());
    assert!(app.process_list.is_empty());
    assert_eq!(app.unfiltered_process_list.len(), 1);
    // A partial filter selects everything first, then nothing.
    app.toggle_all_interface_filters();
    assert_eq!(app.active_interface_filters, None);
    app.toggle_all_interface_filters();
    assert_eq!(app.active_interface_filters, Some(vec![]));
}
