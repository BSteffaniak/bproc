//! Settings overlay (F2): refresh rate, meters, dock, behaviour toggles and
//! per-preset columns. Changes apply live and persist via `SaveSettings`.
use super::hits::{Hit, contains};
use super::overlays::{self, OverlayOutcome};
use super::paint::{Frame, centered, fit, theme, truncate};
use super::screens::input::{is_space, typed_char};
use super::{ExplorerState, UiRequest};
use crate::config::{DockPosition, ExplorerSettings};
use crate::procs::columns::Preset;
use bmux_keyboard::KeyCode;
use bmux_tui::event::{Event, MouseButton, MouseEventKind};
use bmux_tui::geometry::Rect;
use bmux_tui::style::Style;
use bmux_tui::text::{Line, Span};
use bmux_tui::text_width::display_width;
use std::time::Duration;

/// Refresh interval choices in milliseconds.
pub const REFRESH_STEPS: [u64; 9] = [250, 500, 750, 1000, 1500, 2000, 3000, 5000, 10_000];
pub const ESCALATE_RANGE: (u64, u64) = (1, 60);
pub const DOCK_SIZE_RANGE: (u16, u16) = (6, 40);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Refresh,
    ShowMeters,
    PerCoreMeters,
    HeatColors,
    Tree,
    HoverFreeze,
    ConfirmSignals,
    EscalateAfter,
    HideKernelThreads,
    OnlyMine,
    NetActiveOnly,
    Dock,
    DockSize,
    Preset,
    Columns,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Toggle,
    Stepper,
    Choice,
    Action,
}

impl Item {
    pub const ALL: [Item; 16] = [
        Self::Refresh,
        Self::ShowMeters,
        Self::PerCoreMeters,
        Self::HeatColors,
        Self::Tree,
        Self::HoverFreeze,
        Self::ConfirmSignals,
        Self::EscalateAfter,
        Self::HideKernelThreads,
        Self::OnlyMine,
        Self::NetActiveOnly,
        Self::Dock,
        Self::DockSize,
        Self::Preset,
        Self::Columns,
        Self::Reset,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|i| *i == self).unwrap_or(0)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Refresh => "Refresh interval",
            Self::ShowMeters => "Show meters",
            Self::PerCoreMeters => "Per-core CPU meters",
            Self::HeatColors => "Heat colours",
            Self::Tree => "Tree view",
            Self::HoverFreeze => "Hover freeze",
            Self::ConfirmSignals => "Confirm terminate / kill",
            Self::EscalateAfter => "Kill escalation delay",
            Self::HideKernelThreads => "Hide kernel threads",
            Self::OnlyMine => "Only my processes",
            Self::NetActiveOnly => "Network-active only",
            Self::Dock => "Dock position",
            Self::DockSize => "Dock size",
            Self::Preset => "Default preset",
            Self::Columns => "Choose columns for current preset…",
            Self::Reset => "Reset all to defaults",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Refresh => "How often process data is sampled. Faster costs more CPU.",
            Self::ShowMeters => "CPU, memory, swap, load and network meters above the table.",
            Self::PerCoreMeters => "One bar per CPU core instead of a single total.",
            Self::HeatColors => "Colour CPU / memory values by how hot they are.",
            Self::Tree => "Show processes nested under their parents.",
            Self::HoverFreeze => "Pause re-sorting while the pointer rests on the table.",
            Self::ConfirmSignals => "Ask before sending SIGTERM or SIGKILL.",
            Self::EscalateAfter => "Offer a force kill if a terminated process survives this long.",
            Self::HideKernelThreads => "Hide Linux kernel threads (kthreadd children).",
            Self::OnlyMine => "Show only processes owned by you.",
            Self::NetActiveOnly => "Show only processes with network traffic.",
            Self::Dock => "Where the details panel docks: below or beside the table.",
            Self::DockSize => "Height (bottom dock) or width (right dock) of the details panel.",
            Self::Preset => "Column preset shown at startup and selected now.",
            Self::Columns => "Pick and reorder the columns of the current preset.",
            Self::Reset => "Restore every setting to its default. Press twice to confirm.",
        }
    }

    pub const fn kind(self) -> Kind {
        match self {
            Self::Refresh | Self::EscalateAfter | Self::DockSize => Kind::Stepper,
            Self::Dock | Self::Preset => Kind::Choice,
            Self::Columns | Self::Reset => Kind::Action,
            _ => Kind::Toggle,
        }
    }

    /// Section heading shown above this item, if it starts a section.
    const fn section(self) -> Option<&'static str> {
        match self {
            Self::Refresh => Some("Updates"),
            Self::ShowMeters => Some("Display"),
            Self::HoverFreeze => Some("Behaviour"),
            Self::HideKernelThreads => Some("Filters"),
            Self::Dock => Some("Layout"),
            Self::Columns => Some("More"),
            _ => None,
        }
    }

    fn is_on(self, s: &ExplorerSettings) -> bool {
        match self {
            Self::ShowMeters => s.show_meters,
            Self::PerCoreMeters => s.per_core_meters,
            Self::HeatColors => s.heat_colors,
            Self::Tree => s.tree,
            Self::HoverFreeze => s.hover_freeze,
            Self::ConfirmSignals => s.confirm_signals,
            Self::HideKernelThreads => s.hide_kernel_threads,
            Self::OnlyMine => s.only_mine,
            Self::NetActiveOnly => s.net_active_only,
            _ => false,
        }
    }

    fn flag(self, s: &mut ExplorerSettings) -> Option<&mut bool> {
        Some(match self {
            Self::ShowMeters => &mut s.show_meters,
            Self::PerCoreMeters => &mut s.per_core_meters,
            Self::HeatColors => &mut s.heat_colors,
            Self::Tree => &mut s.tree,
            Self::HoverFreeze => &mut s.hover_freeze,
            Self::ConfirmSignals => &mut s.confirm_signals,
            Self::HideKernelThreads => &mut s.hide_kernel_threads,
            Self::OnlyMine => &mut s.only_mine,
            Self::NetActiveOnly => &mut s.net_active_only,
            _ => return None,
        })
    }
}

pub fn format_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Next refresh step above (`delta > 0`) or below the current value.
pub fn step_refresh(current: u64, delta: i32) -> u64 {
    if delta > 0 {
        REFRESH_STEPS
            .iter()
            .copied()
            .find(|s| *s > current)
            .unwrap_or(REFRESH_STEPS[REFRESH_STEPS.len() - 1])
    } else {
        REFRESH_STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s < current)
            .unwrap_or(REFRESH_STEPS[0])
    }
}

/// Current value text for `item`.
pub fn value_text(item: Item, s: &ExplorerSettings, armed_reset: bool) -> String {
    match item {
        Item::Refresh => format_ms(s.refresh_ms),
        Item::EscalateAfter => format!("{}s", s.escalate_after_secs),
        Item::Dock => match s.dock {
            DockPosition::Bottom => "Bottom".into(),
            DockPosition::Right => "Right".into(),
        },
        Item::DockSize => match s.dock {
            DockPosition::Bottom => format!("{} rows", s.dock_size),
            DockPosition::Right => format!("{} cols", s.dock_size.saturating_mul(4)),
        },
        Item::Preset => s.preset.label().into(),
        Item::Columns => format!("{} columns", s.columns_for(s.preset).len()),
        Item::Reset if armed_reset => "Press again".into(),
        Item::Reset => String::new(),
        _ if item.is_on(s) => "On".into(),
        _ => "Off".into(),
    }
}

#[derive(Debug)]
pub struct SettingsState {
    pub selected: usize,
    pub scroll: usize,
    /// "Reset all to defaults" was pressed once and awaits confirmation.
    pub confirm_reset: bool,
}

impl SettingsState {
    pub fn new(settings: &ExplorerSettings) -> Self {
        let _ = settings;
        Self {
            selected: 0,
            scroll: 0,
            confirm_reset: false,
        }
    }

    pub fn item(&self) -> Item {
        Item::ALL[self.selected.min(Item::ALL.len() - 1)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Heading(&'static str),
    Item(Item),
}

fn rows() -> Vec<RowKind> {
    let mut out = Vec::new();
    for item in Item::ALL {
        if let Some(title) = item.section() {
            out.push(RowKind::Heading(title));
        }
        out.push(RowKind::Item(item));
    }
    out
}

fn row_of(item: Item) -> usize {
    rows()
        .iter()
        .position(|r| *r == RowKind::Item(item))
        .unwrap_or(0)
}

struct SettingsLayout {
    rect: Rect,
    list: Rect,
    footer: Rect,
}

fn layout(area: Rect) -> SettingsLayout {
    let rows = rows().len() as u16;
    let rect = centered(area, 74, rows + 5);
    let inner = if rect.width >= 2 && rect.height >= 2 {
        Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2)
    } else {
        Rect::new(rect.x, rect.y, 0, 0)
    };
    let footer_h = if inner.height >= 8 { 3 } else { 0 };
    let list = Rect::new(inner.x, inner.y, inner.width, inner.height - footer_h);
    let footer = Rect::new(inner.x, list.bottom(), inner.width, footer_h);
    SettingsLayout { rect, list, footer }
}

/// Scroll that keeps the selected item (and its heading) visible.
fn effective_scroll(scroll: usize, item: Item, page: usize) -> usize {
    let total = rows().len();
    let row = row_of(item);
    let top = if item.section().is_some() {
        row - 1
    } else {
        row
    };
    let page = page.max(1);
    let mut scroll = scroll.min(total.saturating_sub(page));
    if top < scroll {
        scroll = top;
    } else if row >= scroll + page {
        scroll = row + 1 - page;
    }
    scroll
}

/// `OverlayControl` encoding: item index × 4 + part.
const PART_DEC: u16 = 1;
const PART_INC: u16 = 2;

fn control(item: Item, part: u16) -> u16 {
    item.index() as u16 * 4 + part
}

const CONTROL_W: u16 = 18;

pub fn draw(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    index: usize,
    settings: &SettingsState,
    state: &ExplorerState,
) {
    frame.hit(area, Hit::OverlayBackdrop { overlay: index });
    let l = layout(area);
    if l.rect.width == 0 || l.rect.height == 0 {
        return;
    }
    let bg = theme::panel_bg();
    frame.panel(l.rect, Some("Settings"), theme::panel_border(), bg);
    frame.hit(l.rect, Hit::OverlayBody { overlay: index });
    if l.list.width == 0 || l.list.height == 0 {
        return;
    }
    let page = l.list.height as usize;
    let scroll = effective_scroll(settings.scroll, settings.item(), page);
    let all = rows();
    for (i, row) in all.iter().enumerate().skip(scroll).take(page) {
        let r = Rect::new(l.list.x, l.list.y + (i - scroll) as u16, l.list.width, 1);
        match row {
            RowKind::Heading(title) => frame.line_bg(
                r,
                &Line::from_spans(vec![Span::styled(
                    format!(" {title}"),
                    bg.patch(theme::accent()).patch(theme::bold()),
                )]),
                bg,
            ),
            RowKind::Item(item) => draw_item(frame, r, index, *item, settings, state),
        }
    }
    if scroll > 0 {
        frame.text(
            Rect::new(l.list.right(), l.list.y, 1, 1),
            "▲",
            bg.patch(theme::panel_border()),
        );
    }
    if scroll + page < all.len() {
        frame.text(
            Rect::new(l.list.right(), l.list.bottom() - 1, 1, 1),
            "▼",
            bg.patch(theme::panel_border()),
        );
    }
    if l.footer.height >= 3 {
        let hovered = state.pointer.position.and_then(|p| match state.hits.at(p) {
            Some(Hit::OverlayItem { overlay, item }) if *overlay == index => {
                Item::ALL.get(*item).copied()
            }
            _ => None,
        });
        let item = hovered.unwrap_or(settings.item());
        let w = l.footer.width.saturating_sub(2);
        for x in l.footer.x..l.footer.right() {
            frame.cell(x, l.footer.y, "─", bg.patch(theme::dim()));
        }
        frame.text(
            Rect::new(l.footer.x + 1, l.footer.y + 1, w, 1),
            &truncate(item.description(), w as usize),
            bg,
        );
        frame.text(
            Rect::new(l.footer.x + 1, l.footer.y + 2, w, 1),
            &truncate(
                "↑↓ select · ←→ adjust · Space toggle · Enter open · Esc close",
                w as usize,
            ),
            bg.patch(theme::dim()),
        );
    }
}

fn draw_item(
    frame: &mut Frame<'_, '_, '_>,
    r: Rect,
    index: usize,
    item: Item,
    settings: &SettingsState,
    state: &ExplorerState,
) {
    let bg = theme::panel_bg();
    let selected = settings.item() == item;
    let hovered = state.pointer.position.is_some_and(|p| contains(r, p));
    let row = if selected {
        theme::selected()
    } else if hovered {
        bg.patch(theme::hovered())
    } else {
        bg
    };
    let keep = |s: Style| if selected { row } else { row.patch(s) };
    let armed = item == Item::Reset && settings.confirm_reset;
    let control_w = CONTROL_W.min(r.width / 2);
    let label_w = r.width.saturating_sub(control_w + 1) as usize;
    let label_style = match item {
        Item::Reset if armed => theme::button_danger(),
        Item::Reset => keep(theme::error()),
        _ => row,
    };
    frame.line_bg(
        r,
        &Line::from_spans(vec![Span::styled(
            fit(&format!("  {}", item.label()), label_w, false),
            label_style,
        )]),
        row,
    );
    frame.hit(
        r,
        Hit::OverlayItem {
            overlay: index,
            item: item.index(),
        },
    );
    if control_w < 5 {
        return;
    }
    let cx = r.right() - control_w - 1;
    let value = value_text(item, &state.settings, armed);
    match item.kind() {
        Kind::Toggle => {
            let on = value == "On";
            let text = if on { "[✓] On" } else { "[ ] Off" };
            let style = if on {
                keep(theme::ok().patch(theme::bold()))
            } else {
                keep(theme::dim())
            };
            frame.text(
                Rect::new(cx, r.y, control_w, 1),
                &fit(text, control_w as usize, true),
                style,
            );
        }
        Kind::Stepper | Kind::Choice => {
            let (dec, inc) = if item.kind() == Kind::Stepper {
                ("−", "+")
            } else {
                ("‹", "›")
            };
            let inner_w = control_w.saturating_sub(4) as usize;
            let arrow = if selected {
                row.patch(theme::bold())
            } else {
                row.patch(theme::accent()).patch(theme::bold())
            };
            frame.text(Rect::new(cx, r.y, 1, 1), dec, arrow);
            frame.text(
                Rect::new(cx + 2, r.y, inner_w as u16, 1),
                &centered_text(&value, inner_w),
                keep(theme::bold()),
            );
            frame.text(Rect::new(cx + control_w - 1, r.y, 1, 1), inc, arrow);
            frame.hit(
                Rect::new(cx, r.y, 2, 1),
                Hit::OverlayControl {
                    overlay: index,
                    control: control(item, PART_DEC),
                },
            );
            frame.hit(
                Rect::new(cx + control_w - 2, r.y, 2, 1),
                Hit::OverlayControl {
                    overlay: index,
                    control: control(item, PART_INC),
                },
            );
        }
        Kind::Action => {
            let text = match item {
                Item::Columns => format!("{value} ›"),
                _ if armed => "Press again ›".into(),
                _ => "Reset ›".into(),
            };
            frame.text(
                Rect::new(cx, r.y, control_w, 1),
                &fit(&text, control_w as usize, true),
                keep(theme::dim()),
            );
        }
    }
}

fn centered_text(text: &str, width: usize) -> String {
    let t = truncate(text, width);
    let pad = width.saturating_sub(display_width(&t));
    format!("{}{}{}", " ".repeat(pad / 2), t, " ".repeat(pad - pad / 2))
}

/// Request a settings save unless one is already queued last.
fn save(state: &mut ExplorerState) {
    if state.outbox.last() != Some(&UiRequest::SaveSettings) {
        state.request(UiRequest::SaveSettings);
    }
}

/// Adjust `item` by `delta` (toggle flips; steppers and choices step).
pub fn adjust(state: &mut ExplorerState, item: Item, delta: i32) {
    let s = &mut state.settings;
    match item {
        Item::Refresh => {
            let next = step_refresh(s.refresh_ms, delta);
            if next == s.refresh_ms {
                return;
            }
            s.refresh_ms = next;
            state.request(UiRequest::SetRefresh(Duration::from_millis(next)));
        }
        Item::EscalateAfter => {
            let (lo, hi) = ESCALATE_RANGE;
            let next = s
                .escalate_after_secs
                .saturating_add_signed(i64::from(delta))
                .clamp(lo, hi);
            if next == s.escalate_after_secs {
                return;
            }
            s.escalate_after_secs = next;
        }
        Item::DockSize => {
            let (lo, hi) = DOCK_SIZE_RANGE;
            let next = s
                .dock_size
                .saturating_add_signed(delta.clamp(-100, 100) as i16)
                .clamp(lo, hi);
            if next == s.dock_size {
                return;
            }
            s.dock_size = next;
        }
        Item::Dock => {
            s.dock = match s.dock {
                DockPosition::Bottom => DockPosition::Right,
                DockPosition::Right => DockPosition::Bottom,
            };
        }
        Item::Preset => {
            let n = Preset::ALL.len() as i32;
            let i = Preset::ALL.iter().position(|p| *p == s.preset).unwrap_or(0) as i32;
            let next = Preset::ALL[(i + delta.signum()).rem_euclid(n) as usize];
            state.set_preset(next);
        }
        Item::Columns | Item::Reset => return,
        flag => {
            if let Some(value) = flag.flag(s) {
                *value = !*value;
            }
        }
    }
    state.table.mark_dirty();
    save(state);
}

/// Space / Enter / click on the row body.
fn activate(settings: &mut SettingsState, state: &mut ExplorerState, item: Item) {
    match item {
        Item::Columns => {
            let chooser = overlays::column_chooser(state);
            state.overlays.push(chooser);
        }
        Item::Reset if settings.confirm_reset => {
            settings.confirm_reset = false;
            reset(state);
        }
        Item::Reset => settings.confirm_reset = true,
        _ => match item.kind() {
            Kind::Toggle | Kind::Choice => adjust(state, item, 1),
            _ => {}
        },
    }
}

/// Restore defaults and apply them live.
pub fn reset(state: &mut ExplorerState) {
    let previous_refresh = state.settings.refresh_ms;
    state.settings = ExplorerSettings::default();
    state.table.on_preset_changed(&state.settings);
    state.table.mark_dirty();
    if state.settings.refresh_ms != previous_refresh {
        state.request(UiRequest::SetRefresh(Duration::from_millis(
            state.settings.refresh_ms,
        )));
    }
    save(state);
    state.flash("Settings reset to defaults");
}

pub fn handle(
    settings: &mut SettingsState,
    index: usize,
    event: &Event,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    let page = layout(state.last_area).list.height as usize;
    let last = Item::ALL.len() - 1;
    match event {
        Event::Key(key) => {
            let item = settings.item();
            let activating = key.key == KeyCode::Enter || is_space(key);
            if !(activating && item == Item::Reset) {
                settings.confirm_reset = false;
            }
            match key.key {
                KeyCode::Escape | KeyCode::F(2) => return OverlayOutcome::Close,
                KeyCode::Up => settings.selected = settings.selected.saturating_sub(1),
                KeyCode::Down => settings.selected = (settings.selected + 1).min(last),
                KeyCode::PageUp => {
                    settings.selected = settings.selected.saturating_sub(page.max(1))
                }
                KeyCode::PageDown => {
                    settings.selected = (settings.selected + page.max(1)).min(last)
                }
                KeyCode::Home => settings.selected = 0,
                KeyCode::End => settings.selected = last,
                KeyCode::Left => adjust(state, item, -1),
                KeyCode::Right => adjust(state, item, 1),
                KeyCode::Enter => activate(settings, state, item),
                _ if is_space(key) => activate(settings, state, item),
                _ => match typed_char(key) {
                    Some('q') => return OverlayOutcome::Close,
                    Some('k') => settings.selected = settings.selected.saturating_sub(1),
                    Some('j') => settings.selected = (settings.selected + 1).min(last),
                    Some('-') => adjust(state, item, -1),
                    Some('+' | '=') => adjust(state, item, 1),
                    _ => {}
                },
            }
            settings.scroll = effective_scroll(settings.scroll, settings.item(), page);
            OverlayOutcome::Consumed
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let total = rows().len();
                let up = mouse.kind == MouseEventKind::ScrollUp;
                let scroll = if up {
                    settings.scroll.saturating_sub(3)
                } else {
                    (settings.scroll + 3).min(total.saturating_sub(page.max(1)))
                };
                // Keep the selection on screen so the scroll sticks.
                let visible: Vec<usize> = rows()
                    .iter()
                    .enumerate()
                    .skip(scroll)
                    .take(page.max(1))
                    .filter_map(|(_, r)| match r {
                        RowKind::Item(i) => Some(i.index()),
                        RowKind::Heading(_) => None,
                    })
                    .collect();
                if let (Some(first), Some(last)) = (visible.first(), visible.last()) {
                    settings.selected = settings.selected.clamp(*first, *last);
                }
                settings.scroll = scroll;
                OverlayOutcome::Consumed
            }
            MouseEventKind::Down(MouseButton::Left) => {
                match state.hits.at(mouse.position).cloned() {
                    Some(Hit::OverlayControl { overlay, control }) if overlay == index => {
                        let Some(item) = Item::ALL.get(usize::from(control / 4)).copied() else {
                            return OverlayOutcome::Consumed;
                        };
                        settings.selected = item.index();
                        settings.confirm_reset = false;
                        match control % 4 {
                            PART_DEC => adjust(state, item, -1),
                            PART_INC => adjust(state, item, 1),
                            _ => {}
                        }
                        OverlayOutcome::Consumed
                    }
                    Some(Hit::OverlayItem { overlay, item }) if overlay == index => {
                        let Some(item) = Item::ALL.get(item).copied() else {
                            return OverlayOutcome::Consumed;
                        };
                        if item != Item::Reset {
                            settings.confirm_reset = false;
                        }
                        settings.selected = item.index();
                        activate(settings, state, item);
                        OverlayOutcome::Consumed
                    }
                    Some(Hit::OverlayBackdrop { overlay }) if overlay == index => {
                        OverlayOutcome::Close
                    }
                    _ => OverlayOutcome::Consumed,
                }
            }
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

    fn open(state: &mut ExplorerState) {
        let s = SettingsState::new(&state.settings);
        state.overlays.push(Overlay::Settings(s));
    }

    fn current(state: &ExplorerState) -> &SettingsState {
        match state.overlays.last() {
            Some(Overlay::Settings(s)) => s,
            _ => panic!("settings not open"),
        }
    }

    fn select(state: &mut ExplorerState, item: Item) {
        key(state, KeyCode::Home);
        for _ in 0..item.index() {
            key(state, KeyCode::Down);
        }
        assert_eq!(current(state).item(), item);
    }

    #[test]
    fn refresh_steps_clamp_at_both_ends() {
        assert_eq!(step_refresh(1500, 1), 2000);
        assert_eq!(step_refresh(1500, -1), 1000);
        assert_eq!(step_refresh(1200, 1), 1500, "off-grid values snap");
        assert_eq!(step_refresh(1200, -1), 1000);
        assert_eq!(step_refresh(10_000, 1), 10_000);
        assert_eq!(step_refresh(250, -1), 250);
        assert_eq!(format_ms(250), "250ms");
        assert_eq!(format_ms(1500), "1.5s");
        assert_eq!(format_ms(10_000), "10s");
    }

    #[test]
    fn renders_at_all_sizes() {
        for (w, h) in [(160, 50), (80, 24), (40, 12), (10, 5), (1, 1), (0, 0)] {
            let mut state = ExplorerState::default();
            open(&mut state);
            let text = render(&mut state, w, h);
            if w >= 80 && h >= 24 {
                assert!(text.contains("Settings"));
                assert!(text.contains("Refresh interval"));
                assert!(text.contains("1.5s"));
            }
        }
    }

    #[test]
    fn keyboard_adjusts_refresh_and_queues_requests() {
        let mut state = ExplorerState::default();
        open(&mut state);
        render(&mut state, 100, 40);
        key(&mut state, KeyCode::Right);
        assert_eq!(state.settings.refresh_ms, 2000);
        let requests = state.take_requests();
        assert!(requests.contains(&UiRequest::SetRefresh(Duration::from_millis(2000))));
        assert!(requests.contains(&UiRequest::SaveSettings));
        key(&mut state, KeyCode::Left);
        key(&mut state, KeyCode::Left);
        assert_eq!(state.settings.refresh_ms, 1000);
    }

    #[test]
    fn toggles_flip_with_space_enter_and_click() {
        let mut state = ExplorerState::default();
        open(&mut state);
        render(&mut state, 100, 40);
        select(&mut state, Item::OnlyMine);
        key(&mut state, KeyCode::Char(' '));
        assert!(state.settings.only_mine);
        assert_eq!(state.take_requests(), vec![UiRequest::SaveSettings]);
        key(&mut state, KeyCode::Enter);
        assert!(!state.settings.only_mine);
        render(&mut state, 100, 40);
        let rect = state
            .hits
            .rect_of(|h| {
                *h == Hit::OverlayItem {
                    overlay: 0,
                    item: Item::Tree.index(),
                }
            })
            .unwrap();
        click(&mut state, rect.x + 3, rect.y);
        assert!(state.settings.tree);
        assert_eq!(current(&state).item(), Item::Tree);
    }

    #[test]
    fn stepper_buttons_and_ranges() {
        let mut state = ExplorerState::default();
        state.settings.escalate_after_secs = 59;
        open(&mut state);
        render(&mut state, 100, 40);
        let plus = state
            .hits
            .rect_of(|h| {
                *h == Hit::OverlayControl {
                    overlay: 0,
                    control: control(Item::EscalateAfter, PART_INC),
                }
            })
            .unwrap();
        click(&mut state, plus.x, plus.y);
        click(&mut state, plus.x, plus.y);
        assert_eq!(state.settings.escalate_after_secs, 60);
        let minus = state
            .hits
            .rect_of(|h| {
                *h == Hit::OverlayControl {
                    overlay: 0,
                    control: control(Item::Refresh, PART_DEC),
                }
            })
            .unwrap();
        click(&mut state, minus.x, minus.y);
        assert_eq!(state.settings.refresh_ms, 1000);
        state.settings.dock_size = 6;
        adjust(&mut state, Item::DockSize, -1);
        assert_eq!(state.settings.dock_size, 6);
        state.settings.escalate_after_secs = 1;
        adjust(&mut state, Item::EscalateAfter, -5);
        assert_eq!(state.settings.escalate_after_secs, 1);
    }

    #[test]
    fn choices_cycle_dock_and_preset() {
        let mut state = ExplorerState::default();
        open(&mut state);
        render(&mut state, 100, 40);
        select(&mut state, Item::Dock);
        key(&mut state, KeyCode::Right);
        assert_eq!(state.settings.dock, DockPosition::Right);
        select(&mut state, Item::Preset);
        key(&mut state, KeyCode::Right);
        assert_eq!(state.settings.preset, Preset::Cpu);
        key(&mut state, KeyCode::Left);
        key(&mut state, KeyCode::Left);
        assert_eq!(state.settings.preset, Preset::Custom);
        let requests = state.take_requests();
        assert!(requests.iter().all(|r| *r == UiRequest::SaveSettings));
    }

    #[test]
    fn columns_row_opens_chooser_on_top() {
        let mut state = ExplorerState::default();
        open(&mut state);
        render(&mut state, 100, 40);
        select(&mut state, Item::Columns);
        key(&mut state, KeyCode::Enter);
        assert_eq!(state.overlays.len(), 2);
        assert!(matches!(state.overlays[0], Overlay::Settings(_)));
        assert!(matches!(state.overlays[1], Overlay::Columns(_)));
    }

    #[test]
    fn reset_needs_two_presses() {
        let mut state = ExplorerState::default();
        state.settings.refresh_ms = 5000;
        state.settings.tree = true;
        open(&mut state);
        render(&mut state, 100, 40);
        select(&mut state, Item::Reset);
        key(&mut state, KeyCode::Enter);
        assert!(current(&state).confirm_reset);
        assert!(state.settings.tree, "not reset yet");
        let text = render(&mut state, 100, 40);
        assert!(text.contains("Press again"));
        key(&mut state, KeyCode::Enter);
        assert!(!state.settings.tree);
        assert_eq!(state.settings, ExplorerSettings::default());
        let requests = state.take_requests();
        assert!(requests.contains(&UiRequest::SetRefresh(Duration::from_millis(1500))));
        assert!(requests.contains(&UiRequest::SaveSettings));
        // Moving away disarms.
        key(&mut state, KeyCode::Enter);
        key(&mut state, KeyCode::Up);
        assert!(!current(&state).confirm_reset);
    }

    #[test]
    fn close_keys_and_backdrop() {
        for code in [KeyCode::Escape, KeyCode::F(2), KeyCode::Char('q')] {
            let mut state = ExplorerState::default();
            open(&mut state);
            key(&mut state, code);
            assert!(state.overlays.is_empty(), "{code:?}");
        }
        let mut state = ExplorerState::default();
        open(&mut state);
        render(&mut state, 120, 40);
        click(&mut state, 0, 0);
        assert!(state.overlays.is_empty());
    }

    #[test]
    fn short_terminal_scrolls_to_selection() {
        let mut state = ExplorerState::default();
        open(&mut state);
        render(&mut state, 80, 12);
        key(&mut state, KeyCode::End);
        let text = render(&mut state, 80, 12);
        assert!(text.contains("Reset all to defaults"));
        assert!(current(&state).scroll > 0);
        for _ in 0..6 {
            wheel(&mut state, 40, 6, true);
        }
        assert_eq!(current(&state).scroll, 0);
        let text = render(&mut state, 80, 12);
        assert!(text.contains("Refresh interval"), "{text}");
    }
}
