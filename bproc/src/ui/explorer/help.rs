//! Help overlay generated from the action registry; every row is clickable
//! and runs its action.
use super::ExplorerState;
use super::actions::{Action, BINDINGS, Group};
use super::hits::{Hit, contains};
use super::overlays::OverlayOutcome;
use super::paint::{Frame, centered, fit, theme, truncate};
use super::screens::input::{TextInput, typed_char};
use crate::procs::actions::Signal;
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui::geometry::Rect;
use bmux_tui::style::Style;
use bmux_tui::text::{Line, Span};

#[derive(Debug, Default)]
pub struct HelpState {
    pub scroll: usize,
    /// Type-to-filter query.
    pub query: String,
    /// Selected runnable row (index into the filtered binding rows).
    pub selected: usize,
}

/// Mouse gestures listed in the help's "Mouse" section. Only gestures a
/// terminal can actually report: mouse reports never carry Cmd/⌘.
pub const MOUSE: &[(&str, &str)] = &[
    ("Click", "Select a row"),
    ("Double-click", "Open details"),
    ("Right-click", "Actions menu"),
    ("Ctrl/Alt-click", "Mark / unmark"),
    ("Middle-click", "Mark / unmark"),
    ("Shift-click", "Mark a range"),
    ("Wheel", "Scroll without moving selection"),
    ("Drag header edge", "Resize a column"),
    ("Click header", "Sort by that column"),
    ("Right-click header", "Choose columns"),
    ("Click meters/tabs", "Meters, tabs, chips, F-keys"),
];

/// Keys of the details panel ("inspector"), listed in their own section.
pub const INSPECTOR: &[(&str, &str)] = &[
    ("{ }", "Previous / next tab"),
    (FULL_DETAILS_KEYS, "Full screen"),
    ("Tab ⇧Tab ← →", "Switch tabs (full screen)"),
    ("↑↓ PgUp PgDn", "Scroll (full screen)"),
    ("Esc", "Leave full screen / close"),
    ("Drag splitter", "Resize the panel"),
    ("Dbl-click splitter", "Toggle full screen"),
];

/// Shift+Enter only arrives as such with the kitty keyboard protocol; most
/// terminals send a plain Enter, and Enter on an open panel goes full screen.
const FULL_DETAILS_KEYS: &str = "Shift+Enter, Enter×2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Heading(&'static str),
    /// Index into `BINDINGS`.
    Binding(usize),
    Mouse(usize),
    /// Index into `INSPECTOR`.
    Inspector(usize),
    Blank,
}

/// Signal-dialog shortcut for actions that have no key of their own.
fn signal_shortcut(action: Action) -> Option<usize> {
    let signal = match action {
        Action::Terminate => Signal::Term,
        Action::Kill => Signal::Kill,
        Action::Suspend => Signal::Stop,
        Action::Resume => Signal::Cont,
        _ => return None,
    };
    Signal::ALL.iter().position(|s| *s == signal).map(|i| i + 1)
}

/// Key label shown in the help's key column: real keys only, never a
/// placeholder word. Actions reached through the signal dialog show its
/// shortcut (e.g. "x ▸ 1"); actions without any key show nothing.
pub fn help_keys(action: Action) -> String {
    if action == Action::FullDetails {
        return FULL_DETAILS_KEYS.into();
    }
    let keys = action.keys();
    if !keys.is_empty() {
        return keys.into();
    }
    match signal_shortcut(action) {
        Some(n) => format!("x ▸ {n}"),
        None => String::new(),
    }
}

fn matches(query: &str, haystacks: &[&str]) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || haystacks.iter().any(|h| h.to_lowercase().contains(&q))
}

/// Filtered sections (heading + rows), empty sections dropped.
pub fn sections(query: &str) -> Vec<Vec<Entry>> {
    let mut out = Vec::new();
    for group in Group::ALL {
        let rows: Vec<Entry> = BINDINGS
            .iter()
            .enumerate()
            .filter(|(_, b)| b.group == group)
            .filter(|(_, b)| matches(query, &[&help_keys(b.action), b.label, group.label()]))
            .map(|(i, _)| Entry::Binding(i))
            .collect();
        if !rows.is_empty() {
            let mut section = vec![Entry::Heading(group.label())];
            section.extend(rows);
            out.push(section);
        }
    }
    let inspector: Vec<Entry> = INSPECTOR
        .iter()
        .enumerate()
        .filter(|(_, (keys, what))| matches(query, &[keys, what, "inspector", "details"]))
        .map(|(i, _)| Entry::Inspector(i))
        .collect();
    if !inspector.is_empty() {
        let mut section = vec![Entry::Heading("Inspector")];
        section.extend(inspector);
        out.push(section);
    }
    let mouse: Vec<Entry> = MOUSE
        .iter()
        .enumerate()
        .filter(|(_, (gesture, what))| matches(query, &[gesture, what, "mouse"]))
        .map(|(i, _)| Entry::Mouse(i))
        .collect();
    if !mouse.is_empty() {
        let mut section = vec![Entry::Heading("Mouse")];
        section.extend(mouse);
        out.push(section);
    }
    out
}

/// Lay sections out in one or two columns (balanced at section boundaries).
pub fn columns(query: &str, two: bool) -> Vec<Vec<Entry>> {
    let sections = sections(query);
    let join = |list: &[Vec<Entry>]| {
        let mut col = Vec::new();
        for (i, s) in list.iter().enumerate() {
            if i > 0 {
                col.push(Entry::Blank);
            }
            col.extend_from_slice(s);
        }
        col
    };
    if !two || sections.len() < 2 {
        return vec![join(&sections)];
    }
    let total: usize = sections.iter().map(|s| s.len() + 1).sum();
    let mut best = (usize::MAX, 1);
    let mut acc = 0;
    for split in 1..sections.len() {
        acc += sections[split - 1].len() + 1;
        let diff = acc.abs_diff(total - acc);
        if diff < best.0 {
            best = (diff, split);
        }
    }
    vec![join(&sections[..best.1]), join(&sections[best.1..])]
}

/// Runnable rows in navigation order: (binding, column, row).
fn runnable(cols: &[Vec<Entry>]) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for (c, col) in cols.iter().enumerate() {
        for (r, e) in col.iter().enumerate() {
            if let Entry::Binding(b) = e {
                out.push((*b, c, r));
            }
        }
    }
    out
}

struct HelpLayout {
    rect: Rect,
    inner: Rect,
    content: Rect,
    two: bool,
}

fn layout(area: Rect) -> HelpLayout {
    let w = area.width.saturating_sub(4).clamp(area.width.min(20), 112);
    let rect = centered(
        area,
        w,
        area.height.saturating_sub(2).max(area.height.min(3)),
    );
    let inner = if rect.width >= 2 && rect.height >= 2 {
        Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2)
    } else {
        Rect::new(rect.x, rect.y, 0, 0)
    };
    let content_h = inner.height.saturating_sub(3);
    let content = Rect::new(
        inner.x,
        inner.y + 2.min(inner.height),
        inner.width,
        content_h,
    );
    HelpLayout {
        rect,
        inner,
        content,
        two: inner.width >= 90,
    }
}

fn max_scroll(cols: &[Vec<Entry>], page: usize) -> usize {
    let rows = cols.iter().map(Vec::len).max().unwrap_or(0);
    rows.saturating_sub(page)
}

pub fn draw(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    index: usize,
    help: &HelpState,
    state: &ExplorerState,
) {
    frame.hit(area, Hit::OverlayBackdrop { overlay: index });
    let l = layout(area);
    if l.rect.width == 0 || l.rect.height == 0 {
        return;
    }
    let bg = theme::panel_bg();
    frame.panel(
        l.rect,
        Some("Help · keys & mouse"),
        theme::panel_border(),
        bg,
    );
    frame.hit(l.rect, Hit::OverlayBody { overlay: index });
    let inner = l.inner;
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // Query line.
    let close = " Esc close ";
    let close_w = (close.len() as u16).min(inner.width);
    let q_rect = Rect::new(
        inner.x + 1.min(inner.width),
        inner.y,
        inner.width.saturating_sub(close_w + 2),
        1,
    );
    let label = "Filter ";
    frame.text(q_rect, label, bg.patch(theme::accent()));
    let input = TextInput::new(help.query.clone());
    let field = Rect::new(
        q_rect.x + label.len() as u16,
        q_rect.y,
        q_rect.width.saturating_sub(label.len() as u16),
        1,
    );
    if q_rect.width as usize > label.len() {
        input.draw(frame, field, bg.patch(theme::bold()), true, "");
        if help.query.is_empty() && field.width > 2 {
            frame.text(
                Rect::new(field.x + 1, field.y, field.width - 1, 1),
                &truncate("type to filter…", field.width as usize - 1),
                bg.patch(theme::dim()),
            );
        }
    }
    if inner.width > close_w {
        frame.text(
            Rect::new(inner.right() - close_w, inner.y, close_w, 1),
            close,
            bg.patch(theme::dim()),
        );
    }
    if l.content.height == 0 {
        return;
    }
    let cols = columns(&help.query, l.two);
    let page = l.content.height as usize;
    let scroll = help.scroll.min(max_scroll(&cols, page));
    let runnable = runnable(&cols);
    let selected = runnable.get(help.selected).map(|(b, _, _)| *b);
    let col_w = if cols.len() == 2 {
        l.content.width / 2
    } else {
        l.content.width
    };
    if cols.iter().all(Vec::is_empty) {
        let text = format!("Nothing matches “{}”", help.query);
        frame.text(
            Rect::new(
                l.content.x + 2.min(l.content.width),
                l.content.y,
                l.content.width.saturating_sub(2),
                1,
            ),
            &truncate(&text, l.content.width.saturating_sub(2) as usize),
            bg.patch(theme::dim()),
        );
    }
    for (c, col) in cols.iter().enumerate() {
        let x = l.content.x + col_w * c as u16;
        let w = if c + 1 == cols.len() {
            l.content.right() - x
        } else {
            col_w.saturating_sub(1)
        };
        for (r, entry) in col.iter().enumerate().skip(scroll).take(page) {
            let rect = Rect::new(x, l.content.y + (r - scroll) as u16, w, 1);
            draw_entry(frame, rect, index, *entry, selected, state, bg);
        }
    }
    let footer = Rect::new(
        inner.x + 1.min(inner.width),
        inner.bottom() - 1,
        inner.width.saturating_sub(2),
        1,
    );
    if inner.height >= 3 {
        let more = max_scroll(&cols, page) > scroll;
        let hint = format!(
            "Click a row or press Enter to run it · ↑↓ select{}",
            if more { " · ↓ more" } else { "" }
        );
        frame.text(
            footer,
            &truncate(&hint, footer.width as usize),
            bg.patch(theme::dim()),
        );
    }
}

fn draw_entry(
    frame: &mut Frame<'_, '_, '_>,
    rect: Rect,
    index: usize,
    entry: Entry,
    selected: Option<usize>,
    state: &ExplorerState,
    bg: Style,
) {
    let keys_w = KEYS_W.min(rect.width as usize / 2);
    match entry {
        Entry::Blank => {}
        Entry::Heading(title) => {
            frame.line_bg(
                rect,
                &Line::from_spans(vec![Span::styled(
                    format!(" {title}"),
                    bg.patch(theme::accent()).patch(theme::bold()),
                )]),
                bg,
            );
        }
        Entry::Binding(b) => {
            let binding = &BINDINGS[b];
            let hovered = state.pointer.position.is_some_and(|p| contains(rect, p));
            let row = if selected == Some(b) {
                theme::selected()
            } else if hovered {
                bg.patch(theme::hovered())
            } else {
                bg
            };
            let keep = |s: Style| {
                if selected == Some(b) {
                    row
                } else {
                    row.patch(s)
                }
            };
            let keys = help_keys(binding.action);
            let key_style = if binding.keys.is_empty() {
                theme::dim()
            } else {
                theme::accent().patch(theme::bold())
            };
            frame.line_bg(
                rect,
                &Line::from_spans(vec![
                    Span::styled(format!("  {} ", fit(&keys, keys_w, false)), keep(key_style)),
                    Span::styled(binding.label, row),
                ]),
                row,
            );
            frame.hit(
                rect,
                Hit::OverlayItem {
                    overlay: index,
                    item: b,
                },
            );
        }
        Entry::Mouse(m) => {
            let (gesture, what) = MOUSE[m];
            draw_tip(
                frame,
                rect,
                keys_w,
                gesture,
                what,
                bg.patch(theme::bold()),
                bg,
            );
        }
        Entry::Inspector(i) => {
            let (keys, what) = INSPECTOR[i];
            let style = bg.patch(theme::accent()).patch(theme::bold());
            draw_tip(frame, rect, keys_w, keys, what, style, bg);
        }
    }
}

/// Width of the key column (fits "Shift+Enter, Enter×2").
const KEYS_W: usize = 20;

/// Informational (non-runnable) row: key or gesture, then what it does.
fn draw_tip(
    frame: &mut Frame<'_, '_, '_>,
    rect: Rect,
    keys_w: usize,
    keys: &str,
    what: &str,
    key_style: Style,
    bg: Style,
) {
    frame.line_bg(
        rect,
        &Line::from_spans(vec![
            Span::styled(format!("  {} ", fit(keys, keys_w, false)), key_style),
            Span::styled(what.to_string(), bg.patch(theme::dim())),
        ]),
        bg,
    );
}

/// Keep the selected row inside the viewport.
fn reveal(help: &mut HelpState, cols: &[Vec<Entry>], page: usize) {
    let runnable = runnable(cols);
    if runnable.is_empty() {
        help.selected = 0;
        return;
    }
    help.selected = help.selected.min(runnable.len() - 1);
    let (_, _, row) = runnable[help.selected];
    // Include the section heading when revealing the first row of a section.
    let top = if row > 0 && matches!(cols[runnable[help.selected].1][row - 1], Entry::Heading(_)) {
        row - 1
    } else {
        row
    };
    let page = page.max(1);
    if top < help.scroll {
        help.scroll = top;
    } else if row >= help.scroll + page {
        help.scroll = row + 1 - page;
    }
    help.scroll = help.scroll.min(max_scroll(cols, page));
}

fn run(action: Action, state: &mut ExplorerState) -> OverlayOutcome {
    if action != Action::Help {
        state.run(action);
    }
    OverlayOutcome::Close
}

pub fn handle(
    help: &mut HelpState,
    index: usize,
    event: &Event,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    let l = layout(state.last_area);
    let page = l.content.height as usize;
    let cols = columns(&help.query, l.two);
    match event {
        Event::Key(key) => {
            let runnable = runnable(&cols);
            let last = runnable.len().saturating_sub(1);
            match key.key {
                KeyCode::Escape => {
                    if help.query.is_empty() {
                        return OverlayOutcome::Close;
                    }
                    help.query.clear();
                    help.selected = 0;
                    help.scroll = 0;
                    return OverlayOutcome::Consumed;
                }
                KeyCode::F(1) => return OverlayOutcome::Close,
                KeyCode::Enter => {
                    return match runnable.get(help.selected) {
                        Some((b, _, _)) => run(BINDINGS[*b].action, state),
                        None => OverlayOutcome::Consumed,
                    };
                }
                KeyCode::Up => help.selected = help.selected.saturating_sub(1),
                KeyCode::Down => help.selected = (help.selected + 1).min(last),
                KeyCode::PageUp => help.selected = help.selected.saturating_sub(page.max(1)),
                KeyCode::PageDown => help.selected = (help.selected + page.max(1)).min(last),
                KeyCode::Home if help.query.is_empty() => help.selected = 0,
                KeyCode::End if help.query.is_empty() => help.selected = last,
                KeyCode::Backspace => {
                    help.query.pop();
                    help.selected = 0;
                    help.scroll = 0;
                    return OverlayOutcome::Consumed;
                }
                _ => {
                    if let Some(c) = typed_char(key) {
                        if help.query.is_empty() && (c == '?' || c == 'q') {
                            return OverlayOutcome::Close;
                        }
                        if !(help.query.is_empty() && c == ' ') {
                            help.query.push(c);
                        }
                        help.selected = 0;
                        help.scroll = 0;
                    } else if key.modifiers.ctrl && key.key == KeyCode::Char('u') {
                        help.query.clear();
                    }
                    return OverlayOutcome::Consumed;
                }
            }
            reveal(help, &cols, page);
            OverlayOutcome::Consumed
        }
        Event::Paste(text) => {
            help.query.extend(text.chars().filter(|c| !c.is_control()));
            help.selected = 0;
            help.scroll = 0;
            OverlayOutcome::Consumed
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                help.scroll = help.scroll.saturating_sub(3);
                OverlayOutcome::Consumed
            }
            MouseEventKind::ScrollDown => {
                help.scroll = (help.scroll + 3).min(max_scroll(&cols, page));
                OverlayOutcome::Consumed
            }
            MouseEventKind::Down(MouseButton::Left) => match state.hits.at(mouse.position) {
                Some(Hit::OverlayItem { overlay, item }) if *overlay == index => {
                    match BINDINGS.get(*item) {
                        Some(b) => run(b.action, state),
                        None => OverlayOutcome::Consumed,
                    }
                }
                Some(Hit::OverlayBackdrop { overlay }) if *overlay == index => {
                    OverlayOutcome::Close
                }
                _ => OverlayOutcome::Consumed,
            },
            _ => OverlayOutcome::Consumed,
        },
        _ => OverlayOutcome::Consumed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::explorer::overlays::Overlay;
    use crate::ui::explorer::screens::test_support::*;

    fn open_help(state: &mut ExplorerState) {
        state.overlays.push(Overlay::Help(HelpState::default()));
    }

    fn help_state(state: &ExplorerState) -> &HelpState {
        match state.overlays.last() {
            Some(Overlay::Help(h)) => h,
            _ => panic!("help not open"),
        }
    }

    #[test]
    fn sections_cover_every_binding_and_mouse_tips() {
        let all: Vec<Entry> = sections("").into_iter().flatten().collect();
        for i in 0..BINDINGS.len() {
            assert!(all.contains(&Entry::Binding(i)));
        }
        assert!(all.contains(&Entry::Heading("Mouse")));
        assert_eq!(
            all.iter().filter(|e| matches!(e, Entry::Mouse(_))).count(),
            MOUSE.len()
        );
    }

    #[test]
    fn filtering_narrows_and_drops_empty_sections() {
        let found: Vec<Entry> = sections("throttle").into_iter().flatten().collect();
        assert!(found.contains(&Entry::Heading("Network")));
        assert!(!found.contains(&Entry::Heading("App")));
        assert!(
            found
                .iter()
                .any(|e| matches!(e, Entry::Binding(i) if BINDINGS[*i].action == Action::Throttle))
        );
        assert!(sections("zzzz-nothing").is_empty());
        let mouse: Vec<Entry> = sections("wheel").into_iter().flatten().collect();
        assert!(mouse.contains(&Entry::Heading("Mouse")));
    }

    #[test]
    fn two_columns_are_balanced() {
        let cols = columns("", true);
        assert_eq!(cols.len(), 2);
        let (a, b) = (cols[0].len(), cols[1].len());
        assert!(a.abs_diff(b) < 20, "{a} vs {b}");
        assert_eq!(columns("", false).len(), 1);
    }

    #[test]
    fn renders_at_all_sizes_without_panicking() {
        for (w, h) in [
            (160, 50),
            (100, 30),
            (80, 24),
            (30, 10),
            (5, 3),
            (1, 1),
            (0, 0),
        ] {
            let mut state = ExplorerState::default();
            open_help(&mut state);
            let text = render(&mut state, w, h);
            if w >= 80 {
                assert!(text.contains("Help"), "{w}x{h}");
                assert!(text.contains("Navigate"), "{w}x{h}");
            }
        }
    }

    #[test]
    fn clicking_a_row_runs_its_action_and_closes() {
        let mut state = ExplorerState::default();
        open_help(&mut state);
        render(&mut state, 140, 50);
        let tree = BINDINGS
            .iter()
            .position(|b| b.action == Action::Tree)
            .unwrap();
        let rect = state
            .hits
            .rect_of(|h| {
                *h == Hit::OverlayItem {
                    overlay: 0,
                    item: tree,
                }
            })
            .expect("tree row visible");
        let before = state.settings.tree;
        click(&mut state, rect.x + 1, rect.y);
        assert!(state.overlays.is_empty());
        assert_ne!(state.settings.tree, before);
    }

    #[test]
    fn typing_filters_and_enter_runs_first_match() {
        let mut state = ExplorerState::default();
        open_help(&mut state);
        render(&mut state, 120, 40);
        type_text(&mut state, "uit");
        assert_eq!(help_state(&state).query, "uit");
        let text = render(&mut state, 120, 40);
        assert!(text.contains("Quit"));
        assert!(!text.contains("Throttle bandwidth"));
        key(&mut state, KeyCode::Enter);
        assert!(state.overlays.is_empty());
        assert!(
            state
                .take_requests()
                .contains(&crate::ui::explorer::UiRequest::Quit)
        );
    }

    #[test]
    fn escape_clears_query_then_closes_and_q_closes_when_empty() {
        let mut state = ExplorerState::default();
        open_help(&mut state);
        render(&mut state, 100, 30);
        key(&mut state, KeyCode::Char('x'));
        key(&mut state, KeyCode::Char('q'));
        assert_eq!(help_state(&state).query, "xq", "q types while filtering");
        key(&mut state, KeyCode::Escape);
        assert_eq!(help_state(&state).query, "");
        key(&mut state, KeyCode::Char('q'));
        assert!(state.overlays.is_empty());
        open_help(&mut state);
        key(&mut state, KeyCode::Char('?'));
        assert!(state.overlays.is_empty());
        open_help(&mut state);
        key(&mut state, KeyCode::F(1));
        assert!(state.overlays.is_empty());
    }

    #[test]
    fn arrows_select_and_scroll_and_wheel_scrolls() {
        let mut state = ExplorerState::default();
        open_help(&mut state);
        render(&mut state, 60, 14);
        for _ in 0..30 {
            key(&mut state, KeyCode::Down);
        }
        let h = help_state(&state);
        assert_eq!(h.selected, 30);
        assert!(h.scroll > 0);
        let text = render(&mut state, 60, 14);
        let label = BINDINGS[runnable(&columns("", false))[30].0].label;
        assert!(text.contains(label), "selected row is visible");
        key(&mut state, KeyCode::Home);
        assert_eq!(help_state(&state).scroll, 0);
        wheel(&mut state, 10, 7, false);
        assert_eq!(help_state(&state).scroll, 3);
        assert_eq!(help_state(&state).selected, 0, "wheel keeps the selection");
    }

    #[test]
    fn key_column_shows_only_real_keys() {
        for b in BINDINGS {
            let keys = help_keys(b.action);
            assert_ne!(keys, "menu", "{:?}", b.action);
            assert!(!keys.contains('⌘'), "{:?}", b.action);
        }
        assert_eq!(help_keys(Action::Terminate), "x ▸ 1");
        assert_eq!(help_keys(Action::Kill), "x ▸ 2");
        assert_eq!(help_keys(Action::Suspend), "x ▸ 6");
        assert_eq!(help_keys(Action::Resume), "x ▸ 7");
        assert_eq!(help_keys(Action::FullDetails), "Shift+Enter, Enter×2");
        assert_eq!(help_keys(Action::Tree), "F5 T");
        let mut state = ExplorerState::default();
        open_help(&mut state);
        let text = render(&mut state, 120, 40);
        assert!(!text.contains("menu  Terminate"), "{text}");
        assert!(text.contains("x ▸ 1"), "{text}");
        assert!(
            text.contains("Shift+Enter, Enter×2"),
            "not truncated: {text}"
        );
    }

    #[test]
    fn mouse_tips_list_only_reportable_gestures() {
        let gestures: Vec<&str> = MOUSE.iter().map(|(g, _)| *g).collect();
        assert!(gestures.contains(&"Ctrl/Alt-click"));
        assert!(gestures.contains(&"Middle-click"));
        assert!(gestures.iter().all(|g| !g.contains('⌘')));
    }

    #[test]
    fn inspector_section_lists_detail_panel_keys() {
        let all: Vec<Entry> = sections("").into_iter().flatten().collect();
        assert!(all.contains(&Entry::Heading("Inspector")));
        assert_eq!(
            all.iter()
                .filter(|e| matches!(e, Entry::Inspector(_)))
                .count(),
            INSPECTOR.len()
        );
        let found: Vec<Entry> = sections("inspector").into_iter().flatten().collect();
        assert!(found.contains(&Entry::Heading("Inspector")));
        let mut state = ExplorerState::default();
        open_help(&mut state);
        let text = render(&mut state, 160, 60);
        assert!(text.contains("Inspector"), "{text}");
        assert!(text.contains("Previous / next tab"), "{text}");
    }

    #[test]
    fn backdrop_click_closes() {
        let mut state = ExplorerState::default();
        open_help(&mut state);
        render(&mut state, 120, 40);
        click(&mut state, 0, 0);
        assert!(state.overlays.is_empty());
    }
}
