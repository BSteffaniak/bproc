//! Shared test fixtures for the table and search box.
use super::super::paint::Frame;
use super::super::{ExplorerState, search};
use crate::procs::model::{ProcessEntry, ProcessKey, ProcessRecord, ProcessSample, SystemSnapshot};
use crate::procs::store::ProcessStore;
use bmux_keyboard::{KeyCode, KeyStroke, Modifiers};
use bmux_tui::buffer::Buffer;
use bmux_tui::event::{Event, MouseButton, MouseEvent, MouseEventKind, MouseModifiers};
use bmux_tui::frame::Cursor;
use bmux_tui::geometry::{Point, Rect};
use bmux_tui::paint::PaintCx;
use std::sync::Arc;

pub const MEM_TOTAL: u64 = 16 * 1024 * 1024 * 1024;

pub fn entry(pid: i32, name: &str, cmd: &str, cpu: f32, rss: u64) -> ProcessEntry {
    ProcessEntry {
        record: Arc::new(ProcessRecord {
            key: ProcessKey::new(pid, 1_000 + pid as u64),
            ppid: Some(1),
            name: name.into(),
            cmdline: cmd.split_whitespace().map(String::from).collect(),
            uid: Some(501),
            user: Some("me".into()),
            ..Default::default()
        }),
        sample: ProcessSample {
            cpu_percent: cpu,
            mem_rss: rss,
            mem_percent: (rss as f64 * 100.0 / MEM_TOTAL as f64) as f32,
            ..Default::default()
        },
    }
}

pub fn key(pid: i32) -> ProcessKey {
    ProcessKey::new(pid, 1_000 + pid as u64)
}

/// `n` processes with distinct CPU so the default sort is predictable:
/// pid 1 has the highest CPU.
pub fn many(n: i32) -> Vec<ProcessEntry> {
    (1..=n)
        .map(|pid| {
            entry(
                pid,
                &format!("proc{pid}"),
                &format!("/usr/bin/proc{pid} --flag"),
                (n - pid + 1) as f32,
                pid as u64 * 1024 * 1024,
            )
        })
        .collect()
}

pub fn state_with(entries: Vec<ProcessEntry>) -> ExplorerState {
    let mut state = ExplorerState {
        uid: Some(501),
        store: ProcessStore::from_entries(
            entries,
            SystemSnapshot {
                mem_total: MEM_TOTAL,
                cpus: vec![0.0; 8],
                ..Default::default()
            },
        ),
        ..Default::default()
    };
    state.table.mark_dirty();
    refresh(&mut state);
    state
}

/// Refresh like `ExplorerState::draw` (hover freeze included).
pub fn refresh(state: &mut ExplorerState) {
    let hovering = state.hovering_table();
    state.table.refresh(
        &state.store,
        &state.settings,
        &state.search,
        state.traffic,
        state.uid,
        hovering,
    );
}

pub struct Rendered {
    pub buffer: Buffer,
    pub cursor: Option<Cursor>,
}

impl Rendered {
    pub fn row(&self, y: u16) -> String {
        let area = self.buffer.area();
        (area.x..area.x + area.width)
            .filter_map(|x| self.buffer.get(Point::new(x, y)))
            .map(|c| c.symbol.as_str())
            .collect()
    }
    pub fn text(&self) -> String {
        let area = self.buffer.area();
        (area.y..area.y + area.height)
            .map(|y| self.row(y))
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn cell(&self, x: u16, y: u16) -> &bmux_tui::buffer::Cell {
        self.buffer.get(Point::new(x, y)).expect("cell in buffer")
    }
}

fn paint(
    state: &mut ExplorerState,
    screen: Rect,
    f: impl FnOnce(&mut Frame<'_, '_, '_>, &mut ExplorerState),
) -> Rendered {
    refresh(state);
    let mut buffer = Buffer::empty(screen);
    let cursor;
    {
        let mut frame = bmux_tui::frame::Frame::new(&mut buffer);
        {
            let mut cx = PaintCx::new(&mut frame);
            let mut hits = std::mem::take(&mut state.hits);
            hits.begin();
            {
                let mut fr = Frame {
                    cx: &mut cx,
                    hits: &mut hits,
                    now: std::time::Instant::now(),
                };
                f(&mut fr, state);
            }
            state.hits = hits;
        }
        cursor = frame.cursor();
    }
    state.commit();
    Rendered { buffer, cursor }
}

/// Draw the table filling a `w`×`h` screen and commit its hit regions.
pub fn render(state: &mut ExplorerState, w: u16, h: u16) -> Rendered {
    let area = Rect::new(0, 0, w, h);
    paint(state, area, |f, s| super::draw(f, area, s))
}

/// Draw the search box in `area` of a screen sized to contain it.
pub fn render_search(state: &mut ExplorerState, area: Rect) -> Rendered {
    let screen = Rect::new(0, 0, area.x + area.width, area.y + area.height.max(1));
    paint(state, screen, |f, s| search::draw(f, area, s))
}

/// Search box on row 0 and the table below it, as the explorer lays them out.
pub fn render_both(state: &mut ExplorerState, w: u16, h: u16) -> Rendered {
    let screen = Rect::new(0, 0, w, h);
    paint(state, screen, |f, s| {
        search::draw(f, Rect::new(0, 0, w, 1), s);
        super::draw(f, Rect::new(0, 1, w, h.saturating_sub(1)), s);
    })
}

pub fn mouse(kind: MouseEventKind, x: u16, y: u16, mods: MouseModifiers) -> Event {
    Event::Mouse(MouseEvent::new(kind, Point::new(x, y)).with_modifiers(mods))
}

pub fn down(state: &mut ExplorerState, button: MouseButton, x: u16, y: u16) {
    state.handle_event(&mouse(
        MouseEventKind::Down(button),
        x,
        y,
        MouseModifiers::NONE,
    ));
    state.handle_event(&mouse(
        MouseEventKind::Up(button),
        x,
        y,
        MouseModifiers::NONE,
    ));
}

pub fn left(state: &mut ExplorerState, x: u16, y: u16) {
    down(state, MouseButton::Left, x, y);
}

pub fn left_with(state: &mut ExplorerState, x: u16, y: u16, mods: MouseModifiers) {
    state.handle_event(&mouse(MouseEventKind::Down(MouseButton::Left), x, y, mods));
    state.handle_event(&mouse(MouseEventKind::Up(MouseButton::Left), x, y, mods));
}

pub fn press(state: &mut ExplorerState, code: KeyCode) {
    state.handle_event(&Event::Key(KeyStroke::simple(code)));
}

pub fn press_with(state: &mut ExplorerState, code: KeyCode, modifiers: Modifiers) {
    state.handle_event(&Event::Key(KeyStroke::with_modifiers(code, modifiers)));
}

pub fn ctrl() -> Modifiers {
    Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    }
}

pub fn type_text(state: &mut ExplorerState, text: &str) {
    for c in text.chars() {
        press(state, KeyCode::Char(c));
    }
}
