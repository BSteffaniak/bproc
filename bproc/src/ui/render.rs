//! Domain presentation composition. All text, list, modal and graph rasterization
//! is delegated to BMUX components; interaction remains in AppState/main.
use super::scene::{ComponentScene, Control, ControlKind};
use bmux_tui::{
    chrome::Border,
    component::{Component, Constraints, LayoutCx},
    composition::{Stack, Surface},
    geometry::{Insets, Rect},
    paint::{LocalRect, PaintCx},
    style::{Color, Style},
    text::{Line, Span},
    text_block::Alignment,
};
use bmux_tui_components::{
    canvas::{Canvas, CanvasBounds, CanvasLine, CanvasPolicy},
    modal_frame::{ModalFrame, ModalFrameComponent, ModalSizing, ModalTheme},
    scroll_view::ScrollViewState,
    selectable_list::{SelectableListItem, SelectableListState, SelectableListStyles},
    text_view::{TextViewComponent, TextViewPolicy, TextViewStyles},
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Compatibility state for the existing controller. Offsets count list items.
#[derive(Debug, Default, Clone)]
pub struct ListState {
    selected: Option<usize>,
    offset: usize,
}
impl ListState {
    pub fn select(&mut self, selected: Option<usize>) {
        self.selected = selected;
        if selected.is_none() {
            self.offset = 0;
        }
    }
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }
    pub fn offset(&self) -> usize {
        self.offset
    }
    pub fn offset_mut(&mut self) -> &mut usize {
        &mut self.offset
    }
}

pub trait IntoLine {
    fn into_line(self) -> Line;
}
impl IntoLine for &str {
    fn into_line(self) -> Line {
        Line::raw(self)
    }
}
impl IntoLine for String {
    fn into_line(self) -> Line {
        Line::raw(self)
    }
}
impl IntoLine for Span {
    fn into_line(self) -> Line {
        Line::from_spans(vec![self])
    }
}
impl IntoLine for Vec<Span> {
    fn into_line(self) -> Line {
        Line::from_spans(self)
    }
}
pub fn line(value: impl IntoLine) -> Line {
    value.into_line()
}
pub trait IntoLines {
    fn into_lines(self) -> Vec<Line>;
}
impl IntoLines for &str {
    fn into_lines(self) -> Vec<Line> {
        self.split('\n').map(Line::raw).collect()
    }
}
impl IntoLines for String {
    fn into_lines(self) -> Vec<Line> {
        self.as_str().into_lines()
    }
}
impl IntoLines for Line {
    fn into_lines(self) -> Vec<Line> {
        vec![self]
    }
}
impl IntoLines for Vec<Line> {
    fn into_lines(self) -> Vec<Line> {
        self
    }
}
pub fn lines(value: impl IntoLines) -> Vec<Line> {
    value.into_lines()
}

#[derive(Default)]
pub struct PanelChrome {
    pub title: Option<Line>,
    pub style: Style,
}
#[derive(Default)]
pub struct TextPresentation {
    pub style: Style,
    pub scroll: (u16, u16),
    pub alignment: Alignment,
    pub chrome: Option<PanelChrome>,
}

pub fn root_area(cx: &PaintCx<'_, '_>) -> Rect {
    let area = cx.area();
    Rect::new(
        u16::try_from(area.x).unwrap_or(0),
        u16::try_from(area.y).unwrap_or(0),
        area.width,
        area.height,
    )
}

pub fn split_vertical(area: Rect, top: u16, bottom: u16) -> [Rect; 3] {
    let top = top.min(area.height);
    let bottom = bottom.min(area.height.saturating_sub(top));
    let middle = area.height.saturating_sub(top).saturating_sub(bottom);
    [
        Rect::new(area.x, area.y, area.width, top),
        Rect::new(area.x, area.y.saturating_add(top), area.width, middle),
        Rect::new(
            area.x,
            area.y.saturating_add(top).saturating_add(middle),
            area.width,
            bottom,
        ),
    ]
}

fn paint_component(
    cx: &mut PaintCx<'_, '_>,
    area: Rect,
    component: impl Component,
) -> bmux_tui::component::LayoutNode {
    let layout = component.layout(Constraints::tight(area.size()), &mut LayoutCx::new());
    if !area.is_empty() {
        cx.with_child(
            i32::from(area.x),
            i64::from(area.y),
            LocalRect::new(0, 0, area.width, area.height),
            |cx| component.paint(&layout, cx),
        );
    }
    layout
}

pub fn clear(cx: &mut PaintCx<'_, '_>, area: Rect) {
    paint_component(
        cx,
        area,
        Surface::new(Stack::new()).background(Style::new().fg(Color::Default).bg(Color::Default)),
    );
}

pub fn paint_label(cx: &mut PaintCx<'_, '_>, area: Rect, label: Line, style: Style) {
    let block =
        bmux_tui::composition::TextBlock::new(bmux_tui::text::Text::from_lines(vec![label]))
            .style(style);
    paint_component(cx, area, block);
}

pub fn paint_border(cx: &mut PaintCx<'_, '_>, area: Rect, chrome: PanelChrome) {
    // Surface owns border rasterization; a text view owns the rich border title.
    paint_component(
        cx,
        area,
        Surface::new(Stack::new()).border(Border::single().style(chrome.style)),
    );
    if let Some(title) = chrome.title {
        let title_area = Rect::new(
            area.x.saturating_add(1),
            area.y,
            area.width
                .saturating_sub(2)
                .min(u16::try_from(title.width()).unwrap_or(u16::MAX)),
            u16::from(area.height > 0),
        );
        let title_scene = RefCell::new(ComponentScene::default());
        paint_text(
            &title_scene,
            "chrome.title",
            cx,
            title_area,
            (
                vec![title],
                TextPresentation {
                    style: chrome.style,
                    ..Default::default()
                },
            ),
        );
    }
}

pub fn paint_text(
    scene: &RefCell<ComponentScene>,
    id: &str,
    cx: &mut PaintCx<'_, '_>,
    area: Rect,
    (lines, presentation): (Vec<Line>, TextPresentation),
) {
    let state = scene
        .borrow_mut()
        .texts
        .entry(id.to_owned())
        .or_insert_with(|| Rc::new(Cell::new(ScrollViewState::default())))
        .clone();
    let mut scroll = state.get();
    scroll.set_vertical_offset(u64::from(presentation.scroll.0));
    scroll.set_horizontal_offset(u64::from(presentation.scroll.1));
    state.set(scroll);
    let mut policy = TextViewPolicy::bare();
    policy.alignment = presentation.alignment;
    policy.keyboard = matches!(id, "help" | "backend" | "interfaces" | "compatibility")
        || id.starts_with("detail.");
    policy.mouse_wheel = policy.keyboard;
    let view = TextViewComponent::new(id.to_owned(), &lines, &state)
        .policy(policy)
        .empty("")
        .styles(TextViewStyles {
            text: presentation.style,
            ..Default::default()
        });
    let (layout, text_area) = if let Some(chrome) = presentation.chrome {
        // ModalFrame's opaque panel and exact content inset preserve the old
        // one-cell border geometry, including rich titles and scrolling.
        let mut theme = ModalTheme::dark(Color::Cyan);
        theme.background = presentation.style;
        theme.border = presentation.style.patch(chrome.style);
        theme.title = presentation.style.patch(chrome.style);
        theme.text = presentation.style;
        let mut frame = ModalFrame::new(
            ModalSizing::new(area.size(), area.size(), Insets::all(0)),
            theme,
        )
        .border(Border::single().style(presentation.style.patch(chrome.style)))
        .padding(Insets::all(0));
        if let Some(title) = chrome.title {
            frame = frame.title(title);
        }
        let layout = paint_component(
            cx,
            area,
            ModalFrameComponent::new(format!("{id}.panel"), frame, view),
        );
        let text_layout = layout
            .find(&bmux_tui::component::LayoutId::new(id.to_owned()))
            .cloned()
            .expect("text layout retained in panel");
        let rect = layout
            .find_rect(&bmux_tui::component::LayoutId::new(id.to_owned()))
            .unwrap_or_default();
        (
            text_layout,
            Rect::new(
                area.x.saturating_add(rect.x),
                area.y.saturating_add(rect.y),
                rect.width,
                rect.height,
            ),
        )
    } else {
        (paint_component(cx, area, view), area)
    };
    scene.borrow_mut().staged.push(Control {
        id: id.to_owned(),
        area: text_area,
        layout,
        kind: ControlKind::Text {
            lines,
            state,
            policy,
        },
    });
}

pub fn paint_list(
    scene: &RefCell<ComponentScene>,
    id: &str,
    cx: &mut PaintCx<'_, '_>,
    area: Rect,
    items: &[SelectableListItem],
    highlight: Style,
    state: &mut ListState,
) {
    let visible = usize::from(area.height);
    if let Some(selected) = state.selected {
        if selected < state.offset {
            state.offset = selected;
        }
        if selected >= state.offset.saturating_add(visible) {
            state.offset = selected.saturating_add(1).saturating_sub(visible);
        }
    }
    state.offset = state.offset.min(items.len().saturating_sub(visible));
    let native = scene
        .borrow_mut()
        .lists
        .entry(id.to_owned())
        .or_insert_with(|| Rc::new(Cell::new(SelectableListState::new(None))))
        .clone();
    let mut bmux_state = native.get();
    bmux_state.set_selected(state.selected);

    bmux_state.scroll.set_vertical_offset(state.offset as u64);
    native.set(bmux_state);
    let styles = SelectableListStyles {
        selected: highlight,
        focused: highlight,
        hovered: highlight,
        ..Default::default()
    };
    let policy = super::scene::list_policy();

    // BMUX styles rich spans, while Ratatui highlighted the entire selected
    // row including its trailing blank cells. Supply an explicit measured tail
    // so component selection styling covers the same full-width row.
    let padded_items = items
        .iter()
        .cloned()
        .map(|mut item| {
            for line in &mut item.lines {
                let width = bmux_tui::text_width::display_width(&line.plain_text());
                let padding = usize::from(area.width).saturating_sub(width);
                if padding > 0 {
                    line.spans.push(Span::raw(" ".repeat(padding)));
                }
            }
            item
        })
        .collect::<Vec<_>>();
    let component = bmux_tui_components::selectable_list::SelectableListComponent::new(
        id.to_owned(),
        &padded_items,
        &native,
    )
    .styles(styles)
    .policy(policy);
    let layout = paint_component(cx, area, component);

    scene.borrow_mut().staged.push(Control {
        id: id.to_owned(),
        area,
        layout,
        kind: ControlKind::List {
            items: items.to_vec(),
            state: native,
        },
    });
}

pub fn paint_bandwidth_graph(
    cx: &mut PaintCx<'_, '_>,
    area: Rect,
    downloads: &[(f64, f64)],
    uploads: &[(f64, f64)],
    max: f64,
    title: String,
) {
    paint_border(
        cx,
        area,
        PanelChrome {
            title: Some(Line::raw(title)),
            style: Style::new().fg(Color::Cyan),
        },
    );
    // The original chart paints across the entire bordered interior, including
    // under its overlaid axis titles and legend. Keep exactly that viewport.
    let plot = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let mut segments = Vec::new();
    for (points, color) in [(downloads, Color::Green), (uploads, Color::Yellow)] {
        for pair in points.windows(2) {
            segments.push(
                CanvasLine::new(pair[0].0, pair[0].1, pair[1].0, pair[1].1)
                    .style(Style::new().fg(color)),
            );
        }
    }
    let canvas = Canvas::new(&[], CanvasBounds::new(0.0, 60.0, 0.0, max))
        .lines(&segments)
        .policy(CanvasPolicy::auto().prefer_braille());
    paint_component(cx, plot, canvas);
    let axis_style = Style::new().fg(Color::White);
    paint_label(
        cx,
        Rect::new(
            plot.x,
            plot.y,
            plot.width.min(19),
            u16::from(plot.height > 0),
        ),
        Line::raw("Bandwidth (bytes/s)"),
        axis_style,
    );
    let x_title = "Time (samples)";
    let title_width = (x_title.len() as u16).min(plot.width);
    paint_label(
        cx,
        Rect::new(
            plot.right().saturating_sub(title_width),
            plot.bottom().saturating_sub(1),
            title_width,
            u16::from(plot.height > 0),
        ),
        Line::raw(x_title),
        axis_style,
    );
    // Ratatui's two-series legend is a 10x4 single-border panel at top right.
    if plot.width >= 10 && plot.height >= 4 {
        let legend = Rect::new(plot.right().saturating_sub(10), plot.y, 10, 4);
        paint_border(cx, legend, PanelChrome::default());
        paint_label(
            cx,
            Rect::new(legend.x + 1, legend.y + 1, 8, 1),
            Line::raw("Download"),
            Style::new().fg(Color::Green),
        );
        paint_label(
            cx,
            Rect::new(legend.x + 1, legend.y + 2, 8, 1),
            Line::raw("Upload"),
            Style::new().fg(Color::Yellow),
        );
    }
}

/// Draw the detail selector through BMUX while keeping the controller's legacy
/// absolute tab hit ranges. Brackets are unstyled, as in the original header.
pub fn paint_detail_tabs(
    scene: &RefCell<ComponentScene>,
    id: &str,
    cx: &mut PaintCx<'_, '_>,
    area: Rect,
    selected: usize,
) {
    use bmux_tui_components::tab_bar::{
        TabBarComponent, TabBarPolicy, TabBarState, TabBarStyles, TabItem,
    };
    let names = ["Overview", "Connections", "Traffic", "System"];
    let items: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let style = if index == selected {
                Style::new()
                    .fg(Color::Yellow)
                    .bg(Color::BrightBlack)
                    .add_modifier(bmux_tui::style::Modifier::BOLD)
            } else {
                Style::new().fg(Color::White)
            };
            TabItem::rich(
                name,
                Line::from_spans(vec![
                    Span::raw("["),
                    Span::styled(*name, style),
                    Span::raw("]"),
                ]),
            )
        })
        .collect();
    let state = scene
        .borrow_mut()
        .tabs
        .entry(id.to_owned())
        .or_insert_with(|| Rc::new(RefCell::new(TabBarState::new(Some(selected)))))
        .clone();
    state.borrow_mut().set_selected(Some(selected));
    let styles = TabBarStyles {
        normal: Style::new(),
        selected: Style::new(),
        focused: Style::new(),
        separator: Style::new(),
        ..Default::default()
    };
    let layout = paint_component(
        cx,
        area,
        TabBarComponent::new(id.to_owned(), &items, &state)
            .policy(TabBarPolicy::interactive().padding("", ""))
            .styles(styles),
    );
    scene.borrow_mut().staged.push(Control {
        id: id.to_owned(),
        area,
        layout,
        kind: ControlKind::Tabs { state, items },
    });
}

pub fn paint_input(
    scene: &RefCell<ComponentScene>,
    id: &str,
    cx: &mut PaintCx<'_, '_>,
    area: Rect,
    text: &str,
    selected: bool,
    style: Style,
) {
    use bmux_tui_components::{
        text_input::{TextInputPolicy, TextInputState},
        text_input_box::{TextInputBoxComponent, TextInputBoxPolicy, TextInputBoxStyles},
    };
    let state = scene
        .borrow_mut()
        .inputs
        .entry(id.to_owned())
        .or_insert_with(|| {
            Rc::new(RefCell::new(TextInputState::new(
                bmux_text_edit::TextEditBuffer::from_text(text),
            )))
        })
        .clone();
    if state.borrow().buffer().text() != text {
        *state.borrow_mut() = TextInputState::new(bmux_text_edit::TextEditBuffer::from_text(text));
    }
    let mut policy = TextInputBoxPolicy::bare();
    policy.focused = selected;
    // The BMUX editor projects its cursor only for the active field.
    policy.cursor = selected;
    let style = style.bg(Color::Black);
    let styles = TextInputBoxStyles {
        text: style,
        focused_text: style,
        placeholder: style,
        background: Style::new().bg(Color::Black),
        focused_background: Style::new().bg(Color::Black),
        ..Default::default()
    };
    let layout = paint_component(
        cx,
        area,
        TextInputBoxComponent::new(id.to_owned(), TextInputPolicy::chat_composer(), &state)
            .policy(policy)
            .styles(styles)
            .placeholder("unlimited"),
    );
    scene.borrow_mut().staged.push(Control {
        id: id.to_owned(),
        area,
        layout,
        kind: ControlKind::Input { state },
    });
}
