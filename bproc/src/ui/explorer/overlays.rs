//! Modal overlay stack: context menus, signal / renice dialogs, column
//! chooser, sort and filter menus, export menu, plus hosting for help,
//! settings and the watch editor (drawn/handled by their own modules).
use super::hits::Hit;
use super::paint::Frame;
use super::{ExplorerState, help, screens, settings};
use crate::procs::ProcessKey;
use crate::procs::actions::Signal;
use bmux_tui::event::Event;
use bmux_tui::geometry::{Point, Rect};

pub enum Overlay {
    Menu(MenuOverlay),
    Signal(SignalDialog),
    Renice(ReniceDialog),
    Columns(ColumnChooser),
    Help(help::HelpState),
    Settings(settings::SettingsState),
    WatchEditor(screens::WatchEditor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayOutcome {
    /// Keep the overlay open.
    Consumed,
    /// Remove the overlay.
    Close,
}

pub struct MenuOverlay {
    pub at: Point,
}

pub struct SignalDialog {
    pub targets: Vec<ProcessKey>,
    pub signal: Signal,
}

pub struct ReniceDialog {
    pub targets: Vec<ProcessKey>,
    pub value: i32,
}

pub struct ColumnChooser {}

pub fn process_menu(state: &ExplorerState, key: ProcessKey, at: Point) -> Overlay {
    let _ = (state, key);
    Overlay::Menu(MenuOverlay { at })
}
pub fn sort_menu(state: &ExplorerState) -> Overlay {
    let _ = state;
    Overlay::Menu(MenuOverlay {
        at: Point::new(0, 0),
    })
}
pub fn filter_menu(state: &ExplorerState) -> Overlay {
    let _ = state;
    Overlay::Menu(MenuOverlay {
        at: Point::new(0, 0),
    })
}
pub fn export_menu(state: &ExplorerState) -> Overlay {
    let _ = state;
    Overlay::Menu(MenuOverlay {
        at: Point::new(0, 0),
    })
}
pub fn column_chooser(state: &ExplorerState) -> Overlay {
    let _ = state;
    Overlay::Columns(ColumnChooser {})
}
/// `lower` = F8 (nice +, lower priority); otherwise F7.
pub fn renice_dialog(state: &ExplorerState, targets: Vec<ProcessKey>, lower: bool) -> Overlay {
    let _ = (state, lower);
    Overlay::Renice(ReniceDialog { targets, value: 0 })
}
pub fn signal_dialog(state: &ExplorerState, targets: Vec<ProcessKey>, signal: Signal) -> Overlay {
    let _ = state;
    Overlay::Signal(SignalDialog { targets, signal })
}

/// Draw overlay `index` of `state.overlays` (bottom = 0).
pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, index: usize, state: &ExplorerState) {
    match &state.overlays[index] {
        Overlay::Help(h) => help::draw(frame, area, index, h, state),
        Overlay::Settings(s) => settings::draw(frame, area, index, s, state),
        Overlay::WatchEditor(e) => screens::draw_editor(frame, area, index, e, state),
        _ => frame.hit(area, Hit::OverlayBackdrop { overlay: index }),
    }
}

/// Handle input for the top overlay (already removed from `state.overlays`).
pub fn handle(
    overlay: &mut Overlay,
    index: usize,
    event: &Event,
    state: &mut ExplorerState,
) -> OverlayOutcome {
    match overlay {
        Overlay::Help(h) => help::handle(h, index, event, state),
        Overlay::Settings(s) => settings::handle(s, index, event, state),
        Overlay::WatchEditor(e) => screens::handle_editor(e, index, event, state),
        _ => match event {
            Event::Key(_) | Event::Mouse(_) => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        },
    }
}
