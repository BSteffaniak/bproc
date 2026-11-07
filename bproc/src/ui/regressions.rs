//! Regressions for the legacy network modals (throttle editor, graph).
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
    assert_eq!(buffer.get(Point::new(103, 7)).unwrap().symbol, "╭");
    assert_eq!(buffer.get(Point::new(112, 10)).unwrap().symbol, "╯");
    assert_eq!(buffer.get(Point::new(7, 7)).unwrap().symbol, "B");
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
