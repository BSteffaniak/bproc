use super::fixtures::*;
use super::*;
use crate::config::ExplorerSettings;
use crate::procs::columns::Preset;
use crate::procs::store::Aggregate;
use bmux_tui::event::{MouseButton, MouseEventKind, MouseModifiers};
use bmux_tui::geometry::Point;

fn header_rect(state: &ExplorerState, column: ColumnId) -> Rect {
    state
        .hits
        .rect_of(|h| *h == Hit::Header { column })
        .expect("header cell")
}

fn edge_rect(state: &ExplorerState, column: ColumnId) -> Rect {
    state
        .hits
        .rect_of(|h| *h == Hit::HeaderEdge { column })
        .expect("header edge")
}

fn row_y(state: &ExplorerState, pid: i32) -> u16 {
    state
        .hits
        .rect_of(|h| *h == Hit::Row { key: key(pid) })
        .expect("row painted")
        .y
}

fn order(state: &ExplorerState) -> Vec<i32> {
    state.table.rows().iter().map(|r| r.key.pid).collect()
}

/// Press only: the stub overlay closes on any later mouse event.
fn right_down(state: &mut ExplorerState, x: u16, y: u16) {
    state.handle_event(&mouse(
        MouseEventKind::Down(MouseButton::Right),
        x,
        y,
        MouseModifiers::NONE,
    ));
}

fn forget_click(state: &mut ExplorerState) {
    state.pointer.last_click = None;
}

#[test]
fn layout_fills_width_and_flexible_column_absorbs_slack() {
    let settings = ExplorerSettings::default();
    let cols = Preset::Overview.default_columns();
    let slots = layout_columns(&cols, &settings, 1, 120);
    assert_eq!(slots.len(), cols.len());
    let last = slots.last().unwrap();
    assert_eq!(last.column, ColumnId::Command);
    assert_eq!(last.x + last.width, 121, "flex column reaches the edge");
    for pair in slots.windows(2) {
        assert_eq!(pair[0].x + pair[0].width + 1, pair[1].x, "single space");
    }
    assert_eq!(slots[0].width, ColumnId::Pid.width());
}

#[test]
fn layout_drops_highest_priority_numbers_first() {
    let settings = ExplorerSettings::default();
    let cols = Preset::Overview.default_columns();
    let slots = layout_columns(&cols, &settings, 0, 40);
    let kept: Vec<ColumnId> = slots.iter().map(|s| s.column).collect();
    assert!(kept.contains(&ColumnId::Pid));
    assert!(kept.contains(&ColumnId::Command));
    assert!(kept.contains(&ColumnId::Cpu));
    assert!(!kept.contains(&ColumnId::CpuTime));
    assert!(!kept.contains(&ColumnId::State));
    let end = slots.last().map(|s| s.x + s.width).unwrap();
    assert!(end <= 40);
    let cmd = slots
        .iter()
        .find(|s| s.column == ColumnId::Command)
        .unwrap();
    assert!(cmd.width >= 40 * 3 / 10, "readable command: {slots:?}");

    let tiny = layout_columns(&cols, &settings, 0, 12);
    let kept: Vec<ColumnId> = tiny.iter().map(|s| s.column).collect();
    assert_eq!(kept.first(), Some(&ColumnId::Pid));
    assert!(!kept.contains(&ColumnId::Command), "{kept:?}");
    assert!(tiny.iter().all(|s| s.x + s.width <= 12));

    assert!(layout_columns(&cols, &settings, 0, 0).is_empty());
    let one = layout_columns(&cols, &settings, 0, 3);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].width, 3);
}

#[test]
fn layout_uses_custom_widths() {
    let mut settings = ExplorerSettings::default();
    settings.column_widths.insert("pid".into(), 12);
    let slots = layout_columns(&[ColumnId::Pid, ColumnId::Command], &settings, 0, 60);
    assert_eq!(slots[0].width, 12);
    assert_eq!(slots[1].x, 13);
    assert_eq!(slots[1].width, 47);
}

#[test]
fn never_panics_on_tiny_or_huge_areas() {
    for w in 0..=14 {
        for h in 0..=4 {
            let mut state = state_with(many(30));
            state.settings.tree = h % 2 == 0;
            render(&mut state, w, h);
            let mut empty = state_with(Vec::new());
            render(&mut empty, w, h);
        }
    }
    let mut state = state_with(many(500));
    let out = render(&mut state, 400, 120);
    assert!(out.row(0).contains("COMMAND"));
}

#[test]
fn header_marks_sort_column_and_pushes_hits() {
    let mut state = state_with(many(5));
    let out = render(&mut state, 100, 10);
    let header = out.row(0);
    assert!(header.contains("CPU%▼"), "{header}");
    assert!(header.contains("PID"));
    let cpu = header_rect(&state, ColumnId::Cpu);
    assert_eq!(out.cell(cpu.x, 0).style, theme::header_active());
    let pid = header_rect(&state, ColumnId::Pid);
    assert_eq!(out.cell(pid.x, 0).style, theme::header());
    let edge = edge_rect(&state, ColumnId::Pid);
    assert_eq!(edge.width, 1);
    assert_eq!(edge.x, pid.x + pid.width);
    assert_eq!(
        state.hits.at(Point::new(edge.x, 0)),
        Some(&Hit::HeaderEdge {
            column: ColumnId::Pid
        })
    );
    assert!(
        state
            .hits
            .rect_of(|h| *h
                == Hit::HeaderEdge {
                    column: ColumnId::Command
                })
            .is_none(),
        "flexible column has no edge"
    );
}

#[test]
fn rows_are_virtualised_and_each_row_has_a_hit() {
    let mut state = state_with(many(200));
    let out = render(&mut state, 100, 11);
    let rows: Vec<_> = state
        .hits
        .committed()
        .iter()
        .filter(|(_, h)| matches!(h, Hit::Row { .. }))
        .collect();
    assert_eq!(rows.len(), 10);
    for (r, _) in &rows {
        assert_eq!(r.x, 0);
        assert_eq!(r.width, 99, "full width minus the scrollbar");
    }
    assert!(out.row(1).contains("/usr/bin/proc1 --flag"));
    assert_eq!(state.table.body_height, 10);
}

#[test]
fn clicking_headers_sorts_and_persists() {
    let mut state = state_with(many(5));
    render(&mut state, 100, 10);
    assert_eq!(order(&state), vec![1, 2, 3, 4, 5]);
    let pid = header_rect(&state, ColumnId::Pid);
    state.take_requests();
    left(&mut state, pid.x + 1, 0);
    assert_eq!(state.table.sort().column, ColumnId::Pid);
    assert!(!state.table.sort().descending, "PID defaults ascending");
    assert_eq!(state.settings.sort_column, Some(ColumnId::Pid));
    assert!(!state.settings.sort_descending);
    assert!(state.take_requests().contains(&UiRequest::SaveSettings));

    forget_click(&mut state);
    let rss = header_rect(&state, ColumnId::Rss);
    left(&mut state, rss.x, 0);
    render(&mut state, 100, 10);
    assert_eq!(state.table.sort().column, ColumnId::Rss);
    assert!(state.table.sort().descending);
    assert_eq!(order(&state), vec![5, 4, 3, 2, 1]);

    forget_click(&mut state);
    let rss = header_rect(&state, ColumnId::Rss);
    left(&mut state, rss.x, 0);
    render(&mut state, 100, 10);
    assert!(!state.table.sort().descending, "same column flips");
    assert_eq!(order(&state), vec![1, 2, 3, 4, 5]);
    let out = render(&mut state, 100, 10);
    assert!(out.row(0).contains("RES▲"));
}

#[test]
fn right_click_header_opens_column_chooser() {
    let mut state = state_with(many(3));
    render(&mut state, 100, 10);
    let pid = header_rect(&state, ColumnId::Pid);
    right_down(&mut state, pid.x, 0);
    assert_eq!(state.overlays.len(), 1);
}

#[test]
fn wheel_scrolls_viewport_without_moving_selection() {
    let mut state = state_with(many(100));
    render(&mut state, 100, 11);
    let selected = state.table.selected();
    assert_eq!(selected, Some(key(1)));
    state.handle_event(&mouse(
        MouseEventKind::ScrollDown,
        10,
        5,
        MouseModifiers::NONE,
    ));
    assert_eq!(state.table.scroll, 3);
    render(&mut state, 100, 11);
    assert_eq!(state.table.scroll, 3, "draw does not snap back");
    assert_eq!(state.table.selected(), selected);
    for _ in 0..100 {
        state.handle_event(&mouse(
            MouseEventKind::ScrollDown,
            10,
            5,
            MouseModifiers::NONE,
        ));
    }
    assert_eq!(state.table.scroll, 90, "clamped to rows - height");
    state.handle_event(&mouse(
        MouseEventKind::ScrollUp,
        10,
        5,
        MouseModifiers::NONE,
    ));
    assert_eq!(state.table.scroll, 87);
    assert_eq!(state.table.selected(), selected);
}

#[test]
fn keyboard_moves_selection_and_keeps_it_visible() {
    let mut state = state_with(many(100));
    render(&mut state, 100, 11);
    press(&mut state, KeyCode::Down);
    press(&mut state, KeyCode::Char('j'));
    assert_eq!(state.table.selected(), Some(key(3)));
    press(&mut state, KeyCode::Char('k'));
    assert_eq!(state.table.selected(), Some(key(2)));
    press(&mut state, KeyCode::PageDown);
    assert_eq!(state.table.selected_index(), Some(10), "page = height - 1");
    render(&mut state, 100, 11);
    assert_eq!(state.table.scroll, 1);
    press(&mut state, KeyCode::End);
    render(&mut state, 100, 11);
    assert_eq!(state.table.selected(), Some(key(100)));
    assert_eq!(state.table.scroll, 90);
    press(&mut state, KeyCode::PageUp);
    assert_eq!(state.table.selected_index(), Some(90));
    press(&mut state, KeyCode::Home);
    render(&mut state, 100, 11);
    assert_eq!(state.table.selected(), Some(key(1)));
    assert_eq!(state.table.scroll, 0);
    press(&mut state, KeyCode::Up);
    assert_eq!(state.table.selected(), Some(key(1)), "clamped at the top");

    // After wheel-scrolling away, a key press brings the selection back.
    state.table.scroll_by(50);
    press(&mut state, KeyCode::Down);
    render(&mut state, 100, 11);
    assert_eq!(state.table.scroll, 1);
}

#[test]
fn table_ignores_global_action_keys() {
    let mut state = state_with(many(3));
    for code in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Enter,
        KeyCode::Space,
        KeyCode::Char('x'),
        KeyCode::F(5),
    ] {
        assert_eq!(
            handle_key(&mut state, &KeyStroke::simple(code)),
            Outcome::Ignored
        );
    }
    let ctrl_j = KeyStroke::with_modifiers(KeyCode::Char('j'), ctrl());
    assert_eq!(handle_key(&mut state, &ctrl_j), Outcome::Ignored);
}

#[test]
fn selection_follows_key_across_resorts_and_falls_back_by_index() {
    let mut entries = many(5);
    let mut state = state_with(entries.clone());
    state.table.select(key(2));
    // pid 2 jumps to the top on the next snapshot.
    entries[1].sample.cpu_percent = 99.0;
    state.store = ProcessStore::from_entries(entries.clone(), Default::default());
    state.table.mark_dirty();
    refresh(&mut state);
    assert_eq!(order(&state)[0], 2);
    assert_eq!(state.table.selected(), Some(key(2)));

    // Select index 3 (pid 3), then it disappears: the row now at index 3 wins.
    state.table.select(key(3));
    let idx = state.table.selected_index().unwrap();
    entries.retain(|e| e.record.key.pid != 3);
    state.store = ProcessStore::from_entries(entries.clone(), Default::default());
    state.table.mark_dirty();
    refresh(&mut state);
    let at = state.table.rows()[idx.min(state.table.rows().len() - 1)].key;
    assert_eq!(state.table.selected(), Some(at));

    // Selected last row disappears: the new last row is selected.
    let last = state.table.rows().last().unwrap().key;
    state.table.select(last);
    entries.retain(|e| e.record.key != last);
    state.store = ProcessStore::from_entries(entries, Default::default());
    state.table.mark_dirty();
    refresh(&mut state);
    assert_eq!(
        state.table.selected(),
        state.table.rows().last().map(|r| r.key)
    );
}

#[test]
fn hovering_keeps_the_current_order_but_updates_values() {
    let mut entries = many(4);
    let mut state = state_with(entries.clone());
    assert_eq!(order(&state), vec![1, 2, 3, 4]);
    entries[3].sample.cpu_percent = 500.0;
    state.store = ProcessStore::from_entries(entries.clone(), Default::default());
    state.table.mark_dirty();
    state.table.refresh(
        &state.store,
        &state.settings,
        &state.search,
        state.traffic,
        state.uid,
        true,
    );
    assert_eq!(order(&state), vec![1, 2, 3, 4], "frozen while hovering");
    let out = render(&mut state, 100, 8);
    assert!(out.row(4).contains("500"), "values still update");
    state.table.mark_dirty();
    refresh(&mut state);
    assert_eq!(order(&state), vec![4, 1, 2, 3]);
}

#[test]
fn plain_ctrl_shift_and_middle_clicks() {
    let mut state = state_with(many(10));
    render(&mut state, 100, 12);
    let y = row_y(&state, 3);
    left(&mut state, 20, y);
    assert_eq!(state.table.selected(), Some(key(3)));
    assert!(state.table.marked().is_empty());

    forget_click(&mut state);
    let ctrl = MouseModifiers {
        ctrl: true,
        ..MouseModifiers::NONE
    };
    let y = row_y(&state, 5);
    left_with(&mut state, 20, y, ctrl);
    assert_eq!(state.table.marked(), vec![key(5)]);
    forget_click(&mut state);
    let y = row_y(&state, 5);
    left_with(&mut state, 20, y, ctrl);
    assert!(state.table.marked().is_empty(), "ctrl-click toggles");

    forget_click(&mut state);
    let alt = MouseModifiers {
        alt: true,
        ..MouseModifiers::NONE
    };
    let y = row_y(&state, 6);
    left_with(&mut state, 20, y, alt);
    assert_eq!(state.table.marked(), vec![key(6)]);
    state.table.clear_marks();

    forget_click(&mut state);
    let y = row_y(&state, 2);
    left(&mut state, 20, y);
    forget_click(&mut state);
    let shift = MouseModifiers {
        shift: true,
        ..MouseModifiers::NONE
    };
    let y = row_y(&state, 5);
    left_with(&mut state, 20, y, shift);
    let mut marked = state.table.marked();
    marked.sort();
    assert_eq!(marked, vec![key(2), key(3), key(4), key(5)]);
    assert_eq!(state.table.selected(), Some(key(5)));

    state.table.clear_marks();
    let y = row_y(&state, 7);
    down(&mut state, MouseButton::Middle, 20, y);
    assert_eq!(state.table.marked(), vec![key(7)]);
}

#[test]
fn double_click_opens_details() {
    let mut state = state_with(many(5));
    render(&mut state, 100, 10);
    let y = row_y(&state, 4);
    left(&mut state, 30, y);
    assert!(!state.detail.is_open());
    left(&mut state, 30, y);
    assert!(state.detail.is_open());
    assert_eq!(state.table.selected(), Some(key(4)));
    assert!(
        state
            .take_requests()
            .contains(&UiRequest::LoadDetail(key(4)))
    );
}

#[test]
fn right_click_selects_and_opens_menu() {
    let mut state = state_with(many(5));
    render(&mut state, 100, 10);
    let y = row_y(&state, 3);
    right_down(&mut state, 30, y);
    assert_eq!(state.table.selected(), Some(key(3)));
    assert_eq!(state.overlays.len(), 1);
}

#[test]
fn dragging_a_header_edge_resizes_and_saves_on_release() {
    let mut state = state_with(many(5));
    render(&mut state, 100, 10);
    let edge = edge_rect(&state, ColumnId::User);
    let start = header_rect(&state, ColumnId::User).width;
    state.take_requests();
    let none = MouseModifiers::NONE;
    state.handle_event(&mouse(
        MouseEventKind::Down(MouseButton::Left),
        edge.x,
        0,
        none,
    ));
    state.handle_event(&mouse(
        MouseEventKind::Drag(MouseButton::Left),
        edge.x + 3,
        0,
        none,
    ));
    assert_eq!(state.settings.column_widths.get("user"), Some(&(start + 3)));
    let out = render(&mut state, 100, 10);
    assert_eq!(header_rect(&state, ColumnId::User).width, start + 3);
    assert!(out.row(0).contains('┃'), "edge lit while dragging");
    state.handle_event(&mouse(
        MouseEventKind::Drag(MouseButton::Left),
        edge.x + 5,
        0,
        none,
    ));
    assert_eq!(state.settings.column_widths.get("user"), Some(&(start + 5)));
    assert!(!state.take_requests().contains(&UiRequest::SaveSettings));
    state.handle_event(&mouse(
        MouseEventKind::Up(MouseButton::Left),
        edge.x + 5,
        0,
        none,
    ));
    assert!(state.take_requests().contains(&UiRequest::SaveSettings));

    // Shrinking is clamped to the minimum width.
    render(&mut state, 100, 10);
    let edge = edge_rect(&state, ColumnId::User);
    forget_click(&mut state);
    state.handle_event(&mouse(
        MouseEventKind::Down(MouseButton::Left),
        edge.x,
        0,
        none,
    ));
    state.handle_event(&mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0, none));
    state.handle_event(&mouse(MouseEventKind::Up(MouseButton::Left), 0, 0, none));
    assert_eq!(
        state.settings.column_widths.get("user"),
        Some(&MIN_COLUMN_WIDTH)
    );
}

#[test]
fn double_clicking_an_edge_restores_default_width() {
    let mut state = state_with(many(3));
    state.settings.column_widths.insert("user".into(), 20);
    render(&mut state, 100, 10);
    let edge = edge_rect(&state, ColumnId::User);
    left(&mut state, edge.x, 0);
    left(&mut state, edge.x, 0);
    assert!(!state.settings.column_widths.contains_key("user"));
}

#[test]
fn empty_states_are_centered_and_dim() {
    let mut state = state_with(Vec::new());
    let out = render(&mut state, 60, 6);
    let text = out.text();
    assert!(text.contains("Collecting process data…"), "{text}");
    let y = (1..6).find(|&y| out.row(y).contains("Collecting")).unwrap();
    let x = out.row(y).find('C').unwrap() as u16;
    assert_eq!(out.cell(x, y).style.fg, theme::dim().fg);

    let mut state = state_with(many(3));
    state.search.set_text("zzzz");
    state.table.mark_dirty();
    let out = render(&mut state, 80, 6);
    assert!(
        out.text()
            .contains("No processes match “zzzz” · Esc clears"),
        "{}",
        out.text()
    );
}

#[test]
fn scrollbar_appears_on_overflow_and_jumps_on_click() {
    let mut state = state_with(many(100));
    let out = render(&mut state, 80, 11);
    let bar = state
        .hits
        .rect_of(|h| *h == Hit::TableScrollbar)
        .expect("scrollbar");
    assert_eq!(bar, Rect::new(79, 1, 1, 10));
    assert_eq!(out.cell(79, 1).symbol, "┃");
    assert_eq!(out.cell(79, 10).symbol, "│");
    left(&mut state, 79, 10);
    assert_eq!(state.table.scroll, 90);
    assert_eq!(state.table.selected(), Some(key(1)), "selection untouched");
    let out = render(&mut state, 80, 11);
    assert_eq!(out.cell(79, 10).symbol, "┃");
    forget_click(&mut state);
    left(&mut state, 79, 1);
    assert_eq!(state.table.scroll, 0);
    state.handle_event(&mouse(
        MouseEventKind::Drag(MouseButton::Left),
        79,
        6,
        MouseModifiers::NONE,
    ));
    assert_eq!(state.table.scroll, 50);

    let mut few = state_with(many(3));
    render(&mut few, 80, 11);
    assert!(few.hits.rect_of(|h| *h == Hit::TableScrollbar).is_none());
}

#[test]
fn scrollbar_thumb_geometry() {
    assert_eq!(scrollbar_thumb(0, 100, 10, 10), (0, 1));
    assert_eq!(scrollbar_thumb(90, 100, 10, 10), (9, 1));
    assert_eq!(scrollbar_thumb(0, 20, 10, 10), (0, 5));
    assert_eq!(scrollbar_thumb(10, 20, 10, 10), (5, 5));
    assert_eq!(scrollbar_thumb(0, 5, 10, 10), (0, 10));
    assert_eq!(scrollbar_thumb(0, 0, 0, 0), (0, 0));
}

fn tree_rows(state: &mut ExplorerState) {
    state.settings.tree = true;
    let make = |pid: i32, depth: u16, prefix: &str, has_children: bool, collapsed: bool| ViewRow {
        key: key(pid),
        depth,
        has_children,
        collapsed,
        tree_prefix: prefix.into(),
        pinned: false,
        ghost: false,
        highlights: Vec::new(),
        aggregate: collapsed.then_some(Aggregate {
            processes: 3,
            cpu_percent: 42.5,
            mem_rss: 3 * 1024 * 1024,
            ..Default::default()
        }),
    };
    state.table.rows = vec![
        make(1, 0, "", true, false),
        make(2, 1, "├─ ", true, true),
        make(3, 1, "└─ ", false, false),
    ];
    state.table.dirty = false;
}

#[test]
fn tree_mode_draws_prefix_glyphs_and_toggles() {
    let mut state = state_with(many(3));
    tree_rows(&mut state);
    let out = render(&mut state, 120, 8);
    assert!(out.row(1).contains("▾ /usr/bin/proc1"), "{}", out.row(1));
    assert!(out.row(2).contains("├─ ▸ /usr/bin/proc2"), "{}", out.row(2));
    assert!(out.row(3).contains("└─   /usr/bin/proc3"), "{}", out.row(3));
    assert!(
        out.row(2).contains("Σ42.5"),
        "aggregate CPU: {}",
        out.row(2)
    );
    assert!(
        out.row(2).contains("Σ3.0M"),
        "aggregate RES: {}",
        out.row(2)
    );

    let toggle = state
        .hits
        .rect_of(|h| *h == Hit::TreeToggle { key: key(2) })
        .expect("toggle hit");
    assert_eq!(toggle.width, 2);
    assert_eq!(out.cell(toggle.x, 2).symbol, "▸");
    assert!(
        state
            .hits
            .rect_of(|h| *h == Hit::TreeToggle { key: key(3) })
            .is_none(),
        "leaves have no toggle"
    );
    assert!(!state.table.collapsed.contains(&key(2)));
    left(&mut state, toggle.x, toggle.y);
    assert!(state.table.collapsed.contains(&key(2)));
    assert_eq!(state.table.selected(), Some(key(2)));
    assert!(state.table.dirty);
}

#[test]
fn filter_highlights_are_drawn_in_the_command_column() {
    let mut state = state_with(many(3));
    state.table.rows[1].highlights = vec![(9, 13)];
    state.table.dirty = false;
    let out = render(&mut state, 120, 6);
    let row = out.row(2);
    let start = row.find("/usr/bin/proc").unwrap();
    let x = row[..start].chars().count() as u16;
    let lit = out.cell(x + 9, 2).style;
    assert_eq!(lit.fg, Some(Color::Yellow));
    assert!(lit.modifiers.contains(Modifier::UNDERLINE));
    assert!(lit.modifiers.contains(Modifier::BOLD));
    let plain = out.cell(x + 8, 2).style;
    assert!(!plain.modifiers.contains(Modifier::UNDERLINE));
    assert!(
        !out.cell(x + 13, 2)
            .style
            .modifiers
            .contains(Modifier::UNDERLINE)
    );
}

#[test]
fn row_styles_follow_priority_and_gutter_markers() {
    let mut state = state_with(many(6));
    state.table.select(key(1));
    state.table.toggle_mark(key(2));
    state.alerting.insert(key(3));
    state.table.toggle_pin(key(4));
    state.table.mark_dirty();
    let out = render(&mut state, 100, 10);
    let y_sel = row_y(&state, 1);
    assert_eq!(out.cell(20, y_sel).style.bg, theme::selected().bg);
    let y_mark = row_y(&state, 2);
    assert_eq!(out.cell(0, y_mark).symbol, "●");
    assert_eq!(out.cell(20, y_mark).style.fg, theme::marked().fg);
    let y_alert = row_y(&state, 3);
    assert_eq!(out.cell(20, y_alert).style.fg, theme::alert().fg);
    let y_pin = row_y(&state, 4);
    assert_eq!(y_pin, 1, "pinned rows float to the top");
    assert_eq!(out.cell(0, y_pin).symbol, "↑");

    // Hover beats marks; selection beats hover.
    state.pointer.position = Some(Point::new(30, y_mark));
    let out = render(&mut state, 100, 10);
    assert_eq!(out.cell(20, y_mark).style.bg, theme::hovered().bg);

    // Search focus dims the selection.
    state.search.focused = true;
    let out = render(&mut state, 100, 10);
    let y_sel = row_y(&state, 1);
    assert_eq!(out.cell(20, y_sel).style.bg, theme::selected_unfocused().bg);
}

#[test]
fn heat_colours_cpu_and_mem_cells() {
    let mut state = state_with(vec![
        entry(1, "hot", "hot", 95.0, 1024),
        entry(2, "cool", "cool", 0.5, 1024),
    ]);
    state.table.select(key(2));
    let out = render(&mut state, 100, 6);
    let cpu = header_rect(&state, ColumnId::Cpu);
    let y = row_y(&state, 1);
    let x = cpu.x + cpu.width - 1;
    assert_eq!(out.cell(x, y).style.fg, Some(Color::BrightRed));
    state.settings.heat_colors = false;
    let out = render(&mut state, 100, 6);
    assert_ne!(out.cell(x, y).style.fg, Some(Color::BrightRed));
}

#[test]
fn ghost_rows_are_dim() {
    let mut state = state_with(many(3));
    state.table.rows[2].ghost = true;
    state.table.dirty = false;
    let out = render(&mut state, 100, 6);
    assert!(
        out.cell(20, 3)
            .style
            .modifiers
            .contains(Modifier::CROSSED_OUT)
    );
}

#[test]
fn preset_change_resets_columns_sort_and_scroll() {
    let mut state = state_with(many(100));
    render(&mut state, 100, 11);
    state.table.scroll_by(30);
    state.settings.preset = Preset::Memory;
    state.table.on_preset_changed(&state.settings);
    assert_eq!(state.table.columns(), Preset::Memory.default_columns());
    assert_eq!(state.table.sort().column, ColumnId::Rss);
    assert!(state.table.sort().descending);
    assert_eq!(state.table.scroll, 0);
    assert!(state.table.dirty);
}

#[test]
fn marks_for_vanished_processes_are_pruned() {
    let mut state = state_with(many(3));
    state.table.toggle_mark(key(2));
    state.table.toggle_mark(ProcessKey::new(999, 1));
    state.table.mark_dirty();
    refresh(&mut state);
    assert_eq!(state.table.marked(), vec![key(2)]);
}

#[test]
fn fit_spans_truncates_with_ellipsis() {
    let s = Style::new();
    let spans = fit_spans(vec![("abc".into(), s), ("def".into(), s)], 4, s);
    let text: String = spans.iter().map(|s| s.content.as_str()).collect();
    assert_eq!(text, "abc…");
    let spans = fit_spans(vec![("ab".into(), s)], 4, s);
    let text: String = spans.iter().map(|s| s.content.as_str()).collect();
    assert_eq!(text, "ab  ");
    assert!(fit_spans(vec![("ab".into(), s)], 0, s).is_empty());
    let wide = fit_spans(vec![("界界界".into(), s)], 4, s);
    let text: String = wide.iter().map(|s| s.content.as_str()).collect();
    assert_eq!(display_width(&text), 4);
}

fn kept(slots: &[ColumnSlot]) -> Vec<ColumnId> {
    slots.iter().map(|s| s.column).collect()
}

fn command_width(slots: &[ColumnSlot]) -> u16 {
    slots
        .iter()
        .find(|s| s.column == ColumnId::Command)
        .map_or(0, |s| s.width)
}

#[test]
fn classic_width_keeps_command_readable_and_drops_dead_net_columns() {
    let settings = ExplorerSettings::default();
    let cols = Preset::Overview.default_columns();
    let off = LayoutHints {
        net_available: false,
        ..LayoutHints::default()
    };
    let slots = layout_columns_with(&cols, &settings, 0, 78, &off);
    let ids = kept(&slots);
    assert!(!ids.contains(&ColumnId::NetDown), "{ids:?}");
    assert!(!ids.contains(&ColumnId::NetUp), "{ids:?}");
    assert!(ids.contains(&ColumnId::Pid) && ids.contains(&ColumnId::Cpu));
    assert!(command_width(&slots) >= 24, "{slots:?}");
    assert!(slots.iter().all(|s| s.x + s.width <= 78));

    // With capture on, NET survives longer than less useful columns but the
    // command still gets its readable share.
    let slots = layout_columns_with(&cols, &settings, 0, 78, &LayoutHints::default());
    assert!(command_width(&slots) >= 78 * 3 / 10, "{slots:?}");
    assert!(kept(&slots).contains(&ColumnId::NetDown), "{slots:?}");

    // Small terminals too.
    let slots = layout_columns_with(&cols, &settings, 0, 59, &off);
    assert!(!kept(&slots).contains(&ColumnId::NetDown), "{slots:?}");
    assert!(command_width(&slots) >= 59 * 3 / 10, "{slots:?}");

    // Wide terminals keep every column.
    let slots = layout_columns_with(&cols, &settings, 0, 160, &off);
    assert_eq!(slots.len(), cols.len());
}

#[test]
fn net_columns_drop_first_only_when_capture_is_off() {
    let settings = ExplorerSettings::default();
    let cols = vec![
        ColumnId::Pid,
        ColumnId::Connections,
        ColumnId::NetDownTotal,
        ColumnId::Started,
        ColumnId::Command,
    ];
    let off = LayoutHints {
        net_available: false,
        ..LayoutHints::default()
    };
    // Fixed columns need 7+5+7+9 plus 4 gaps; at 40 cells the flexible
    // column's comfort width (12) forces exactly one fixed column out.
    let width = 40;
    let ids = kept(&layout_columns_with(&cols, &settings, 0, width, &off));
    assert!(ids.contains(&ColumnId::Started), "{ids:?}");
    assert_eq!(ids.len(), cols.len() - 1, "{ids:?}");
}

#[test]
fn net_column_hint_follows_capture_state() {
    let mut state = state_with(many(3));
    assert!(layout_hints(&state).net_available);
    let width = (40..120)
        .find(|&w| render(&mut state, w, 6).row(0).contains("NET"))
        .expect("NET column appears at some width");
    state.network_note = Some("capture off".into());
    assert!(!layout_hints(&state).net_available);
    let out = render(&mut state, width, 6);
    assert!(!out.row(0).contains("NET"), "{}", out.row(0));
}

#[test]
fn auto_user_width_bounds() {
    assert_eq!(auto_user_width([]), None);
    assert_eq!(auto_user_width(["me"]), Some(4));
    assert_eq!(auto_user_width(["root", "bsteffaniak"]), Some(11));
    assert_eq!(auto_user_width(["a_very_long_service_account"]), Some(14));
}

#[test]
fn user_column_fits_names_unless_resized() {
    let mut state = state_with(many(3));
    render(&mut state, 100, 6);
    assert_eq!(header_rect(&state, ColumnId::User).width, 4, "fits \"me\"");

    let mut entries = many(3);
    let mut long = entry(9, "svc", "svc", 0.5, 1024);
    std::sync::Arc::make_mut(&mut long.record).user = Some("_windowserver_daemon".into());
    entries.push(long);
    let mut state = state_with(entries);
    render(&mut state, 100, 6);
    assert_eq!(header_rect(&state, ColumnId::User).width, 14, "capped");

    state.settings.column_widths.insert("user".into(), 9);
    render(&mut state, 100, 6);
    assert_eq!(header_rect(&state, ColumnId::User).width, 9, "override wins");
}

#[test]
fn rows_are_dimmed_while_the_filter_does_not_parse() {
    let mut state = state_with(many(3));
    state.search.set_text("proc");
    state.table.mark_dirty();
    refresh(&mut state);
    state.search.set_text("proc cpu>>5");
    assert!(state.search.error.is_some());
    let out = render(&mut state, 80, 6);
    let y = row_y(&state, 2);
    assert!(out.cell(30, y).style.modifiers.contains(Modifier::DIM));
    state.search.set_text("proc");
    let out = render(&mut state, 80, 6);
    assert!(!out.cell(30, y).style.modifiers.contains(Modifier::DIM));
}
