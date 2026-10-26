use super::*;
use bmux_tui::{buffer::Buffer, frame::Frame};

fn fixture() -> AppState {
    let mut app = AppState::new();
    let mut process = ProcessInfo::new(4242, "界面🦀 café e\u{301} worker 界面🦀 café".into());
    process.download_rate = 4096;
    process.upload_rate = 2048;
    app.process_list = vec![process.clone()];
    app.unfiltered_process_list = vec![process];
    app.list_state.select(Some(0));
    app.selected_index = Some(0);
    app.selected_process_detail_pid = Some(4242);
    app.interface_list.push(InterfaceInfo {
        name: "en0界".into(),
        mac_address: Some("00:11:22:33:44:55".into()),
        ip_addresses: vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()],
        is_up: true,
        is_loopback: false,
        total_download_rate: 4096,
        total_upload_rate: 2048,
        process_count: 1,
    });
    app.selected_interface_index = Some(0);
    app.selected_interface_name = Some("en0界".into());
    app.interface_list_state.select(Some(0));
    let mut details = app.details_for(&app.process_list[0]);
    details.cmdline = Some(vec!["界🦀/".repeat(40), "--test".into()]);
    details.exe_path = Some("/tmp/界🦀".into());
    details.cwd = Some("/tmp/café".into());
    details.memory_rss = Some(1024);
    details.threads = Some(3);
    app.update_process_details(Some(details));
    app
}

fn backend() -> BackendInfo {
    BackendInfo {
        active_upload: Some("userspace".into()),
        active_download: Some("userspace".into()),
        active_monitoring: Some("pnet".into()),
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
        backend_stats: HashMap::new(),
    }
}

fn render(app: &mut AppState, width: u16, height: u16) -> Buffer {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    let mut frame = Frame::new(&mut buffer);
    draw_ui_with_backend_info(&mut PaintCx::new(&mut frame), app, Some(&backend()));
    buffer
}
fn text(buffer: &Buffer) -> String {
    buffer
        .cells()
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn all_views_and_detail_tabs_render_deterministically() {
    for view in [
        ViewMode::ProcessView,
        ViewMode::InterfaceList,
        ViewMode::InterfaceDetail,
        ViewMode::ProcessDetail,
    ] {
        for tab in [
            ProcessDetailTab::Overview,
            ProcessDetailTab::Connections,
            ProcessDetailTab::Traffic,
            ProcessDetailTab::System,
        ] {
            for (width, height) in [(120, 45), (80, 24), (20, 10), (4, 4), (1, 1), (0, 0)] {
                let mut app = fixture();
                app.view_mode = view;
                app.detail_tab = tab;
                let first = render(&mut app, width, height);
                let second = render(&mut app, width, height);
                assert_eq!(first, second, "{view:?}/{tab:?} at {width}x{height}");
                if width == 120 && height == 45 {
                    assert!(
                        text(&first).contains("bproc") || text(&first).contains("Process Details")
                    );
                }
            }
        }
    }
}

#[test]
fn every_overlay_and_empty_state_renders_at_tiny_sizes() {
    for overlay in 0..7 {
        for (width, height) in [(140, 50), (80, 24), (16, 8), (3, 2), (0, 0)] {
            let mut app = fixture();
            match overlay {
                0 => app.show_help = true,
                1 => {
                    app.show_throttle_dialog = true;
                    app.throttle_dialog.target_pid = Some(4242);
                    app.throttle_dialog.target_name = Some("界🦀".into());
                    app.throttle_dialog.download_input = "1024".into();
                }
                2 => app.show_backend_info = true,
                3 => {
                    app.show_backend_compatibility_dialog = true;
                    app.backend_compatibility_dialog = Some(BackendCompatibilityDialog::new(
                        "userspace".into(),
                        crate::process::TrafficType::Internet,
                        vec!["界backend".into()],
                        true,
                    ));
                }
                4 => app.show_graph = true,
                5 => {
                    app.show_graph = true;
                    app.history
                        .update_at(1, 4242, "界worker".into(), 4096, 2048);
                    app.history
                        .update_at(2, 4242, "界worker".into(), 8192, 1024);
                }
                _ => {
                    app.process_list.clear();
                    app.unfiltered_process_list.clear();
                    app.view_mode = ViewMode::ProcessDetail;
                }
            }
            let first = render(&mut app, width, height);
            assert_eq!(
                first,
                render(&mut app, width, height),
                "overlay {overlay} {width}x{height}"
            );
        }
    }
}

#[test]
fn selection_scroll_and_legacy_hit_regions_remain_stable() {
    let mut app = fixture();
    for pid in 1..60 {
        app.process_list
            .push(ProcessInfo::new(pid, format!("worker {pid}")));
    }
    app.list_state.select(Some(45));
    render(&mut app, 100, 24);
    assert!(app.list_state.offset() > 0);
    assert!(
        app.clickable_regions
            .iter()
            .any(|region| matches!(region.region_type, ClickableRegionType::ProcessList { .. }))
    );
    app.view_mode = ViewMode::ProcessDetail;
    render(&mut app, 120, 40);
    assert!(app.clickable_regions.iter().any(|region| matches!(&region.region_type, ClickableRegionType::ProcessDetailTabs { tab_ranges } if tab_ranges.len() == 4)));
}

#[test]
fn footer_wrap_preserves_every_hint_and_unicode_width() {
    let spans = vec![
        Span::raw("[a] "),
        Span::raw("界面"),
        Span::raw("  "),
        Span::raw("[b] "),
        Span::raw("other"),
    ];
    let rows = wrap_spans_to_lines(spans, 12);
    assert!(rows.len() >= 2);
    let combined = rows.iter().map(Line::plain_text).collect::<String>();
    assert!(combined.contains("[a] 界面"));
    assert!(combined.contains("[b] other"));
}

#[test]
fn process_details_are_supplied_by_runtime_and_pid_checked() {
    let mut app = fixture();
    assert_eq!(
        app.cached_process_details.as_ref().unwrap().threads,
        Some(3)
    );
    let other = ProcessInfo::new(9999, "other".into());
    assert!(app.details_for(&other).threads.is_none());
    app.update_process_details(None);
    let buffer = render(&mut app, 80, 24);
    assert!(!text(&buffer).is_empty());
}
