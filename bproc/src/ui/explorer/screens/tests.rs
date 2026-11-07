use super::editor::{Field, TargetKind};
use super::test_support::*;
use super::*;
use crate::procs::actions::Signal;
use crate::procs::model::{ExitedProcess, ProcSnapshot};
use crate::procs::watch::{Comparison, WatchAction, WatchMetric, glob_match};

fn events_state() -> ExplorerState {
    let mut state = state_with_processes();
    state.screen = Screen::Events;
    state
        .events
        .push(event(EventKind::Started, Some(100), "node", "started"));
    state.events.push(event(
        EventKind::Exited,
        Some(999),
        "gone",
        "exited after 3s",
    ));
    state
        .events
        .push(event(EventKind::Alert, Some(100), "node", "CPU above 80%"));
    state.events.push(event(
        EventKind::Action,
        Some(200),
        "postgres",
        "SIGTERM sent",
    ));
    state
        .events
        .push(event(EventKind::Info, None, "bproc", "monitoring started"));
    state
}

fn rule(id: u64, name: &str, target: WatchTarget) -> WatchRule {
    WatchRule {
        id,
        name: name.into(),
        enabled: true,
        target,
        metric: WatchMetric::CpuPercent,
        comparison: Comparison::Above,
        threshold: 80.0,
        sustain_secs: 10,
        cooldown_secs: 60,
        action: WatchAction::Notify,
    }
}

fn watches_state() -> ExplorerState {
    let mut state = state_with_processes();
    state.screen = Screen::Watches;
    state.watch.rules = vec![
        rule(1, "node hot", WatchTarget::Name("node".into())),
        rule(2, "pg hot", WatchTarget::Name("post*".into())),
    ];
    state
}

fn rect_of(state: &ExplorerState, hit: Hit) -> Rect {
    state
        .hits
        .rect_of(|h| *h == hit)
        .unwrap_or_else(|| panic!("no hit region {hit:?}"))
}

fn editor(state: &ExplorerState) -> &WatchEditor {
    match state.overlays.last() {
        Some(Overlay::WatchEditor(e)) => e,
        _ => panic!("editor not open"),
    }
}

fn row_y(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{text}"))
}

// ---- events ----------------------------------------------------------------------

#[test]
fn unseen_counts_events_since_last_view() {
    let mut state = events_state();
    state.screen = Screen::Processes;
    assert_eq!(state.events_view.unseen(&state.events), 5);
    state.screen = Screen::Events;
    render(&mut state, 120, 30);
    assert_eq!(state.events_view.unseen(&state.events), 0);
    state.events.push(event(
        EventKind::Started,
        Some(300),
        "chrome helper",
        "started",
    ));
    assert_eq!(state.events_view.unseen(&state.events), 1);
}

#[test]
fn events_render_newest_first_with_kinds_and_chips() {
    let mut state = events_state();
    let text = render(&mut state, 120, 30);
    for needle in [
        "All 5",
        "Started 1",
        "Alerts 1",
        "TIME",
        "MESSAGE",
        "▶ started",
        "⚠ alert",
        "CPU above 80%",
        "monitoring started",
        "● live",
    ] {
        assert!(text.contains(needle), "missing {needle:?}\n{text}");
    }
    assert!(row_y(&text, "monitoring started") < row_y(&text, "SIGTERM sent"));
    assert!(row_y(&text, "SIGTERM sent") < row_y(&text, "exited after 3s"));
    let time = clock(state.events.iter().next().unwrap().at_ms);
    assert_eq!(time.len(), 8);
    assert!(text.contains(&time));
}

#[test]
fn events_empty_states() {
    let mut state = state_with_processes();
    state.screen = Screen::Events;
    let text = render(&mut state, 100, 30);
    assert!(text.contains("No events yet"));
    let mut only_actions = state_with_processes();
    only_actions.screen = Screen::Events;
    only_actions
        .events
        .push(event(EventKind::Info, None, "bproc", "hello"));
    only_actions.events_view.chips = [true, false, false, false];
    let text = render(&mut only_actions, 100, 30);
    assert!(text.contains("No events match this filter"));
}

#[test]
fn events_screen_survives_tiny_sizes() {
    for (w, h) in [
        (200, 60),
        (80, 24),
        (40, 8),
        (12, 3),
        (3, 2),
        (1, 1),
        (0, 0),
    ] {
        let mut state = events_state();
        render(&mut state, w, h);
        let (_, hits) = paint_with(&mut state, w, h, draw_events);
        for (r, hit) in hits {
            assert!(
                r.right() <= w && r.bottom() <= h,
                "{hit:?} {r:?} at {w}x{h}"
            );
        }
    }
}

#[test]
fn chip_clicks_filter_and_all_resets() {
    let mut state = events_state();
    render(&mut state, 120, 30);
    let alerts = rect_of(
        &state,
        Hit::WatchControl {
            index: EVENTS_CTRL,
            control: EventChip::Alerts.index() as u16,
        },
    );
    click(&mut state, alerts.x + 1, alerts.y);
    assert!(state.events_view.chip_on(EventChip::Alerts));
    let text = render(&mut state, 120, 30);
    assert!(text.contains("CPU above 80%"));
    assert!(!text.contains("SIGTERM sent"));
    let actions = rect_of(
        &state,
        Hit::WatchControl {
            index: EVENTS_CTRL,
            control: EventChip::Actions.index() as u16,
        },
    );
    click(&mut state, actions.x + 1, actions.y);
    let text = render(&mut state, 120, 30);
    assert!(text.contains("CPU above 80%") && text.contains("SIGTERM sent"));
    assert!(!text.contains("exited after 3s"));
    let all = rect_of(
        &state,
        Hit::WatchControl {
            index: EVENTS_CTRL,
            control: 0,
        },
    );
    click(&mut state, all.x + 1, all.y);
    assert!(state.events_view.is_all());
}

#[test]
fn tab_and_arrows_cycle_single_chip_filters() {
    let mut state = events_state();
    render(&mut state, 120, 30);
    key(&mut state, KeyCode::Tab);
    assert!(state.events_view.chip_on(EventChip::Started));
    key(&mut state, KeyCode::Right);
    assert!(state.events_view.chip_on(EventChip::Exited));
    assert!(!state.events_view.chip_on(EventChip::Started));
    key_with(&mut state, KeyCode::Tab, shift());
    key(&mut state, KeyCode::Left);
    assert!(state.events_view.is_all());
    key(&mut state, KeyCode::Left);
    assert!(state.events_view.chip_on(EventChip::Actions));
}

#[test]
fn follows_newest_until_user_moves_then_anchors() {
    let mut state = events_state();
    for i in 0..60 {
        state
            .events
            .push(event(EventKind::Info, None, "bproc", &format!("tick {i}")));
    }
    render(&mut state, 120, 20);
    let newest = state.events.pushed() - 1;
    assert_eq!(state.events_view.selected, Some(newest));
    state
        .events
        .push(event(EventKind::Info, None, "bproc", "fresh"));
    render(&mut state, 120, 20);
    assert_eq!(state.events_view.selected, Some(newest + 1), "auto-follow");
    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Down);
    let picked = state.events_view.selected.unwrap();
    assert!(state.events_view.detached);
    let text = render(&mut state, 120, 20);
    assert!(text.contains("newer · Home to follow"));
    for _ in 0..3 {
        state
            .events
            .push(event(EventKind::Info, None, "bproc", "later"));
    }
    render(&mut state, 120, 20);
    assert_eq!(
        state.events_view.selected,
        Some(picked),
        "selection is stable"
    );
    assert_eq!(state.events_view.scroll, 3, "viewport anchored");
    key(&mut state, KeyCode::Home);
    render(&mut state, 120, 20);
    assert!(!state.events_view.detached);
    assert_eq!(state.events_view.selected, Some(state.events.pushed() - 1));
}

#[test]
fn paging_and_end_move_through_the_log() {
    let mut state = events_state();
    for i in 0..100 {
        state
            .events
            .push(event(EventKind::Info, None, "bproc", &format!("tick {i}")));
    }
    render(&mut state, 120, 20);
    let page = state.events_view.page;
    key(&mut state, KeyCode::PageDown);
    let list = state.events_view.visible(&state.events);
    assert_eq!(state.events_view.selected_index(&list), Some(page));
    key(&mut state, KeyCode::End);
    let list = state.events_view.visible(&state.events);
    assert_eq!(
        state.events_view.selected_index(&list),
        Some(list.len() - 1)
    );
    let text = render(&mut state, 120, 20);
    assert!(text.contains("monitoring started") || text.contains("started"));
    key(&mut state, KeyCode::PageUp);
    key(&mut state, KeyCode::Char('k'));
    let list = state.events_view.visible(&state.events);
    assert_eq!(
        state.events_view.selected_index(&list),
        Some(list.len() - 2 - page)
    );
}

#[test]
fn wheel_scrolls_without_moving_selection() {
    let mut state = events_state();
    for i in 0..100 {
        state
            .events
            .push(event(EventKind::Info, None, "bproc", &format!("tick {i}")));
    }
    render(&mut state, 120, 20);
    let selected = state.events_view.selected;
    let body = rect_of(&state, Hit::ScreenBody);
    wheel(&mut state, body.x + 5, body.y + 5, false);
    assert_eq!(state.events_view.scroll, 3);
    assert!(state.events_view.detached);
    render(&mut state, 120, 20);
    assert_eq!(state.events_view.selected, selected);
    wheel(&mut state, body.x + 5, body.y + 5, true);
    assert_eq!(state.events_view.scroll, 0);
}

#[test]
fn click_selects_and_double_click_opens_live_process() {
    let mut state = events_state();
    render(&mut state, 120, 30);
    let alert_seq = 2u64;
    let row = rect_of(
        &state,
        Hit::EventRow {
            index: alert_seq as usize,
        },
    );
    click(&mut state, row.x + 2, row.y);
    assert_eq!(state.events_view.selected, Some(alert_seq));
    assert_eq!(state.screen, Screen::Events);
    render(&mut state, 120, 30);
    double_click(&mut state, row.x + 2, row.y);
    assert_eq!(state.screen, Screen::Processes);
    assert_eq!(state.table.selected(), Some(key_of(100)));
    assert_eq!(state.detail.key(), Some(key_of(100)));
    assert!(
        state
            .take_requests()
            .contains(&UiRequest::LoadDetail(key_of(100)))
    );
}

#[test]
fn enter_on_dead_process_explains_instead_of_jumping() {
    let mut state = events_state();
    render(&mut state, 120, 30);
    let row = rect_of(&state, Hit::EventRow { index: 1 });
    click(&mut state, row.x, row.y);
    key(&mut state, KeyCode::Enter);
    assert_eq!(state.screen, Screen::Events);
    let flash = state.flash.as_ref().map(|(m, _)| m.clone()).unwrap();
    assert!(flash.contains("no longer running"), "{flash}");
    // Ghost rows (recently exited) count as dead too.
    let mut snapshot = ProcSnapshot {
        sequence: 2,
        acquired_at_ms: crate::procs::model::now_ms(),
        ..Default::default()
    };
    let record = state.store.get(&key_of(100)).unwrap().entry.record.clone();
    snapshot.exited.push(ExitedProcess {
        record,
        last_sample: Default::default(),
    });
    state.store.apply_snapshot(snapshot);
    assert!(!jump_to_process(&mut state, key_of(100)));
    assert_eq!(state.screen, Screen::Events);
}

#[test]
fn enter_on_live_event_jumps() {
    let mut state = events_state();
    render(&mut state, 120, 30);
    key(&mut state, KeyCode::Down);
    assert_eq!(state.events_view.selected, Some(3));
    key(&mut state, KeyCode::Enter);
    assert_eq!(state.screen, Screen::Processes);
    assert_eq!(state.detail.key(), Some(key_of(200)));
}

#[test]
fn jump_clears_a_search_that_would_hide_the_target() {
    let mut state = state_with_processes();
    state.search.set_text("postgres");
    render(&mut state, 120, 30);
    assert!(jump_to_process(&mut state, key_of(100)));
    assert!(
        state.search.text.is_empty(),
        "filter hid node, so it is cleared"
    );
    let text = render(&mut state, 120, 30);
    assert!(text.contains("node"), "{text}");
    assert_eq!(state.table.selected(), Some(key_of(100)));

    // A search that already shows the target is left alone.
    let mut state = state_with_processes();
    state.search.set_text("node");
    render(&mut state, 120, 30);
    assert!(jump_to_process(&mut state, key_of(100)));
    assert_eq!(state.search.text, "node");
}

#[test]
fn right_click_on_live_event_opens_process_menu() {
    let mut state = events_state();
    render(&mut state, 120, 30);
    let row = rect_of(&state, Hit::EventRow { index: 3 });
    right_click(&mut state, row.x + 4, row.y);
    assert!(matches!(state.overlays.last(), Some(Overlay::Menu(_))));
    let mut state = events_state();
    render(&mut state, 120, 30);
    let row = rect_of(&state, Hit::EventRow { index: 1 });
    right_click(&mut state, row.x + 4, row.y);
    assert!(state.overlays.is_empty(), "no menu for an exited process");
}

#[test]
fn unhandled_keys_fall_through_to_global_actions() {
    let mut state = events_state();
    render(&mut state, 120, 30);
    key(&mut state, KeyCode::Char('3'));
    assert_eq!(state.screen, Screen::Watches);
    key(&mut state, KeyCode::Char('2'));
    assert_eq!(state.screen, Screen::Events);
}

// ---- watches ---------------------------------------------------------------------

#[test]
fn glob_and_target_matching() {
    assert!(glob_match("node", "NODE"));
    assert!(!glob_match("node", "nodejs"));
    assert!(glob_match("node*", "nodejs"));
    assert!(glob_match("*chrome*", "Google Chrome Helper"));
    assert!(glob_match("p?stgres", "postgres"));
    assert!(glob_match("*", ""));
    assert!(!glob_match("a*b", "acd"));
    assert!(glob_match("a*b*c", "aXXbYYc"));
    let state = state_with_processes();
    let row = state.store.get(&key_of(300));
    let k = key_of(300);
    assert!(target_matches(
        &WatchTarget::Command("SERVE".into()),
        &k,
        row
    ));
    assert!(target_matches(&WatchTarget::User("alice".into()), &k, row));
    // Same (case-sensitive) user rule as the alert evaluator.
    assert!(!target_matches(&WatchTarget::User("Alice".into()), &k, row));
    assert!(target_matches(
        &WatchTarget::Command(String::new()),
        &k,
        row
    ));
    assert!(!target_matches(&WatchTarget::Command("x".into()), &k, None));
    assert!(!target_matches(&WatchTarget::User("bob".into()), &k, row));
    assert!(target_matches(&WatchTarget::Any, &k, None));
    assert!(target_matches(
        &WatchTarget::Process {
            key: k,
            name: "x".into()
        },
        &k,
        None
    ));
    assert!(!target_matches(&WatchTarget::Name("x".into()), &k, None));
}

#[test]
fn watches_render_rules_status_and_alerts() {
    let mut state = watches_state();
    state.alerting.insert(key_of(100));
    let text = render(&mut state, 120, 30);
    for needle in [
        "+ New rule",
        "Edit",
        "Delete",
        "Disable",
        "node hot",
        "pg hot",
        "● alerting",
        "idle",
        "Currently alerting (1)",
        "CPU",
    ] {
        assert!(text.contains(needle), "missing {needle:?}\n{text}");
    }
    assert!(row_y(&text, "node hot") < row_y(&text, "pg hot"));
    assert!(row_y(&text, "Currently alerting") > row_y(&text, "pg hot"));
    state.watch.rules[0].enabled = false;
    let text = render(&mut state, 120, 30);
    assert!(text.contains("Enable"));
    assert!(!text.contains("● alerting"), "disabled rules never alert");
}

#[test]
fn watches_empty_state_and_tiny_sizes() {
    let mut state = state_with_processes();
    state.screen = Screen::Watches;
    let text = render(&mut state, 100, 30);
    assert!(text.contains("No watch rules yet"));
    assert!(text.contains("Nothing is alerting"));
    assert!(
        state
            .hits
            .rect_of(|h| matches!(
                h,
                Hit::WatchControl {
                    index: WATCH_BUTTONS,
                    control: 1
                }
            ))
            .is_none(),
        "Edit is disabled without rules"
    );
    for (w, h) in [(200, 60), (80, 24), (30, 6), (5, 2), (1, 1), (0, 0)] {
        let mut state = watches_state();
        state.alerting.insert(key_of(100));
        state.alerting.insert(key_of(42));
        render(&mut state, w, h);
        let (_, hits) = paint_with(&mut state, w, h, draw_watches);
        for (r, hit) in hits {
            assert!(
                r.right() <= w && r.bottom() <= h,
                "{hit:?} {r:?} at {w}x{h}"
            );
        }
    }
}

#[test]
fn keyboard_toggle_delete_and_navigation() {
    let mut state = watches_state();
    render(&mut state, 120, 30);
    key(&mut state, KeyCode::Down);
    assert_eq!(state.watches_view.selected, 1);
    key(&mut state, KeyCode::Char(' '));
    assert!(!state.watch.rules[1].enabled);
    assert_eq!(state.take_requests(), vec![UiRequest::SaveSettings]);
    key(&mut state, KeyCode::Char('d'));
    assert_eq!(state.watch.rules.len(), 2, "first press arms");
    assert_eq!(state.watches_view.confirm_delete, Some(2));
    let text = render(&mut state, 120, 30);
    assert!(text.contains("Confirm delete"));
    key(&mut state, KeyCode::Up);
    assert_eq!(state.watches_view.confirm_delete, None, "other keys disarm");
    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Delete);
    key(&mut state, KeyCode::Char('d'));
    assert_eq!(state.watch.rules.len(), 1);
    assert_eq!(state.watch.rules[0].id, 1);
    assert_eq!(state.watches_view.selected, 0);
    assert!(state.take_requests().contains(&UiRequest::SaveSettings));
    assert!(
        state.settings.dock == crate::config::DockPosition::Bottom,
        "d must not toggle the dock"
    );
}

#[test]
fn n_and_enter_open_editors() {
    let mut state = watches_state();
    render(&mut state, 120, 30);
    key(&mut state, KeyCode::Char('n'));
    assert_eq!(editor(&state).id, None);
    key(&mut state, KeyCode::Escape);
    assert!(state.overlays.is_empty());
    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Enter);
    assert_eq!(editor(&state).id, Some(2));
    assert_eq!(editor(&state).value.text, "post*");
}

#[test]
fn mouse_buttons_checkbox_and_rows() {
    let mut state = watches_state();
    render(&mut state, 120, 30);
    let check = rect_of(
        &state,
        Hit::WatchControl {
            index: 1,
            control: CHECKBOX,
        },
    );
    click(&mut state, check.x + 1, check.y);
    assert!(!state.watch.rules[1].enabled);
    assert_eq!(state.watches_view.selected, 1);
    render(&mut state, 120, 30);
    let delete = rect_of(
        &state,
        Hit::WatchControl {
            index: WATCH_BUTTONS,
            control: WatchButton::Delete as u16,
        },
    );
    click(&mut state, delete.x + 1, delete.y);
    render(&mut state, 120, 30);
    let delete = rect_of(
        &state,
        Hit::WatchControl {
            index: WATCH_BUTTONS,
            control: WatchButton::Delete as u16,
        },
    );
    click(&mut state, delete.x + 1, delete.y);
    assert_eq!(state.watch.rules.len(), 1);
    render(&mut state, 120, 30);
    let toggle = rect_of(
        &state,
        Hit::WatchControl {
            index: WATCH_BUTTONS,
            control: WatchButton::Toggle as u16,
        },
    );
    click(&mut state, toggle.x + 1, toggle.y);
    assert!(!state.watch.rules[0].enabled);
    render(&mut state, 120, 30);
    let row = rect_of(&state, Hit::WatchRow { index: 0 });
    double_click(&mut state, row.x + 30, row.y);
    assert_eq!(editor(&state).id, Some(1));
    key(&mut state, KeyCode::Escape);
    render(&mut state, 120, 30);
    let new = rect_of(
        &state,
        Hit::WatchControl {
            index: WATCH_BUTTONS,
            control: WatchButton::New as u16,
        },
    );
    click(&mut state, new.x + 1, new.y);
    assert_eq!(editor(&state).id, None);
}

#[test]
fn alerting_rows_jump_by_mouse_and_keyboard() {
    let mut state = watches_state();
    state.alerting.insert(key_of(200));
    state.alerting.insert(key_of(100));
    render(&mut state, 120, 30);
    assert_eq!(
        state.watches_view.alert_keys,
        vec![key_of(100), key_of(200)]
    );
    let row = rect_of(
        &state,
        Hit::WatchControl {
            index: ALERT_ROWS,
            control: 1,
        },
    );
    click(&mut state, row.x + 3, row.y);
    assert_eq!(state.screen, Screen::Processes);
    assert_eq!(state.detail.key(), Some(key_of(200)));
    state.screen = Screen::Watches;
    state.watches_view.focus_alerts = false;
    render(&mut state, 120, 30);
    key(&mut state, KeyCode::Tab);
    assert!(state.watches_view.focus_alerts);
    key(&mut state, KeyCode::Up);
    key(&mut state, KeyCode::Enter);
    assert_eq!(state.screen, Screen::Processes);
    assert_eq!(state.table.selected(), Some(key_of(100)));
}

#[test]
fn wheel_scrolls_long_rule_lists() {
    let mut state = watches_state();
    for i in 3..60 {
        state
            .watch
            .rules
            .push(rule(i, &format!("rule {i}"), WatchTarget::Any));
    }
    render(&mut state, 120, 30);
    let body = rect_of(&state, Hit::WatchRow { index: 0 });
    wheel(&mut state, body.x + 5, body.y, false);
    assert_eq!(state.watches_view.scroll, 3);
    let text = render(&mut state, 120, 30);
    assert!(!text.contains("node hot"));
    key(&mut state, KeyCode::End);
    let text = render(&mut state, 120, 30);
    assert!(text.contains("rule 59"));
}

// ---- editor ----------------------------------------------------------------------

#[test]
fn watch_process_prefills_and_saves() {
    let mut state = state_with_processes();
    state.overlays.push(watch_editor_for(&state, key_of(100)));
    let e = editor(&state);
    assert_eq!(e.name.text, "node CPU");
    assert_eq!(e.target, TargetKind::Process);
    assert_eq!(e.metric, WatchMetric::CpuPercent);
    assert_eq!(e.comparison, Comparison::Above);
    let rule = e.build(1).unwrap();
    assert_eq!(rule.threshold, 80.0);
    assert_eq!(rule.sustain_secs, 10);
    assert_eq!(rule.cooldown_secs, 60);
    assert_eq!(rule.action, WatchAction::Notify);
    assert_eq!(
        rule.target,
        WatchTarget::Process {
            key: key_of(100),
            name: "node".into()
        }
    );
    let text = render(&mut state, 120, 40);
    assert!(text.contains("New watch rule"));
    assert!(text.contains("node · PID 100"));
    key(&mut state, KeyCode::Enter);
    assert!(state.overlays.is_empty());
    assert_eq!(state.watch.rules.len(), 1);
    assert_eq!(state.watch.rules[0].id, 1);
    assert!(state.take_requests().contains(&UiRequest::SaveSettings));
    assert!(state.flash.as_ref().unwrap().0.contains("node CPU"));
}

#[test]
fn validation_errors_are_inline_and_focus_the_field() {
    let mut state = watches_state();
    render(&mut state, 120, 40);
    key(&mut state, KeyCode::Char('n'));
    key(&mut state, KeyCode::Enter);
    assert_eq!(editor(&state).focus, Field::Value);
    assert!(editor(&state).error.is_some());
    let text = render(&mut state, 120, 40);
    assert!(text.contains("⚠ Enter a process name"), "{text}");
    type_text(&mut state, "redis*");
    assert!(editor(&state).error.is_none(), "typing clears the error");
    key(&mut state, KeyCode::Tab);
    assert_eq!(editor(&state).focus, Field::Metric);
    key(&mut state, KeyCode::Right);
    key(&mut state, KeyCode::Right);
    assert_eq!(editor(&state).metric, WatchMetric::MemRss);
    key(&mut state, KeyCode::Tab);
    key(&mut state, KeyCode::Tab);
    assert_eq!(editor(&state).focus, Field::Threshold);
    key_with(&mut state, KeyCode::Char('u'), ctrl());
    type_text(&mut state, "500x");
    key(&mut state, KeyCode::Enter);
    assert_eq!(editor(&state).focus, Field::Threshold);
    let text = render(&mut state, 120, 40);
    assert!(text.contains("not a size"));
    key(&mut state, KeyCode::Backspace);
    type_text(&mut state, "M");
    key(&mut state, KeyCode::Enter);
    assert!(state.overlays.is_empty());
    let saved = state.watch.rules.last().unwrap();
    assert_eq!(saved.id, 3);
    assert_eq!(saved.metric, WatchMetric::MemRss);
    assert_eq!(saved.threshold, 500.0 * 1048576.0);
    assert_eq!(saved.target, WatchTarget::Name("redis*".into()));
    assert_eq!(state.watches_view.selected, 2);
}

#[test]
fn editor_mouse_controls() {
    let mut state = watches_state();
    state.overlays.push(edit_rule_editor(&state, 1).unwrap());
    render(&mut state, 120, 40);
    let next = rect_of(
        &state,
        Hit::OverlayControl {
            overlay: 0,
            control: editor::control(Field::Metric, 2),
        },
    );
    click(&mut state, next.x, next.y);
    assert_eq!(editor(&state).metric, WatchMetric::MemPercent);
    assert_eq!(editor(&state).focus, Field::Metric);
    render(&mut state, 120, 40);
    let prev = rect_of(
        &state,
        Hit::OverlayControl {
            overlay: 0,
            control: editor::control(Field::Action, 1),
        },
    );
    click(&mut state, prev.x, prev.y);
    assert_eq!(editor(&state).action, WatchAction::Signal(Signal::Stop));
    render(&mut state, 120, 40);
    let enabled = rect_of(
        &state,
        Hit::OverlayControl {
            overlay: 0,
            control: editor::control(Field::Enabled, 0),
        },
    );
    click(&mut state, enabled.x, enabled.y);
    assert!(!editor(&state).enabled);
    render(&mut state, 120, 40);
    let name = rect_of(
        &state,
        Hit::OverlayControl {
            overlay: 0,
            control: editor::control(Field::Name, 3),
        },
    );
    click(&mut state, name.x + 2, name.y);
    assert_eq!(editor(&state).focus, Field::Name);
    assert_eq!(editor(&state).name.cursor, 2);
    type_text(&mut state, "X");
    assert_eq!(editor(&state).name.text, "noXde hot");
    // Backdrop clicks keep the form (no accidental data loss).
    click(&mut state, 0, 0);
    assert!(!state.overlays.is_empty());
    render(&mut state, 120, 40);
    let save = rect_of(
        &state,
        Hit::OverlayControl {
            overlay: 0,
            control: editor::control(Field::Save, 0),
        },
    );
    click(&mut state, save.x + 1, save.y);
    assert!(state.overlays.is_empty());
    assert_eq!(state.watch.rules.len(), 2, "edit replaces in place");
    let r = &state.watch.rules[0];
    assert_eq!((r.id, r.name.as_str()), (1, "noXde hot"));
    assert_eq!(r.metric, WatchMetric::MemPercent);
    assert!(!r.enabled);
    assert_eq!(r.action, WatchAction::Signal(Signal::Stop));
}

#[test]
fn editor_cancel_discards() {
    let mut state = watches_state();
    state.overlays.push(edit_rule_editor(&state, 2).unwrap());
    render(&mut state, 100, 30);
    type_text(&mut state, "zzz");
    let cancel = rect_of(
        &state,
        Hit::OverlayControl {
            overlay: 0,
            control: editor::control(Field::Cancel, 0),
        },
    );
    click(&mut state, cancel.x + 1, cancel.y);
    assert!(state.overlays.is_empty());
    assert_eq!(state.watch.rules[1].name, "pg hot");
    assert!(state.take_requests().is_empty());
    assert!(edit_rule_editor(&state, 99).is_none());
}

#[test]
fn editor_paste_and_shift_tab() {
    let mut state = state_with_processes();
    state.overlays.push(new_rule_editor(&state));
    state.handle_event(&Event::Paste("my rule".into()));
    assert_eq!(editor(&state).name.text, "my rule");
    key_with(&mut state, KeyCode::Tab, shift());
    assert_eq!(editor(&state).focus, Field::Save);
    key(&mut state, KeyCode::Left);
    assert_eq!(editor(&state).focus, Field::Cancel);
    key(&mut state, KeyCode::Enter);
    assert!(state.overlays.is_empty());
}

#[test]
fn new_rule_offers_selected_process() {
    let mut state = state_with_processes();
    state.table.select(key_of(200));
    state.overlays.push(new_rule_editor(&state));
    let e = editor(&state);
    assert_eq!(e.process.as_ref().map(|p| p.1.as_str()), Some("postgres"));
    assert_eq!(e.target, TargetKind::Name);
}

#[test]
fn editor_renders_within_bounds_at_all_sizes() {
    for (w, h) in [
        (200, 60),
        (100, 30),
        (60, 16),
        (30, 8),
        (10, 4),
        (2, 2),
        (1, 1),
        (0, 0),
    ] {
        let mut state = state_with_processes();
        state.overlays.push(watch_editor_for(&state, key_of(100)));
        if let Some(Overlay::WatchEditor(e)) = state.overlays.last_mut() {
            e.focus = Field::Save;
            e.error = Some((Field::Threshold, "bad".into()));
        }
        render(&mut state, w, h);
        for (r, hit) in state.hits.committed() {
            assert!(
                r.right() <= w && r.bottom() <= h,
                "{hit:?} {r:?} at {w}x{h}"
            );
        }
        if w >= 60 && h >= 16 {
            let text = render(&mut state, w, h);
            assert!(text.contains("Save"), "{w}x{h}\n{text}");
        }
    }
}
