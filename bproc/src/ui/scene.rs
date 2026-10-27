//! Retained component interaction state and successfully committed layouts.
use bmux_tui::{
    component::{Component, EventCx, LayoutNode},
    event::{Event, EventOutcome},
    geometry::Rect,
    interaction::InteractionRoute,
    text::Line,
};
use bmux_tui_components::{
    scroll_view::ScrollViewState,
    selectable_list::{
        SelectableListHighlightPolicy, SelectableListItem, SelectableListPolicy,
        SelectableListState,
    },
    tab_bar::{TabBarComponent, TabBarPolicy, TabBarState, TabItem},
    text_input::{TextInputPolicy, TextInputState},
    text_input_box::{TextInputBoxComponent, TextInputBoxPolicy},
    text_view::{TextViewComponent, TextViewPolicy},
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

#[derive(Default)]
pub struct ComponentScene {
    pub texts: HashMap<String, Rc<Cell<ScrollViewState>>>,
    pub inputs: HashMap<String, Rc<RefCell<TextInputState>>>,
    pub tabs: HashMap<String, Rc<RefCell<TabBarState>>>,
    pub lists: HashMap<String, Rc<Cell<SelectableListState>>>,
    pub staged: Vec<Control>,
    pub committed: Vec<Control>,
}
#[derive(Clone)]
pub struct Control {
    pub id: String,
    pub area: Rect,
    pub layout: LayoutNode,
    pub kind: ControlKind,
}
#[derive(Clone)]
pub enum ControlKind {
    Text {
        lines: Vec<Line>,
        state: Rc<Cell<ScrollViewState>>,
        policy: TextViewPolicy,
    },
    Input {
        state: Rc<RefCell<TextInputState>>,
    },
    Tabs {
        state: Rc<RefCell<TabBarState>>,
        items: Vec<TabItem<'static>>,
    },
    List {
        items: Vec<SelectableListItem>,
        state: Rc<Cell<SelectableListState>>,
    },
}
pub enum Change {
    Scroll(String, usize),
    Input(String, String),
    Tab(usize),
    List(String, Option<usize>, usize),
    Consumed,
}
impl ComponentScene {
    pub fn begin(&mut self) {
        self.staged.clear();
    }
    pub fn commit(&mut self) {
        self.committed = std::mem::take(&mut self.staged);
    }
    pub fn event(
        &mut self,
        route: &InteractionRoute,
        active: &str,
        input_id: &str,
    ) -> Option<Change> {
        if route.traversal_consumed {
            return Some(Change::Consumed);
        }
        let target = route.target.as_ref().map(|id| id.as_str());
        let pointer = matches!(route.event, Event::Mouse(_));
        let control = self
            .committed
            .iter()
            .rev()
            .find(|c| {
                let targeted = target.is_some_and(|target| {
                    target == c.id || target.starts_with(&format!("{}.", c.id))
                });
                if pointer {
                    targeted
                        && (c.id == active
                            || (active == "throttle" && c.id.starts_with("throttle."))
                            || (active.starts_with("detail.") && c.id == "detail.tabs"))
                } else {
                    match &c.kind {
                        ControlKind::Input { .. } => {
                            active == "throttle"
                                && if target.is_some() {
                                    targeted
                                } else {
                                    c.id == input_id
                                }
                        }
                        ControlKind::Text { policy, .. } => {
                            policy.keyboard && c.id == active && targeted
                        }
                        ControlKind::Tabs { .. } => targeted && active.starts_with("detail."),
                        ControlKind::List { .. } => c.id == active && targeted,
                    }
                }
            })?
            .clone();
        // The authoritative layout is staged during painting, promoted only by
        // TerminalPresenter's successful commit callback, never by event handling.
        let mut cx = EventCx::with_clip(&control.layout, control.area);
        match control.kind {
            ControlKind::Text {
                lines,
                state,
                policy,
            } => {
                let view =
                    TextViewComponent::new(control.id.clone(), &lines, &state).policy(policy);
                let before = state.get();
                let _ = view.handle_event(control.area, &control.layout, &route.event);
                let after = state.get();
                (before != after)
                    .then(|| Change::Scroll(control.id, after.vertical_offset() as usize))
            }
            ControlKind::Input { state, .. } => {
                let selected = true;
                use bmux_keyboard::KeyCode;
                if let Event::Paste(value) = &route.event
                    && !value.chars().all(|c| c.is_ascii_digit())
                {
                    return Some(Change::Consumed);
                }
                if let Event::Key(key) = &route.event {
                    if matches!(key.key, KeyCode::Space) {
                        return Some(Change::Consumed);
                    }
                    if let KeyCode::Char(ch) = key.key
                        && !ch.is_ascii_digit()
                        && !matches!(ch, 'q' | 't')
                        && !key.modifiers.ctrl
                    {
                        return Some(Change::Consumed);
                    }
                }
                if let Event::Key(key) = &route.event {
                    // Application actions remain semantic aliases, not text edits.
                    if matches!(
                        key.key,
                        KeyCode::Escape
                            | KeyCode::Enter
                            | KeyCode::Tab
                            | KeyCode::Char('t')
                            | KeyCode::Char('q')
                    ) {
                        return None;
                    }
                }
                let mut policy = TextInputBoxPolicy::bare();
                policy.focused = selected;
                policy.cursor = selected;
                let view = TextInputBoxComponent::new(
                    control.id.clone(),
                    TextInputPolicy::chat_composer(),
                    &state,
                )
                .policy(policy);
                let outcome = cx.with_transform(
                    0,
                    0,
                    i32::from(control.area.x),
                    i64::from(control.area.y),
                    control.area,
                    |cx| view.event(&route.event, &control.layout, cx),
                );
                let value = state.borrow().buffer().text().to_owned();
                if !matches!(outcome, EventOutcome::Ignored) {
                    Some(Change::Input(control.id, value))
                } else {
                    None
                }
            }
            ControlKind::Tabs { state, items } => {
                let view = TabBarComponent::new(control.id, &items, &state)
                    .policy(TabBarPolicy::interactive().padding("", ""));
                let before = state.borrow().selected();
                let outcome = cx.with_transform(
                    0,
                    0,
                    i32::from(control.area.x),
                    i64::from(control.area.y),
                    control.area,
                    |cx| view.event(&route.event, &control.layout, cx),
                );
                let after = state.borrow().selected();
                if before != after {
                    after.map(Change::Tab)
                } else if !matches!(outcome, EventOutcome::Ignored) {
                    Some(Change::Consumed)
                } else {
                    None
                }
            }
            ControlKind::List { items, state } => {
                use bmux_keyboard::{KeyCode, KeyStroke};
                if let Event::Key(key) = &route.event
                    && matches!(
                        key.key,
                        KeyCode::Enter | KeyCode::Space | KeyCode::Char(' ')
                    )
                {
                    return None;
                }
                let policy = list_policy();
                let mut initial = state.get();
                if initial.selected().is_none()
                    && !items.is_empty()
                    && matches!(&route.event, Event::Key(key) if matches!(key.key, KeyCode::Down | KeyCode::Up))
                {
                    initial.set_selected(Some(0));
                    initial.set_focused(Some(0));
                    state.set(initial);
                    return Some(Change::List(
                        control.id,
                        Some(0),
                        initial.vertical_scroll() as usize,
                    ));
                }
                initial.set_focused(initial.selected());
                state.set(initial);
                let (event, repeats) = match &route.event {
                    Event::Key(key) => match key.key {
                        KeyCode::PageDown => (Event::Key(KeyStroke::simple(KeyCode::Down)), 10),
                        KeyCode::PageUp => (Event::Key(KeyStroke::simple(KeyCode::Up)), 10),
                        _ => (route.event.clone(), 1),
                    },
                    _ => (route.event.clone(), 1),
                };
                let view = bmux_tui_components::selectable_list::SelectableListComponent::new(
                    control.id.clone(),
                    &items,
                    &state,
                )
                .policy(policy);
                let before = state.get();
                for _ in 0..repeats {
                    let _ = cx.with_transform(
                        0,
                        0,
                        i32::from(control.area.x),
                        i64::from(control.area.y),
                        control.area,
                        |cx| view.handle_event(&event, &control.layout, cx),
                    );
                }
                let mut after = state.get();
                after.set_selected(after.focused());
                state.set(after);
                (after != before).then(|| {
                    Change::List(
                        control.id,
                        after.selected(),
                        after.vertical_scroll() as usize,
                    )
                })
            }
        }
    }
}
pub fn list_policy() -> SelectableListPolicy {
    let mut policy = SelectableListPolicy {
        highlight: SelectableListHighlightPolicy::new("", false),
        ..Default::default()
    };
    policy.keyboard.wrap = true;
    policy.keyboard.enter_selects = false;
    policy.keyboard.space_selects = false;
    policy.mouse.enabled = false; // process list wheel intentionally disabled
    policy
}
