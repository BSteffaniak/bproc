use super::*;
use crate::procs::ProcessKey;
use crate::procs::model::{
    ProcSnapshot, ProcessEntry, ProcessRecord, ProcessSample, SystemSnapshot, TaskCounts, now_ms,
};
use crate::procs::store::ProcessStore;
use crate::ui::explorer::hits::HitMap;
use crate::ui::explorer::{LegacyRequest, UiRequest};
use bmux_tui::buffer::Buffer;
use bmux_tui::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use bmux_tui::frame::Frame as TuiFrame;
use bmux_tui::geometry::Point;
use bmux_tui::paint::PaintCx;
use std::sync::Arc;
use std::time::Instant;

const WIDTHS: [u16; 6] = [1, 20, 40, 80, 120, 160];
const GIB: u64 = 1024 * 1024 * 1024;

fn entry(pid: i32, name: &str, cpu: f32) -> ProcessEntry {
    ProcessEntry {
        record: Arc::new(ProcessRecord {
            key: ProcessKey::new(pid, 1000),
            ppid: Some(1),
            name: name.into(),
            uid: Some(501),
            ..Default::default()
        }),
        sample: ProcessSample {
            cpu_percent: cpu,
            mem_rss: 1024 * 1024 * u64::from(pid.unsigned_abs()),
            disk_read_rate: 3 * 1024 * 1024,
            disk_write_rate: 200 * 1024,
            ..Default::default()
        },
    }
}

fn system(cores: usize) -> SystemSnapshot {
    SystemSnapshot {
        cpus: (0..cores).map(|i| (i * 13 % 101) as f32).collect(),
        cpu_total: 42.0,
        mem_total: 16 * GIB,
        mem_used: 5 * GIB,
        swap_total: 2 * GIB,
        swap_used: GIB / 4,
        load_average: [1.2, 0.98, 0.75],
        uptime_secs: 3 * 86_400 + 4 * 3600 + 12 * 60 + 55,
        tasks: TaskCounts {
            total: 523,
            running: 3,
            threads: 1203,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn entries() -> Vec<ProcessEntry> {
    vec![
        entry(1, "launchd", 1.0),
        entry(200, "bproc", 12.0),
        entry(300, "cargo", 80.0),
    ]
}

fn fixture(cores: usize) -> ExplorerState {
    ExplorerState {
        store: ProcessStore::from_entries(entries(), system(cores)),
        ..Default::default()
    }
}

type Painter = fn(&mut Frame<'_, '_, '_>, Rect, &ExplorerState);

fn render_with(
    state: &ExplorerState,
    width: u16,
    height: u16,
    paint: impl FnOnce(&mut Frame<'_, '_, '_>, Rect, &ExplorerState),
) -> (Buffer, Vec<(Rect, Hit)>) {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let mut hits = HitMap::default();
    {
        let mut tui = TuiFrame::new(&mut buffer);
        let mut cx = PaintCx::new(&mut tui);
        let mut frame = Frame {
            cx: &mut cx,
            hits: &mut hits,
            now: Instant::now(),
        };
        paint(&mut frame, area, state);
    }
    hits.commit();
    (buffer, hits.committed().to_vec())
}

fn render(
    state: &ExplorerState,
    width: u16,
    height: u16,
    paint: Painter,
) -> (Buffer, Vec<(Rect, Hit)>) {
    render_with(state, width, height, paint)
}

fn row_text(buffer: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .filter_map(|x| buffer.get(Point::new(x, y)))
        .map(|c| c.symbol.as_str())
        .collect()
}

fn all_text(buffer: &Buffer, width: u16, height: u16) -> String {
    (0..height)
        .map(|y| row_text(buffer, y, width))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_in_bounds(hits: &[(Rect, Hit)], width: u16, height: u16) {
    for (r, hit) in hits {
        assert!(
            r.x + r.width <= width && r.y + r.height <= height,
            "{hit:?} at {r:?} escapes {width}x{height}"
        );
    }
}

fn assert_disjoint(hits: &[(Rect, Hit)]) {
    for (i, (a, ha)) in hits.iter().enumerate() {
        for (b, hb) in &hits[i + 1..] {
            let overlap = a.y == b.y && a.x < b.x + b.width && b.x < a.x + a.width;
            assert!(!overlap, "{ha:?} {a:?} overlaps {hb:?} {b:?}");
        }
    }
}

fn has(hits: &[(Rect, Hit)], hit: &Hit) -> bool {
    hits.iter().any(|(_, h)| h == hit)
}

fn mouse(state: &mut ExplorerState, kind: MouseEventKind, rect: Rect) -> Outcome {
    let point = Point::new(rect.x + rect.width / 2, rect.y);
    let outcome = state.handle_event(&Event::Mouse(MouseEvent::new(kind, point)));
    state.handle_event(&Event::Mouse(MouseEvent::new(
        MouseEventKind::Up(MouseButton::Left),
        point,
    )));
    outcome
}

/// Redraw (so hit regions match the current state), then left-click `target`.
fn click(state: &mut ExplorerState, target: Hit) -> Outcome {
    draw_full(state, 160, 40);
    let rect = state
        .hits
        .rect_of(|h| *h == target)
        .unwrap_or_else(|| panic!("no hit region for {target:?}"));
    mouse(state, MouseEventKind::Down(MouseButton::Left), rect)
}

fn draw_full(state: &mut ExplorerState, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    {
        let mut tui = TuiFrame::new(&mut buffer);
        let mut cx = PaintCx::new(&mut tui);
        state.draw(&mut cx, area, &DrawEnv::default());
    }
    state.commit();
    buffer
}

// ---- header -----------------------------------------------------------------------

#[test]
fn header_height_is_bounded_and_zero_when_tiny() {
    for cores in [0, 1, 4, 8, 12, 16, 24, 64, 256] {
        let state = fixture(cores);
        for width in 0..=200 {
            let h = header_height(&state, width);
            assert!(h <= MAX_HEADER_ROWS, "{cores} cores at {width}: {h}");
            if width < MIN_HEADER_WIDTH {
                assert_eq!(h, 0);
            } else {
                assert!(h >= 2, "{cores} cores at {width}: {h}");
            }
        }
    }
    assert_eq!(header_height(&ExplorerState::default(), 80), 6);
}

#[test]
fn cpu_layout_picks_columns_by_width_and_cores() {
    assert_eq!(cpu_layout(8, 40, 6, false), CpuLayout::Total);
    assert_eq!(cpu_layout(1, 40, 6, true), CpuLayout::Total);
    assert_eq!(cpu_layout(0, 40, 6, true), CpuLayout::Total);
    assert_eq!(
        cpu_layout(8, 40, 6, true),
        CpuLayout::Cores { cols: 2, rows: 4 }
    );
    assert_eq!(
        cpu_layout(16, 80, 6, true),
        CpuLayout::Cores { cols: 4, rows: 4 }
    );
    assert_eq!(
        cpu_layout(12, 39, 6, true),
        CpuLayout::Cores { cols: 3, rows: 4 }
    );
    assert_eq!(
        cpu_layout(24, 80, 6, true),
        CpuLayout::Cores { cols: 4, rows: 6 }
    );
    assert_eq!(cpu_layout(16, 20, 6, true), CpuLayout::Strip { rows: 2 });
    assert_eq!(cpu_layout(64, 80, 6, true), CpuLayout::Strip { rows: 1 });
    assert_eq!(cpu_layout(16, 40, 2, true), CpuLayout::Strip { rows: 1 });
    assert_eq!(cpu_layout(16, 6, 2, true), CpuLayout::Total);
    assert_eq!(cpu_layout(16, 80, 0, true), CpuLayout::Total);
}

#[test]
fn header_renders_meters_and_hits_at_every_width() {
    for cores in [0, 8, 16] {
        let mut state = fixture(cores);
        for per_core in [true, false] {
            state.settings.per_core_meters = per_core;
            for width in WIDTHS {
                let height = header_height(&state, width).max(1);
                let (buffer, hits) = render(&state, width, height, draw_header);
                assert_in_bounds(&hits, width, height);
                assert_disjoint(&hits);
                if width < MIN_HEADER_WIDTH {
                    assert!(hits.is_empty());
                    continue;
                }
                let text = all_text(&buffer, width, height);
                assert!(text.contains("Mem["), "{width}: {text}");
                assert!(
                    text.contains("CPU[") || text.contains("0["),
                    "{width}: {text}"
                );
                assert!(has(&hits, &Hit::Meter(MeterKind::Mem)));
                assert!(
                    hits.iter()
                        .any(|(_, h)| matches!(h, Hit::Meter(MeterKind::Cpu | MeterKind::Core(_))))
                );
            }
        }
    }
}

#[test]
fn wide_header_shows_per_core_bars_and_all_info() {
    let state = fixture(16);
    let (buffer, hits) = render(&state, 160, 6, draw_header);
    let text = all_text(&buffer, 160, 6);
    for needle in [
        " 3[",
        "15[",
        "Mem[",
        "5.0G/16.0G]",
        "Swp[",
        "256M/2.0G]",
        "Tasks: 523, 1203 thr; 3 running",
        "Load average: 1.20 0.98 0.75",
        "Uptime: 3d 04:12:55",
        "Net ↓ 0 ↑ 0",
        "Disk R 9.0M/s W 600K/s",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in\n{text}");
    }
    for i in 0..16 {
        assert!(has(&hits, &Hit::Meter(MeterKind::Core(i))));
    }
    for kind in [
        MeterKind::Mem,
        MeterKind::Swap,
        MeterKind::Tasks,
        MeterKind::Load,
        MeterKind::Uptime,
        MeterKind::Net,
        MeterKind::Disk,
    ] {
        assert!(has(&hits, &Hit::Meter(kind)), "{kind:?}");
    }
}

#[test]
fn bar_fill_is_heat_coloured_and_text_right_aligned() {
    let joined = |item: &Item| {
        item.parts
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<String>()
    };
    let item = bar("CPU", 20, 50.0, &["50.0%"], Hit::Meter(MeterKind::Cpu));
    assert_eq!(item.width(), 20);
    assert_eq!(joined(&item), "CPU[||||||||  50.0%]");
    let fill = item.parts.iter().find(|(t, _)| t.starts_with('|')).unwrap();
    assert_eq!(fill.1.fg, Some(theme::heat(50.0)));
    let item = bar(
        "Mem",
        12,
        100.0,
        &["5.2G/16.0G", "33%"],
        Hit::Meter(MeterKind::Mem),
    );
    assert_eq!(joined(&item), "Mem[||||33%]");
    let item = bar("Mem", 6, 100.0, &["5.2G/16.0G"], Hit::Meter(MeterKind::Mem));
    assert_eq!(joined(&item), "Mem[|]");
    let item = bar("X", 2, f64::NAN, &["long"], Hit::Meter(MeterKind::Cpu));
    assert!(item.width() <= 2);
    let item = bar("CPU", 10, -5.0, &[], Hit::Meter(MeterKind::Cpu));
    assert_eq!(joined(&item), "CPU[     ]");
}

#[test]
fn network_meter_shows_rates_or_capture_off() {
    let mut state = fixture(8);
    state.network_note = Some("Packet capture needs root".into());
    let (buffer, _) = render(&state, 160, 6, draw_header);
    assert!(all_text(&buffer, 160, 6).contains("Net: capture off"));

    let mut info = crate::process::ProcessInfo::new(200, "bproc".into());
    info.download_rate = 1_258_291;
    info.upload_rate = 80 * 1024;
    let map: crate::process::ProcessMap = [(200, info)].into_iter().collect();
    state.store.merge_network(&map);
    state.store.apply_snapshot(ProcSnapshot {
        sequence: 2,
        acquired_at_ms: now_ms(),
        system: system(8),
        processes: entries(),
        ..Default::default()
    });
    let height = header_height(&state, 160);
    let (buffer, hits) = render(&state, 160, height, draw_header);
    let text = all_text(&buffer, 160, height);
    assert!(text.contains("Net ↓ 1.2M/s ↑ 80.0K/s"), "{text}");
    // A second sample enables the CPU history sparkline.
    assert!(text.contains("CPU ▄▄"), "{text}");
    assert_disjoint(&hits);
}

#[test]
fn sparkline_uses_spare_rows_under_a_total_bar() {
    let mut state = fixture(8);
    state.settings.per_core_meters = false;
    state.store.apply_snapshot(ProcSnapshot {
        sequence: 2,
        acquired_at_ms: now_ms(),
        system: SystemSnapshot {
            cpu_total: 95.0,
            ..system(8)
        },
        processes: entries(),
        ..Default::default()
    });
    let height = header_height(&state, 120);
    let (buffer, hits) = render(&state, 120, height, draw_header);
    assert_eq!(row_text(&buffer, 1, 10), "    ▄█    ");
    let spark = hits
        .iter()
        .filter(|(r, h)| *h == Hit::Meter(MeterKind::Cpu) && r.y == 1)
        .map(|(r, _)| *r)
        .next()
        .unwrap();
    assert_eq!((spark.x, spark.width), (4, 2));
}

#[test]
fn header_clips_to_a_short_area() {
    let state = fixture(16);
    let (_, hits) = render(&state, 160, 2, draw_header);
    assert_in_bounds(&hits, 160, 2);
    let (_, hits) = render(&state, 160, 0, draw_header);
    assert!(hits.is_empty());
}

#[test]
fn narrow_header_uses_core_strip() {
    let state = fixture(16);
    let h = header_height(&state, 40);
    let (_, hits) = render(&state, 40, h, draw_header);
    assert!(has(&hits, &Hit::Meter(MeterKind::Cpu)));
    let cores = hits
        .iter()
        .filter(|(r, h)| matches!(h, Hit::Meter(MeterKind::Core(_))) && r.width == 1)
        .count();
    assert_eq!(cores, 16);
}

#[test]
fn uptime_and_packing_helpers() {
    assert_eq!(uptime_text(59), "00:00:59");
    assert_eq!(uptime_text(3 * 86_400 + 15_175), "3d 04:12:55");
    let entries = info_entries(&fixture(4));
    assert!(pack_info(&entries, 10, 4).len() <= 4);
    assert!(pack_info(&entries, 80, 0).is_empty());
    let wide = pack_info(&entries, 200, 4);
    assert_eq!(wide.iter().map(Vec::len).sum::<usize>(), 5);
    assert_eq!(wide.len(), 1);
    // Tight budgets drop the highest-ranked entries first.
    let one = pack_info(&entries, 40, 1);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0][0].hit, Some(Hit::Meter(MeterKind::Tasks)));
}

// ---- toolbar ----------------------------------------------------------------------

#[test]
fn toolbar_shows_screens_presets_and_chips() {
    let mut state = fixture(8);
    state.paused = true;
    state.table.toggle_mark(ProcessKey::new(200, 1000));
    let (buffer, hits) = render(&state, 160, 1, draw_toolbar);
    let text = row_text(&buffer, 0, 160);
    for needle in [
        "Processes",
        "Events",
        "Watches",
        "Overview",
        "Network",
        "Tree",
        "Mine",
        "Net-active",
        "Paused ⏸",
        "1 marked ✕",
    ] {
        assert!(text.contains(needle), "missing {needle} in {text}");
    }
    assert!(!text.contains("Frozen"));
    for preset in Preset::ALL {
        assert!(has(&hits, &Hit::Preset(preset)), "{preset:?}");
    }
    for screen in Screen::ALL {
        assert!(has(&hits, &Hit::Screen(screen)));
    }
    let style_at = |hit: Hit| {
        let r = hits.iter().find(|(_, h)| *h == hit).unwrap().0;
        buffer.get(Point::new(r.x + 1, 0)).unwrap().style
    };
    assert_eq!(
        style_at(Hit::Preset(Preset::Overview)),
        theme::header_active()
    );
    assert_eq!(style_at(Hit::Preset(Preset::Cpu)).fg, theme::dim().fg);
    assert_eq!(style_at(Hit::Chip(Chip::Tree)), theme::chip_off());
    assert_eq!(
        style_at(Hit::Screen(Screen::Processes)),
        theme::header_active()
    );
    assert_disjoint(&hits);
}

#[test]
fn toolbar_chips_reflect_toggles() {
    let mut state = fixture(8);
    state.settings.tree = true;
    state.settings.only_mine = true;
    state.table.toggle_freeze();
    let (buffer, hits) = render(&state, 160, 1, draw_toolbar);
    let style_at = |hit: Hit| {
        let r = hits.iter().find(|(_, h)| *h == hit).unwrap().0;
        buffer.get(Point::new(r.x + 1, 0)).unwrap().style
    };
    assert_eq!(style_at(Hit::Chip(Chip::Tree)), theme::chip_on());
    assert_eq!(style_at(Hit::Chip(Chip::Users)), theme::chip_on());
    assert_eq!(style_at(Hit::Chip(Chip::NetActive)), theme::chip_off());
    assert!(row_text(&buffer, 0, 160).contains("Frozen ❄"));
    assert!(!has(&hits, &Hit::Chip(Chip::Pause)));
    assert!(!has(&hits, &Hit::Chip(Chip::Marks)));
}

#[test]
fn toolbar_badges_alerts() {
    let mut state = fixture(8);
    state.alerting.insert(ProcessKey::new(200, 1000));
    state.alerting.insert(ProcessKey::new(300, 1000));
    let (buffer, _) = render(&state, 160, 1, draw_toolbar);
    assert!(row_text(&buffer, 0, 160).contains("Watches ⚠2"));
    assert_eq!(badge(7), "7");
    assert_eq!(badge(1000), "99+");
}

#[test]
fn toolbar_never_overflows_and_degrades() {
    let mut state = fixture(8);
    state.paused = true;
    state.table.toggle_freeze();
    state.table.toggle_mark(ProcessKey::new(200, 1000));
    for width in 0..=170u16 {
        let (_, hits) = render_with(&state, width.max(1), 1, |f, a, s| {
            draw_toolbar(f, Rect::new(a.x, a.y, width, 1), s)
        });
        assert_in_bounds(&hits, width.max(1), 1);
        assert_disjoint(&hits);
        if width >= 8 {
            assert!(has(&hits, &Hit::Screen(Screen::Processes)), "{width}");
        }
    }
    // Compact widths keep the important state chips longest.
    let (_, hits) = render(&state, 70, 1, draw_toolbar);
    assert!(has(&hits, &Hit::Chip(Chip::Marks)));
    assert!(!has(&hits, &Hit::Chip(Chip::NetActive)));
    let (_, hits) = render(&state, 40, 1, draw_toolbar);
    assert!(has(&hits, &Hit::Preset(Preset::Overview)));
}

#[test]
fn toolbar_hides_presets_off_the_processes_screen() {
    let mut state = fixture(8);
    state.screen = Screen::Events;
    let (buffer, hits) = render(&state, 160, 1, draw_toolbar);
    assert!(
        !hits
            .iter()
            .any(|(_, h)| matches!(h, Hit::Preset(_) | Hit::Chip(_)))
    );
    let r = hits
        .iter()
        .find(|(_, h)| *h == Hit::Screen(Screen::Events))
        .unwrap()
        .0;
    assert_eq!(
        buffer.get(Point::new(r.x + 1, 0)).unwrap().style,
        theme::header_active()
    );
}

fn push_events(state: &mut ExplorerState, n: usize) {
    for i in 0..n {
        state.events.push(crate::procs::watch::ProcEvent {
            at_ms: now_ms(),
            kind: crate::procs::watch::EventKind::Info,
            key: None,
            name: format!("e{i}"),
            message: String::new(),
        });
    }
}

fn hit_rect(hits: &[(Rect, Hit)], target: &Hit) -> Rect {
    hits.iter()
        .find(|(_, h)| h == target)
        .unwrap_or_else(|| panic!("no {target:?}"))
        .0
}

/// The toolbar width a 120-column terminal leaves next to the search box.
const TOOLBAR_AT_120: u16 = 80;

#[test]
fn toolbar_keeps_readable_screen_tabs_at_120_columns() {
    let mut state = fixture(8);
    push_events(&mut state, 18);
    let (buffer, hits) = render(&state, TOOLBAR_AT_120, 1, draw_toolbar);
    let text = row_text(&buffer, 0, TOOLBAR_AT_120);
    assert!(text.contains("Events •18"), "{text}");
    assert!(text.contains("│ Watch"), "{text}");
    assert!(!text.contains("│ Wa "), "{text}");
    for preset in Preset::ALL {
        assert!(has(&hits, &Hit::Preset(preset)), "{preset:?} in {text}");
    }
}

#[test]
fn toolbar_tabs_stay_put_as_the_event_badge_grows() {
    for width in [40, 60, TOOLBAR_AT_120, 100, 120] {
        let mut previous: Option<(Rect, Rect)> = None;
        let mut presets = None;
        for count in [0, 1, 9, 10, 99, 100, 5000] {
            let mut state = fixture(8);
            state.paused = true;
            state.table.toggle_mark(ProcessKey::new(200, 1000));
            push_events(&mut state, count);
            let (_, hits) = render(&state, width, 1, draw_toolbar);
            let watches = hit_rect(&hits, &Hit::Screen(Screen::Watches));
            let processes = hit_rect(&hits, &Hit::Screen(Screen::Processes));
            // Tabs keep their label form: only the badge digits change.
            if let Some((p, w)) = previous {
                assert_eq!(p, processes, "width {width} count {count}");
                assert_eq!(watches.width, w.width, "width {width} count {count}");
            }
            previous = Some((processes, watches));
            let shown: Vec<_> = hits
                .iter()
                .filter(|(_, h)| matches!(h, Hit::Preset(_)))
                .map(|(_, h)| h.clone())
                .collect();
            assert_eq!(
                *presets.get_or_insert_with(|| shown.clone()),
                shown,
                "width {width} count {count}"
            );
        }
    }
}

#[test]
fn toolbar_tabs_do_not_move_between_screens() {
    let mut state = fixture(8);
    push_events(&mut state, 18);
    for width in [60, TOOLBAR_AT_120, 100] {
        state.screen = Screen::Processes;
        let (_, on_procs) = render(&state, width, 1, draw_toolbar);
        state.screen = Screen::Events;
        let (_, on_events) = render(&state, width, 1, draw_toolbar);
        for screen in Screen::ALL {
            assert_eq!(
                hit_rect(&on_procs, &Hit::Screen(screen)).width,
                hit_rect(&on_events, &Hit::Screen(screen)).width,
                "{screen:?} at {width}"
            );
        }
    }
}

#[test]
fn net_active_chip_is_not_labelled_like_the_net_preset() {
    let state = fixture(8);
    for width in 30..=170u16 {
        let (buffer, hits) = render(&state, width, 1, draw_toolbar);
        if let Some((r, _)) = hits.iter().find(|(_, h)| *h == Hit::Chip(Chip::NetActive)) {
            let label: String = (r.x..r.x + r.width)
                .filter_map(|x| buffer.get(Point::new(x, 0)))
                .map(|c| c.symbol.as_str())
                .collect();
            assert_ne!(label.trim(), preset_short(Preset::Network), "{width}");
            assert_ne!(label.trim(), Preset::Network.label(), "{width}");
        }
    }
}

#[test]
fn toolbar_leaves_a_gap_before_the_search_box() {
    let mut state = fixture(8);
    state.paused = true;
    state.table.toggle_mark(ProcessKey::new(200, 1000));
    for width in 20..=170u16 {
        let (buffer, hits) = render(&state, width, 1, draw_toolbar);
        let last = buffer.get(Point::new(width - 1, 0)).unwrap();
        assert_eq!(last.symbol, " ", "width {width}");
        assert!(
            hits.iter().all(|(r, _)| r.x + r.width < width),
            "a hit touches the right edge at {width}"
        );
    }
}

// ---- F-key bar --------------------------------------------------------------------

fn action_label(action: Action) -> &'static str {
    FKEYS.iter().find(|(_, a, _)| *a == action).unwrap().2
}

#[test]
fn fkeys_fill_width_and_hit_every_visible_key() {
    let state = fixture(8);
    for width in WIDTHS {
        let (buffer, hits) = render(&state, width, 1, draw_fkeys);
        assert_in_bounds(&hits, width, 1);
        assert_disjoint(&hits);
        let text = row_text(&buffer, 0, width);
        for (key, action, _) in FKEYS {
            let painted = text.contains(&format!("{key}{}", fkey_short(*action)))
                || text.contains(&format!("{key}{}", action_label(*action)));
            if painted {
                assert!(has(&hits, &Hit::FKey(*action)), "{key} at {width}");
            }
        }
        assert!(has(&hits, &Hit::FKey(Action::Quit)), "{width}");
        if width >= 80 {
            assert_eq!(hits.len(), FKEYS.len());
            let covered: u16 = hits.iter().map(|(r, _)| r.width).sum();
            assert_eq!(covered, width);
            assert!(text.starts_with("F1Help"));
        }
    }
    let (buffer, _) = render(&state, 160, 1, draw_fkeys);
    assert!(row_text(&buffer, 0, 160).contains("F2Setup"));
    let (buffer, _) = render(&state, 40, 1, draw_fkeys);
    let text = row_text(&buffer, 0, 40);
    assert!(
        text.contains("F10Quit") && text.contains("F9Kill") && text.contains("F3Find"),
        "{text}"
    );
}

#[test]
fn fkeys_drop_nice_before_kill() {
    let state = fixture(8);
    let (buffer, hits) = render(&state, 60, 1, draw_fkeys);
    let text = row_text(&buffer, 0, 60);
    assert!(
        text.contains("F9Kill") && text.contains("F10Quit"),
        "{text}"
    );
    assert!(!has(&hits, &Hit::FKey(Action::NiceDown)), "{text}");
    assert!(!has(&hits, &Hit::FKey(Action::NiceUp)), "{text}");
    // F9 sits right next to F10.
    let f9 = hit_rect(&hits, &Hit::FKey(Action::Signal));
    let f10 = hit_rect(&hits, &Hit::FKey(Action::Quit));
    assert_eq!(f9.x + f9.width, f10.x);
    // Kill outlives every other key except Quit.
    let index = |action: Action| FKEYS.iter().position(|(_, a, _)| *a == action).unwrap();
    for width in 0..=120 {
        let (keys, _) = fkey_selection(width);
        if keys.len() >= 2 {
            assert!(keys.contains(&index(Action::Signal)), "{width}: {keys:?}");
        }
        // Nice- and Nice+ come and go together.
        assert_eq!(
            keys.contains(&index(Action::NiceDown)),
            keys.contains(&index(Action::NiceUp)),
            "{width}"
        );
    }
}

#[test]
fn fkeys_drop_by_priority_but_keep_quit() {
    let (keys, short) = fkey_selection(40);
    assert!(short);
    assert_eq!(*keys.last().unwrap(), FKEYS.len() - 1);
    assert!(keys.len() < FKEYS.len());
    assert!(keys.windows(2).all(|w| w[0] < w[1]));
    let (keys, short) = fkey_selection(200);
    assert!(!short);
    assert_eq!(keys.len(), FKEYS.len());
    assert_eq!(fkey_selection(1).0, vec![FKEYS.len() - 1]);
    assert_eq!(fkey_selection(0).0, vec![FKEYS.len() - 1]);
}

#[test]
fn fkeys_dim_process_actions_without_a_target() {
    let state = ExplorerState::default();
    let (buffer, hits) = render(&state, 120, 1, draw_fkeys);
    let label_style = |action: Action| {
        let r = hits
            .iter()
            .find(|(_, h)| *h == Hit::FKey(action))
            .unwrap()
            .0;
        buffer.get(Point::new(r.x + 3, 0)).unwrap().style
    };
    for action in [Action::NiceDown, Action::NiceUp, Action::Signal] {
        assert_eq!(label_style(action).fg, theme::dim().fg);
    }
    assert_ne!(label_style(Action::Help).fg, theme::dim().fg);
    let key_cell = buffer.get(Point::new(0, 0)).unwrap();
    assert_eq!(key_cell.style, theme::key());

    let mut state = fixture(4);
    assert!(!fkey_enabled(&state, Action::Signal));
    state.table.toggle_mark(ProcessKey::new(200, 1000));
    assert!(fkey_enabled(&state, Action::Signal));
    state.table.clear_marks();
    state.table.select(ProcessKey::new(200, 1000));
    assert!(fkey_enabled(&state, Action::NiceUp));
}

// ---- status line ------------------------------------------------------------------

#[test]
fn status_shows_flash_counts_note_and_right_side() {
    let mut state = fixture(8);
    state.network_note = Some("capture needs root".into());
    state.flash("Tree view");
    state.search.set_text("cargo");
    state.traffic = TrafficType::Internet;
    let env = DrawEnv {
        status: "legacy",
        interface_filter: Some("en0".into()),
        socket_mapper: None,
    };
    let (buffer, hits) = render_with(&state, 160, 1, |f, a, s| draw_status(f, a, s, &env));
    let text = row_text(&buffer, 0, 160);
    for needle in [
        "Tree view",
        "3 processes · showing 0 · filter: cargo",
        "⚠ net capture off",
        "Traffic: Internet",
        "iface: en0",
        "1.5s",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in {text}");
    }
    assert!(!text.contains("legacy"));
    assert!(text.trim_end().ends_with("1.5s"));
    assert!(has(&hits, &Hit::StatusNote));
    assert!(has(&hits, &Hit::FKey(Action::TrafficMode)));
    assert!(has(&hits, &Hit::FKey(Action::Interfaces)));
    assert_disjoint(&hits);

    state.flash = Some(("old".into(), Instant::now() - Duration::from_secs(5)));
    state.paused = true;
    let (buffer, _) = render_with(&state, 160, 1, |f, a, s| draw_status(f, a, s, &env));
    let text = row_text(&buffer, 0, 160);
    assert!(text.contains("legacy") && !text.contains("old"), "{text}");
    assert!(text.contains("PAUSED"));
}

#[test]
fn status_counts_exclude_and_mention_exited_rows() {
    let mut state = fixture(8);
    let gone = entry(300, "cargo", 80.0);
    state.store.apply_snapshot(ProcSnapshot {
        sequence: 2,
        acquired_at_ms: now_ms(),
        system: system(8),
        processes: vec![entry(1, "launchd", 1.0), entry(200, "bproc", 12.0)],
        exited: vec![crate::procs::model::ExitedProcess {
            record: gone.record.clone(),
            last_sample: gone.sample.clone(),
        }],
        ..Default::default()
    });
    draw_full(&mut state, 160, 40);
    assert!(state.table.rows().iter().any(|r| r.ghost));
    let env = DrawEnv::default();
    let (buffer, _) = render_with(&state, 160, 1, |f, a, s| draw_status(f, a, s, &env));
    let text = row_text(&buffer, 0, 160);
    assert!(
        text.contains("2 processes · showing 2 · 1 exited"),
        "{text}"
    );
    let x = text.find("1 exited").unwrap();
    let x = cells(display_width(&text[..x]));
    let style = buffer.get(Point::new(x, 0)).unwrap().style;
    assert_eq!(style.fg, theme::dim().fg);

    // Without ghosts there is no exited note at all.
    let state = fixture(8);
    let (buffer, _) = render_with(&state, 160, 1, |f, a, s| draw_status(f, a, s, &env));
    assert!(!row_text(&buffer, 0, 160).contains("exited"));
}

#[test]
fn status_never_overflows() {
    let mut state = fixture(8);
    state.network_note = Some("capture needs root".into());
    state.flash("A rather long flash message that should be truncated nicely");
    let env = DrawEnv {
        status: "",
        interface_filter: Some("en0-with-a-long-name".into()),
        socket_mapper: None,
    };
    for width in 1..=170 {
        let (buffer, hits) = render_with(&state, width, 1, |f, a, s| draw_status(f, a, s, &env));
        assert_in_bounds(&hits, width, 1);
        if width >= 8 {
            assert_disjoint(&hits);
            assert!(row_text(&buffer, 0, width).contains("1.5s"), "{width}");
        }
    }
    let (buffer, _) = render_with(&state, 80, 1, |f, a, s| draw_status(f, a, s, &env));
    assert!(row_text(&buffer, 0, 80).contains("A rather long"));
}

#[test]
fn status_helpers() {
    assert_eq!(refresh_label(1500), "1.5s");
    assert_eq!(refresh_label(2000), "2s");
    assert_eq!(refresh_label(250), "0.2s");
    assert_eq!(traffic_label(TrafficType::Local), "Local");
    let help = capture_help("Capture failed.");
    assert!(help.starts_with("Capture failed. Per-process"), "{help}");
    assert!(capture_help("").starts_with("Per-process"));
}

// ---- integration through ExplorerState --------------------------------------------

#[test]
fn full_draw_never_panics_on_tiny_terminals() {
    for (w, h) in [
        (1, 1),
        (2, 1),
        (1, 5),
        (10, 3),
        (20, 8),
        (40, 12),
        (80, 24),
        (160, 50),
    ] {
        let mut state = fixture(16);
        state.paused = true;
        state.network_note = Some("no capture".into());
        draw_full(&mut state, w, h);
        assert_in_bounds(state.hits.committed(), w, h);
    }
}

#[test]
fn every_chrome_target_is_clickable_in_a_full_frame() {
    let mut state = fixture(8);
    state.network_note = Some("no capture".into());
    draw_full(&mut state, 160, 40);
    let hits = state.hits.committed().to_vec();
    for (_, action, _) in FKEYS {
        assert!(has(&hits, &Hit::FKey(*action)), "{action:?}");
    }
    for preset in Preset::ALL {
        assert!(has(&hits, &Hit::Preset(preset)), "{preset:?}");
    }
    assert!(has(&hits, &Hit::StatusNote));
    assert!(has(&hits, &Hit::Meter(MeterKind::Core(7))));
}

#[test]
fn clicking_presets_screens_and_chips_updates_state() {
    let mut state = fixture(8);
    assert_eq!(
        click(&mut state, Hit::Preset(Preset::Memory)),
        Outcome::Consumed
    );
    assert_eq!(state.settings.preset, Preset::Memory);
    assert!(state.outbox.contains(&UiRequest::SaveSettings));

    draw_full(&mut state, 160, 40);
    let rect = state.hits.rect_of(|h| matches!(h, Hit::Preset(_))).unwrap();
    mouse(&mut state, MouseEventKind::ScrollDown, rect);
    assert_eq!(state.settings.preset, Preset::Disk);
    mouse(&mut state, MouseEventKind::ScrollUp, rect);
    assert_eq!(state.settings.preset, Preset::Memory);

    assert!(!state.settings.tree);
    click(&mut state, Hit::Chip(Chip::Tree));
    assert!(state.settings.tree);
    click(&mut state, Hit::Chip(Chip::Users));
    assert!(state.settings.only_mine);
    click(&mut state, Hit::Chip(Chip::NetActive));
    assert!(state.settings.net_active_only);

    click(&mut state, Hit::Screen(Screen::Watches));
    assert_eq!(state.screen, Screen::Watches);
    draw_full(&mut state, 160, 40);
    let rect = state.hits.rect_of(|h| matches!(h, Hit::Screen(_))).unwrap();
    mouse(&mut state, MouseEventKind::ScrollDown, rect);
    assert_eq!(state.screen, Screen::Processes);
    click(&mut state, Hit::Screen(Screen::Events));
    assert_eq!(state.screen, Screen::Events);
    click(&mut state, Hit::Screen(Screen::Processes));
    assert_eq!(state.screen, Screen::Processes);
}

#[test]
fn clicking_meters_switches_presets_and_sort() {
    let mut state = fixture(8);
    click(&mut state, Hit::Meter(MeterKind::Mem));
    assert_eq!(state.settings.preset, Preset::Memory);
    assert_eq!(state.table.sort().column, ColumnId::Rss);

    click(&mut state, Hit::Meter(MeterKind::Core(3)));
    assert_eq!(state.settings.preset, Preset::Cpu);
    assert_eq!(state.table.sort().column, ColumnId::Cpu);
    assert!(state.table.sort().descending);

    click(&mut state, Hit::Meter(MeterKind::Swap));
    assert_eq!(state.settings.preset, Preset::Memory);
    click(&mut state, Hit::Meter(MeterKind::Net));
    assert_eq!(state.settings.preset, Preset::Network);
    click(&mut state, Hit::Meter(MeterKind::Disk));
    assert_eq!(state.settings.preset, Preset::Disk);

    click(&mut state, Hit::Meter(MeterKind::Tasks));
    assert!(state.settings.tree);

    click(&mut state, Hit::Meter(MeterKind::Load));
    assert_eq!(state.screen, Screen::Events);
    state.screen = Screen::Processes;
    click(&mut state, Hit::Meter(MeterKind::Uptime));
    assert_eq!(state.screen, Screen::Events);
    // Meters work from any screen and bring the table back.
    click(&mut state, Hit::Meter(MeterKind::Mem));
    assert_eq!(state.screen, Screen::Processes);
}

#[test]
fn clicking_fkeys_status_and_marks() {
    let mut state = fixture(8);
    state.network_note = Some("capture needs root".into());
    state.table.toggle_mark(ProcessKey::new(200, 1000));
    click(&mut state, Hit::Chip(Chip::Marks));
    assert!(state.table.marked().is_empty());

    click(&mut state, Hit::FKey(Action::Tree));
    assert!(state.settings.tree);

    state.take_requests();
    click(&mut state, Hit::FKey(Action::Quit));
    assert!(state.take_requests().contains(&UiRequest::Quit));

    click(&mut state, Hit::FKey(Action::TrafficMode));
    assert!(
        state
            .take_requests()
            .contains(&UiRequest::Legacy(LegacyRequest::TrafficMode))
    );

    let before = state.toasts.items.len();
    click(&mut state, Hit::StatusNote);
    assert_eq!(state.toasts.items.len(), before + 1);
    let toast = state.toasts.items.last().unwrap();
    assert_eq!(toast.severity, Severity::Info);
    assert!(toast.text.contains("capture needs root"));

    click(&mut state, Hit::Chip(Chip::Pause));
    assert!(state.paused);
    click(&mut state, Hit::Chip(Chip::Pause));
    assert!(!state.paused);
}

#[test]
fn non_left_buttons_and_moves_are_ignored() {
    let mut state = fixture(8);
    let hit = Hit::Preset(Preset::Memory);
    for kind in [
        MouseEventKind::Down(MouseButton::Right),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Move,
    ] {
        let ev = MouseEvent::new(kind, Point::new(0, 0));
        let c = Click {
            mouse: &ev,
            hit: &hit,
            double: false,
        };
        assert_eq!(handle_mouse(&mut state, &c), Outcome::Ignored);
    }
    assert_eq!(state.settings.preset, Preset::Overview);
    let ev = MouseEvent::new(MouseEventKind::ScrollDown, Point::new(0, 0));
    let other = Hit::Meter(MeterKind::Mem);
    let c = Click {
        mouse: &ev,
        hit: &other,
        double: false,
    };
    assert_eq!(handle_mouse(&mut state, &c), Outcome::Ignored);
}
