//! Test helpers for the screens, help and settings modules: render the whole
//! explorer, commit hits and drive it with synthetic events.
use super::super::{DrawEnv, ExplorerState};
use crate::procs::ProcessKey;
use crate::procs::model::{ProcessEntry, ProcessRecord, ProcessSample, SystemSnapshot};
use crate::procs::store::ProcessStore;
use crate::procs::watch::{EventKind, ProcEvent};
use bmux_keyboard::{KeyCode, KeyStroke, Modifiers};
use bmux_tui::buffer::Buffer;
use bmux_tui::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use bmux_tui::frame::Frame as TuiFrame;
use bmux_tui::geometry::{Point, Rect};
use bmux_tui::paint::PaintCx;
use std::sync::Arc;

/// Draw the explorer at `w`×`h`, commit its hits and return the screen text
/// (rows joined by newlines).
pub fn render(state: &mut ExplorerState, w: u16, h: u16) -> String {
    let area = Rect::new(0, 0, w, h);
    let mut buffer = Buffer::empty(area);
    {
        let mut frame = TuiFrame::new(&mut buffer);
        let mut cx = PaintCx::new(&mut frame);
        state.draw(&mut cx, area, &DrawEnv::default());
    }
    state.commit();
    (0..h)
        .filter_map(|y| buffer.row_symbols(y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Paint only `paint` (a module painter) into a fresh buffer; returns text
/// and the staged hit regions.
pub fn paint_with(
    state: &mut ExplorerState,
    w: u16,
    h: u16,
    paint: impl FnOnce(&mut super::super::paint::Frame<'_, '_, '_>, Rect, &mut ExplorerState),
) -> (String, Vec<(Rect, super::super::hits::Hit)>) {
    let area = Rect::new(0, 0, w, h);
    let mut buffer = Buffer::empty(area);
    let mut hits = super::super::hits::HitMap::default();
    {
        let mut frame = TuiFrame::new(&mut buffer);
        let mut cx = PaintCx::new(&mut frame);
        let mut f = super::super::paint::Frame {
            cx: &mut cx,
            hits: &mut hits,
            now: std::time::Instant::now(),
        };
        paint(&mut f, area, state);
    }
    let text = (0..h)
        .filter_map(|y| buffer.row_symbols(y))
        .collect::<Vec<_>>()
        .join("\n");
    (text, hits.staged().to_vec())
}

fn mouse(state: &mut ExplorerState, kind: MouseEventKind, x: u16, y: u16) {
    state.handle_event(&Event::Mouse(MouseEvent::new(kind, Point::new(x, y))));
}

pub fn click(state: &mut ExplorerState, x: u16, y: u16) {
    mouse(state, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(state, MouseEventKind::Up(MouseButton::Left), x, y);
}

pub fn right_click(state: &mut ExplorerState, x: u16, y: u16) {
    mouse(state, MouseEventKind::Down(MouseButton::Right), x, y);
}

pub fn double_click(state: &mut ExplorerState, x: u16, y: u16) {
    click(state, x, y);
    click(state, x, y);
}

pub fn wheel(state: &mut ExplorerState, x: u16, y: u16, up: bool) {
    let kind = if up {
        MouseEventKind::ScrollUp
    } else {
        MouseEventKind::ScrollDown
    };
    mouse(state, kind, x, y);
}

pub fn key(state: &mut ExplorerState, code: KeyCode) {
    state.handle_event(&Event::Key(KeyStroke::simple(code)));
}

pub fn key_with(state: &mut ExplorerState, code: KeyCode, modifiers: Modifiers) {
    state.handle_event(&Event::Key(KeyStroke::with_modifiers(code, modifiers)));
}

pub fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::NONE
    }
}

pub fn ctrl() -> Modifiers {
    Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    }
}

pub fn type_text(state: &mut ExplorerState, text: &str) {
    for c in text.chars() {
        key(state, KeyCode::Char(c));
    }
}

pub fn entry(pid: i32, name: &str, user: &str, cpu: f32, mem: f32) -> ProcessEntry {
    ProcessEntry {
        record: Arc::new(ProcessRecord {
            key: ProcessKey::new(pid, 1000 + pid as u64),
            ppid: Some(1),
            name: name.into(),
            cmdline: vec![format!("/usr/bin/{name}"), "--serve".into()],
            user: Some(user.into()),
            ..Default::default()
        }),
        sample: ProcessSample {
            cpu_percent: cpu,
            mem_percent: mem,
            mem_rss: 1 << 20,
            ..Default::default()
        },
    }
}

pub fn key_of(pid: i32) -> ProcessKey {
    ProcessKey::new(pid, 1000 + pid as u64)
}

/// State with a few live processes.
pub fn state_with_processes() -> ExplorerState {
    ExplorerState {
        store: ProcessStore::from_entries(
            vec![
                entry(100, "node", "alice", 95.0, 3.5),
                entry(200, "postgres", "postgres", 4.0, 12.0),
                entry(300, "chrome helper", "alice", 1.0, 2.0),
            ],
            SystemSnapshot {
                mem_total: 16 << 30,
                cpus: vec![10.0; 4],
                ..Default::default()
            },
        ),
        ..Default::default()
    }
}

pub fn event(kind: EventKind, pid: Option<i32>, name: &str, message: &str) -> ProcEvent {
    ProcEvent {
        at_ms: crate::procs::model::now_ms(),
        kind,
        key: pid.map(key_of),
        name: name.into(),
        message: message.into(),
    }
}
