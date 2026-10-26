//! Exact original renderer fixture, shared with the migration parity oracle.
use super::*;
use crate::process::{InterfaceStats, ThrottleLimit, TrafficType};
use bmux_tui::{buffer::Buffer, frame::Frame, geometry::Point};
fn fixture(screen: &str) -> (AppState, BackendInfo) {
    let mut app = AppState::new();
    let mut process = ProcessInfo::new(1234, "fixture-worker".into());
    process.download_rate = 32768;
    process.upload_rate = 4096;
    process.total_download = 1048576;
    process.total_upload = 65536;
    process.internet_download_rate = 24576;
    process.local_download_rate = 8192;
    process.throttle_limit = Some(ThrottleLimit {
        download_limit: Some(65536),
        upload_limit: None,
        traffic_type: TrafficType::All,
    });
    process.interface_stats.insert(
        "en0".into(),
        InterfaceStats {
            download_rate: 32768,
            upload_rate: 4096,
            total_download: 1048576,
            total_upload: 65536,
            internet_download_rate: 24576,
            internet_upload_rate: 2048,
            local_download_rate: 8192,
            local_upload_rate: 2048,
        },
    );
    app.update_processes([(1234, process)].into());
    app.update_interfaces(
        [(
            "en0".into(),
            InterfaceInfo {
                name: "en0".into(),
                mac_address: Some("00:11:22:33:44:55".into()),
                ip_addresses: vec!["192.168.1.2".parse().unwrap()],
                is_up: true,
                is_loopback: false,
                total_download_rate: 32768,
                total_upload_rate: 4096,
                process_count: 1,
            },
        )]
        .into(),
    );
    app.select_next();
    for i in 0..60 {
        app.history
            .update(1234, "fixture-worker".into(), i * 1024, i * 128);
    }
    match screen {
        "interfaces" => app.toggle_view_mode(),
        "interface-detail" => {
            app.toggle_view_mode();
            app.enter_interface_detail();
        }
        "overview" | "connections" | "traffic" | "system" => {
            app.enter_process_detail();
            app.detail_tab = match screen {
                "connections" => ProcessDetailTab::Connections,
                "traffic" => ProcessDetailTab::Traffic,
                "system" => ProcessDetailTab::System,
                _ => ProcessDetailTab::Overview,
            };
        }
        "help" => app.show_help = true,
        "throttle" => {
            app.show_throttle_dialog = true;
            app.throttle_dialog.target_pid = Some(1234);
            app.throttle_dialog.target_name = Some("fixture-worker".into());
        }
        "graph" => app.show_graph = true,
        "compatibility" => {
            app.show_backend_compatibility_dialog = true;
            app.backend_compatibility_dialog = Some(BackendCompatibilityDialog::new(
                "tc-htb".into(),
                TrafficType::Internet,
                vec!["nftables".into()],
                true,
            ));
        }
        _ => {}
    }
    let backend = crate::backends::throttle::BackendInfo {
        active_upload: Some("tc_htb".into()),
        active_download: Some("ifb_tc".into()),
        active_monitoring: Some("pnet".into()),
        active_socket_mapper: Some("libproc".into()),
        available_upload: vec![(
            "tc_htb".into(),
            crate::backends::BackendPriority::Good,
            true,
        )],
        available_download: vec![(
            "ifb_tc".into(),
            crate::backends::BackendPriority::Good,
            true,
        )],
        available_socket_mappers: vec![(
            "libproc".into(),
            crate::backends::BackendPriority::Best,
            true,
        )],
        preferred_upload: None,
        preferred_download: None,
        preferred_socket_mapper: None,
        upload_capabilities: None,
        download_capabilities: None,
        socket_mapper_capabilities: None,
        backend_stats: std::collections::HashMap::new(),
    };
    if screen == "backend" {
        app.show_backend_info = true;
        app.build_backend_items(&backend);
    }

    (app, backend)
}

#[test]
fn original_fixture_screens() {
    let output = std::env::var_os("BPROC_PARITY_OUTPUT").map(std::path::PathBuf::from);
    if let Some(path) = &output {
        std::fs::create_dir_all(path).unwrap();
    }
    for (width, height) in [(120, 40), (80, 24), (160, 50)] {
        for screen in [
            "process",
            "interfaces",
            "interface-detail",
            "overview",
            "connections",
            "traffic",
            "system",
            "help",
            "backend",
            "throttle",
            "graph",
            "compatibility",
        ] {
            let (mut app, backend) = fixture(screen);
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
            draw_ui_with_backend_info(
                &mut PaintCx::new(&mut Frame::new(&mut buffer)),
                &mut app,
                Some(&backend),
            );
            if screen != "graph" {
                // Captured from the pre-migration Ratatui TestBackend at d48ec4d9286d.
                // Process metadata was replaced with a deterministic empty fixture.
                let baselines: std::collections::HashMap<String, Vec<(usize, String)>> =
                    serde_json::from_str(include_str!("fixtures/ratatui-baseline.json")).unwrap();
                let key = format!("{screen}-{width}x{height}");
                let expected = baselines[&key]
                    .iter()
                    .flat_map(|(count, symbol)| std::iter::repeat_n(symbol.as_str(), *count))
                    .collect::<Vec<_>>();
                assert_eq!(expected.len(), usize::from(width) * usize::from(height));
                for y in 0..height {
                    for x in 0..width {
                        let cell = buffer.get(Point::new(x, y)).unwrap();
                        // BMUX explicitly represents the second cell of a wide glyph;
                        // Ratatui stores an ordinary blank there.
                        if cell.is_wide_continuation() {
                            continue;
                        }
                        let old = expected[usize::from(y) * usize::from(width) + usize::from(x)];
                        // The old header overwrote its right border at narrow widths.
                        // Preserve the repaired border rather than snapshotting that defect.
                        if width == 80 && x == 79 && y == 4 && old == "a" && cell.symbol == "│" {
                            continue;
                        }
                        assert_eq!(cell.symbol, old, "{key} at ({x},{y})");
                    }
                }
            }
            let lines = (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer.get(Point::new(x, y)).unwrap().symbol.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            let text = lines.join("\n");
            assert!(
                text.contains("bproc") || text.contains("Process Details"),
                "missing header: {screen}"
            );
            if let Some(output) = &output {
                std::fs::write(output.join(format!("{screen}-{width}x{height}.txt")), &text)
                    .unwrap();
                let cells = (0..height)
                    .flat_map(|y| (0..width).map(move |x| (x, y)))
                    .map(|(x, y)| format!("{x},{y}: {:?}", buffer.get(Point::new(x, y)).unwrap()))
                    .collect::<Vec<_>>();
                std::fs::write(
                    output.join(format!("{screen}-{width}x{height}.cells")),
                    cells.join("\n"),
                )
                .unwrap();
            }
        }
    }
}
