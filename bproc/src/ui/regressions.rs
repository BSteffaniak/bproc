use super::*;
use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::{
    buffer::Buffer, event::Event, frame::Frame, geometry::Point, interaction::InteractionRoute,
};
fn route(target: &str, event: Event) -> InteractionRoute {
    InteractionRoute {
        event,
        target: Some(target.into()),
        local_position: None,
        bounds: None,
        role: None,
        hover_left: None,
        hover_entered: None,
        focus_changed: None,
        traversal_consumed: false,
    }
}
fn paint(app: &mut AppState, width: u16, height: u16) -> Buffer {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    draw_ui(&mut PaintCx::new(&mut Frame::new(&mut buffer)), app);
    buffer
}
fn key(target: &str, code: KeyCode) -> InteractionRoute {
    route(target, Event::Key(KeyStroke::simple(code)))
}
#[test]
fn title_rule_and_process_header_right_border_are_preserved() {
    let mut app = AppState::new();
    let buffer = paint(&mut app, 120, 40);
    assert_eq!(buffer.get(Point::new(30, 3)).unwrap().symbol, "─");
    assert_eq!(buffer.get(Point::new(119, 4)).unwrap().symbol, "│");
}
#[test]
fn percentage_modal_geometry_matches_original_rounding() {
    for (percent_x, percent_y, width, height, x, y, w, h) in [
        (60, 50, 160, 50, 32, 13, 96, 25),
        (60, 30, 160, 50, 32, 18, 96, 15),
        (90, 70, 160, 50, 8, 8, 144, 35),
        (60, 30, 80, 24, 16, 8, 48, 8),
    ] {
        assert_eq!(
            centered_rect(percent_x, percent_y, Rect::new(0, 0, width, height)),
            Rect::new(x, y, w, h)
        );
    }
}
#[test]
fn committed_list_navigation_selects_wraps_pages_and_leaves_enter_semantic() {
    let mut app = AppState::new();
    app.process_list = (1..=25)
        .map(|pid| ProcessInfo::new(pid, format!("worker {pid}")))
        .collect();
    app.list_state.select(Some(0));
    app.selected_index = Some(0);
    paint(&mut app, 120, 40);
    app.commit_component_scene();
    assert!(app.handle_component_event(&key("draw_process_list", KeyCode::Down)));
    assert_eq!(app.get_selected_process().unwrap().pid, 2);
    assert!(!app.handle_component_event(&key("draw_process_list", KeyCode::Enter)));
    app.enter_process_detail();
    assert_eq!(app.selected_process_detail_pid, Some(2));
    app.view_mode = ViewMode::ProcessView;
    app.list_state.select(Some(0));
    app.selected_index = Some(0);
    paint(&mut app, 120, 40);
    app.commit_component_scene();
    assert!(app.handle_component_event(&key("draw_process_list", KeyCode::Up)));
    assert_eq!(app.selected_index, Some(24));
    paint(&mut app, 120, 40);
    app.commit_component_scene();
    assert!(app.handle_component_event(&key("draw_process_list", KeyCode::PageDown)));
    assert_eq!(app.selected_index, Some(9));
    assert!(!app.handle_component_event(&key("draw_header", KeyCode::Down)));
    assert_eq!(app.selected_index, Some(9));
}
#[test]
fn uncommitted_frame_cannot_replace_input_authority_and_numeric_editor_preserves_aliases() {
    let mut app = AppState::new();
    app.show_throttle_dialog = true;
    paint(&mut app, 120, 40);
    assert!(!app.handle_component_event(&route("throttle.download", Event::Paste("123".into()))));
    app.commit_component_scene();
    assert!(app.handle_component_event(&route("throttle.download", Event::Paste("123".into()))));
    assert_eq!(app.throttle_dialog.download_input, "123");
    assert!(app.handle_component_event(&route("throttle.download", Event::Paste("bad".into()))));
    assert_eq!(app.throttle_dialog.download_input, "123");
    for code in [
        KeyCode::Char('q'),
        KeyCode::Escape,
        KeyCode::Tab,
        KeyCode::Enter,
    ] {
        assert!(!app.handle_component_event(&key("throttle.download", code)));
    }
    assert!(app.handle_component_event(&key("throttle.download", KeyCode::Left)));
    assert!(app.handle_component_event(&key("throttle.download", KeyCode::Char('4'))));
    assert_eq!(app.throttle_dialog.download_input, "1243");
}
#[test]
fn graph_preserves_boxed_legend_full_plot_extent_and_border_under_instructions() {
    let mut app = AppState::new();
    app.process_list.push(ProcessInfo::new(1, "graph".into()));
    app.list_state.select(Some(0));
    app.selected_index = Some(0);
    app.show_graph = true;
    app.history.update_at(1, 1, "graph".into(), 1024, 128);
    app.history.update_at(2, 1, "graph".into(), 2048, 256);
    let buffer = paint(&mut app, 120, 40);
    assert_eq!(buffer.get(Point::new(6, 32)).unwrap().symbol, "│");
    assert_eq!(buffer.get(Point::new(103, 7)).unwrap().symbol, "┌");
    assert_eq!(buffer.get(Point::new(112, 10)).unwrap().symbol, "┘");
    assert_eq!(buffer.get(Point::new(7, 7)).unwrap().symbol, "B");
}

#[test]
fn first_navigation_selects_first_row_in_both_directions() {
    for direction in [KeyCode::Down, KeyCode::Up] {
        let mut app = AppState::new();
        app.process_list = (1..=3)
            .map(|pid| ProcessInfo::new(pid, format!("p{pid}")))
            .collect();
        paint(&mut app, 120, 40);
        app.commit_component_scene();
        assert!(app.handle_component_event(&key("draw_process_list", direction)));
        assert_eq!(app.get_selected_process().unwrap().pid, 1);
    }
}
#[test]
fn tab_focus_edits_new_field_without_repainting_old_committed_geometry() {
    let mut app = AppState::new();
    app.show_throttle_dialog = true;
    paint(&mut app, 120, 40);
    app.commit_component_scene();
    assert!(!app.handle_component_event(&key("throttle.download.input", KeyCode::Tab)));
    app.throttle_dialog.selected_field = ThrottleField::Upload;
    assert!(app.handle_component_event(&key("throttle.upload.input", KeyCode::Char('7'))));
    assert_eq!(app.throttle_dialog.upload_input, "7");
    assert_eq!(app.throttle_dialog.download_input, "");
}

#[test]
fn footer_uses_bmux_display_width_for_emoji_clusters() {
    let emoji = "👩‍💻";
    let width = bmux_tui::text_width::display_width(emoji) as u16;
    let rows = wrap_spans_to_lines(
        vec![
            Span::raw(emoji),
            Span::raw(" "),
            Span::raw("[q]"),
            Span::raw(" Quit"),
        ],
        width + 1,
    );
    assert!(rows.len() > 1);
    assert_eq!(rows[0].plain_text(), format!("{emoji} "));
    assert!(
        rows.iter()
            .map(Line::plain_text)
            .collect::<String>()
            .contains("[q] Quit")
    );
}
