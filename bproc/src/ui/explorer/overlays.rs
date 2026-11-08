//! Modal overlay stack: context menus, signal / renice / CPU affinity
//! dialogs, column chooser, sort and filter menus, export menu, plus hosting for help,
//! settings and the watch editor (drawn/handled by their own modules).
//!
//! Every overlay pushes `Hit::OverlayBackdrop` over the whole screen first,
//! so clicks outside it are captured (and dismiss it) instead of reaching
//! whatever is underneath.
mod affinity;
mod columns;
pub mod menu;
pub mod menus;
mod renice;
pub mod signal;
#[cfg(test)]
mod tests;
pub(super) mod widgets;

use super::hits::Hit;
use super::paint::Frame;
use super::{ExplorerState, help, screens, settings};
use crate::procs::ProcessKey;
use crate::procs::actions::Signal;
use bmux_tui::event::Event;
use bmux_tui::geometry::{Point, Rect};

pub use affinity::AffinityDialog;
pub use columns::ColumnChooser;
pub use menu::MenuOverlay;
pub use renice::ReniceDialog;
pub use signal::SignalDialog;

pub enum Overlay {
    Menu(MenuOverlay),
    Signal(SignalDialog),
    Renice(ReniceDialog),
    Affinity(AffinityDialog),
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

/// Context menu for `key` at `at`. When `key` is marked alongside other
/// rows, process actions target every marked row.
pub fn process_menu(state: &ExplorerState, key: ProcessKey, at: Point) -> Overlay {
    Overlay::Menu(menus::process_menu(state, key, at))
}
pub fn sort_menu(state: &ExplorerState) -> Overlay {
    Overlay::Menu(menus::sort_menu(state))
}
pub fn filter_menu(state: &ExplorerState) -> Overlay {
    Overlay::Menu(menus::filter_menu(state))
}
pub fn export_menu(state: &ExplorerState) -> Overlay {
    Overlay::Menu(menus::export_menu(state))
}
pub fn column_chooser(state: &ExplorerState) -> Overlay {
    Overlay::Columns(ColumnChooser::new(state))
}
pub fn signal_dialog(state: &ExplorerState, targets: Vec<ProcessKey>, signal: Signal) -> Overlay {
    Overlay::Signal(SignalDialog::new(state, targets, signal))
}
/// F7 / F8 entry point (`lower` = F8, nice +): opens the renice dialog, or flashes instead when
/// the user cannot raise the targets' priority (F7 without root on a
/// process whose nice value is known).
pub fn open_renice(state: &mut ExplorerState, targets: Vec<ProcessKey>, lower: bool) {
    if targets.is_empty() {
        return;
    }
    let dialog = ReniceDialog::new(state, targets, Some(lower));
    if !lower && dialog.cannot_raise() {
        state.flash("Raising priority needs root");
        return;
    }
    state.overlays.push(Overlay::Renice(dialog));
}

/// Draw overlay `index` of `state.overlays` (bottom = 0).
pub fn draw(frame: &mut Frame<'_, '_, '_>, area: Rect, index: usize, state: &ExplorerState) {
    let Some(overlay) = state.overlays.get(index) else {
        return;
    };
    match overlay {
        Overlay::Help(h) => help::draw(frame, area, index, h, state),
        Overlay::Settings(s) => settings::draw(frame, area, index, s, state),
        Overlay::WatchEditor(e) => screens::draw_editor(frame, area, index, e, state),
        Overlay::Menu(m) => {
            frame.hit(area, Hit::OverlayBackdrop { overlay: index });
            m.draw(frame, area, index);
        }
        Overlay::Signal(d) => {
            widgets::backdrop(frame, area, index);
            d.draw(frame, area, index, state);
        }
        Overlay::Renice(d) => {
            widgets::backdrop(frame, area, index);
            d.draw(frame, area, index, state);
        }
        Overlay::Affinity(d) => {
            widgets::backdrop(frame, area, index);
            d.draw(frame, area, index, state);
        }
        Overlay::Columns(d) => {
            widgets::backdrop(frame, area, index);
            d.draw(frame, area, index);
        }
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
        Overlay::Menu(m) => m.handle(index, event, state),
        Overlay::Signal(d) => d.handle(index, event, state),
        Overlay::Renice(d) => d.handle(index, event, state),
        Overlay::Affinity(d) => d.handle(index, event, state),
        Overlay::Columns(d) => d.handle(index, event, state),
    }
}
