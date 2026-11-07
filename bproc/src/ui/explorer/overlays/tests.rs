use super::menu::{ItemKind, MenuCommand, MenuItem, MenuOverlay};
use super::*;
use crate::procs::actions::ActionRequest;
use crate::procs::columns::{ColumnId, Preset};
use crate::procs::export::ExportFormat;
use crate::procs::model::{ProcState, ProcessEntry, ProcessRecord, ProcessSample, SystemSnapshot};
use crate::procs::store::ProcessStore;
use crate::ui::explorer::UiRequest;
use crate::ui::explorer::hits::{Hit, contains};
use bmux_keyboard::{KeyCode, KeyStroke, Modifiers};
use bmux_tui::buffer::Buffer;
use bmux_tui::event::{MouseButton, MouseEvent, MouseEventKind};
use bmux_tui::paint::PaintCx;
use std::sync::Arc;

fn entry(pid: i32, ppid: Option<i32>, name: &str, user: &str) -> ProcessEntry {
    ProcessEntry {
        record: Arc::new(ProcessRecord {
            key: ProcessKey::new(pid, 1000 + pid as u64),
            ppid,
            name: name.into(),
            cmdline: vec![format!("/usr/bin/{name}"), "--flag".into()],
            exe: Some(format!("/usr/bin/{name}")),
            user: Some(user.into()),
            uid: Some(501),
            ..Default::default()
        }),
        sample: ProcessSample {
            state: ProcState::Sleeping,
            nice: Some(0),
            ..Default::default()
        },
    }
}

fn key(pid: i32) -> ProcessKey {
    ProcessKey::new(pid, 1000 + pid as u64)
}

fn fixture() -> ExplorerState {
    let mut stopped = entry(300, Some(1), "paused", "me");
    stopped.sample.state = ProcState::Stopped;
    stopped.sample.nice = Some(5);
    let store = ProcessStore::from_entries(
        vec![
            entry(1, None, "launchd", "root"),
            entry(100, Some(1), "nginx", "www"),
            entry(101, Some(100), "nginx-worker", "www"),
            entry(102, Some(100), "nginx-worker", "www"),
            entry(103, Some(101), "helper", "www"),
            entry(200, Some(1), "zsh", "me"),
            stopped,
        ],
        SystemSnapshot::default(),
    );
    ExplorerState {
        store,
        last_area: Rect::new(0, 0, 100, 40),
        ..Default::default()
    }
}

/// Draw the overlay stack only and commit its hit regions.
fn render(state: &mut ExplorerState, w: u16, h: u16) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    state.last_area = area;
    let mut buffer = Buffer::empty(area);
    let mut bf = bmux_tui::frame::Frame::new(&mut buffer);
    let mut cx = PaintCx::new(&mut bf);
    let mut hits = std::mem::take(&mut state.hits);
    hits.begin();
    {
        let mut frame = Frame {
            cx: &mut cx,
            hits: &mut hits,
            now: std::time::Instant::now(),
        };
        for index in 0..state.overlays.len() {
            draw(&mut frame, area, index, state);
        }
    }
    state.hits = hits;
    state.commit();
    buffer
}

fn screen(buffer: &Buffer) -> String {
    (0..buffer.area().height)
        .filter_map(|y| buffer.row_symbols(y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn cells(buffer: &Buffer, r: Rect) -> String {
    (r.x..r.x + r.width)
        .filter_map(|x| buffer.get(Point::new(x, r.y)).map(|c| c.symbol.clone()))
        .collect()
}

fn press(state: &mut ExplorerState, code: KeyCode) {
    state.handle_event(&Event::Key(KeyStroke::simple(code)));
}

fn press_shift(state: &mut ExplorerState, code: KeyCode) {
    let modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    state.handle_event(&Event::Key(KeyStroke::with_modifiers(code, modifiers)));
}

fn mouse(state: &mut ExplorerState, kind: MouseEventKind, at: Point) {
    state.handle_event(&Event::Mouse(MouseEvent::new(kind, at)));
}

fn click(state: &mut ExplorerState, at: Point) {
    mouse(state, MouseEventKind::Down(MouseButton::Left), at);
    mouse(state, MouseEventKind::Up(MouseButton::Left), at);
}

/// Rect of the item / control hit whose painted cells contain `label`.
fn find(state: &ExplorerState, buffer: &Buffer, label: &str) -> Rect {
    state
        .hits
        .committed()
        .iter()
        .rev()
        .find(|(r, h)| {
            matches!(h, Hit::OverlayItem { .. } | Hit::OverlayControl { .. })
                && cells(buffer, *r).contains(label)
        })
        .map(|(r, _)| *r)
        .unwrap_or_else(|| panic!("no hit labelled {label:?} in\n{}", screen(buffer)))
}

/// Click the item or control labelled `label`.
fn tap(state: &mut ExplorerState, buffer: &Buffer, label: &str) {
    let r = find(state, buffer, label);
    click(state, center(r));
}

fn center(r: Rect) -> Point {
    Point::new(r.x + r.width / 2, r.y + r.height / 2)
}

fn menu(state: &ExplorerState) -> &MenuOverlay {
    match state.overlays.last() {
        Some(Overlay::Menu(m)) => m,
        _ => panic!("top overlay is not a menu"),
    }
}

fn signal(state: &ExplorerState) -> &SignalDialog {
    match state.overlays.last() {
        Some(Overlay::Signal(d)) => d,
        _ => panic!("top overlay is not the signal dialog"),
    }
}

fn renice(state: &ExplorerState) -> &ReniceDialog {
    match state.overlays.last() {
        Some(Overlay::Renice(d)) => d,
        _ => panic!("top overlay is not the renice dialog"),
    }
}

fn root_renice(state: &ExplorerState, targets: Vec<ProcessKey>, lower: bool) -> Overlay {
    Overlay::Renice(ReniceDialog::with_privilege(
        state,
        targets,
        Some(lower),
        true,
    ))
}

fn user_renice(state: &ExplorerState, targets: Vec<ProcessKey>, lower: Option<bool>) -> Overlay {
    Overlay::Renice(ReniceDialog::with_privilege(state, targets, lower, false))
}

fn affinity(state: &ExplorerState) -> &AffinityDialog {
    match state.overlays.last() {
        Some(Overlay::Affinity(d)) => d,
        _ => panic!("top overlay is not the affinity dialog"),
    }
}

fn chooser(state: &ExplorerState) -> &ColumnChooser {
    match state.overlays.last() {
        Some(Overlay::Columns(d)) => d,
        _ => panic!("top overlay is not the column chooser"),
    }
}

fn copy(label: &str) -> MenuItem {
    MenuItem::command(
        label,
        MenuCommand::Copy {
            text: label.into(),
            what: label.into(),
        },
    )
}

fn sample_menu(at: Point) -> MenuOverlay {
    MenuOverlay::new(
        at,
        vec![
            MenuItem::header("Header"),
            copy("Alpha"),
            MenuItem::separator(),
            copy("Bravo").enabled(false),
            copy("Charlie"),
            MenuItem::submenu("Delta", vec![copy("Echo"), copy("Foxtrot"), copy("Fig")]),
            copy("Cedar"),
        ],
    )
}

fn copied(state: &mut ExplorerState) -> Vec<String> {
    state
        .take_requests()
        .into_iter()
        .filter_map(|r| match r {
            UiRequest::Copy(t) => Some(t),
            _ => None,
        })
        .collect()
}

// ---- menu mechanics --------------------------------------------------------

#[test]
fn keyboard_navigation_skips_headers_separators_and_disabled() {
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(2, 2))));
    assert_eq!(menu(&state).cursor, vec![Some(1)], "starts on first item");
    press(&mut state, KeyCode::Down);
    assert_eq!(
        menu(&state).cursor,
        vec![Some(4)],
        "skips separator + disabled"
    );
    press(&mut state, KeyCode::Up);
    assert_eq!(menu(&state).cursor, vec![Some(1)]);
    press(&mut state, KeyCode::Up);
    assert_eq!(menu(&state).cursor, vec![Some(6)], "wraps to the end");
    press(&mut state, KeyCode::Home);
    assert_eq!(menu(&state).cursor, vec![Some(1)]);
    press(&mut state, KeyCode::End);
    assert_eq!(menu(&state).cursor, vec![Some(6)]);
}

#[test]
fn submenus_open_with_right_and_close_with_left_and_escape() {
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(2, 2))));
    press(&mut state, KeyCode::Down);
    press(&mut state, KeyCode::Down);
    assert_eq!(menu(&state).current().unwrap().label, "Delta");
    press(&mut state, KeyCode::Right);
    assert_eq!(menu(&state).cursor, vec![Some(5), Some(0)]);
    press(&mut state, KeyCode::Down);
    assert_eq!(menu(&state).current().unwrap().label, "Foxtrot");
    press(&mut state, KeyCode::Left);
    assert_eq!(menu(&state).levels(), 1);
    press(&mut state, KeyCode::Enter);
    assert_eq!(menu(&state).levels(), 2, "Enter opens a submenu too");
    press(&mut state, KeyCode::Escape);
    assert_eq!(menu(&state).levels(), 1, "Escape closes one level");
    press(&mut state, KeyCode::Escape);
    assert!(state.overlays.is_empty(), "Escape at the top closes");
}

#[test]
fn enter_activates_and_closes() {
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(2, 2))));
    press(&mut state, KeyCode::Enter);
    assert!(state.overlays.is_empty());
    assert_eq!(copied(&mut state), ["Alpha"]);
    assert_eq!(state.flash.as_ref().unwrap().0, "Copied Alpha");
}

#[test]
fn accelerators_jump_and_activate_unique_matches() {
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(2, 2))));
    // Two items start with "c": cycle between them without activating.
    press(&mut state, KeyCode::Char('c'));
    assert_eq!(menu(&state).current().unwrap().label, "Charlie");
    press(&mut state, KeyCode::Char('C'));
    assert_eq!(menu(&state).current().unwrap().label, "Cedar");
    // Disabled "Bravo" is not a match.
    press(&mut state, KeyCode::Char('b'));
    assert_eq!(menu(&state).current().unwrap().label, "Cedar");
    // Unique match on a submenu opens it.
    press(&mut state, KeyCode::Char('d'));
    assert_eq!(menu(&state).levels(), 2);
    press(&mut state, KeyCode::Char('e'));
    assert!(state.overlays.is_empty());
    assert_eq!(copied(&mut state), ["Echo"]);
}

#[test]
fn mouse_hover_highlights_and_click_activates() {
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(2, 2))));
    let buffer = render(&mut state, 80, 24);
    let charlie = find(&state, &buffer, "Charlie");
    mouse(&mut state, MouseEventKind::Move, center(charlie));
    assert_eq!(menu(&state).cursor, vec![Some(4)]);
    // Hovering a submenu opens it; hovering inside keeps it open.
    let delta = find(&state, &buffer, "Delta");
    mouse(&mut state, MouseEventKind::Move, center(delta));
    assert_eq!(menu(&state).cursor, vec![Some(5), None]);
    let buffer = render(&mut state, 80, 24);
    let fox = find(&state, &buffer, "Foxtrot");
    assert!(fox.x >= delta.x + delta.width, "submenu opens to the right");
    mouse(&mut state, MouseEventKind::Move, center(fox));
    assert_eq!(menu(&state).cursor, vec![Some(5), Some(1)]);
    mouse(&mut state, MouseEventKind::Move, center(delta));
    assert_eq!(
        menu(&state).levels(),
        2,
        "returning to the parent keeps it open"
    );
    click(&mut state, center(fox));
    assert!(state.overlays.is_empty());
    assert_eq!(copied(&mut state), ["Foxtrot"]);
}

#[test]
fn clicking_disabled_items_or_headers_does_nothing() {
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(2, 2))));
    let buffer = render(&mut state, 80, 24);
    let text = screen(&buffer);
    let bravo_y = text.lines().position(|l| l.contains("Bravo")).unwrap() as u16;
    let header_y = text.lines().position(|l| l.contains("Header")).unwrap() as u16;
    click(&mut state, Point::new(6, bravo_y));
    click(&mut state, Point::new(6, header_y));
    assert_eq!(state.overlays.len(), 1);
    assert!(state.take_requests().is_empty());
}

#[test]
fn click_outside_closes_without_passing_through() {
    let mut state = fixture();
    state.table.select(key(1));
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(2, 2))));
    render(&mut state, 80, 24);
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        Point::new(70, 20),
    );
    assert!(state.overlays.is_empty());
    assert!(state.take_requests().is_empty());
    assert_eq!(state.table.selected(), Some(key(1)));
}

#[test]
fn backdrop_is_pushed_first_and_hits_stay_on_screen() {
    let mut state = fixture();
    state
        .overlays
        .push(process_menu(&state, key(100), Point::new(95, 38)));
    render(&mut state, 100, 40);
    let hits = state.hits.committed();
    assert_eq!(hits[0].1, Hit::OverlayBackdrop { overlay: 0 });
    assert_eq!(hits[0].0, Rect::new(0, 0, 100, 40));
    for (r, _) in hits {
        assert!(r.x + r.width <= 100 && r.y + r.height <= 40, "{r:?}");
    }
}

#[test]
fn menus_clamp_and_flip_at_screen_edges() {
    let area = Rect::new(0, 0, 60, 20);
    let m = sample_menu(Point::new(58, 18));
    let g = m.geometry(area);
    let r = g[0].rect;
    assert!(r.x + r.width <= 60, "clamped horizontally: {r:?}");
    assert!(r.y + r.height <= 18, "flipped above the anchor: {r:?}");
    let m = sample_menu(Point::new(5, 3));
    assert_eq!(m.geometry(area)[0].rect.y, 3, "opens downward with room");

    // Submenu flips to the left when there is no room on the right.
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(50, 2))));
    let buffer = render(&mut state, 60, 20);
    let delta = find(&state, &buffer, "Delta");
    click(&mut state, center(delta));
    let buffer = render(&mut state, 60, 20);
    let echo = find(&state, &buffer, "Echo");
    assert!(
        echo.x + echo.width <= delta.x,
        "submenu flipped left: {echo:?}"
    );
    assert_eq!(
        echo.y, delta.y,
        "first submenu row lines up with its parent"
    );
}

#[test]
fn tall_menus_scroll_within_the_screen() {
    let items: Vec<MenuItem> = (0..30).map(|i| copy(&format!("Item {i:02}"))).collect();
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(MenuOverlay::new(Point::new(1, 1), items)));
    let buffer = render(&mut state, 40, 10);
    assert!(screen(&buffer).contains("Item 07"));
    assert!(!screen(&buffer).contains("Item 08"));
    for _ in 0..12 {
        press(&mut state, KeyCode::Down);
    }
    let buffer = render(&mut state, 40, 10);
    let text = screen(&buffer);
    assert!(
        text.contains("Item 12"),
        "cursor scrolled into view:\n{text}"
    );
    assert!(text.contains('▲') && text.contains('▼'));
    let item = find(&state, &buffer, "Item 12");
    assert!(item.y < 10);
    mouse(&mut state, MouseEventKind::ScrollUp, center(item));
    let buffer = render(&mut state, 40, 10);
    assert!(screen(&buffer).contains("Item 02"));
}

// ---- process menu -----------------------------------------------------------

fn open_process_menu(state: &mut ExplorerState, pid: i32) -> Buffer {
    let m = process_menu(state, key(pid), Point::new(5, 5));
    state.overlays.push(m);
    render(state, 100, 40)
}

#[test]
fn process_menu_has_header_and_actions() {
    let mut state = fixture();
    let buffer = open_process_menu(&mut state, 100);
    let text = screen(&buffer);
    for label in [
        "nginx · PID 100",
        "Details",
        "Full screen details",
        "Signal",
        "Terminate",
        "Force kill",
        "Suspend",
        "Renice…",
        "Kill process tree…",
        "Throttle bandwidth…",
        "Pin to top",
        "Mark",
        "Watch this process…",
        "Filter: same name",
        "Filter: same user",
        "Show in tree",
        "Go to parent",
        "Copy",
    ] {
        assert!(text.contains(label), "missing {label:?}\n{text}");
    }
    assert!(!text.contains("Remove throttle"));
    assert_eq!(menu(&state).cursor[0], Some(1), "Details preselected");
}

#[test]
fn stopped_processes_offer_resume() {
    let mut state = fixture();
    let buffer = open_process_menu(&mut state, 300);
    let text = screen(&buffer);
    assert!(text.contains("Resume") && !text.contains("Suspend"));
    let resume = find(&state, &buffer, "Resume");
    click(&mut state, center(resume));
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Signal {
            targets: vec![key(300)],
            signal: Signal::Cont,
        })]
    );
}

#[test]
fn terminate_confirms_when_enabled() {
    let mut state = fixture();
    assert!(state.settings.confirm_signals);
    let buffer = open_process_menu(&mut state, 200);
    let terminate = find(&state, &buffer, "Terminate");
    click(&mut state, center(terminate));
    assert_eq!(state.overlays.len(), 1);
    let d = signal(&state);
    assert_eq!(d.targets, vec![key(200)]);
    assert_eq!(d.signal, Signal::Term);
    assert!(state.take_requests().is_empty());
}

#[test]
fn terminate_sends_directly_without_confirmation() {
    let mut state = fixture();
    state.settings.confirm_signals = false;
    let buffer = open_process_menu(&mut state, 200);
    tap(&mut state, &buffer, "Force kill");
    assert!(state.overlays.is_empty());
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Signal {
            targets: vec![key(200)],
            signal: Signal::Kill,
        })]
    );
}

#[test]
fn signal_submenu_lists_every_signal() {
    let mut state = fixture();
    state.settings.confirm_signals = false;
    let buffer = open_process_menu(&mut state, 200);
    tap(&mut state, &buffer, "Signal");
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    for s in Signal::ALL {
        assert!(text.contains(s.name()), "missing {}", s.name());
    }
    tap(&mut state, &buffer, "SIGUSR1");
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Signal {
            targets: vec![key(200)],
            signal: Signal::Usr1,
        })]
    );
}

#[test]
fn copy_submenu_queues_clipboard_requests() {
    let mut state = fixture();
    let buffer = open_process_menu(&mut state, 100);
    tap(&mut state, &buffer, "Copy");
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "PID");
    assert_eq!(copied(&mut state), ["100"]);
    assert_eq!(state.flash.as_ref().unwrap().0, "Copied PID");

    let buffer = open_process_menu(&mut state, 100);
    tap(&mut state, &buffer, "Copy");
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Command line");
    assert_eq!(copied(&mut state), ["/usr/bin/nginx --flag"]);
}

#[test]
fn marked_rows_are_all_targeted() {
    let mut state = fixture();
    state.settings.confirm_signals = false;
    state.table.mark_all(vec![key(101), key(102), key(200)]);
    let buffer = open_process_menu(&mut state, 102);
    assert!(screen(&buffer).contains("3 marked processes"));
    tap(&mut state, &buffer, "Terminate");
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Signal {
            targets: vec![key(101), key(102), key(200)],
            signal: Signal::Term,
        })]
    );
    // Right-clicking an unmarked row acts on that row only.
    let buffer = open_process_menu(&mut state, 1);
    assert!(screen(&buffer).contains("launchd · PID 1"));
    tap(&mut state, &buffer, "Terminate");
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Signal {
            targets: vec![key(1)],
            signal: Signal::Term,
        })]
    );
}

#[test]
fn kill_tree_preselects_descendants() {
    let mut state = fixture();
    let buffer = open_process_menu(&mut state, 100);
    tap(&mut state, &buffer, "Kill process tree");
    let d = signal(&state);
    assert!(d.include_children);
    let mut children = d.children.clone();
    children.sort();
    assert_eq!(children, vec![key(101), key(102), key(103)]);
}

#[test]
fn throttled_processes_offer_removal() {
    let mut state = fixture();
    let mut info = crate::process::ProcessInfo::new(100, "nginx".into());
    info.throttle_limit = Some(crate::process::ThrottleLimit {
        download_limit: Some(1024),
        upload_limit: None,
        traffic_type: crate::process::TrafficType::All,
    });
    let mut map = crate::process::ProcessMap::new();
    map.insert(100, info);
    state.store.merge_network(&map);
    let buffer = open_process_menu(&mut state, 100);
    tap(&mut state, &buffer, "Remove throttle");
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Legacy(
            crate::ui::explorer::LegacyRequest::RemoveThrottle {
                pid: 100,
                name: "nginx".into(),
            }
        )]
    );
}

#[test]
fn filter_pin_mark_tree_and_parent_items() {
    let mut state = fixture();
    let buffer = open_process_menu(&mut state, 101);
    tap(&mut state, &buffer, "same name");
    assert_eq!(state.search.text, "name:nginx-worker");

    let buffer = open_process_menu(&mut state, 101);
    tap(&mut state, &buffer, "same user");
    assert_eq!(state.search.text, "user:www");

    let buffer = open_process_menu(&mut state, 101);
    tap(&mut state, &buffer, "Pin to top");
    assert_eq!(state.table.pinned(), &[key(101)]);
    let buffer = open_process_menu(&mut state, 101);
    assert!(screen(&buffer).contains("Unpin"));
    state.overlays.clear();

    let buffer = open_process_menu(&mut state, 101);
    tap(&mut state, &buffer, "Mark");
    assert_eq!(state.table.marked(), vec![key(101)]);

    let buffer = open_process_menu(&mut state, 103);
    tap(&mut state, &buffer, "Show in tree");
    assert!(state.settings.tree);
    assert_eq!(state.table.selected(), Some(key(103)));

    let buffer = open_process_menu(&mut state, 103);
    tap(&mut state, &buffer, "Go to parent");
    assert_eq!(state.table.selected(), Some(key(101)));
}

#[test]
fn renice_item_starts_at_the_current_value() {
    let mut state = fixture();
    let buffer = open_process_menu(&mut state, 300);
    tap(&mut state, &buffer, "Renice");
    let d = renice(&state);
    assert_eq!((d.current, d.value), (Some(5), 5));
}

#[test]
fn exited_processes_disable_signals() {
    let mut state = fixture();
    let buffer = open_process_menu(&mut state, 999);
    let text = screen(&buffer);
    assert!(text.contains("PID 999"));
    for label in ["Terminate", "Force kill", "Details"] {
        assert!(
            !state
                .hits
                .committed()
                .iter()
                .any(|(r, h)| matches!(h, Hit::OverlayItem { .. })
                    && cells(&buffer, *r).contains(label)),
            "{label} should be disabled"
        );
    }
}

// ---- sort / filter / export --------------------------------------------------

#[test]
fn sort_menu_sets_and_persists_sort() {
    let mut state = fixture();
    state.overlays.push(sort_menu(&state));
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    assert!(text.contains("More") && text.contains("Descending") && text.contains('✓'));
    tap(&mut state, &buffer, "PID ");
    assert!(state.overlays.is_empty());
    assert_eq!(state.table.sort().column, ColumnId::Pid);
    assert_eq!(state.settings.sort_column, Some(ColumnId::Pid));
    assert_eq!(
        state.settings.sort_descending,
        ColumnId::Pid.default_descending()
    );
    assert!(state.take_requests().contains(&UiRequest::SaveSettings));

    // Columns that are not visible live under "More".
    state.overlays.push(sort_menu(&state));
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "More");
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "PPID");
    assert_eq!(state.table.sort().column, ColumnId::Ppid);

    // The Descending toggle keeps the menu open and flips its check.
    state.overlays.push(sort_menu(&state));
    let before = state.table.sort().descending;
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Descending");
    assert_eq!(state.overlays.len(), 1);
    assert_eq!(state.table.sort().descending, !before);
    assert_eq!(state.settings.sort_descending, !before);
    let toggle = menu(&state)
        .items
        .iter()
        .find(|i| i.label == "Descending")
        .unwrap();
    assert_eq!(toggle.checked, Some(!before));
}

#[test]
fn sort_menu_anchors_above_its_function_key() {
    let mut state = fixture();
    state.hits.push(
        Rect::new(30, 39, 8, 1),
        Hit::FKey(crate::ui::explorer::actions::Action::SortMenu),
    );
    state.commit();
    state.overlays.push(sort_menu(&state));
    assert_eq!(menu(&state).at, Point::new(30, 39));
    let g = menu(&state).geometry(Rect::new(0, 0, 100, 40));
    assert!(
        g[0].rect.y + g[0].rect.height <= 39,
        "menu sits above the F-key bar"
    );
}

#[test]
fn filter_menu_toggles_stay_open_and_update() {
    let mut state = fixture();
    state.overlays.push(filter_menu(&state));
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Network-active only");
    assert!(state.settings.net_active_only);
    assert_eq!(state.overlays.len(), 1);
    tap(&mut state, &buffer, "Show kernel threads");
    assert!(!state.settings.hide_kernel_threads);
    let buffer = render(&mut state, 100, 40);
    assert_eq!(screen(&buffer).matches('✓').count(), 2);

    tap(&mut state, &buffer, "Filter by user");
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    for user in ["me", "root", "www"] {
        assert!(text.contains(user));
    }
    tap(&mut state, &buffer, "root");
    assert_eq!(state.search.text, "user:root");

    state.overlays.push(filter_menu(&state));
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Filter by state");
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Zombie");
    assert_eq!(state.search.text, "state:Z");

    state.overlays.push(filter_menu(&state));
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Clear search");
    assert!(state.search.text.is_empty());
}

#[test]
fn export_menu_requests_exports() {
    let mut state = fixture();
    state.overlays.push(export_menu(&state));
    let buffer = render(&mut state, 100, 40);
    assert!(
        !screen(&buffer).is_empty() && find_opt(&state, &buffer, "Copy visible PIDs").is_none(),
        "copy is disabled without visible rows"
    );
    tap(&mut state, &buffer, "Export JSON");
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Export(ExportFormat::Json)]
    );
}

fn find_opt(state: &ExplorerState, buffer: &Buffer, label: &str) -> Option<Rect> {
    state
        .hits
        .committed()
        .iter()
        .find(|(r, h)| matches!(h, Hit::OverlayItem { .. }) && cells(buffer, *r).contains(label))
        .map(|(r, _)| *r)
}

// ---- signal dialog -----------------------------------------------------------

#[test]
fn signal_dialog_sends_with_children_deduped() {
    let mut state = fixture();
    // 101 is both a target and a child of 100: it must be signalled once.
    state.overlays.push(signal_dialog(
        &state,
        vec![key(100), key(101)],
        Signal::Term,
    ));
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    assert!(text.contains("Also signal 2 child processes"), "{text}");
    assert!(text.contains("Send SIGTERM"));
    press(&mut state, KeyCode::Space);
    assert!(signal(&state).include_children);
    press(&mut state, KeyCode::Char('2'));
    assert_eq!(signal(&state).signal, Signal::Kill);
    press(&mut state, KeyCode::Enter);
    assert!(state.overlays.is_empty());
    let requests = state.take_requests();
    let [UiRequest::Action(ActionRequest::Signal { targets, signal })] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(*signal, Signal::Kill);
    let mut sorted = targets.clone();
    sorted.sort();
    assert_eq!(sorted, vec![key(100), key(101), key(102), key(103)]);
}

#[test]
fn signal_dialog_mouse_controls() {
    let mut state = fixture();
    state
        .overlays
        .push(signal_dialog(&state, vec![key(100)], Signal::Term));
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Also signal");
    assert!(signal(&state).include_children);
    tap(&mut state, &buffer, "SIGHUP");
    assert_eq!(signal(&state).signal, Signal::Hup);
    let buffer = render(&mut state, 100, 40);
    assert!(screen(&buffer).contains("Send SIGHUP"));
    mouse(&mut state, MouseEventKind::ScrollDown, Point::new(50, 20));
    assert_eq!(signal(&state).signal, Signal::Int);
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Send SIGINT");
    assert!(state.overlays.is_empty());
    assert_eq!(state.take_requests().len(), 1);
}

#[test]
fn signal_dialog_cancel_paths_send_nothing() {
    let mut state = fixture();
    state
        .overlays
        .push(signal_dialog(&state, vec![key(200)], Signal::Term));
    press(&mut state, KeyCode::Escape);
    assert!(state.overlays.is_empty());

    state
        .overlays
        .push(signal_dialog(&state, vec![key(200)], Signal::Term));
    let buffer = render(&mut state, 100, 40);
    assert!(
        !screen(&buffer).contains("child process"),
        "no children, no checkbox"
    );
    tap(&mut state, &buffer, "Cancel");
    assert!(state.overlays.is_empty());

    state
        .overlays
        .push(signal_dialog(&state, vec![key(200)], Signal::Term));
    render(&mut state, 100, 40);
    click(&mut state, Point::new(0, 0));
    assert!(state.overlays.is_empty(), "click outside cancels");

    state
        .overlays
        .push(signal_dialog(&state, vec![key(200)], Signal::Term));
    press(&mut state, KeyCode::Tab);
    press(&mut state, KeyCode::Enter);
    assert!(state.overlays.is_empty(), "Enter on focused Cancel");
    assert!(state.take_requests().is_empty());
}

#[test]
fn signal_dialog_summarises_many_targets() {
    let mut state = fixture();
    let targets = vec![key(1), key(100), key(101), key(102), key(200), key(300)];
    state
        .overlays
        .push(signal_dialog(&state, targets, Signal::Kill));
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    assert!(text.contains("6 processes"));
    assert!(text.contains("launchd") && text.contains("nginx-worker"));
    assert!(text.contains("and 2 more"));
    assert!(text.contains("Force kill"), "SIGKILL retitles the dialog");
}

#[test]
fn signal_dialog_shows_single_target_details() {
    let mut state = fixture();
    state
        .overlays
        .push(signal_dialog(&state, vec![key(200)], Signal::Term));
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    assert!(text.contains("zsh") && text.contains("PID 200") && text.contains("me"));
    assert!(text.contains("/usr/bin/zsh --flag"));
}

// ---- renice dialog -----------------------------------------------------------

#[test]
fn renice_starts_one_step_from_current() {
    let state = fixture();
    let Overlay::Renice(lower) = root_renice(&state, vec![key(300)], true) else {
        unreachable!()
    };
    assert_eq!((lower.current, lower.value), (Some(5), 6));
    let Overlay::Renice(higher) = root_renice(&state, vec![key(300)], false) else {
        unreachable!()
    };
    assert_eq!(higher.value, 4);
    let Overlay::Renice(mixed) = root_renice(&state, vec![key(300), key(200)], true) else {
        unreachable!()
    };
    assert!(mixed.mixed && mixed.current.is_none());
    let Overlay::Renice(unknown) = root_renice(&state, vec![key(999)], true) else {
        unreachable!()
    };
    assert_eq!((unknown.current, unknown.value), (None, 1));
}

#[test]
fn renice_keyboard_and_typing() {
    let mut state = fixture();
    state
        .overlays
        .push(root_renice(&state, vec![key(200)], true));
    press(&mut state, KeyCode::Right);
    assert_eq!(renice(&state).value, 2);
    press(&mut state, KeyCode::Left);
    press(&mut state, KeyCode::Left);
    assert_eq!(renice(&state).value, 0);
    press(&mut state, KeyCode::End);
    press(&mut state, KeyCode::Right);
    assert_eq!(renice(&state).value, 19, "clamped");
    press(&mut state, KeyCode::Char('-'));
    press(&mut state, KeyCode::Char('1'));
    press(&mut state, KeyCode::Char('2'));
    assert_eq!(renice(&state).value, -12);
    press(&mut state, KeyCode::Char('9'));
    assert_eq!(renice(&state).value, -12, "at most two digits");
    press(&mut state, KeyCode::Backspace);
    assert_eq!(renice(&state).value, -1);
    press(&mut state, KeyCode::Enter);
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Renice {
            targets: vec![key(200)],
            nice: -1,
        })]
    );
    assert!(state.overlays.is_empty());
}

#[test]
fn renice_slider_is_clickable_and_draggable() {
    let mut state = fixture();
    state
        .overlays
        .push(root_renice(&state, vec![key(200)], true));
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    assert!(text.contains("Lower values = higher priority"));
    assert!(text.contains("Current nice: 0"));
    let slider: Vec<Rect> = state
        .hits
        .committed()
        .iter()
        .filter(|(_, h)| matches!(h, Hit::OverlayItem { .. }))
        .map(|(r, _)| *r)
        .collect();
    assert_eq!(slider.len(), 40, "one cell per nice value");
    click(&mut state, center(slider[0]));
    assert_eq!(renice(&state).value, -20);
    mouse(
        &mut state,
        MouseEventKind::Drag(MouseButton::Left),
        center(slider[30]),
    );
    assert_eq!(renice(&state).value, 10);
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "◀");
    assert_eq!(renice(&state).value, 9);
    let buffer = render(&mut state, 100, 40);
    assert!(screen(&buffer).contains("lower priority"));
    tap(&mut state, &buffer, "Apply");
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Renice {
            targets: vec![key(200)],
            nice: 9,
        })]
    );
}

#[test]
fn renice_slider_scales_to_narrow_dialogs() {
    for cells in [1, 2, 10, 39, 40] {
        assert_eq!(
            renice::cell_value(0, cells).min(0),
            if cells > 1 { -20 } else { 0 }
        );
        if cells > 1 {
            assert_eq!(renice::cell_value(cells - 1, cells), 19);
        }
        for v in -20..=19 {
            assert!(renice::value_cell(v, cells) < cells.max(1));
        }
    }
    for v in -20..=19 {
        assert_eq!(renice::cell_value(renice::value_cell(v, 40), 40), v);
    }
}

// ---- column chooser ----------------------------------------------------------

#[test]
fn column_chooser_toggles_and_persists_live() {
    let mut state = fixture();
    let defaults = Preset::Overview.default_columns();
    state.overlays.push(column_chooser(&state));
    assert_eq!(chooser(&state).focus, defaults[0]);
    press(&mut state, KeyCode::Space);
    let expected: Vec<ColumnId> = defaults[1..].to_vec();
    assert_eq!(state.table.columns(), expected.as_slice());
    assert_eq!(
        state.settings.preset_columns.get("Overview"),
        Some(&expected)
    );
    assert!(state.take_requests().contains(&UiRequest::SaveSettings));
    // Toggling back on appends it at the end of the enabled set.
    press(&mut state, KeyCode::Space);
    assert_eq!(state.table.columns().last(), Some(&defaults[0]));
}

#[test]
fn column_chooser_reorders_with_shift_arrows_and_buttons() {
    let mut state = fixture();
    let defaults = Preset::Overview.default_columns();
    state.overlays.push(column_chooser(&state));
    press_shift(&mut state, KeyCode::Down);
    assert_eq!(state.table.columns()[1], defaults[0]);
    assert_eq!(state.table.columns()[0], defaults[1]);
    let buffer = render(&mut state, 100, 50);
    let up = find(&state, &buffer, "▲");
    click(&mut state, center(up));
    assert_eq!(state.table.columns(), defaults.as_slice());
    // At the top the ▲ arrow is inactive (no hit).
    let buffer = render(&mut state, 100, 50);
    assert!(find_control(&state, &buffer, "▲").is_none());
}

fn find_control(state: &ExplorerState, buffer: &Buffer, label: &str) -> Option<Rect> {
    state
        .hits
        .committed()
        .iter()
        .find(|(r, h)| matches!(h, Hit::OverlayControl { .. }) && cells(buffer, *r).contains(label))
        .map(|(r, _)| *r)
}

#[test]
fn column_chooser_click_toggles_and_reset_restores() {
    let mut state = fixture();
    state.overlays.push(column_chooser(&state));
    let buffer = render(&mut state, 100, 50);
    let text = screen(&buffer);
    assert!(text.contains("Columns · Overview"));
    assert!(text.contains("[x] PID") && text.contains("[ ] PPID"));
    tap(&mut state, &buffer, "PPID");
    assert!(state.table.columns().contains(&ColumnId::Ppid));
    assert_eq!(chooser(&state).focus, ColumnId::Ppid);
    let buffer = render(&mut state, 100, 50);
    assert!(
        screen(&buffer).contains("Parent process ID"),
        "focused description"
    );
    tap(&mut state, &buffer, "Reset to defaults");
    assert_eq!(
        state.table.columns(),
        Preset::Overview.default_columns().as_slice()
    );
    assert!(!state.settings.preset_columns.contains_key("Overview"));
    let buffer = render(&mut state, 100, 50);
    tap(&mut state, &buffer, "Done");
    assert!(state.overlays.is_empty());
}

#[test]
fn column_chooser_keeps_at_least_one_column() {
    let mut state = fixture();
    state.table.set_columns(vec![ColumnId::Pid]);
    state.overlays.push(column_chooser(&state));
    press(&mut state, KeyCode::Space);
    assert_eq!(state.table.columns(), &[ColumnId::Pid]);
    assert!(state.flash.is_some());
}

#[test]
fn column_chooser_scrolls_in_short_terminals() {
    let mut state = fixture();
    state.overlays.push(column_chooser(&state));
    render(&mut state, 80, 16);
    press(&mut state, KeyCode::End);
    let buffer = render(&mut state, 80, 16);
    let text = screen(&buffer);
    let last = crate::procs::columns::ALL_COLUMNS
        .iter()
        .rev()
        .find(|c| !Preset::Overview.default_columns().contains(c))
        .unwrap();
    assert!(text.contains(last.title()), "{text}");
    mouse(&mut state, MouseEventKind::ScrollUp, Point::new(40, 8));
    let focus = chooser(&state).focus;
    let buffer = render(&mut state, 80, 16);
    assert!(screen(&buffer).contains(focus.title()));
}

// ---- robustness ----------------------------------------------------------------

fn every_overlay(state: &ExplorerState) -> Vec<Overlay> {
    vec![
        process_menu(state, key(100), Point::new(3, 3)),
        sort_menu(state),
        filter_menu(state),
        export_menu(state),
        column_chooser(state),
        renice_dialog(state, vec![key(100)], true),
        signal_dialog(state, vec![key(100), key(200)], Signal::Term),
        Overlay::Affinity(AffinityDialog::with_mask(12, vec![key(100)], None)),
    ]
}

#[test]
fn tiny_terminals_never_panic() {
    let state = fixture();
    let count = every_overlay(&state).len();
    for i in 0..count {
        for (w, h) in [
            (1, 1),
            (2, 1),
            (1, 2),
            (3, 3),
            (5, 4),
            (12, 6),
            (20, 8),
            (200, 3),
        ] {
            let mut state = fixture();
            let overlay = every_overlay(&state).remove(i);
            state.overlays.push(overlay);
            let buffer = render(&mut state, w, h);
            for (r, _) in state.hits.committed() {
                assert!(
                    r.x + r.width <= w && r.y + r.height <= h,
                    "{i} {w}x{h} {r:?}"
                );
            }
            let _ = buffer;
            for code in [
                KeyCode::Down,
                KeyCode::Right,
                KeyCode::End,
                KeyCode::PageDown,
                KeyCode::Char('x'),
            ] {
                press(&mut state, code);
                if state.overlays.is_empty() {
                    break;
                }
                render(&mut state, w, h);
            }
            for p in [Point::new(0, 0), Point::new(w - 1, h - 1)] {
                mouse(&mut state, MouseEventKind::Move, p);
                mouse(&mut state, MouseEventKind::ScrollDown, p);
            }
        }
    }
}

#[test]
fn empty_store_menus_still_work() {
    let mut state = ExplorerState {
        last_area: Rect::new(0, 0, 80, 24),
        ..Default::default()
    };
    for overlay in every_overlay(&state) {
        state.overlays.push(overlay);
        render(&mut state, 80, 24);
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Escape);
        state.overlays.clear();
    }
    assert!(
        state
            .take_requests()
            .iter()
            .all(|r| matches!(r, UiRequest::SaveSettings)),
        "navigation alone never acts"
    );
}

#[test]
fn menus_are_only_hit_tested_against_their_own_index() {
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(2, 2))));
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(40, 2))));
    let buffer = render(&mut state, 80, 24);
    // Item of the lower menu: covered by the top menu's backdrop.
    let lower_alpha = state
        .hits
        .committed()
        .iter()
        .find(|(r, h)| {
            matches!(h, Hit::OverlayItem { overlay: 0, .. }) && cells(&buffer, *r).contains("Alpha")
        })
        .map(|(r, _)| *r)
        .unwrap();
    assert!(matches!(
        state.hits.at(center(lower_alpha)),
        Some(Hit::OverlayBackdrop { overlay: 1 })
    ));
    click(&mut state, center(lower_alpha));
    assert_eq!(state.overlays.len(), 1, "only the top menu closed");
    assert!(state.take_requests().is_empty());
    assert!(contains(Rect::new(0, 0, 80, 24), center(lower_alpha)));
}

#[test]
fn menu_items_expose_kinds() {
    let m = sample_menu(Point::new(0, 0));
    assert!(matches!(m.items[0].kind, ItemKind::Header));
    assert!(matches!(m.items[2].kind, ItemKind::Separator));
    assert!(!m.items[3].selectable());
    assert!(m.items[5].selectable());
    assert!(!MenuItem::submenu("Empty", vec![]).selectable());
}

// ---- menu frame ------------------------------------------------------------------

/// Every row of a menu panel is closed on the right: `╮` / `╯` corners,
/// `┤` on separators and `│` elsewhere.
fn assert_closed_frame(buffer: &Buffer, r: Rect, items: &[MenuItem], scroll: usize) {
    let right = r.x + r.width - 1;
    let at = |y| buffer.get(Point::new(right, y)).unwrap().symbol.clone();
    let left = |y| buffer.get(Point::new(r.x, y)).unwrap().symbol.clone();
    assert_eq!(at(r.y), "╮", "top-right corner\n{}", screen(buffer));
    assert_eq!(at(r.y + r.height - 1), "╯", "bottom-right corner");
    for row in 0..r.height.saturating_sub(2) {
        let y = r.y + 1 + row;
        let separator = matches!(
            items.get(scroll + row as usize).map(|i| &i.kind),
            Some(ItemKind::Separator)
        );
        let (l, rt) = if separator {
            ("├", "┤")
        } else {
            ("│", "│")
        };
        assert_eq!(left(y), l, "left edge at row {y}\n{}", screen(buffer));
        assert_eq!(at(y), rt, "right edge at row {y}\n{}", screen(buffer));
    }
}

#[test]
fn menu_frames_close_on_the_right_including_separators() {
    let mut state = fixture();
    for at in [Point::new(10, 2), Point::new(95, 2), Point::new(0, 30)] {
        state.overlays.clear();
        state.overlays.push(process_menu(&state, key(100), at));
        let buffer = render(&mut state, 100, 40);
        let m = menu(&state);
        let g = m.geometry(state.last_area);
        assert_closed_frame(&buffer, g[0].rect, &m.items, g[0].scroll);
    }
    // A scrolled menu in a short terminal and an open submenu.
    state.overlays.clear();
    state
        .overlays
        .push(process_menu(&state, key(100), Point::new(2, 1)));
    let buffer = render(&mut state, 60, 12);
    let m = menu(&state);
    let g = m.geometry(state.last_area);
    assert_closed_frame(&buffer, g[0].rect, &m.items, g[0].scroll);

    state.overlays.clear();
    state
        .overlays
        .push(Overlay::Menu(sample_menu(Point::new(5, 5))));
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Delta");
    let buffer = render(&mut state, 100, 40);
    let m = menu(&state);
    let g = m.geometry(state.last_area);
    assert_eq!(g.len(), 2, "submenu open");
    for (level, geometry) in g.iter().enumerate() {
        assert_closed_frame(
            &buffer,
            geometry.rect,
            m.level_items(level),
            geometry.scroll,
        );
    }
}

// ---- dimmed backdrop ---------------------------------------------------------------

#[test]
fn dimmed_backdrop_keeps_selected_and_highlighted_colours() {
    use bmux_tui::style::{Color, Modifier, Style};
    let mut state = fixture();
    let area = Rect::new(0, 0, 100, 40);
    state.last_area = area;
    let mut buffer = Buffer::empty(area);
    let selected = crate::ui::explorer::paint::theme::selected();
    let marked = Style::new().fg(Color::Yellow);
    {
        let mut bf = bmux_tui::frame::Frame::new(&mut buffer);
        let mut cx = PaintCx::new(&mut bf);
        cx.fill(
            crate::ui::explorer::paint::lr(Rect::new(0, 0, 100, 1)),
            "s",
            selected,
        );
        cx.fill(
            crate::ui::explorer::paint::lr(Rect::new(0, 1, 100, 1)),
            "m",
            marked,
        );
        let mut hits = std::mem::take(&mut state.hits);
        hits.begin();
        state
            .overlays
            .push(root_renice(&state, vec![key(200)], true));
        {
            let mut frame = Frame {
                cx: &mut cx,
                hits: &mut hits,
                now: std::time::Instant::now(),
            };
            draw(&mut frame, area, 0, &state);
        }
        state.hits = hits;
    }
    let sel = buffer.get(Point::new(0, 0)).unwrap();
    assert_eq!(sel.style.fg, selected.fg, "selected text keeps its colour");
    assert_eq!(sel.style.bg, selected.bg);
    assert!(sel.style.modifiers.contains(Modifier::DIM));
    let hl = buffer.get(Point::new(0, 1)).unwrap();
    assert_eq!(
        hl.style.fg,
        Some(Color::Yellow),
        "highlighted text keeps its colour"
    );
    assert!(hl.style.modifiers.contains(Modifier::DIM));
}

// ---- bproc never signals itself ----------------------------------------------------

/// The fixture plus this test process (as "bproc") under zsh, with a child.
fn fixture_with_self() -> (ExplorerState, ProcessKey) {
    let me = std::process::id() as i32;
    let mut state = fixture();
    let mut entries: Vec<ProcessEntry> =
        state.store.rows().iter().map(|r| r.entry.clone()).collect();
    entries.push(entry(me, Some(200), "bproc", "me"));
    entries.push(entry(me + 100_000, Some(me), "bproc-helper", "me"));
    state.store = ProcessStore::from_entries(entries, SystemSnapshot::default());
    (state, key(me))
}

#[test]
fn kill_tree_never_includes_bproc_itself() {
    let (mut state, me) = fixture_with_self();
    let buffer = open_process_menu(&mut state, 200);
    tap(&mut state, &buffer, "Kill process tree");
    let d = signal(&state);
    assert!(d.include_children);
    assert!(!d.children.contains(&me), "bproc is not a recipient");
    assert!(!d.recipients().contains(&me));
    // Its own children are still offered.
    assert!(d.children.contains(&key(me.pid + 100_000)));
    press(&mut state, KeyCode::Enter);
    let requests = state.take_requests();
    let [UiRequest::Action(ActionRequest::Signal { targets, .. })] = requests.as_slice() else {
        panic!("{requests:?}")
    };
    assert!(!targets.contains(&me));
    assert!(targets.contains(&key(200)));
}

#[test]
fn signalling_bproc_directly_skips_it_with_a_warning() {
    let (mut state, me) = fixture_with_self();
    // Without confirmation: the menu drops bproc and raises a toast.
    state.settings.confirm_signals = false;
    state.table.mark_all(vec![me, key(200)]);
    let buffer = open_process_menu(&mut state, 200);
    tap(&mut state, &buffer, "Terminate");
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Signal {
            targets: vec![key(200)],
            signal: Signal::Term,
        })]
    );
    assert!(
        state
            .toasts
            .items
            .iter()
            .any(|t| t.text.contains("does not signal itself"))
    );
    // Only bproc targeted: nothing is sent at all.
    state.table.clear_marks();
    state.toasts.clear();
    let buffer = open_process_menu(&mut state, me.pid);
    tap(&mut state, &buffer, "Force kill");
    assert!(state.take_requests().is_empty());
    assert_eq!(state.toasts.items.len(), 1);

    // The dialog says so and never lists bproc as a recipient.
    state
        .overlays
        .push(signal_dialog(&state, vec![me, key(200)], Signal::Term));
    let buffer = render(&mut state, 100, 40);
    assert!(signal(&state).skipped_self);
    assert_eq!(signal(&state).targets, vec![key(200)]);
    assert!(
        screen(&buffer).contains("is skipped"),
        "{}",
        screen(&buffer)
    );
    state.overlays.clear();
    state
        .overlays
        .push(signal_dialog(&state, vec![me], Signal::Term));
    press(&mut state, KeyCode::Enter);
    assert!(state.take_requests().is_empty());
    assert!(state.overlays.is_empty());
}

#[test]
fn system_roots_never_offer_their_tree() {
    let mut state = fixture();
    // launchd (PID 1) has children but "Kill process tree…" is disabled.
    open_process_menu(&mut state, 1);
    let item = menu(&state)
        .items
        .iter()
        .find(|i| i.label.starts_with("Kill process tree"))
        .unwrap();
    assert!(!item.enabled);
    // The dialog does not offer its children either.
    let d = SignalDialog::new(&state, vec![key(1)], Signal::Term);
    assert!(d.children.is_empty() && d.tree_blocked);
    assert_eq!(d.recipients(), vec![key(1)]);
    // Executing the command directly (e.g. stale menu) does nothing.
    state.overlays.clear();
    super::menus::execute(
        &MenuCommand::SignalTree,
        Some(key(1)),
        &[key(1)],
        &mut state,
    );
    assert!(state.overlays.is_empty());
    assert!(signal::is_system_root(0) && signal::is_system_root(1));
    assert!(!signal::is_system_root(100));
}

#[test]
fn large_trees_need_a_second_press() {
    let mut entries = vec![
        entry(1, None, "launchd", "root"),
        entry(10, Some(1), "tmux", "me"),
    ];
    for pid in 0..60 {
        entries.push(entry(1000 + pid, Some(10), "sh", "me"));
    }
    let mut state = ExplorerState {
        store: ProcessStore::from_entries(entries, SystemSnapshot::default()),
        last_area: Rect::new(0, 0, 100, 40),
        ..Default::default()
    };
    let buffer = open_process_menu(&mut state, 10);
    tap(&mut state, &buffer, "Kill process tree");
    assert!(signal(&state).needs_second_press());
    let buffer = render(&mut state, 100, 40);
    assert!(screen(&buffer).contains("This signals 61 processes"));
    press(&mut state, KeyCode::Enter);
    assert!(state.take_requests().is_empty(), "first press only arms");
    assert!(signal(&state).armed);
    let buffer = render(&mut state, 100, 40);
    assert!(screen(&buffer).contains("Press Send again"));
    // Toggling the children disarms.
    press(&mut state, KeyCode::Char('c'));
    press(&mut state, KeyCode::Char('c'));
    assert!(!signal(&state).armed);
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Send SIGTERM");
    assert!(state.take_requests().is_empty());
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Confirm SIGTERM");
    let requests = state.take_requests();
    let [UiRequest::Action(ActionRequest::Signal { targets, .. })] = requests.as_slice() else {
        panic!("{requests:?}")
    };
    assert_eq!(targets.len(), 61);
    assert!(state.overlays.is_empty());
}

// ---- renice without root -------------------------------------------------------

#[test]
fn renice_without_root_cannot_go_below_the_current_value() {
    let mut state = fixture();
    // paused (300) has nice 5.
    state
        .overlays
        .push(user_renice(&state, vec![key(300)], Some(false)));
    let d = renice(&state);
    assert_eq!(d.floor(), 5);
    assert_eq!(d.value, 5, "F7 opens at the current value, not 4");
    assert!(d.cannot_raise());
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    assert!(
        text.contains("Without root you can only lower priority"),
        "{text}"
    );
    assert!(text.contains("5..19"), "{text}");
    assert!(!text.contains("Values below 0 need root"));
    press(&mut state, KeyCode::Left);
    press(&mut state, KeyCode::Home);
    assert_eq!(renice(&state).value, 5);
    press(&mut state, KeyCode::Char('-'));
    press(&mut state, KeyCode::Char('3'));
    assert_eq!(renice(&state).value, 5, "typed values clamp to the floor");
    // Clicking a slider step below the floor clamps too.
    let buffer = render(&mut state, 100, 40);
    let first = state
        .hits
        .committed()
        .iter()
        .find(|(_, h)| matches!(h, Hit::OverlayItem { item: 0, .. }))
        .map(|(r, _)| *r)
        .unwrap();
    assert_eq!(
        cells(&buffer, first),
        "┄",
        "unreachable steps are drawn dashed"
    );
    click(&mut state, center(first));
    assert_eq!(renice(&state).value, 5);
    press(&mut state, KeyCode::Right);
    assert_eq!(renice(&state).value, 6);

    // Mixed or unknown targets keep the full range (the action reports
    // permission errors).
    let mixed = ReniceDialog::with_privilege(&state, vec![key(300), key(200)], Some(false), false);
    assert_eq!(mixed.floor(), -20);
    assert!(!mixed.cannot_raise());
    // Root can go anywhere.
    let Overlay::Renice(root) = root_renice(&state, vec![key(300)], false) else {
        unreachable!()
    };
    assert_eq!((root.floor(), root.value), (-20, 4));
}

#[test]
fn renice_values_stay_in_nice_range() {
    use crate::procs::actions::NICE_RANGE;
    assert_eq!(renice::MIN, *NICE_RANGE.start());
    assert_eq!(renice::MAX, *NICE_RANGE.end());
    let mut state = fixture();
    state
        .overlays
        .push(root_renice(&state, vec![key(200)], true));
    for _ in 0..50 {
        press(&mut state, KeyCode::PageUp);
    }
    assert_eq!(renice(&state).value, *NICE_RANGE.end());
    for _ in 0..50 {
        press(&mut state, KeyCode::PageDown);
    }
    assert_eq!(renice(&state).value, *NICE_RANGE.start());
}

#[test]
fn open_renice_flashes_when_priority_cannot_be_raised() {
    let mut state = fixture();
    if renice::is_privileged() {
        return; // root may always raise priority
    }
    open_renice(&mut state, vec![key(300)], false);
    assert!(state.overlays.is_empty());
    assert_eq!(
        state.flash.as_ref().unwrap().0,
        "Raising priority needs root"
    );
    open_renice(&mut state, vec![key(300)], true);
    assert_eq!(renice(&state).value, 6);
    state.overlays.clear();
    open_renice(&mut state, vec![], true);
    assert!(state.overlays.is_empty());
}

// ---- CPU affinity --------------------------------------------------------------

#[test]
fn affinity_dialog_toggles_cpus_and_queues_request() {
    let mut state = fixture();
    state
        .overlays
        .push(Overlay::Affinity(AffinityDialog::with_mask(
            12,
            vec![key(100)],
            Some(&[0, 2]),
        )));
    assert_eq!(affinity(&state).selected(), vec![0, 2]);
    let buffer = render(&mut state, 100, 40);
    let text = screen(&buffer);
    assert!(text.contains("CPU affinity") && text.contains("[x] 0") && text.contains("[ ] 11"));
    // Click CPU 5 on, keyboard-toggle CPU 0 off.
    tap(&mut state, &buffer, "[ ] 5");
    press(&mut state, KeyCode::Home);
    press(&mut state, KeyCode::Space);
    assert_eq!(affinity(&state).selected(), vec![2, 5]);
    // Grid navigation wraps by rows of 8.
    press(&mut state, KeyCode::Down);
    assert_eq!(affinity(&state).cursor, 8);
    // None, then Apply refuses an empty mask.
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "None");
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "Apply");
    assert!(state.take_requests().is_empty());
    assert_eq!(state.flash.as_ref().unwrap().0, "Select at least one CPU");
    assert_eq!(state.overlays.len(), 1);
    let buffer = render(&mut state, 100, 40);
    tap(&mut state, &buffer, "All");
    press(&mut state, KeyCode::Enter);
    assert_eq!(
        state.take_requests(),
        vec![UiRequest::Action(ActionRequest::Affinity {
            targets: vec![key(100)],
            cpus: (0..12).collect(),
        })]
    );
    assert!(state.overlays.is_empty());
    // Unknown mask = all CPUs; Escape cancels.
    let d = AffinityDialog::with_mask(4, vec![key(100)], None);
    assert_eq!(d.selected(), vec![0, 1, 2, 3]);
    state.overlays.push(Overlay::Affinity(d));
    press(&mut state, KeyCode::Escape);
    assert!(state.overlays.is_empty() && state.take_requests().is_empty());
}

#[test]
fn affinity_menu_item_follows_platform_support() {
    let mut state = fixture();
    open_process_menu(&mut state, 100);
    let has = menu(&state)
        .items
        .iter()
        .any(|i| i.label == "Set CPU affinity…");
    assert_eq!(has, crate::procs::actions::affinity_supported());
    // The command itself opens the dialog only where supported.
    state.overlays.clear();
    super::menus::execute(
        &MenuCommand::Affinity,
        Some(key(100)),
        &[key(100)],
        &mut state,
    );
    assert_eq!(
        matches!(state.overlays.last(), Some(Overlay::Affinity(_))),
        crate::procs::actions::affinity_supported()
    );
}
