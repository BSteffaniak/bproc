use super::*;
use crate::ui::explorer::table::fixtures::*;
use bmux_keyboard::Modifiers;
use bmux_tui::event::{Event, MouseButton};

fn pids(state: &ExplorerState) -> Vec<i32> {
    state.table.rows().iter().map(|r| r.key.pid).collect()
}

fn named() -> ExplorerState {
    state_with(vec![
        entry(1, "node", "node server.js", 5.0, 1024),
        entry(2, "python", "python app.py", 4.0, 1024),
        entry(3, "nodemon", "nodemon watch", 3.0, 1024),
        entry(4, "bash", "bash", 2.0, 1024),
    ])
}

#[test]
fn draws_placeholder_and_box_hit_when_empty() {
    let mut state = named();
    let area = Rect::new(4, 0, 50, 1);
    let out = render_search(&mut state, area);
    let row = out.row(0);
    assert!(row.contains("⌕ Search  /"), "{row}");
    assert_eq!(state.hits.at(Point::new(10, 0)), Some(&Hit::SearchBox));
    assert!(state.hits.rect_of(|h| *h == Hit::SearchClear).is_none());
    assert!(out.cursor.is_none(), "no cursor while unfocused");
    assert_eq!(out.cell(8, 0).style.fg, theme::dim().fg);
}

#[test]
fn never_panics_on_tiny_boxes() {
    for w in 0..=8 {
        for text in ["", "abc", "界界界界界界"] {
            let mut state = named();
            state.search.set_text(text);
            state.search.focused = true;
            render_search(&mut state, Rect::new(0, 0, w, 1));
            render_search(&mut state, Rect::new(0, 0, w, 0));
        }
    }
}

#[test]
fn typing_filters_rows_and_shows_count_and_clear() {
    let mut state = named();
    state.search.focused = true;
    type_text(&mut state, "node");
    assert_eq!(state.search.text, "node");
    assert_eq!(state.search.cursor, 4);
    refresh(&mut state);
    assert_eq!(pids(&state), vec![1, 3]);
    let area = Rect::new(0, 0, 40, 1);
    let out = render_search(&mut state, area);
    let row = out.row(0);
    assert!(row.contains("⌕ node"), "{row}");
    assert!(row.contains("2/4"), "{row}");
    assert!(row.contains('✕'), "{row}");
    let clear = state
        .hits
        .rect_of(|h| *h == Hit::SearchClear)
        .expect("clear button");
    assert_eq!(clear.x + clear.width, 40);
    let cursor = out.cursor.expect("cursor while focused");
    assert!(cursor.visible);
    assert_eq!(cursor.position, Point::new(6, 0));
}

#[test]
fn editing_keys_move_cursor_and_delete() {
    let mut state = named();
    state.search.focused = true;
    type_text(&mut state, "foo bar");
    press(&mut state, KeyCode::Left);
    press(&mut state, KeyCode::Left);
    press(&mut state, KeyCode::Backspace);
    assert_eq!(state.search.text, "foo ar");
    assert_eq!(state.search.cursor, 4);
    press(&mut state, KeyCode::Delete);
    assert_eq!(state.search.text, "foo r");
    press(&mut state, KeyCode::Home);
    type_text(&mut state, "x");
    assert_eq!(state.search.text, "xfoo r");
    press(&mut state, KeyCode::End);
    assert_eq!(state.search.cursor, 6);
    press(&mut state, KeyCode::Space);
    type_text(&mut state, "baz");
    press_with(&mut state, KeyCode::Char('w'), ctrl());
    assert_eq!(state.search.text, "xfoo r ");
    press_with(&mut state, KeyCode::Char('w'), ctrl());
    assert_eq!(state.search.text, "xfoo ");
    press_with(&mut state, KeyCode::Char('u'), ctrl());
    assert_eq!(state.search.text, "");
    assert!(state.search.focused);
    // Unicode is handled per char.
    type_text(&mut state, "界a");
    press(&mut state, KeyCode::Left);
    press(&mut state, KeyCode::Backspace);
    assert_eq!(state.search.text, "a");
}

#[test]
fn arrows_move_table_selection_while_typing() {
    let mut state = named();
    render_both(&mut state, 80, 10);
    state.search.focused = true;
    press(&mut state, KeyCode::Down);
    assert_eq!(state.table.selected().map(|k| k.pid), Some(2));
    assert!(state.search.focused);
    press(&mut state, KeyCode::Up);
    assert_eq!(state.table.selected().map(|k| k.pid), Some(1));
    // `j` is text while typing, not navigation.
    type_text(&mut state, "j");
    assert_eq!(state.search.text, "j");
}

#[test]
fn enter_keeps_filter_unfocuses_and_selects_first() {
    let mut state = named();
    state.table.select(key(4));
    state.search.focused = true;
    type_text(&mut state, "python");
    press(&mut state, KeyCode::Enter);
    assert!(!state.search.focused);
    assert_eq!(state.search.text, "python");
    assert_eq!(pids(&state), vec![2]);
    assert_eq!(state.table.selected(), Some(key(2)));
    assert!(
        !state.detail.is_open(),
        "Enter in search does not open details"
    );
}

#[test]
fn escape_clears_then_unfocuses() {
    let mut state = named();
    state.search.focused = true;
    type_text(&mut state, "node");
    press(&mut state, KeyCode::Escape);
    assert_eq!(state.search.text, "");
    assert!(!state.search.focused);
    refresh(&mut state);
    assert_eq!(pids(&state).len(), 4);

    state.search.focused = true;
    press(&mut state, KeyCode::Escape);
    assert!(!state.search.focused);

    state.search.focused = true;
    press(&mut state, KeyCode::Tab);
    assert!(!state.search.focused);
}

#[test]
fn function_keys_fall_through_to_actions() {
    let mut state = named();
    state.search.focused = true;
    assert_eq!(
        handle_key(&mut state, &KeyStroke::simple(KeyCode::F(5))),
        Outcome::Ignored
    );
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    assert_eq!(
        handle_key(
            &mut state,
            &KeyStroke::with_modifiers(KeyCode::Char('x'), alt)
        ),
        Outcome::Ignored
    );
}

#[test]
fn clicking_focuses_and_positions_cursor() {
    let mut state = named();
    state.search.set_text("hello world");
    render_both(&mut state, 60, 8);
    assert!(!state.search.focused);
    // Text starts after "⌕ " at column 2; click on the 'w' (index 6).
    left(&mut state, 8, 0);
    assert!(state.search.focused);
    assert_eq!(state.search.cursor, 6);
    // Clicking past the text puts the cursor at the end.
    state.pointer.last_click = None;
    render_both(&mut state, 60, 8);
    left(&mut state, 30, 0);
    assert_eq!(state.search.cursor, 11);
    // Clicking the table unfocuses.
    render_both(&mut state, 60, 8);
    left(&mut state, 20, 2);
    assert!(!state.search.focused);
}

#[test]
fn clear_button_clears_and_focuses() {
    let mut state = named();
    state.search.set_text("node");
    state.table.mark_dirty();
    render_both(&mut state, 60, 8);
    assert_eq!(pids(&state), vec![1, 3]);
    let clear = state.hits.rect_of(|h| *h == Hit::SearchClear).unwrap();
    left(&mut state, clear.x, clear.y);
    assert_eq!(state.search.text, "");
    assert!(state.search.focused);
    refresh(&mut state);
    assert_eq!(pids(&state).len(), 4);
}

#[test]
fn paste_inserts_at_cursor_without_control_chars() {
    let mut state = named();
    state.search.focused = true;
    type_text(&mut state, "ab");
    press(&mut state, KeyCode::Left);
    state.handle_event(&Event::Paste("X\nY\n".into()));
    assert_eq!(state.search.text, "aX Yb");
    assert_eq!(state.search.cursor, 4);
}

#[test]
fn long_text_scrolls_to_keep_cursor_visible() {
    let mut state = named();
    state.search.focused = true;
    let long = "abcdefghijklmnopqrstuvwxyz0123456789";
    type_text(&mut state, long);
    let out = render_search(&mut state, Rect::new(0, 0, 20, 1));
    let cursor = out.cursor.unwrap();
    assert!(cursor.position.x < 20);
    assert!(out.row(0).contains('9'), "tail visible: {}", out.row(0));
    press(&mut state, KeyCode::Home);
    let out = render_search(&mut state, Rect::new(0, 0, 20, 1));
    assert!(out.row(0).contains("abc"), "{}", out.row(0));
    assert_eq!(out.cursor.unwrap().position.x, 2);
}

#[test]
fn parse_errors_replace_the_count() {
    let mut state = named();
    state.search.set_text("node");
    state.search.error = Some("unknown column".into());
    let out = render_search(&mut state, Rect::new(0, 0, 50, 1));
    let row = out.row(0);
    assert!(row.contains("unknown column"), "{row}");
    assert!(!row.contains("/4"), "{row}");
    assert_eq!(out.cell(2, 0).style.fg, theme::error().fg);
}

#[test]
fn right_click_focuses_without_moving_cursor() {
    let mut state = named();
    state.search.set_text("abc");
    state.search.cursor = 1;
    render_both(&mut state, 60, 8);
    down(&mut state, MouseButton::Right, 3, 0);
    assert!(state.search.focused);
    assert_eq!(state.search.cursor, 1);
}
