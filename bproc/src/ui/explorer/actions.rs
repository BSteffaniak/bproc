//! Action registry: one table drives key handling, the clickable F-key bar,
//! help, and context menus so they can never disagree.
use bmux_keyboard::{KeyCode, KeyStroke};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Help,
    Settings,
    Search,
    FilterMenu,
    Tree,
    SortMenu,
    NiceUp,
    NiceDown,
    Signal,
    Quit,
    Details,
    FullDetails,
    CloseDetails,
    ToggleDock,
    ContextMenu,
    Mark,
    MarkTree,
    ClearMarks,
    Pin,
    Freeze,
    Pause,
    Expand,
    Collapse,
    ExpandAll,
    CollapseAll,
    GoParent,
    Columns,
    NextPreset,
    PrevPreset,
    ToggleUsers,
    ToggleKernelThreads,
    ToggleNetActive,
    ToggleMeters,
    Processes,
    Events,
    Watches,
    WatchProcess,
    Export,
    Throttle,
    RemoveThrottle,
    Interfaces,
    TrafficMode,
    Graph,
    Backends,
    Terminate,
    Kill,
    Suspend,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Navigate,
    View,
    Process,
    Network,
    App,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Navigate,
        Group::View,
        Group::Process,
        Group::Network,
        Group::App,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Navigate => "Navigate",
            Self::View => "View",
            Self::Process => "Process",
            Self::Network => "Network",
            Self::App => "App",
        }
    }
}

pub struct Binding {
    pub action: Action,
    /// Human key label shown in help and menus.
    pub keys: &'static str,
    pub label: &'static str,
    pub group: Group,
}

macro_rules! b {
    ($a:ident, $k:expr, $l:expr, $g:ident) => {
        Binding {
            action: Action::$a,
            keys: $k,
            label: $l,
            group: Group::$g,
        }
    };
}

pub const BINDINGS: &[Binding] = &[
    b!(Search, "F3 /", "Search & filter", Navigate),
    b!(Details, "Enter", "Open details panel", Navigate),
    b!(FullDetails, "Shift+Enter", "Full-screen details", Navigate),
    b!(CloseDetails, "Esc", "Close panel / clear search", Navigate),
    b!(
        ContextMenu,
        ". Right-click",
        "Actions for selected process",
        Navigate
    ),
    b!(GoParent, "Backspace", "Select parent process", Navigate),
    b!(Expand, "→ +", "Expand tree node", Navigate),
    b!(Collapse, "← -", "Collapse tree node", Navigate),
    b!(ExpandAll, "*", "Expand all", Navigate),
    b!(CollapseAll, "_", "Collapse all", Navigate),
    b!(Tree, "F5 T", "Tree view", View),
    b!(SortMenu, "F6 >", "Sort by…", View),
    b!(Columns, "C", "Choose columns", View),
    b!(NextPreset, "] Tab", "Next column preset", View),
    b!(PrevPreset, "[ Shift+Tab", "Previous column preset", View),
    b!(FilterMenu, "F4", "Quick filters", View),
    b!(ToggleUsers, "u", "Only my processes", View),
    b!(ToggleKernelThreads, "K", "Show kernel threads", View),
    b!(ToggleNetActive, "N", "Only network-active", View),
    b!(ToggleMeters, "m", "Toggle meters", View),
    b!(ToggleDock, "d", "Dock details bottom/right", View),
    b!(Freeze, "f", "Freeze sort order", View),
    b!(Pause, "z", "Pause updates", View),
    b!(Processes, "1", "Processes screen", View),
    b!(Events, "2 e", "Events timeline", View),
    b!(Watches, "3 w", "Watches & alerts", View),
    b!(Signal, "F9 x", "Send signal…", Process),
    b!(Terminate, "", "Terminate (SIGTERM)", Process),
    b!(Kill, "", "Force kill (SIGKILL)", Process),
    b!(Suspend, "", "Suspend (SIGSTOP)", Process),
    b!(Resume, "", "Resume (SIGCONT)", Process),
    b!(NiceDown, "F7", "Raise priority (nice −)", Process),
    b!(NiceUp, "F8", "Lower priority (nice +)", Process),
    b!(Mark, "Space", "Mark / unmark", Process),
    b!(MarkTree, "c", "Mark with children", Process),
    b!(ClearMarks, "U", "Clear marks", Process),
    b!(Pin, "P", "Pin to top", Process),
    b!(WatchProcess, "W", "Watch this process…", Process),
    b!(Throttle, "t", "Throttle bandwidth…", Network),
    b!(RemoveThrottle, "r", "Remove throttle", Network),
    b!(Interfaces, "i", "Interfaces", Network),
    b!(TrafficMode, "l", "Traffic: all/internet/local", Network),
    b!(Graph, "g", "Bandwidth graph", Network),
    b!(Backends, "b", "Throttle backends", Network),
    b!(Help, "F1 ?", "Help", App),
    b!(Settings, "F2 S", "Settings", App),
    b!(Export, "E", "Export view (CSV/JSON)", App),
    b!(Quit, "F10 q", "Quit", App),
];

/// The clickable function-key bar (htop layout).
pub const FKEYS: &[(&str, Action, &str)] = &[
    ("F1", Action::Help, "Help"),
    ("F2", Action::Settings, "Setup"),
    ("F3", Action::Search, "Search"),
    ("F4", Action::FilterMenu, "Filter"),
    ("F5", Action::Tree, "Tree"),
    ("F6", Action::SortMenu, "SortBy"),
    ("F7", Action::NiceDown, "Nice-"),
    ("F8", Action::NiceUp, "Nice+"),
    ("F9", Action::Signal, "Kill"),
    ("F10", Action::Quit, "Quit"),
];

impl Action {
    /// Actions that act on one process (or the marked set): off the
    /// Processes screen they resolve to the highlighted event / alert.
    pub const fn is_process_scoped(self) -> bool {
        matches!(
            self,
            Self::ContextMenu
                | Self::Signal
                | Self::Terminate
                | Self::Kill
                | Self::Suspend
                | Self::Resume
                | Self::NiceUp
                | Self::NiceDown
                | Self::Mark
                | Self::MarkTree
                | Self::Pin
                | Self::Details
                | Self::FullDetails
                | Self::WatchProcess
                | Self::Expand
                | Self::Collapse
                | Self::GoParent
                | Self::Throttle
                | Self::RemoveThrottle
                | Self::Graph
        )
    }
    #[allow(dead_code)] // registry lookups for help/menus
    pub fn binding(self) -> Option<&'static Binding> {
        BINDINGS.iter().find(|b| b.action == self)
    }
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        self.binding().map(|b| b.label).unwrap_or("")
    }
    #[allow(dead_code)]
    pub fn keys(self) -> &'static str {
        self.binding().map(|b| b.keys).unwrap_or("")
    }

    /// Map a key press on the processes screen to an action.
    pub fn from_key(key: &KeyStroke) -> Option<Action> {
        let m = key.modifiers;
        if m.ctrl || m.alt {
            return None;
        }
        Some(match key.key {
            KeyCode::F(1) => Action::Help,
            KeyCode::F(2) => Action::Settings,
            KeyCode::F(3) => Action::Search,
            KeyCode::F(4) => Action::FilterMenu,
            KeyCode::F(5) => Action::Tree,
            KeyCode::F(6) => Action::SortMenu,
            KeyCode::F(7) => Action::NiceDown,
            KeyCode::F(8) => Action::NiceUp,
            KeyCode::F(9) => Action::Signal,
            KeyCode::F(10) => Action::Quit,
            KeyCode::Enter if m.shift => Action::FullDetails,
            KeyCode::Enter => Action::Details,
            KeyCode::Escape => Action::CloseDetails,
            KeyCode::Backspace => Action::GoParent,
            KeyCode::Tab if m.shift => Action::PrevPreset,
            KeyCode::Tab => Action::NextPreset,
            KeyCode::Right => Action::Expand,
            KeyCode::Left => Action::Collapse,
            KeyCode::Space => Action::Mark,
            KeyCode::Char(c) => match c {
                '/' => Action::Search,
                '?' | 'h' => Action::Help,
                'q' => Action::Quit,
                'S' => Action::Settings,
                'T' => Action::Tree,
                '>' | '<' => Action::SortMenu,
                'x' => Action::Signal,
                '.' => Action::ContextMenu,
                '+' | '=' => Action::Expand,
                '-' => Action::Collapse,
                '*' => Action::ExpandAll,
                '_' => Action::CollapseAll,
                'C' => Action::Columns,
                ']' => Action::NextPreset,
                '[' => Action::PrevPreset,
                'u' => Action::ToggleUsers,
                'K' => Action::ToggleKernelThreads,
                'N' => Action::ToggleNetActive,
                'm' => Action::ToggleMeters,
                'd' => Action::ToggleDock,
                'f' => Action::Freeze,
                'z' => Action::Pause,
                '1' => Action::Processes,
                '2' | 'e' => Action::Events,
                '3' | 'w' => Action::Watches,
                ' ' => Action::Mark,
                'c' => Action::MarkTree,
                'U' => Action::ClearMarks,
                'P' => Action::Pin,
                'W' => Action::WatchProcess,
                't' => Action::Throttle,
                'r' => Action::RemoveThrottle,
                'i' => Action::Interfaces,
                'l' => Action::TrafficMode,
                'g' => Action::Graph,
                'b' => Action::Backends,
                'E' => Action::Export,
                _ => return None,
            },
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_fkey_and_action_has_a_binding_label() {
        for (_, action, _) in FKEYS {
            assert!(!action.label().is_empty(), "{action:?}");
        }
        let mut seen = std::collections::HashSet::new();
        for b in BINDINGS {
            assert!(seen.insert(b.action), "duplicate binding {:?}", b.action);
        }
    }
}
