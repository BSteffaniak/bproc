use super::*;
use crate::config::DockPosition;
use crate::process::{ConnectionDetail, InterfaceStats, ProcessInfo};
use crate::procs::detail::{FileKind, OpenFile, ResourceLimit, ThreadInfo};
use crate::procs::model::{
    ExitedProcess, ProcSnapshot, ProcessEntry, ProcessRecord, ProcessSample, SystemSnapshot, now_ms,
};
use crate::procs::store::ProcessStore;
use crate::procs::watch::{EventKind, ProcEvent};
use crate::ui::explorer::LegacyRequest;
use crate::ui::explorer::hits::HitMap;
use bmux_tui::buffer::Buffer;
use bmux_tui::event::Event;
use bmux_tui::geometry::Point;
use bmux_tui::paint::PaintCx;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const START: u64 = 1_700_000_000;

fn k(pid: i32) -> ProcessKey {
    ProcessKey::new(pid, START + pid as u64)
}

fn entry(pid: i32, ppid: Option<i32>, name: &str, cpu: f32) -> ProcessEntry {
    ProcessEntry {
        record: Arc::new(ProcessRecord {
            key: k(pid),
            ppid,
            name: name.into(),
            cmdline: vec![
                format!("/usr/bin/{name}"),
                "--flag".into(),
                "a-very-long-argument-that-needs-wrapping-in-narrow-panels".into(),
            ],
            exe: Some(format!("/usr/bin/{name}")),
            cwd: Some("/Users/me/src".into()),
            uid: Some(501),
            user: Some("me".into()),
            ..Default::default()
        }),
        sample: ProcessSample {
            state: ProcState::Running,
            cpu_percent: cpu,
            cpu_time_ms: 61_230,
            mem_rss: 120 * 1024 * 1024,
            mem_virtual: 4 * 1024 * 1024 * 1024,
            mem_percent: 1.4,
            threads: Some(12),
            nice: Some(0),
            priority: Some(31),
            open_fds: Some(40),
            ..Default::default()
        },
    }
}

fn entries() -> Vec<ProcessEntry> {
    vec![
        entry(1, None, "launchd", 0.5),
        entry(100, Some(1), "zsh", 0.0),
        entry(200, Some(100), "cargo", 3.0),
        entry(300, Some(200), "bproc", 12.5),
        entry(400, Some(300), "worker-a", 1.0),
        entry(401, Some(300), "worker-b", 7.0),
    ]
}

fn deep(key: ProcessKey) -> DeepDetail {
    DeepDetail {
        key,
        collected_at_ms: now_ms(),
        facts: vec![("Session".into(), "42".into())],
        threads: Ok(vec![
            ThreadInfo {
                tid: 1,
                name: Some("main".into()),
                state: ProcState::Sleeping,
                cpu_percent: Some(0.5),
                cpu_time_ms: Some(1000),
                priority: Some(31),
            },
            ThreadInfo {
                tid: 2,
                name: Some("hot-loop".into()),
                state: ProcState::Running,
                cpu_percent: Some(11.0),
                cpu_time_ms: Some(9000),
                priority: Some(31),
            },
        ]),
        files: Ok(vec![
            OpenFile {
                fd: Some(0),
                kind: FileKind::Device,
                target: "/dev/ttys001".into(),
            },
            OpenFile {
                fd: Some(3),
                kind: FileKind::Socket,
                target: "TCP 127.0.0.1:8080".into(),
            },
            OpenFile {
                fd: Some(4),
                kind: FileKind::File,
                target: "/tmp/log.txt".into(),
            },
        ]),
        environment: Ok(vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), "/Users/me".into()),
        ]),
        memory_maps: Err("permission denied".into()),
        limits: Ok(vec![ResourceLimit {
            name: "Max open files".into(),
            soft: Some(256),
            hard: None,
            unit: "files".into(),
        }]),
        cgroups: Err("not supported".into()),
    }
}

fn fixture() -> ExplorerState {
    let mut state = ExplorerState::default();
    state.settings.confirm_signals = true;
    state.store = ProcessStore::from_entries(
        entries(),
        SystemSnapshot {
            mem_total: 16 * 1024 * 1024 * 1024,
            cpus: vec![0.0; 8],
            ..Default::default()
        },
    );
    state.table.select(k(300));
    state.detail.open(k(300), false);
    state.detail.apply(deep(k(300)));
    state
}

/// Paint only the panel into `area` of a `w`×`h` screen and commit its hits.
fn render(state: &mut ExplorerState, area: Rect, w: u16, h: u16) -> Buffer {
    let screen = Rect::new(0, 0, w, h);
    state.last_area = screen;
    let mut buffer = Buffer::empty(screen);
    let mut hits = HitMap::default();
    hits.begin();
    {
        let mut tf = bmux_tui::frame::Frame::new(&mut buffer);
        let mut cx = PaintCx::new(&mut tf);
        let mut frame = Frame {
            cx: &mut cx,
            hits: &mut hits,
            now: Instant::now(),
        };
        draw(&mut frame, area, state);
    }
    state.hits = hits;
    state.commit();
    buffer
}

/// Render where the explorer layout would place the panel.
fn render_laid_out(state: &mut ExplorerState, w: u16, h: u16) -> (Buffer, Rect) {
    let layout = state.layout(Rect::new(0, 0, w, h));
    let area = if state.detail.is_full() {
        layout.main
    } else {
        layout.dock
    };
    (render(state, area, w, h), area)
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    let area = buffer.area();
    (area.x..area.x + area.width)
        .filter_map(|x| buffer.get(Point::new(x, y)).map(|c| c.symbol.clone()))
        .collect()
}

fn all_text(buffer: &Buffer) -> String {
    let area = buffer.area();
    (area.y..area.y + area.height)
        .map(|y| row_text(buffer, y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn rect(state: &ExplorerState, hit: Hit) -> Rect {
    state
        .hits
        .rect_of(|h| *h == hit)
        .unwrap_or_else(|| panic!("no hit region for {hit:?}"))
}

fn mouse(state: &mut ExplorerState, kind: MouseEventKind, at: Point) -> Outcome {
    state.handle_event(&Event::Mouse(MouseEvent::new(kind, at)))
}

fn click(state: &mut ExplorerState, r: Rect) -> Outcome {
    state.pointer.last_click = None;
    mouse(
        state,
        MouseEventKind::Down(MouseButton::Left),
        Point::new(r.x, r.y),
    )
}

fn click_hit(state: &mut ExplorerState, hit: Hit) -> Outcome {
    let r = rect(state, hit);
    click(state, r)
}

fn key(state: &mut ExplorerState, code: KeyCode) -> Outcome {
    handle_key(state, &KeyStroke::simple(code))
}

#[test]
fn docked_panel_shows_title_crumbs_tabs_and_actions() {
    let mut state = fixture();
    let area = Rect::new(0, 20, 120, 16);
    let buffer = render(&mut state, area, 120, 40);
    let title = row_text(&buffer, 20);
    assert!(title.contains("bproc · PID 300 · me · running"), "{title}");
    assert!(title.contains("[⤢]") && title.contains("[✕]"), "{title}");
    let crumbs = row_text(&buffer, 21);
    assert!(crumbs.contains("launchd › zsh › cargo › bproc"), "{crumbs}");
    let tabs = row_text(&buffer, 22);
    for label in [
        "Overview",
        "Threads 2",
        "Files 3",
        "Env 2",
        "Tree 2",
        "Limits",
    ] {
        assert!(tabs.contains(label), "{label} missing from {tabs}");
    }
    let actions = row_text(&buffer, 35);
    for label in [
        "Terminate",
        "Kill",
        "Suspend",
        "Renice",
        "Throttle",
        "Watch",
        "Pin",
    ] {
        assert!(actions.contains(label), "{label} missing from {actions}");
    }

    // Hit regions cover exactly what is painted.
    let close = rect(&state, Hit::DetailClose);
    assert_eq!((close.y, close.width), (20, 3));
    assert!(row_text(&buffer, 20)[..].contains("[✕]"));
    assert_eq!(close.x + close.width, 119);
    let toggle = rect(&state, Hit::DetailDockToggle);
    assert_eq!(toggle.x + toggle.width + 1, close.x);
    let splitter = rect(&state, Hit::DetailSplitter);
    assert_eq!(splitter, Rect::new(0, 20, 120, 1));
    let launchd = rect(&state, Hit::ProcessLink { key: k(1) });
    assert_eq!(launchd.y, 21);
    assert_eq!(launchd.width, 7);
    let cell = buffer.get(Point::new(launchd.x, 21)).unwrap();
    assert_eq!(cell.symbol, "l");
    for i in 0..Tab::ALL.len() {
        assert_eq!(rect(&state, Hit::DetailTab(i)).y, 22);
    }
    let kill = rect(&state, Hit::DetailAction(Action::Kill));
    assert_eq!(kill.y, 35);
    let painted: String = (kill.x..kill.x + kill.width)
        .map(|x| buffer.get(Point::new(x, 35)).unwrap().symbol.clone())
        .collect();
    assert_eq!(painted, " Kill ");
    let body = rect(&state, Hit::DetailBody);
    assert_eq!((body.y, body.height), (23, 12));
}

#[test]
fn every_tab_renders_at_every_size_without_escaping_its_area() {
    for dock in [DockPosition::Bottom, DockPosition::Right] {
        for full in [false, true] {
            for tab in Tab::ALL {
                for (w, h) in [
                    (0, 0),
                    (1, 1),
                    (2, 2),
                    (10, 3),
                    (30, 6),
                    (44, 9),
                    (90, 30),
                    (220, 70),
                ] {
                    let mut state = fixture();
                    state.settings.dock = dock;
                    state.detail.set_full(full);
                    state.detail.set_tab(tab);
                    let area = Rect::new(0, 0, w, h);
                    let first = render(&mut state, area, w.max(1), h.max(1));
                    let second = render(&mut state, area, w.max(1), h.max(1));
                    assert_eq!(first, second, "{dock:?}/{full}/{tab:?} at {w}x{h}");
                    for (r, hit) in state.hits.committed() {
                        assert!(
                            r.x >= area.x
                                && r.y >= area.y
                                && r.x + r.width <= area.x + area.width
                                && r.y + r.height <= area.y + area.height,
                            "{hit:?} {r:?} escapes {area:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn sections_explain_errors_and_loading() {
    let mut state = fixture();
    state.detail.set_tab(Tab::Memory);
    let buffer = render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    let text = all_text(&buffer);
    assert!(text.contains("Memory maps unavailable"), "{text}");
    assert!(text.contains("permission denied"));

    state.detail.open(k(200), false);
    state.table.select(k(200));
    state.detail.set_tab(Tab::Threads);
    let text = all_text(&render(&mut state, Rect::new(0, 0, 100, 20), 100, 20));
    assert!(text.contains("Loading…"), "{text}");
}

#[test]
fn tab_contents_render_their_data() {
    let mut state = fixture();
    let cases = [
        (Tab::Threads, vec!["hot-loop", "TID"]),
        (Tab::Files, vec!["3 open", "1 socket", "/tmp/log.txt"]),
        (Tab::Env, vec!["HOME = /Users/me", "PATH = /usr/bin:/bin"]),
        (Tab::Limits, vec!["Max open files", "unlimited"]),
        (Tab::Tree, vec!["launchd", "worker-a", "worker-b"]),
        (Tab::Network, vec!["No network activity observed"]),
        (Tab::Events, vec!["No events recorded"]),
    ];
    for (tab, needles) in cases {
        state.detail.set_tab(tab);
        let text = all_text(&render(&mut state, Rect::new(0, 0, 110, 24), 110, 24));
        for needle in needles {
            assert!(text.contains(needle), "{tab:?}: {needle} missing\n{text}");
        }
    }
    // Threads sorted by CPU descending.
    state.detail.set_tab(Tab::Threads);
    let text = all_text(&render(&mut state, Rect::new(0, 0, 110, 24), 110, 24));
    assert!(text.find("hot-loop").unwrap() < text.find("main").unwrap());
}

#[test]
fn network_tab_shows_connections_interfaces_and_split() {
    let mut state = fixture();
    let mut info = ProcessInfo::new(300, "bproc".into());
    info.download_rate = 2048;
    info.internet_download_rate = 1024;
    info.interface_stats.insert(
        "en0".into(),
        InterfaceStats {
            download_rate: 2048,
            upload_rate: 10,
            total_download: 4096,
            total_upload: 20,
            internet_download_rate: 0,
            internet_upload_rate: 0,
            local_download_rate: 0,
            local_upload_rate: 0,
        },
    );
    info.connections.push(ConnectionDetail {
        protocol: "TCP".into(),
        local_addr: "10.0.0.2".parse().unwrap(),
        local_port: 50000,
        remote_addr: "1.1.1.1".parse().unwrap(),
        remote_port: 443,
        state: "ESTABLISHED".into(),
    });
    let mut map = HashMap::new();
    map.insert(300, info);
    state.store.merge_network(&map);
    state.detail.set_tab(Tab::Network);
    let buffer = render(&mut state, Rect::new(0, 0, 110, 24), 110, 24);
    let text = all_text(&buffer);
    for needle in [
        "Internet",
        "Local",
        "en0",
        "10.0.0.2:50000",
        "1.1.1.1:443",
        "ESTABLISHED",
    ] {
        assert!(text.contains(needle), "{needle} missing\n{text}");
    }
    assert!(text.contains("Network 1"));

    state.store.merge_network(&HashMap::new());
    state.network_note = Some("Capture needs root".into());
    let text = all_text(&render(&mut state, Rect::new(0, 0, 110, 24), 110, 24));
    assert!(text.contains("Capture needs root"), "{text}");
}

#[test]
fn overview_uses_two_columns_when_wide_and_stacks_when_narrow() {
    let mut state = fixture();
    let buffer = render(&mut state, Rect::new(0, 0, 140, 40), 140, 40);
    let text = all_text(&buffer);
    for needle in [
        "PPID", "launchd", "Command", "Session", "CPU time", "max", "Memory",
    ] {
        assert!(text.contains(needle), "{needle} missing\n{text}");
    }
    // Facts and graph labels share a row in two-column mode.
    let pid_row = (0..40)
        .map(|y| row_text(&buffer, y))
        .find(|r| r.contains("PID") && !r.contains("PPID") && !r.contains("·"))
        .unwrap();
    assert!(pid_row.contains("CPU"), "{pid_row}");
    // PPID is a link to the parent.
    let link = rect(&state, Hit::ProcessLink { key: k(200) });
    let painted: String = (link.x..link.x + link.width)
        .map(|x| buffer.get(Point::new(x, link.y)).unwrap().symbol.clone())
        .collect();
    assert!(painted == "200" || painted == "cargo", "{painted}");

    let buffer = render(&mut state, Rect::new(0, 0, 50, 40), 50, 40);
    let text = all_text(&buffer);
    assert!(
        text.contains("Disk R") && text.contains("Command"),
        "{text}"
    );
}

#[test]
fn clicking_tabs_close_and_dock_toggle() {
    let mut state = fixture();
    render(&mut state, Rect::new(0, 10, 120, 16), 120, 30);
    let files = rect(&state, Hit::DetailTab(Tab::Files.index()));
    assert_eq!(click(&mut state, files), Outcome::Consumed);
    assert_eq!(state.detail.tab(), Tab::Files);

    render(&mut state, Rect::new(0, 10, 120, 16), 120, 30);
    let at = rect(&state, Hit::DetailTab(0));
    mouse(
        &mut state,
        MouseEventKind::ScrollDown,
        Point::new(at.x, at.y),
    );
    assert_eq!(state.detail.tab(), Tab::Network);

    render(&mut state, Rect::new(0, 10, 120, 16), 120, 30);
    click_hit(&mut state, Hit::DetailDockToggle);
    assert!(state.detail.is_full());
    let (buffer, area) = render_laid_out(&mut state, 120, 30);
    assert!(all_text(&buffer).contains("[⤡]"));
    assert!(state.hits.rect_of(|h| *h == Hit::DetailSplitter).is_none());
    assert_eq!(rect(&state, Hit::DetailClose).y, area.y);

    click_hit(&mut state, Hit::DetailDockToggle);
    assert!(state.detail.is_docked());
    render(&mut state, Rect::new(0, 10, 120, 16), 120, 30);
    click_hit(&mut state, Hit::DetailClose);
    assert!(!state.detail.is_open());
}

#[test]
fn docked_panel_follows_selection_but_full_screen_keeps_its_key() {
    let mut state = fixture();
    state.take_requests();
    state.table.select(k(200));
    render(&mut state, Rect::new(0, 0, 100, 14), 100, 14);
    assert_eq!(state.detail.key(), Some(k(200)));
    assert!(state.detail.detail().is_none());
    assert_eq!(state.take_requests(), vec![UiRequest::LoadDetail(k(200))]);
    render(&mut state, Rect::new(0, 0, 100, 14), 100, 14);
    assert!(state.take_requests().is_empty(), "no duplicate loads");

    state.detail.set_full(true);
    state.table.select(k(1));
    render(&mut state, Rect::new(0, 0, 100, 30), 100, 30);
    assert_eq!(state.detail.key(), Some(k(200)));
    assert!(state.take_requests().is_empty());
}

#[test]
fn links_navigate_and_request_detail() {
    let mut state = fixture();
    state.take_requests();
    render(&mut state, Rect::new(0, 0, 120, 16), 120, 16);
    click_hit(&mut state, Hit::ProcessLink { key: k(100) });
    assert_eq!(state.table.selected(), Some(k(100)));
    assert_eq!(state.detail.key(), Some(k(100)));
    assert!(state.detail.is_docked());
    assert_eq!(state.take_requests(), vec![UiRequest::LoadDetail(k(100))]);

    // Tree tab rows link to children, in full-screen too.
    state.detail.set_full(true);
    state.detail.open(k(300), true);
    state.detail.set_tab(Tab::Tree);
    render(&mut state, Rect::new(0, 0, 120, 30), 120, 30);
    let child = rect(&state, Hit::ProcessLink { key: k(401) });
    assert_eq!(child.width, 118, "tree rows link across the content width");
    click(&mut state, child);
    assert_eq!(state.detail.key(), Some(k(401)));
    assert!(state.detail.is_full());
    assert_eq!(state.table.selected(), Some(k(401)));
}

#[test]
fn clicking_an_env_row_copies_it() {
    let mut state = fixture();
    state.detail.set_tab(Tab::Env);
    render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    state.take_requests();
    click_hit(&mut state, Hit::DetailRow { row: 1 });
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Copy("PATH=/usr/bin:/bin".into())]
    );
    assert!(state.flash.as_ref().unwrap().0.contains("PATH"));
}

#[test]
fn action_buttons_target_the_panel_process_and_keep_marks() {
    let mut state = fixture();
    state.table.mark_all(vec![k(1), k(100)]);
    state.table.select(k(1));
    state.detail.set_full(true);
    render(&mut state, Rect::new(0, 0, 120, 30), 120, 30);
    state.take_requests();
    click_hit(&mut state, Hit::DetailAction(Action::Throttle));
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Legacy(LegacyRequest::OpenThrottle {
            pid: 300,
            name: "bproc".into()
        })]
    );
    assert_eq!(state.table.selected(), Some(k(300)));
    assert_eq!(state.table.marked(), vec![k(1), k(100)]);

    state.settings.confirm_signals = false;
    render(&mut state, Rect::new(0, 0, 120, 30), 120, 30);
    click_hit(&mut state, Hit::DetailAction(Action::Suspend));
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(
            crate::procs::actions::ActionRequest::Signal {
                targets: vec![k(300)],
                signal: crate::procs::actions::Signal::Stop,
            }
        )]
    );
}

#[test]
fn exited_process_shows_banner_and_disables_actions() {
    let mut state = fixture();
    let mut live = entries();
    let gone = live.remove(3);
    state.store.apply_snapshot(ProcSnapshot {
        sequence: 2,
        acquired_at_ms: now_ms(),
        processes: live,
        exited: vec![ExitedProcess {
            record: gone.record,
            last_sample: gone.sample,
        }],
        ..Default::default()
    });
    let buffer = render(&mut state, Rect::new(0, 0, 120, 20), 120, 20);
    let text = all_text(&buffer);
    assert!(text.contains("Process exited"), "{text}");
    assert!(text.contains("exited"));
    assert!(text.contains("Terminate"), "buttons still drawn, disabled");
    assert!(
        !state
            .hits
            .committed()
            .iter()
            .any(|(_, h)| matches!(h, Hit::DetailAction(_)))
    );

    // Expired from the store entirely.
    state.store = ProcessStore::from_entries(Vec::new(), SystemSnapshot::default());
    state.table.select(k(300));
    let text = all_text(&render(&mut state, Rect::new(0, 0, 120, 20), 120, 20));
    assert!(text.contains("Process exited"), "{text}");
    assert!(text.contains("PID 300"));
}

#[test]
fn bottom_splitter_drag_resizes_and_saves() {
    let mut state = fixture();
    state.settings.dock_size = 14;
    let (_, area) = render_laid_out(&mut state, 120, 50);
    state.take_requests();
    let grip = rect(&state, Hit::DetailSplitter);
    assert_eq!(grip.y, area.y);
    let start = Point::new(grip.x + 2, grip.y);
    mouse(&mut state, MouseEventKind::Down(MouseButton::Left), start);
    mouse(
        &mut state,
        MouseEventKind::Drag(MouseButton::Left),
        Point::new(start.x, start.y - 3),
    );
    assert_eq!(state.settings.dock_size, area.height + 3);
    mouse(
        &mut state,
        MouseEventKind::Up(MouseButton::Left),
        Point::new(start.x, start.y - 3),
    );
    assert_eq!(state.take_requests(), vec![UiRequest::SaveSettings]);

    // Dragging far beyond the edges clamps.
    render_laid_out(&mut state, 120, 50);
    let grip = rect(&state, Hit::DetailSplitter);
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        Point::new(grip.x + 2, grip.y),
    );
    mouse(
        &mut state,
        MouseEventKind::Drag(MouseButton::Left),
        Point::new(grip.x + 2, 49),
    );
    assert_eq!(state.settings.dock_size, 6);
}

#[test]
fn right_splitter_drag_uses_four_cell_units() {
    let mut state = fixture();
    state.settings.dock = DockPosition::Right;
    state.settings.dock_size = 12;
    let (_, area) = render_laid_out(&mut state, 160, 40);
    let grip = rect(&state, Hit::DetailSplitter);
    assert_eq!((grip.x, grip.width, grip.height), (area.x, 1, area.height));
    let start = Point::new(grip.x, grip.y + 3);
    mouse(&mut state, MouseEventKind::Down(MouseButton::Left), start);
    mouse(
        &mut state,
        MouseEventKind::Drag(MouseButton::Left),
        Point::new(start.x - 8, start.y),
    );
    assert_eq!(state.settings.dock_size, 14);
    state.take_requests();
    mouse(
        &mut state,
        MouseEventKind::Up(MouseButton::Left),
        Point::new(start.x - 8, start.y),
    );
    assert_eq!(state.take_requests(), vec![UiRequest::SaveSettings]);
}

#[test]
fn click_without_drag_does_not_resize_or_save() {
    let mut state = fixture();
    state.settings.dock_size = 200;
    render_laid_out(&mut state, 120, 40);
    state.take_requests();
    let grip = rect(&state, Hit::DetailSplitter);
    let at = Point::new(grip.x + 2, grip.y);
    mouse(&mut state, MouseEventKind::Down(MouseButton::Left), at);
    mouse(&mut state, MouseEventKind::Up(MouseButton::Left), at);
    assert_eq!(state.settings.dock_size, 200);
    assert!(state.take_requests().is_empty());
}

#[test]
fn keyboard_full_screen_tabs_scroll_and_escape() {
    let mut state = fixture();
    let mut vars: Vec<(String, String)> = (0..60)
        .map(|i| (format!("VAR_{i:02}"), format!("value {i}")))
        .collect();
    vars.push(("HOME".into(), "/Users/me".into()));
    let mut detail = deep(k(300));
    detail.environment = Ok(vars);
    state.detail.apply(detail);

    // Docked: only braces are handled; scrolling keys go to the table.
    assert_eq!(key(&mut state, KeyCode::Char('}')), Outcome::Consumed);
    assert_eq!(state.detail.tab(), Tab::Threads);
    assert_eq!(key(&mut state, KeyCode::Char('{')), Outcome::Consumed);
    assert_eq!(key(&mut state, KeyCode::Char('{')), Outcome::Consumed);
    assert_eq!(state.detail.tab(), Tab::Events);
    assert_eq!(key(&mut state, KeyCode::Down), Outcome::Ignored);
    assert_eq!(key(&mut state, KeyCode::Tab), Outcome::Ignored);
    assert_eq!(key(&mut state, KeyCode::Escape), Outcome::Ignored);

    state.detail.set_full(true);
    assert_eq!(key(&mut state, KeyCode::Tab), Outcome::Consumed);
    assert_eq!(state.detail.tab(), Tab::Overview);
    let shift_tab = KeyStroke {
        key: KeyCode::Tab,
        modifiers: bmux_keyboard::Modifiers {
            shift: true,
            ..Default::default()
        },
    };
    assert_eq!(handle_key(&mut state, &shift_tab), Outcome::Consumed);
    assert_eq!(state.detail.tab(), Tab::Events);

    state.detail.set_tab(Tab::Env);
    render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    key(&mut state, KeyCode::PageDown);
    let page = state.detail.scroll();
    assert!(page > 2, "{page}");
    key(&mut state, KeyCode::End);
    let max = state.detail.scroll();
    assert!(max > page);
    key(&mut state, KeyCode::PageDown);
    assert_eq!(state.detail.scroll(), max, "clamped to content");
    key(&mut state, KeyCode::PageUp);
    assert!(state.detail.scroll() < max);
    key(&mut state, KeyCode::Home);
    assert_eq!(state.detail.scroll(), 0);
    key(&mut state, KeyCode::Up);
    assert_eq!(state.detail.scroll(), 0);

    // Wheel over the body scrolls; offsets are per tab.
    render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    let body = rect(&state, Hit::DetailBody);
    mouse(
        &mut state,
        MouseEventKind::ScrollDown,
        Point::new(body.x + 1, body.y + 1),
    );
    assert_eq!(state.detail.scroll(), 3);
    state.detail.set_tab(Tab::Files);
    assert_eq!(state.detail.scroll(), 0);
    state.detail.set_tab(Tab::Env);
    assert_eq!(state.detail.scroll(), 3);

    state.table.select(k(1));
    assert_eq!(key(&mut state, KeyCode::Escape), Outcome::Consumed);
    assert!(state.detail.is_docked());
    assert_eq!(state.table.selected(), Some(k(300)));
}

#[test]
fn events_tab_lists_this_process_newest_first() {
    let mut state = fixture();
    for (i, (key, message)) in [(k(300), "first"), (k(1), "other"), (k(300), "second")]
        .into_iter()
        .enumerate()
    {
        state.events.push(ProcEvent {
            at_ms: now_ms() + i as u64,
            kind: EventKind::Alert,
            key: Some(key),
            name: "bproc".into(),
            message: message.into(),
        });
    }
    state.detail.set_tab(Tab::Events);
    let text = all_text(&render(&mut state, Rect::new(0, 0, 100, 20), 100, 20));
    assert!(text.contains("Events 2"), "{text}");
    assert!(!text.contains("other"));
    assert!(text.find("second").unwrap() < text.find("first").unwrap());
}

#[test]
fn full_explorer_draw_routes_clicks_to_the_panel() {
    let mut state = fixture();
    let area = Rect::new(0, 0, 120, 40);
    let mut terminal = bmux_tui::terminal::Terminal::new(Vec::<u8>::new(), area);
    terminal
        .draw(|cx| state.draw(cx, area, &crate::ui::explorer::DrawEnv::default()))
        .unwrap();
    state.commit();
    let tab = rect(&state, Hit::DetailTab(Tab::Limits.index()));
    let layout = state.layout(area);
    assert!(tab.y >= layout.dock.y && tab.y < layout.dock.y + layout.dock.height);
    click(&mut state, tab);
    assert_eq!(state.detail.tab(), Tab::Limits);
}

fn shifted(state: &mut ExplorerState, code: KeyCode) -> Outcome {
    let stroke = KeyStroke {
        key: code,
        modifiers: bmux_keyboard::Modifiers {
            shift: true,
            ..Default::default()
        },
    };
    handle_key(state, &stroke)
}

fn long_env(state: &mut ExplorerState) {
    let vars: Vec<(String, String)> = (0..60)
        .map(|i| (format!("VAR_{i:02}"), format!("value {i}")))
        .collect();
    let mut detail = deep(k(300));
    detail.environment = Ok(vars);
    state.detail.apply(detail);
}

#[test]
fn docked_panel_scrolls_with_shifted_keys() {
    let mut state = fixture();
    long_env(&mut state);
    state.detail.set_tab(Tab::Env);
    render(&mut state, Rect::new(0, 20, 100, 16), 100, 40);
    assert!(state.detail.is_docked());
    // Plain arrows still belong to the table.
    assert_eq!(key(&mut state, KeyCode::Down), Outcome::Ignored);
    assert_eq!(shifted(&mut state, KeyCode::Down), Outcome::Consumed);
    assert_eq!(shifted(&mut state, KeyCode::Down), Outcome::Consumed);
    assert_eq!(state.detail.scroll(), 2);
    assert_eq!(shifted(&mut state, KeyCode::Up), Outcome::Consumed);
    assert_eq!(state.detail.scroll(), 1);
    assert_eq!(shifted(&mut state, KeyCode::PageDown), Outcome::Consumed);
    assert!(state.detail.scroll() > 2);
    assert_eq!(shifted(&mut state, KeyCode::PageUp), Outcome::Consumed);
    assert_eq!(state.detail.scroll(), 1);
    // Table selection untouched.
    assert_eq!(state.table.selected(), Some(k(300)));
    // The docked hint advertises it.
    let text = all_text(&render(&mut state, Rect::new(0, 20, 120, 16), 120, 40));
    assert!(text.contains("⇧↑↓ scroll"), "{text}");
}

#[test]
fn full_screen_env_cursor_copies_with_y_and_enter() {
    let mut state = fixture();
    state.detail.set_full(true);
    state.detail.set_tab(Tab::Env);
    render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    state.take_requests();
    // No cursor yet: y explains rather than copying.
    assert_eq!(key(&mut state, KeyCode::Char('y')), Outcome::Consumed);
    assert!(state.take_requests().is_empty());
    assert_eq!(key(&mut state, KeyCode::Enter), Outcome::Ignored);

    assert_eq!(key(&mut state, KeyCode::Down), Outcome::Consumed);
    assert_eq!(key(&mut state, KeyCode::Char('y')), Outcome::Consumed);
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Copy("HOME=/Users/me".into())]
    );
    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Down); // clamped at the last row
    assert_eq!(key(&mut state, KeyCode::Enter), Outcome::Consumed);
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Copy("PATH=/usr/bin:/bin".into())]
    );

    // The cursor row is highlighted across the content width.
    let buffer = render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    let line = state.detail.cursor_line().unwrap();
    let y = (0..20)
        .find(|y| row_text(&buffer, *y).contains("PATH = "))
        .unwrap();
    let body = rect(&state, Hit::DetailBody);
    assert_eq!(usize::from(y - body.y), line - state.detail.scroll());
    let sel = theme::selected();
    for x in [body.x + 1, body.x + 60] {
        assert_eq!(buffer.get(Point::new(x, y)).unwrap().style.bg, sel.bg);
    }
    let wide = render(&mut state, Rect::new(0, 0, 120, 20), 120, 20);
    assert!(all_text(&wide).contains("↑↓ row · y copy"));
}

#[test]
fn full_screen_cursor_follows_and_snaps_to_scroll() {
    let mut state = fixture();
    long_env(&mut state);
    state.detail.set_full(true);
    state.detail.set_tab(Tab::Env);
    render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    for _ in 0..30 {
        key(&mut state, KeyCode::Down);
    }
    // Moving past the bottom scrolls to keep the cursor visible.
    let line = state.detail.cursor_line().unwrap();
    assert!(state.detail.scroll() > 0);
    let buffer = render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    assert!(all_text(&buffer).contains("VAR_29"));
    assert_eq!(state.detail.cursor_line(), Some(line));

    // Home scrolls; the cursor snaps onto a visible row on the next paint.
    key(&mut state, KeyCode::Home);
    render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    let line = state.detail.cursor_line().unwrap();
    assert!(line < 20, "{line}");
    state.take_requests();
    key(&mut state, KeyCode::Char('y'));
    let copied = state.take_requests();
    assert!(
        matches!(&copied[..], [UiRequest::Copy(v)] if v.starts_with("VAR_")),
        "{copied:?}"
    );

    // Up from the first row reveals the heading.
    for _ in 0..80 {
        key(&mut state, KeyCode::Up);
    }
    assert_eq!(state.detail.scroll(), 0);
    assert_eq!(state.detail.cursor_line(), Some(2));
}

#[test]
fn full_screen_tree_cursor_follows_links_with_enter() {
    let mut state = fixture();
    state.detail.set_full(true);
    state.detail.set_tab(Tab::Tree);
    render(&mut state, Rect::new(0, 0, 120, 30), 120, 30);
    state.take_requests();
    // launchd, zsh, cargo, then (skipping bproc itself) worker-b, worker-a.
    for _ in 0..4 {
        key(&mut state, KeyCode::Down);
    }
    assert_eq!(key(&mut state, KeyCode::Enter), Outcome::Consumed);
    assert_eq!(state.detail.key(), Some(k(401)));
    assert!(state.detail.is_full());
    assert_eq!(state.take_requests(), vec![UiRequest::LoadDetail(k(401))]);
    // New process: cursor resets.
    assert_eq!(state.detail.cursor_line(), None);
    let text = all_text(&render(&mut state, Rect::new(0, 0, 120, 30), 120, 30));
    assert!(text.contains("Enter open"), "{text}");
    // `y` does nothing outside env rows.
    key(&mut state, KeyCode::Down);
    assert_eq!(key(&mut state, KeyCode::Char('y')), Outcome::Ignored);
}

#[test]
fn tabs_without_rows_scroll_with_arrows() {
    let mut state = fixture();
    let mut detail = deep(k(300));
    detail.limits = Ok((0..40)
        .map(|i| ResourceLimit {
            name: format!("Limit {i}"),
            soft: Some(i),
            hard: None,
            unit: "files".into(),
        })
        .collect());
    state.detail.apply(detail);
    state.detail.set_full(true);
    state.detail.set_tab(Tab::Limits);
    render(&mut state, Rect::new(0, 0, 100, 20), 100, 20);
    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Down);
    assert_eq!(state.detail.scroll(), 2);
    assert_eq!(state.detail.cursor_line(), None);

    // The overview's PPID link does not capture the arrows: a narrow,
    // stacked overview keeps scrolling.
    state.detail.set_tab(Tab::Overview);
    render(&mut state, Rect::new(0, 0, 50, 16), 50, 16);
    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Down);
    assert_eq!(state.detail.scroll(), 2);
    assert_eq!(state.detail.cursor_line(), None);
}

fn deep_with_platform_facts(key: ProcessKey) -> DeepDetail {
    let mut d = deep(key);
    d.facts = [
        ("Executable", "/usr/bin/bproc"),
        ("Command", "/usr/bin/bproc --flag"),
        ("Working dir", "/Users/me/src"),
        ("Root", "/"),
        ("User", "me (501)"),
        ("Parent", "200 (cargo)"),
        ("Process group", "300"),
        ("Session", "42"),
        ("Started", "2023-11-14 22:13:20"),
        ("Nice", "0"),
        ("Threads", "12"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    d
}

#[test]
fn overview_does_not_repeat_facts_and_labels_keep_a_gap() {
    let mut state = fixture();
    state.detail.apply(deep_with_platform_facts(k(300)));
    state.detail.set_full(true);
    let buffer = render(&mut state, Rect::new(0, 0, 120, 40), 120, 40);
    let text = all_text(&buffer);
    for label in [
        "Executable",
        "Working dir",
        "Command",
        "User ",
        "PPID",
        "Started",
    ] {
        assert_eq!(text.matches(label).count(), 1, "{label} repeated\n{text}");
    }
    assert!(!text.contains("Parent"), "{text}");
    assert!(!text.contains("me (501)"), "{text}");
    // Extra platform facts are still merged.
    for needle in ["Root", "Session", "42"] {
        assert!(text.contains(needle), "{needle} missing\n{text}");
    }
    // The longest label is shown whole, with a gap before its value.
    assert!(text.contains("Process group 300"), "{text}");
    assert!(text.contains("Working dir   /Users/me/src"), "{text}");
    assert!(!text.contains("Process gr…"));
}

#[test]
fn overview_falls_back_to_detail_facts_when_the_sample_lacks_them() {
    let mut state = fixture();
    let mut bare = entries();
    {
        let record = Arc::make_mut(&mut bare[3].record);
        record.exe = None;
        record.cwd = None;
        record.user = None;
        record.uid = None;
    }
    state.store = ProcessStore::from_entries(bare, SystemSnapshot::default());
    state.detail.apply(deep_with_platform_facts(k(300)));
    let text = all_text(&render(&mut state, Rect::new(0, 0, 120, 40), 120, 40));
    assert!(text.contains("me (501)"), "{text}");
    assert_eq!(text.matches("Working dir").count(), 1, "{text}");
    assert!(text.contains("/Users/me/src"), "{text}");
}

#[test]
fn limited_access_and_truncation_show_a_notice_under_the_tabs() {
    let mut state = fixture();
    let mut d = deep(k(300));
    d.facts.push((
        "Access".into(),
        "limited: run bproc with sudo for full detail".into(),
    ));
    d.facts.push((
        "Truncated".into(),
        "showing the first 4096 open files".into(),
    ));
    state.detail.apply(d);
    let area = Rect::new(0, 20, 120, 16);
    let buffer = render(&mut state, area, 120, 40);
    let tabs = row_text(&buffer, 22);
    assert!(tabs.contains("Overview"), "{tabs}");
    let notice = row_text(&buffer, 23);
    assert!(
        notice.contains("⚠ Access limited: run bproc with sudo"),
        "{notice}"
    );
    assert!(
        notice.contains("showing the first 4096 open files"),
        "{notice}"
    );
    assert_eq!(rect(&state, Hit::DetailBody).y, 24);
    // Not repeated in the overview body.
    assert_eq!(all_text(&buffer).matches("sudo").count(), 1);

    // No caveats, no notice row.
    state.detail.apply(deep(k(300)));
    render(&mut state, area, 120, 40);
    assert_eq!(rect(&state, Hit::DetailBody).y, 23);
}

#[test]
fn limits_tab_lists_cgroups_when_known() {
    let mut state = fixture();
    let mut d = deep(k(300));
    d.cgroups = Ok(vec!["0::/user.slice/user-501.slice/session-2.scope".into()]);
    state.detail.apply(d);
    state.detail.set_tab(Tab::Limits);
    let text = all_text(&render(&mut state, Rect::new(0, 0, 110, 24), 110, 24));
    for needle in ["Max open files", "CGROUP", "user-501.slice/session-2.scope"] {
        assert!(text.contains(needle), "{needle} missing\n{text}");
    }

    // Cgroups survive a limits failure; errors elsewhere stay explained.
    let mut d = deep(k(300));
    d.limits = Err("permission denied".into());
    d.cgroups = Ok(vec!["0::/init.scope".into()]);
    state.detail.apply(d);
    let text = all_text(&render(&mut state, Rect::new(0, 0, 110, 24), 110, 24));
    assert!(
        text.contains("Limits unavailable · permission denied"),
        "{text}"
    );
    assert!(text.contains("/init.scope"), "{text}");

    let mut d = deep(k(300));
    d.limits = Err("permission denied".into());
    state.detail.apply(d);
    let text = all_text(&render(&mut state, Rect::new(0, 0, 110, 24), 110, 24));
    assert!(text.contains("Limits unavailable"), "{text}");
    assert!(!text.contains("CGROUP"));
}
