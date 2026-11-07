//! Frame-local hit regions. Painters push regions in paint order; the last
//! region containing a point wins (topmost). Regions become authoritative only
//! after the presenter commits the frame, matching the component scene rules.
use super::actions::Action;
use crate::procs::ProcessKey;
use crate::procs::columns::{ColumnId, Preset};
use bmux_tui::geometry::{Point, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeterKind {
    Cpu,
    Core(usize),
    Mem,
    Swap,
    Load,
    Tasks,
    Net,
    Disk,
    Uptime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chip {
    Tree,
    Users,
    KernelThreads,
    NetActive,
    Freeze,
    Pause,
    Dock,
    Marks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {
    Processes,
    Events,
    Watches,
}

impl Screen {
    pub const ALL: [Screen; 3] = [Screen::Processes, Screen::Events, Screen::Watches];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Events => "Events",
            Self::Watches => "Watches",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    // Process table (table.rs)
    Row {
        key: ProcessKey,
    },
    TreeToggle {
        key: ProcessKey,
    },
    Header {
        column: ColumnId,
    },
    /// The draggable right edge of a header cell.
    HeaderEdge {
        column: ColumnId,
    },
    TableBody,
    TableScrollbar,
    // Chrome (chrome.rs)
    Meter(MeterKind),
    Preset(Preset),
    Chip(Chip),
    FKey(Action),
    Screen(Screen),
    StatusNote,
    // Search box (search.rs)
    SearchBox,
    SearchClear,
    // Overlays (overlays.rs); `overlay` is the stack index.
    OverlayBackdrop {
        overlay: usize,
    },
    OverlayBody {
        overlay: usize,
    },
    OverlayItem {
        overlay: usize,
        item: usize,
    },
    OverlayControl {
        overlay: usize,
        control: u16,
    },
    // Toasts (toasts.rs)
    Toast {
        id: u64,
    },
    ToastAction {
        id: u64,
    },
    // Detail panel (detail_panel.rs)
    DetailTab(usize),
    DetailAction(Action),
    DetailClose,
    DetailDockToggle,
    /// The splitter between table and docked panel (drag to resize).
    DetailSplitter,
    DetailBody,
    DetailRow {
        row: usize,
    },
    ProcessLink {
        key: ProcessKey,
    },
    // Events / watches screens (screens.rs)
    ScreenBody,
    EventRow {
        index: usize,
    },
    WatchRow {
        index: usize,
    },
    WatchControl {
        index: usize,
        control: u16,
    },
}

#[derive(Debug, Default, Clone)]
pub struct HitMap {
    staged: Vec<(Rect, Hit)>,
    committed: Vec<(Rect, Hit)>,
}

impl HitMap {
    pub fn begin(&mut self) {
        self.staged.clear();
    }
    pub fn push(&mut self, rect: Rect, hit: Hit) {
        if rect.width > 0 && rect.height > 0 {
            self.staged.push((rect, hit));
        }
    }
    pub fn commit(&mut self) {
        self.committed = std::mem::take(&mut self.staged);
    }
    /// Topmost committed region at `point`.
    pub fn at(&self, point: Point) -> Option<&Hit> {
        self.committed
            .iter()
            .rev()
            .find(|(r, _)| contains(*r, point))
            .map(|(_, h)| h)
    }
    /// Committed rectangle of the first region matching `pred`.
    pub fn rect_of(&self, pred: impl Fn(&Hit) -> bool) -> Option<Rect> {
        self.committed
            .iter()
            .find(|(_, h)| pred(h))
            .map(|(r, _)| *r)
    }
    pub fn committed(&self) -> &[(Rect, Hit)] {
        &self.committed
    }
    /// Test helper: make staged regions authoritative without a presenter.
    #[cfg(test)]
    pub fn staged(&self) -> &[(Rect, Hit)] {
        &self.staged
    }
}

pub fn contains(rect: Rect, point: Point) -> bool {
    point.x >= rect.x
        && point.x < rect.x.saturating_add(rect.width)
        && point.y >= rect.y
        && point.y < rect.y.saturating_add(rect.height)
}
