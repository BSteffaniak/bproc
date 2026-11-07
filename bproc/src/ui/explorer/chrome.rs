//! Screen chrome: meters header, toolbar (screen tabs, preset tabs, chips),
//! clickable F-key bar and status line.
//!
//! Every painter builds a list of [`Item`]s (styled text runs plus an optional
//! hit target), picks the richest variant that fits the width and paints it
//! clipped, so nothing ever overflows and every hit covers what is painted.
use super::actions::{Action, FKEYS};
use super::hits::{Chip, Hit, MeterKind, Screen};
use super::paint::{Frame, theme, truncate};
use super::toasts::Severity;
use super::{Click, DrawEnv, ExplorerState, Outcome};
use crate::process::TrafficType;
use crate::procs::columns::{ColumnId, Preset};
use crate::procs::model::fmt;
use crate::procs::store::SortSpec;
use bmux_tui::geometry::Rect;
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text_width::display_width;
use std::time::Duration;

/// The header never grows past this many rows.
pub const MAX_HEADER_ROWS: u16 = 6;
/// Narrower terminals get no header at all.
const MIN_HEADER_WIDTH: u16 = 16;
/// From this width CPU meters and the info column sit side by side.
const SIDE_BY_SIDE_WIDTH: u16 = 70;
const COLUMN_GAP: u16 = 2;
/// Preferred number of per-core rows before trying more columns.
const PREFERRED_CORE_ROWS: u16 = 4;
const FLASH_TTL: Duration = Duration::from_secs(4);
const SPARK: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

// ---- items ----------------------------------------------------------------------

/// A run of styled text painted left to right with one optional hit target.
#[derive(Debug, Clone, Default)]
struct Item {
    parts: Vec<(String, Style)>,
    hit: Option<Hit>,
}

impl Item {
    fn new(hit: Hit) -> Self {
        Self {
            parts: Vec::new(),
            hit: Some(hit),
        }
    }
    fn plain(text: impl Into<String>, style: Style) -> Self {
        Self::default().with(text, style)
    }
    fn with(mut self, text: impl Into<String>, style: Style) -> Self {
        let text = text.into();
        if !text.is_empty() {
            self.parts.push((text, style));
        }
        self
    }
    fn width(&self) -> usize {
        self.parts.iter().map(|(t, _)| display_width(t)).sum()
    }
}

fn items_width(items: &[Item]) -> usize {
    items.iter().map(Item::width).sum()
}

fn cells(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// Paint `items` on `area`'s first row starting at `x`, clipped at the right
/// edge. Each hit covers exactly the cells its item painted. Returns the next x.
fn paint_items(frame: &mut Frame<'_, '_, '_>, area: Rect, x: u16, items: &[Item]) -> u16 {
    let end = area.x.saturating_add(area.width);
    let mut x = x.max(area.x);
    if area.height == 0 {
        return x;
    }
    for item in items {
        let start = x;
        for (text, style) in &item.parts {
            if x >= end {
                break;
            }
            let room = end - x;
            frame.text(Rect::new(x, area.y, room, 1), text, *style);
            x = x.saturating_add(cells(display_width(text))).min(end);
        }
        if let Some(hit) = &item.hit
            && x > start
        {
            frame.hit(Rect::new(start, area.y, x - start, 1), hit.clone());
        }
    }
    x
}

/// One-row rectangle `rel_y` rows into `area`, or `None` when clipped away.
fn row(area: Rect, x: u16, rel_y: u16, width: u16) -> Option<Rect> {
    if rel_y >= area.height || width == 0 {
        return None;
    }
    Some(Rect::new(x, area.y.saturating_add(rel_y), width, 1))
}

// ---- meters header ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuLayout {
    /// One aggregate bar.
    Total,
    /// One bar per core, column-major.
    Cores { cols: u16, rows: u16 },
    /// Aggregate bar plus one heat cell per core.
    Strip { rows: u16 },
}

impl CpuLayout {
    fn rows(self) -> u16 {
        match self {
            Self::Total => 1,
            Self::Cores { rows, .. } => rows,
            Self::Strip { rows } => rows + 1,
        }
    }
}

struct InfoEntry {
    long: Item,
    short: Item,
    /// Higher ranks are dropped first when space runs out.
    rank: usize,
}

struct HeaderPlan {
    side: bool,
    left_w: u16,
    cpu: CpuLayout,
    swap: bool,
    info: Vec<Vec<Item>>,
    spark: bool,
    height: u16,
}

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// Arrange `cores` bars in 1-4 columns within `width` and `max_rows`.
fn cpu_layout(cores: usize, width: u16, max_rows: u16, per_core: bool) -> CpuLayout {
    if !per_core || cores < 2 || max_rows == 0 {
        return CpuLayout::Total;
    }
    let min_w = cells(digits(cores - 1) + 2 + 6);
    let max_cols = (width / min_w.max(1)).min(4);
    let rows_for = |cols: u16| cells(cores.div_ceil(usize::from(cols.max(1))));
    let target = PREFERRED_CORE_ROWS.min(max_rows);
    for limit in [target, max_rows] {
        if let Some(cols) = (1..=max_cols).find(|&c| rows_for(c) <= limit) {
            return CpuLayout::Cores {
                cols,
                rows: rows_for(cols),
            };
        }
    }
    let per_row = usize::from(width.saturating_sub(5).max(1));
    if max_rows >= 2 && width >= 8 {
        return CpuLayout::Strip {
            rows: cells(cores.div_ceil(per_row)).clamp(1, max_rows - 1),
        };
    }
    CpuLayout::Total
}

fn heat_style(percent: f64) -> Style {
    Style::new().fg(match theme::heat(percent) {
        Color::Default => Color::Green,
        c => c,
    })
}

fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn ratio_percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        clamp_percent(used as f64 / total as f64 * 100.0)
    }
}

fn percent_text(value: f64) -> String {
    format!("{}%", fmt::percent(value as f32))
}

/// htop-style bar: `label[||||||     text]` exactly `width` cells wide. The
/// first of `texts` that leaves room for some fill (else merely fits) is
/// right-aligned inside the brackets.
fn bar(label: &str, width: usize, percent: f64, texts: &[&str], hit: Hit) -> Item {
    const MIN_FILL: usize = 3;
    let label_w = display_width(label);
    let item = Item::new(hit);
    if width < label_w + 3 {
        return item.with(truncate(label, width), theme::accent());
    }
    let inner = width - label_w - 2;
    let fits = |room: usize| texts.iter().copied().find(|t| display_width(t) <= room);
    let text = fits(inner.saturating_sub(MIN_FILL))
        .or_else(|| fits(inner))
        .unwrap_or("");
    let text_start = inner - display_width(text);
    let pct = clamp_percent(percent);
    let fill = ((pct / 100.0) * inner as f64).round() as usize;
    let shown = fill.min(text_start);
    item.with(label, theme::accent())
        .with("[", theme::bold())
        .with("|".repeat(shown), heat_style(pct))
        .with(" ".repeat(text_start - shown), theme::base())
        .with(text, theme::dim())
        .with("]", theme::bold())
}

fn spark_cell(value: f64) -> (&'static str, Style) {
    let v = clamp_percent(value);
    let index = ((v / 100.0) * 7.0).round() as usize;
    let style = if v < 5.0 { theme::dim() } else { heat_style(v) };
    (SPARK[index.min(7)], style)
}

/// Recent total CPU history as `prefix` plus up to `samples` block characters.
fn spark_item(state: &ExplorerState, prefix: &str, samples: usize) -> Item {
    let mut item = Item::new(Hit::Meter(MeterKind::Cpu)).with(prefix, theme::accent());
    for v in state.store.system_history().cpu_total.tail(samples) {
        let (ch, style) = spark_cell(f64::from(v));
        item = item.with(ch, style);
    }
    item
}

fn uptime_text(secs: u64) -> String {
    let days = secs / 86_400;
    let (h, m, s) = ((secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    if days > 0 {
        format!("{days}d {h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}

fn info_entries(state: &ExplorerState) -> Vec<InfoEntry> {
    let sys = state.store.system();
    let hist = state.store.system_history();
    let tasks = if sys.tasks.total > 0 {
        sys.tasks.total
    } else {
        state.store.len_live()
    };
    let num = theme::bold();
    let label = theme::accent();

    let mut tasks_long = Item::new(Hit::Meter(MeterKind::Tasks))
        .with("Tasks: ", label)
        .with(tasks.to_string(), num);
    if sys.tasks.threads > 0 {
        tasks_long = tasks_long
            .with(", ", theme::base())
            .with(sys.tasks.threads.to_string(), num)
            .with(" thr", theme::base());
    }
    let tasks_long = tasks_long
        .with("; ", theme::base())
        .with(sys.tasks.running.to_string(), theme::ok().patch(num))
        .with(" running", theme::base());
    let tasks_short = Item::new(Hit::Meter(MeterKind::Tasks))
        .with("Tasks ", label)
        .with(tasks.to_string(), num)
        .with(", ", theme::base())
        .with(sys.tasks.running.to_string(), theme::ok().patch(num))
        .with(" run", theme::base());

    let [l1, l5, l15] = sys.load_average;
    let load = |prefix: &str| {
        Item::new(Hit::Meter(MeterKind::Load))
            .with(prefix, label)
            .with(format!("{l1:.2} "), num)
            .with(format!("{l5:.2} "), theme::base())
            .with(format!("{l15:.2}"), theme::dim())
    };

    let uptime = uptime_text(sys.uptime_secs);
    let up = |prefix: &str| {
        Item::new(Hit::Meter(MeterKind::Uptime))
            .with(prefix, label)
            .with(uptime.clone(), num)
    };

    let (down, upl) = (
        hist.net_down.last().unwrap_or(0),
        hist.net_up.last().unwrap_or(0),
    );
    let capture_off = !state.store.has_network() && state.network_note.is_some();
    let (net_long, net_short) = if capture_off {
        (
            Item::new(Hit::Meter(MeterKind::Net)).with("Net: capture off", theme::dim()),
            Item::new(Hit::Meter(MeterKind::Net)).with("Net off", theme::dim()),
        )
    } else {
        let dl = theme::download();
        let ul = theme::upload();
        (
            Item::new(Hit::Meter(MeterKind::Net))
                .with("Net ", label)
                .with("↓ ", dl)
                .with(fmt::rate(down), dl.patch(num))
                .with(" ↑ ", ul)
                .with(fmt::rate(upl), ul.patch(num)),
            Item::new(Hit::Meter(MeterKind::Net))
                .with("↓", dl)
                .with(fmt::rate(down), dl.patch(num))
                .with(" ↑", ul)
                .with(fmt::rate(upl), ul.patch(num)),
        )
    };

    let (read, write) = (
        hist.disk_read.last().unwrap_or(0),
        hist.disk_write.last().unwrap_or(0),
    );
    let disk_long = Item::new(Hit::Meter(MeterKind::Disk))
        .with("Disk ", label)
        .with("R ", theme::dim())
        .with(fmt::rate(read), num)
        .with(" W ", theme::dim())
        .with(fmt::rate(write), num);
    let disk_short = Item::new(Hit::Meter(MeterKind::Disk))
        .with("Disk ", label)
        .with("R", theme::dim())
        .with(fmt::rate(read), num)
        .with(" W", theme::dim())
        .with(fmt::rate(write), num);

    vec![
        InfoEntry {
            long: tasks_long,
            short: tasks_short,
            rank: 0,
        },
        InfoEntry {
            long: load("Load average: "),
            short: load("Load "),
            rank: 2,
        },
        InfoEntry {
            long: up("Uptime: "),
            short: up("Up "),
            rank: 4,
        },
        InfoEntry {
            long: net_long,
            short: net_short,
            rank: 1,
        },
        InfoEntry {
            long: disk_long,
            short: disk_short,
            rank: 3,
        },
    ]
}

/// Greedily pack items into rows of `width` (two-cell gaps). `None` when an
/// item is wider than a row.
fn pack_rows(items: Vec<Item>, width: usize) -> Option<Vec<Vec<Item>>> {
    let mut rows: Vec<Vec<Item>> = Vec::new();
    let mut used = 0;
    for item in items {
        let w = item.width();
        if w > width {
            return None;
        }
        match rows.last_mut() {
            Some(current) if used + 2 + w <= width => {
                used += 2 + w;
                current.push(item);
            }
            _ => {
                used = w;
                rows.push(vec![item]);
            }
        }
    }
    Some(rows)
}

/// Richest packing of the info entries that fits `budget` rows: long forms
/// first, then short forms, then dropping the highest-ranked entries.
fn pack_info(entries: &[InfoEntry], width: usize, budget: usize) -> Vec<Vec<Item>> {
    if budget == 0 || width == 0 {
        return Vec::new();
    }
    let n = entries.len();
    for keep in (1..=n).rev() {
        for short in [false, true] {
            let items = entries
                .iter()
                .filter(|e| e.rank < keep)
                .map(|e| if short { &e.short } else { &e.long }.clone())
                .collect();
            if let Some(rows) = pack_rows(items, width)
                && rows.len() <= budget
            {
                return rows;
            }
        }
    }
    Vec::new()
}

fn plan_header(state: &ExplorerState, width: u16) -> Option<HeaderPlan> {
    if width < MIN_HEADER_WIDTH {
        return None;
    }
    let sys = state.store.system();
    let cores = sys.cpus.len();
    let per_core = state.settings.per_core_meters;
    let entries = info_entries(state);
    if width >= SIDE_BY_SIDE_WIDTH {
        let left_w = (width - COLUMN_GAP) / 2;
        let right_w = width - COLUMN_GAP - left_w;
        let cpu = cpu_layout(cores, left_w, MAX_HEADER_ROWS, per_core);
        let info = pack_info(
            &entries,
            usize::from(right_w),
            usize::from(MAX_HEADER_ROWS - 2),
        );
        let mut info = info;
        let mut height = cpu.rows().max(2 + cells(info.len()));
        let history = state.store.system_history().cpu_total.len() > 1;
        // The CPU history sparkline goes under the CPU meters when there is a
        // spare row, else into the info column when every entry still fits.
        let spark = history && cpu.rows() < height;
        if history && !spark {
            let count = |rows: &[Vec<Item>]| rows.iter().map(Vec::len).sum::<usize>();
            let cells_of =
                |rows: &[Vec<Item>]| rows.iter().flatten().map(Item::width).sum::<usize>();
            let (long, short) = (spark_item(state, "CPU ", 24), spark_item(state, "CPU ", 12));
            let spark_widths = [long.width(), short.width()];
            let mut with = entries;
            with.push(InfoEntry {
                long,
                short,
                rank: with.len(),
            });
            let packed = pack_info(
                &with,
                usize::from(right_w),
                usize::from(MAX_HEADER_ROWS - 2),
            );
            // Accept only when every other entry keeps its form.
            let base = cells_of(&info);
            if count(&packed) == count(&info) + 1
                && spark_widths.contains(&cells_of(&packed).saturating_sub(base))
            {
                height = cpu.rows().max(2 + cells(packed.len()));
                info = packed;
            }
        }
        Some(HeaderPlan {
            side: true,
            left_w,
            cpu,
            swap: true,
            info,
            spark,
            height,
        })
    } else {
        let cpu = cpu_layout(cores, width, 2, per_core);
        let swap = width >= 30 && sys.swap_total > 0;
        let used = cpu.rows() + 1 + u16::from(swap);
        let budget = if width < 40 { 1 } else { 2 }.min(MAX_HEADER_ROWS.saturating_sub(used));
        let info = pack_info(&entries, usize::from(width), usize::from(budget));
        Some(HeaderPlan {
            side: false,
            left_w: width,
            cpu,
            swap,
            height: used + cells(info.len()),
            info,
            spark: false,
        })
    }
}

/// Rows the meters header needs at `width` (0 hides it).
pub fn header_height(state: &ExplorerState, width: u16) -> u16 {
    plan_header(state, width).map_or(0, |p| p.height)
}

pub fn draw_header(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &ExplorerState) {
    let Some(plan) = plan_header(state, area.width) else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let sys = state.store.system();
    let left = Rect::new(area.x, area.y, plan.left_w, area.height);
    draw_cpu(frame, left, state, plan.cpu);

    if plan.spark {
        let x = left.x.saturating_add(4);
        if let Some(r) = row(area, x, plan.cpu.rows(), left.width.saturating_sub(4)) {
            let item = spark_item(state, "", usize::from(r.width.saturating_sub(1)));
            paint_items(frame, r, r.x, &[item]);
        }
    }

    let (info_x, info_y, info_w) = if plan.side {
        let x = area
            .x
            .saturating_add(plan.left_w)
            .saturating_add(COLUMN_GAP);
        (x, 0, area.width.saturating_sub(plan.left_w + COLUMN_GAP))
    } else {
        (area.x, plan.cpu.rows(), area.width)
    };
    let w = usize::from(info_w);
    let mut rel = info_y;
    let mem_long = format!("{}/{}", fmt::bytes(sys.mem_used), fmt::bytes(sys.mem_total));
    let mem_pct = ratio_percent(sys.mem_used, sys.mem_total);
    if let Some(r) = row(area, info_x, rel, info_w) {
        let item = bar(
            "Mem",
            w,
            mem_pct,
            &[&mem_long, &percent_text(mem_pct)],
            Hit::Meter(MeterKind::Mem),
        );
        paint_items(frame, r, r.x, &[item]);
    }
    rel += 1;
    if plan.swap {
        let swap_long = format!(
            "{}/{}",
            fmt::bytes(sys.swap_used),
            fmt::bytes(sys.swap_total)
        );
        let swap_pct = ratio_percent(sys.swap_used, sys.swap_total);
        if let Some(r) = row(area, info_x, rel, info_w) {
            let item = bar(
                "Swp",
                w,
                swap_pct,
                &[&swap_long, &percent_text(swap_pct)],
                Hit::Meter(MeterKind::Swap),
            );
            paint_items(frame, r, r.x, &[item]);
        }
        rel += 1;
    }
    for items in &plan.info {
        if let Some(r) = row(area, info_x, rel, info_w) {
            let mut x = r.x;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    x = paint_items(frame, r, x, &[Item::plain("  ", theme::base())]);
                }
                x = paint_items(frame, r, x, std::slice::from_ref(item));
            }
        }
        rel += 1;
    }
}

fn draw_cpu(frame: &mut Frame<'_, '_, '_>, left: Rect, state: &ExplorerState, layout: CpuLayout) {
    let sys = state.store.system();
    let total = f64::from(sys.cpu_total);
    let total_bar = |w: u16| {
        bar(
            "CPU",
            usize::from(w),
            total,
            &[&percent_text(total)],
            Hit::Meter(MeterKind::Cpu),
        )
    };
    match layout {
        CpuLayout::Total => {
            if let Some(r) = row(left, left.x, 0, left.width) {
                paint_items(frame, r, r.x, &[total_bar(left.width)]);
            }
        }
        CpuLayout::Cores { cols, rows } => {
            let cols = cols.max(1);
            let col_w = left.width.saturating_sub(cols - 1) / cols;
            let label_w = digits(sys.cpus.len().saturating_sub(1));
            for (i, value) in sys.cpus.iter().enumerate() {
                let col = cells(i) / rows.max(1);
                let rel = cells(i) % rows.max(1);
                let x = left.x.saturating_add(col.saturating_mul(col_w + 1));
                let Some(r) = row(left, x, rel, col_w) else {
                    continue;
                };
                let pct = f64::from(*value);
                let item = bar(
                    &format!("{i:>label_w$}"),
                    usize::from(col_w),
                    pct,
                    &[&percent_text(pct), &format!("{pct:.0}")],
                    Hit::Meter(MeterKind::Core(i)),
                );
                paint_items(frame, r, r.x, &[item]);
            }
        }
        CpuLayout::Strip { rows } => {
            if let Some(r) = row(left, left.x, 0, left.width) {
                paint_items(frame, r, r.x, &[total_bar(left.width)]);
            }
            let per_row = usize::from(left.width.saturating_sub(5).max(1));
            for (i, value) in sys.cpus.iter().enumerate() {
                let rel = cells(i / per_row);
                if rel >= rows {
                    break;
                }
                let x = left.x.saturating_add(4).saturating_add(cells(i % per_row));
                let Some(r) = row(left, x, rel + 1, 1) else {
                    break;
                };
                let (ch, style) = spark_cell(f64::from(*value));
                frame.text(r, ch, style);
                frame.hit(r, Hit::Meter(MeterKind::Core(i)));
            }
        }
    }
}

// ---- toolbar ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresetMode {
    All,
    Active,
    Hidden,
}

#[derive(Debug, Clone, Copy)]
struct ToolbarOpts {
    short_screens: bool,
    /// Inactive screen tabs shrink to two letters.
    tiny_screens: bool,
    short_presets: bool,
    short_chips: bool,
    dropped_chips: usize,
    presets: PresetMode,
    active_screen_only: bool,
}

/// Chips in drop order (first dropped first) when space runs out.
const CHIP_DROP_ORDER: [Chip; 8] = [
    Chip::KernelThreads,
    Chip::NetActive,
    Chip::Users,
    Chip::Tree,
    Chip::Dock,
    Chip::Freeze,
    Chip::Pause,
    Chip::Marks,
];

fn screen_short(screen: Screen) -> &'static str {
    match screen {
        Screen::Processes => "Procs",
        Screen::Events => "Events",
        Screen::Watches => "Watch",
    }
}

fn preset_short(preset: Preset) -> &'static str {
    match preset {
        Preset::Overview => "Ovw",
        Preset::Cpu => "CPU",
        Preset::Memory => "Mem",
        Preset::Disk => "Disk",
        Preset::Network => "Net",
        Preset::Custom => "Cust",
    }
}

fn badge(count: u64) -> String {
    if count > 99 {
        "99+".into()
    } else {
        count.to_string()
    }
}

/// Widest badge value, used to reserve room so the toolbar variant does not
/// change every time a new event arrives.
const WIDEST_BADGE: u64 = 100;

/// What the toolbar items are built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// Real labels and badges, for painting.
    Paint,
    /// Widest plausible badges, for choosing a variant that stays stable as
    /// counts grow.
    Measure,
}

fn screen_tabs(
    state: &ExplorerState,
    screen_now: Screen,
    opts: ToolbarOpts,
    pass: Pass,
) -> Vec<Item> {
    let unseen = state.events_view.unseen(&state.events);
    let alerting = state.alerting.len() as u64;
    let has_rules = !state.watch.rules.is_empty() || alerting > 0;
    let mut items = Vec::new();
    for screen in Screen::ALL {
        let active = screen_now == screen;
        if opts.active_screen_only && !active {
            continue;
        }
        if !items.is_empty() {
            items.push(Item::plain("│", theme::dim()));
        }
        let style = if active {
            theme::header_active()
        } else {
            theme::base()
        };
        let label = if opts.tiny_screens && !active {
            &screen.label()[..2]
        } else if opts.short_screens {
            screen_short(screen)
        } else {
            screen.label()
        };
        let mut item = Item::new(Hit::Screen(screen)).with(format!(" {label}"), style);
        let reserve = |n: u64, keep: bool| match pass {
            Pass::Paint => n,
            Pass::Measure if keep => WIDEST_BADGE,
            Pass::Measure => 0,
        };
        let (count, mark, color) = match screen {
            Screen::Processes => (0, "", Style::new()),
            Screen::Events => (reserve(unseen, true), " •", theme::accent()),
            Screen::Watches => (reserve(alerting, has_rules), " ⚠", theme::alert()),
        };
        if count > 0 {
            let badge_style = if active { style } else { color };
            item = item.with(format!("{mark}{}", badge(count)), badge_style);
        }
        items.push(item.with(" ", style));
    }
    items
}

struct ChipSpec {
    chip: Chip,
    long: String,
    short: String,
    style: Style,
}

fn chip_specs(state: &ExplorerState, screen: Screen) -> Vec<ChipSpec> {
    let s = &state.settings;
    let toggle = |on: bool| {
        if on {
            theme::chip_on()
        } else {
            theme::chip_off()
        }
    };
    let mut out = Vec::new();
    let mut push = |chip: Chip, long: &str, short: &str, style: Style| {
        out.push(ChipSpec {
            chip,
            long: long.into(),
            short: short.into(),
            style,
        })
    };
    let processes = screen == Screen::Processes;
    if processes {
        push(Chip::Tree, "Tree", "Tree", toggle(s.tree));
        push(Chip::Users, "Mine", "Mine", toggle(s.only_mine));
        let has_kthreads = cfg!(target_os = "linux")
            || state
                .store
                .rows()
                .iter()
                .any(|r| r.entry.record.kernel_thread);
        if has_kthreads {
            push(
                Chip::KernelThreads,
                "Kthreads",
                "Kth",
                toggle(!s.hide_kernel_threads),
            );
        }
        // Never just "Net": that is the Network preset's short label.
        push(
            Chip::NetActive,
            "Net-active",
            "Net-act",
            toggle(s.net_active_only),
        );
        if state.table.is_frozen() {
            push(Chip::Freeze, "Frozen ❄", "❄", theme::chip_on());
        }
    }
    if state.paused {
        let paused = Style::new()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        push(Chip::Pause, "Paused ⏸", "⏸", paused);
    }
    if processes {
        if state.detail.is_docked() {
            let (long, short) = match s.dock {
                crate::config::DockPosition::Bottom => ("Dock: bottom", "Dock"),
                crate::config::DockPosition::Right => ("Dock: right", "Dock"),
            };
            push(Chip::Dock, long, short, theme::chip_off());
        }
        let marks = state.table.marked().len();
        if marks > 0 {
            let style = Style::new()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD);
            push(
                Chip::Marks,
                &format!("{marks} marked ✕"),
                &format!("{marks} ✕"),
                style,
            );
        }
    }
    out
}

fn toolbar_items(
    state: &ExplorerState,
    screen: Screen,
    chips: &[ChipSpec],
    opts: ToolbarOpts,
    pass: Pass,
) -> Vec<Item> {
    let mut items = screen_tabs(state, screen, opts, pass);
    if screen == Screen::Processes && opts.presets != PresetMode::Hidden {
        items.push(Item::plain(" ", theme::base()));
        for preset in Preset::ALL {
            let active = preset == state.settings.preset;
            if opts.presets == PresetMode::Active && !active {
                continue;
            }
            let label = if opts.short_presets && opts.presets == PresetMode::All {
                preset_short(preset)
            } else {
                preset.label()
            };
            let style = if active {
                theme::header_active()
            } else {
                theme::dim()
            };
            items.push(Item::new(Hit::Preset(preset)).with(format!(" {label} "), style));
        }
    }
    let dropped = &CHIP_DROP_ORDER[..opts.dropped_chips.min(CHIP_DROP_ORDER.len())];
    for spec in chips.iter().filter(|c| !dropped.contains(&c.chip)) {
        let label = if opts.short_chips {
            &spec.short
        } else {
            &spec.long
        };
        items.push(Item::plain(" ", theme::base()));
        items.push(Item::new(Hit::Chip(spec.chip)).with(format!(" {label} "), spec.style));
    }
    items
}

/// Toolbar variants from richest to most compact.
fn toolbar_variants() -> Vec<ToolbarOpts> {
    let full = ToolbarOpts {
        short_screens: false,
        tiny_screens: false,
        short_presets: false,
        short_chips: false,
        dropped_chips: 0,
        presets: PresetMode::All,
        active_screen_only: false,
    };
    let mut out = vec![
        full,
        ToolbarOpts {
            short_chips: true,
            ..full
        },
        ToolbarOpts {
            short_chips: true,
            short_presets: true,
            ..full
        },
    ];
    let compact = ToolbarOpts {
        short_screens: true,
        short_presets: true,
        short_chips: true,
        ..full
    };
    // Toggle chips go first, then presets collapse to the active one while
    // the live-state chips (dock, freeze, pause, marks) hang on.
    for dropped in 0..=TOGGLE_CHIPS {
        out.push(ToolbarOpts {
            dropped_chips: dropped,
            ..compact
        });
    }
    for dropped in TOGGLE_CHIPS..=CHIP_DROP_ORDER.len() {
        out.push(ToolbarOpts {
            dropped_chips: dropped,
            presets: PresetMode::Active,
            ..compact
        });
    }
    // Last resorts: two-letter inactive tabs, no presets, the active tab only.
    let bare = ToolbarOpts {
        dropped_chips: CHIP_DROP_ORDER.len(),
        presets: PresetMode::Active,
        ..compact
    };
    let tiny = ToolbarOpts {
        tiny_screens: true,
        ..bare
    };
    out.push(tiny);
    out.push(ToolbarOpts {
        presets: PresetMode::Hidden,
        ..bare
    });
    out.push(ToolbarOpts {
        presets: PresetMode::Hidden,
        ..tiny
    });
    out.push(ToolbarOpts {
        presets: PresetMode::Hidden,
        active_screen_only: true,
        ..bare
    });
    out
}

/// Chips dropped first: toggles that are also reachable from F4/F5.
const TOGGLE_CHIPS: usize = 4;

/// The richest variant whose items fit `budget` cells. Fitting is judged
/// with the widest badges, and on other screens against the Processes
/// layout too, so screen tabs neither change form as events arrive nor move
/// when switching screens.
fn choose_toolbar(state: &ExplorerState, budget: usize) -> ToolbarOpts {
    let variants = toolbar_variants();
    let current = state.screen;
    let chips = chip_specs(state, current);
    let layout_chips = chip_specs(state, Screen::Processes);
    let width = |screen: Screen, chips: &[ChipSpec], opts: ToolbarOpts, pass: Pass| {
        items_width(&toolbar_items(state, screen, chips, opts, pass))
    };
    let stable = |opts: &ToolbarOpts| {
        width(current, &chips, *opts, Pass::Measure) <= budget
            && (current == Screen::Processes
                || width(Screen::Processes, &layout_chips, *opts, Pass::Measure) <= budget)
    };
    let fits = |opts: &ToolbarOpts| width(current, &chips, *opts, Pass::Paint) <= budget;
    let last = variants[variants.len() - 1];
    variants
        .iter()
        .copied()
        .find(stable)
        .or_else(|| variants.iter().copied().find(fits))
        .unwrap_or(last)
}

pub fn draw_toolbar(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &ExplorerState) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Keep a one-cell gap before whatever sits to the right (the search box).
    let budget = usize::from(area.width).saturating_sub(1);
    let opts = choose_toolbar(state, budget);
    let chips = chip_specs(state, state.screen);
    let items = toolbar_items(state, state.screen, &chips, opts, Pass::Paint);
    paint_items(frame, area, area.x, &items);
}

// ---- F-key bar --------------------------------------------------------------------

fn fkey_short(action: Action) -> &'static str {
    match action {
        Action::Settings => "Set",
        Action::Search => "Find",
        Action::FilterMenu => "Filt",
        Action::SortMenu => "Sort",
        Action::NiceDown => "N-",
        Action::NiceUp => "N+",
        _ => FKEYS
            .iter()
            .find(|(_, a, _)| *a == action)
            .map_or("", |(_, _, l)| l),
    }
}

/// Whether an F-key's action currently has something to act on.
fn fkey_enabled(state: &ExplorerState, action: Action) -> bool {
    !matches!(action, Action::NiceDown | Action::NiceUp | Action::Signal)
        || state.table.selected().is_some()
        || !state.table.marked().is_empty()
}

/// Indices into `FKEYS` and whether short labels are used for `width`.
fn fkey_selection(width: usize) -> (Vec<usize>, bool) {
    let need = |keys: &[usize], short: bool| -> usize {
        keys.iter()
            .map(|&i| {
                let (key, action, label) = FKEYS[i];
                display_width(key) + display_width(if short { fkey_short(action) } else { label })
            })
            .sum::<usize>()
            + keys.len().saturating_sub(1)
    };
    let mut keys: Vec<usize> = (0..FKEYS.len()).collect();
    if need(&keys, false) <= width {
        return (keys, false);
    }
    let index = |action: Action| FKEYS.iter().position(|(_, a, _)| *a == action);
    for group in FKEY_DROP_GROUPS {
        if need(&keys, true) <= width {
            break;
        }
        keys.retain(|i| !group.iter().any(|a| index(*a) == Some(*i)));
    }
    if need(&keys, true) > width {
        // Unlisted keys go last; Quit stays no matter what.
        keys.retain(|&i| FKEYS[i].1 == Action::Quit);
    }
    (keys, true)
}

/// F-keys dropped, a group at a time, when the bar is too narrow. Nice-/Nice+
/// go together and first; Kill stays longest beside Quit, which never goes.
const FKEY_DROP_GROUPS: &[&[Action]] = &[
    &[Action::NiceDown, Action::NiceUp],
    &[Action::SortMenu],
    &[Action::FilterMenu],
    &[Action::Settings],
    &[Action::Tree],
    &[Action::Search],
    &[Action::Help],
    &[Action::Signal],
];

pub fn draw_fkeys(frame: &mut Frame<'_, '_, '_>, area: Rect, state: &ExplorerState) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let (keys, short) = fkey_selection(width);
    let entries: Vec<(&str, Action, &str)> = keys
        .iter()
        .map(|&i| {
            let (key, action, label) = FKEYS[i];
            (key, action, if short { fkey_short(action) } else { label })
        })
        .collect();
    let need: usize = entries
        .iter()
        .map(|(k, _, l)| display_width(k) + display_width(l))
        .sum();
    let slack = width.saturating_sub(need);
    let n = entries.len().max(1);
    let mut x = area.x;
    for (i, (key, action, label)) in entries.iter().enumerate() {
        let extra = slack / n + usize::from(i < slack % n);
        let label_w = display_width(label) + extra;
        let style = if fkey_enabled(state, *action) {
            theme::key_label()
        } else {
            theme::dim()
        };
        let item = Item::new(Hit::FKey(*action))
            .with(*key, theme::key())
            .with(super::paint::fit(label, label_w, false), style);
        x = paint_items(frame, area, x, &[item]);
    }
}

// ---- status line ------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct StatusOpts {
    message: bool,
    short_counts: bool,
    counts: bool,
    short_note: bool,
    note: bool,
    short_right: bool,
    interface: bool,
    traffic: bool,
}

fn status_variants() -> Vec<StatusOpts> {
    let full = StatusOpts {
        message: true,
        short_counts: false,
        counts: true,
        short_note: false,
        note: true,
        short_right: false,
        interface: true,
        traffic: true,
    };
    let steps: [fn(StatusOpts) -> StatusOpts; 9] = [
        |o| o,
        |o| StatusOpts {
            short_right: true,
            ..o
        },
        |o| StatusOpts {
            short_counts: true,
            ..o
        },
        |o| StatusOpts {
            short_note: true,
            ..o
        },
        |o| StatusOpts {
            interface: false,
            ..o
        },
        |o| StatusOpts {
            traffic: false,
            ..o
        },
        |o| StatusOpts {
            message: false,
            ..o
        },
        |o| StatusOpts { counts: false, ..o },
        |o| StatusOpts { note: false, ..o },
    ];
    let mut out = Vec::new();
    let mut opts = full;
    for step in steps {
        opts = step(opts);
        out.push(opts);
    }
    out
}

fn traffic_label(traffic: TrafficType) -> &'static str {
    match traffic {
        TrafficType::All => "All",
        TrafficType::Internet => "Internet",
        TrafficType::Local => "Local",
    }
}

fn refresh_label(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// The message shown at the left of the status line, if any.
fn status_message(
    state: &ExplorerState,
    env: &DrawEnv<'_>,
    now: std::time::Instant,
) -> Option<Item> {
    if let Some((text, at)) = &state.flash
        && now.saturating_duration_since(*at) < FLASH_TTL
        && !text.is_empty()
    {
        let style = Style::new()
            .fg(Color::BrightWhite)
            .add_modifier(Modifier::BOLD);
        return Some(Item::plain(text.clone(), style));
    }
    (!env.status.is_empty()).then(|| Item::plain(env.status.to_string(), theme::dim()))
}

fn status_parts(
    state: &ExplorerState,
    env: &DrawEnv<'_>,
    opts: StatusOpts,
) -> (Vec<Item>, Vec<Item>) {
    let mut left = Vec::new();
    if opts.counts {
        let live = state.store.len_live();
        let showing = state.table.rows().iter().filter(|r| !r.ghost).count();
        let text = if opts.short_counts {
            format!("{showing}/{live}")
        } else {
            let mut t = format!("{live} processes · showing {showing}");
            let filter = state.search.text.trim();
            if !filter.is_empty() {
                t.push_str(&format!(" · filter: {}", truncate(filter, 24)));
            }
            t
        };
        // Exited (ghost) rows are visible but never counted as showing.
        let exited = state.table.rows().iter().filter(|r| r.ghost).count();
        let exited_text = match (exited, opts.short_counts) {
            (0, _) => String::new(),
            (n, true) => format!(" +{n}x"),
            (n, false) => format!(" · {n} exited"),
        };
        let faint = theme::dim().add_modifier(Modifier::ITALIC);
        left.push(Item::plain(text, theme::dim()).with(exited_text, faint));
    }
    if opts.note && state.network_note.is_some() {
        let text = if opts.short_note {
            "⚠ net"
        } else {
            "⚠ net capture off"
        };
        left.push(Item::new(Hit::StatusNote).with(text, Style::new().fg(Color::Yellow)));
    }
    let mut right = Vec::new();
    if opts.traffic {
        let label = traffic_label(state.traffic);
        let item = Item::new(Hit::FKey(Action::TrafficMode));
        right.push(if opts.short_right {
            item.with(label, theme::accent())
        } else {
            item.with("Traffic: ", theme::dim())
                .with(label, theme::accent())
        });
    }
    if opts.interface
        && let Some(iface) = env.interface_filter.as_deref().filter(|s| !s.is_empty())
    {
        let item = Item::new(Hit::FKey(Action::Interfaces));
        let name = truncate(iface, 16);
        right.push(if opts.short_right {
            item.with(name, theme::accent())
        } else {
            item.with("iface: ", theme::dim())
                .with(name, theme::accent())
        });
    }
    right.push(if state.paused {
        Item::new(Hit::Chip(Chip::Pause)).with("PAUSED", theme::alert())
    } else {
        Item::new(Hit::Chip(Chip::Pause))
            .with(refresh_label(state.settings.refresh_ms), theme::dim())
    });
    (left, right)
}

fn join(items: Vec<Item>, gap: &str) -> Vec<Item> {
    let mut out = Vec::with_capacity(items.len() * 2);
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(Item::plain(gap, theme::dim()));
        }
        out.push(item);
    }
    out
}

/// Minimum message cells worth showing before dropping other status parts.
const MIN_MESSAGE: usize = 16;

pub fn draw_status(
    frame: &mut Frame<'_, '_, '_>,
    area: Rect,
    state: &ExplorerState,
    env: &DrawEnv<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let message = status_message(state, env, frame.now);
    let variants = status_variants();
    let mut chosen = None;
    for opts in &variants {
        let (left, right) = status_parts(state, env, *opts);
        let left = join(left, "  ");
        let right = join(right, "  ");
        let fixed = 1 + items_width(&left) + items_width(&right) + 1;
        let gaps = usize::from(!left.is_empty()) * 2 + usize::from(!right.is_empty()) * 2;
        let msg_need = match (&message, opts.message) {
            (Some(m), true) => m.width().min(MIN_MESSAGE) + 2,
            _ => 0,
        };
        if fixed + gaps + msg_need <= width {
            chosen = Some((*opts, left, right, fixed + gaps));
            break;
        }
    }
    let (opts, left, right, fixed) = chosen.unwrap_or_else(|| {
        let opts = variants[variants.len() - 1];
        let (l, r) = status_parts(state, env, opts);
        (opts, join(l, "  "), join(r, "  "), 0)
    });

    let mut x = area.x.saturating_add(u16::from(area.width > 1));
    if opts.message
        && let Some(message) = &message
    {
        let room = width.saturating_sub(fixed + 2);
        let (text, style) = message
            .parts
            .first()
            .map_or((String::new(), theme::dim()), |(t, s)| (t.clone(), *s));
        if room > 0 {
            x = paint_items(frame, area, x, &[Item::plain(truncate(&text, room), style)]);
            x = paint_items(frame, area, x, &[Item::plain("  ", theme::base())]);
        }
    }
    paint_items(frame, area, x, &left);
    let right_w = cells(items_width(&right));
    let end = area.x.saturating_add(area.width);
    let right_x = end.saturating_sub(right_w.saturating_add(1)).max(area.x);
    paint_items(frame, area, right_x, &right);
}

// ---- mouse --------------------------------------------------------------------------

fn screen_action(screen: Screen) -> Action {
    match screen {
        Screen::Processes => Action::Processes,
        Screen::Events => Action::Events,
        Screen::Watches => Action::Watches,
    }
}

fn chip_action(chip: Chip) -> Action {
    match chip {
        Chip::Tree => Action::Tree,
        Chip::Users => Action::ToggleUsers,
        Chip::KernelThreads => Action::ToggleKernelThreads,
        Chip::NetActive => Action::ToggleNetActive,
        Chip::Freeze => Action::Freeze,
        Chip::Pause => Action::Pause,
        Chip::Dock => Action::ToggleDock,
        Chip::Marks => Action::ClearMarks,
    }
}

/// Switch to `preset` (keeping customisations when already active) sorted by `column`.
fn focus_preset(state: &mut ExplorerState, preset: Preset, column: ColumnId) {
    state.screen = Screen::Processes;
    if state.settings.preset != preset {
        state.set_preset(preset);
    }
    state.table.set_sort(SortSpec {
        column,
        descending: true,
    });
    state.flash(format!(
        "{} view · sorted by {}",
        preset.label(),
        column.title()
    ));
}

fn meter_click(state: &mut ExplorerState, meter: MeterKind) {
    match meter {
        MeterKind::Cpu | MeterKind::Core(_) => focus_preset(state, Preset::Cpu, ColumnId::Cpu),
        MeterKind::Mem | MeterKind::Swap => focus_preset(state, Preset::Memory, ColumnId::Rss),
        MeterKind::Net => focus_preset(state, Preset::Network, Preset::Network.default_sort()),
        MeterKind::Disk => focus_preset(state, Preset::Disk, Preset::Disk.default_sort()),
        MeterKind::Tasks => {
            state.screen = Screen::Processes;
            state.run(Action::Tree);
        }
        MeterKind::Load | MeterKind::Uptime => state.run(Action::Events),
    }
}

/// Toast text explaining why per-process network data is missing.
pub fn capture_help(note: &str) -> String {
    let fix = if cfg!(target_os = "linux") {
        "run bproc with sudo, or grant it CAP_NET_RAW + CAP_NET_ADMIN"
    } else if cfg!(target_os = "macos") {
        "run bproc with sudo (packet capture needs access to /dev/bpf)"
    } else {
        "run bproc with administrator privileges"
    };
    let note = note.trim().trim_end_matches('.');
    if note.is_empty() {
        format!("Per-process bandwidth needs packet capture: {fix}.")
    } else {
        format!("{note}. Per-process bandwidth needs packet capture: {fix}.")
    }
}

pub fn handle_mouse(state: &mut ExplorerState, click: &Click<'_>) -> Outcome {
    let wheel = click.wheel();
    if wheel != 0 {
        return match click.hit {
            Hit::Preset(_) => {
                state.run(if wheel > 0 {
                    Action::NextPreset
                } else {
                    Action::PrevPreset
                });
                Outcome::Consumed
            }
            Hit::Screen(_) => {
                let n = Screen::ALL.len();
                let i = Screen::ALL
                    .iter()
                    .position(|s| *s == state.screen)
                    .unwrap_or(0);
                let next = if wheel > 0 {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                state.run(screen_action(Screen::ALL[next]));
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        };
    }
    if !click.is_left() {
        return Outcome::Ignored;
    }
    match click.hit {
        Hit::FKey(action) => state.run(*action),
        Hit::Screen(screen) => state.run(screen_action(*screen)),
        Hit::Preset(preset) => {
            state.screen = Screen::Processes;
            if state.settings.preset != *preset {
                state.set_preset(*preset);
            }
        }
        Hit::Chip(chip) => state.run(chip_action(*chip)),
        Hit::Meter(meter) => meter_click(state, *meter),
        Hit::StatusNote => {
            let text = capture_help(state.network_note.as_deref().unwrap_or(""));
            state.toasts.push(Severity::Info, text);
        }
        _ => return Outcome::Ignored,
    }
    Outcome::Consumed
}

#[cfg(test)]
mod tests;
