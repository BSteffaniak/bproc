//! Tab content builders. Pure functions of explorer state: they return lines
//! (with hit regions) that the panel paints and scrolls.
use super::Tab;
use super::graph;
use super::text::{BLine, Seg, wrap};
use crate::procs::ProcessKey;
use crate::procs::detail::{DeepDetail, FileKind, Section};
use crate::procs::history::MetricHistory;
use crate::procs::model::{ProcState, fmt};
use crate::procs::store::Row;
use crate::procs::watch::EventKind;
use crate::ui::explorer::ExplorerState;
use crate::ui::explorer::hits::Hit;
use crate::ui::explorer::paint::{fit, theme, truncate};
use bmux_tui::style::{Color, Modifier, Style};

/// Inner width at which the overview splits into facts | graphs.
pub const TWO_COLUMN_MIN: usize = 84;
const LABEL_W: usize = 11;

pub enum Body {
    Lines(Vec<BLine>),
    /// Scrolling facts on the left, graphs fitted to the height on the right.
    Columns {
        left: Vec<BLine>,
        right: Vec<BLine>,
        left_width: u16,
    },
    /// Dim, centered explanation (unavailable section, loading, empty).
    Message(String),
}

impl Body {
    /// Rows that scroll.
    pub fn scroll_rows(&self) -> usize {
        match self {
            Self::Lines(lines) | Self::Columns { left: lines, .. } => lines.len(),
            Self::Message(_) => 0,
        }
    }
    /// The rows that scroll (indexes match `scroll_rows`).
    pub fn scroll_lines(&self) -> Option<&[BLine]> {
        match self {
            Self::Lines(lines) | Self::Columns { left: lines, .. } => Some(lines),
            Self::Message(_) => None,
        }
    }
    pub fn scroll_lines_mut(&mut self) -> Option<&mut Vec<BLine>> {
        match self {
            Self::Lines(lines) | Self::Columns { left: lines, .. } => Some(lines),
            Self::Message(_) => None,
        }
    }
}

pub struct Ctx<'a> {
    pub state: &'a ExplorerState,
    pub key: ProcessKey,
    pub row: Option<&'a Row>,
    pub detail: Option<&'a DeepDetail>,
    pub width: usize,
    pub height: usize,
}

pub fn build(cx: &Ctx<'_>, tab: Tab) -> Body {
    match tab {
        Tab::Overview => overview(cx),
        Tab::Threads => section(cx, "Threads", |d| &d.threads, |v, w| threads(v, w)),
        Tab::Files => section(cx, "Open files", |d| &d.files, |v, w| files(v, w)),
        Tab::Network => network(cx),
        Tab::Env => section(cx, "Environment", |d| &d.environment, |v, w| env(v, w)),
        Tab::Memory => section(cx, "Memory maps", |d| &d.memory_maps, |v, w| memory(v, w)),
        Tab::Tree => tree(cx),
        Tab::Limits => limits_tab(cx),
        Tab::Events => events(cx),
    }
}

/// Count shown next to a tab label, when known.
pub fn count(state: &ExplorerState, key: &ProcessKey, tab: Tab) -> Option<usize> {
    let detail = state.detail.detail();
    let row = state.store.get(key);
    match tab {
        Tab::Overview | Tab::Limits => None,
        Tab::Threads => detail
            .and_then(|d| d.threads.as_ref().ok().map(Vec::len))
            .or_else(|| row.and_then(|r| r.entry.sample.threads.map(|t| t as usize))),
        Tab::Files => detail
            .and_then(|d| d.files.as_ref().ok().map(Vec::len))
            .or_else(|| row.and_then(|r| r.entry.sample.open_fds.map(|t| t as usize))),
        Tab::Network => row.and_then(|r| r.net.as_ref().map(|n| n.connections.len())),
        Tab::Env => detail.and_then(|d| d.environment.as_ref().ok().map(Vec::len)),
        Tab::Memory => detail.and_then(|d| d.memory_maps.as_ref().ok().map(Vec::len)),
        Tab::Tree => Some(state.store.children(key).len()),
        Tab::Events => Some(state.events.iter().filter(|e| e.key == Some(*key)).count()),
    }
}

/// Environment rows in display order (clicking row `i` copies entry `i`).
pub fn sorted_env(detail: &DeepDetail) -> Vec<(String, String)> {
    let mut vars = detail.environment.clone().unwrap_or_default();
    vars.sort();
    vars
}

fn section<T>(
    cx: &Ctx<'_>,
    title: &str,
    get: impl Fn(&DeepDetail) -> &Section<T>,
    render: impl Fn(&T, usize) -> Body,
) -> Body {
    match cx.detail {
        None => Body::Message("Loading…".into()),
        Some(d) => match get(d) {
            Ok(v) => render(v, cx.width),
            Err(reason) => Body::Message(format!("{title} unavailable\n{reason}")),
        },
    }
}

// ---- overview ----------------------------------------------------------------

fn overview(cx: &Ctx<'_>) -> Body {
    let Some(row) = cx.row else {
        return Body::Message(
            "Process exited\nIt is no longer reported by the system, so live data is gone.".into(),
        );
    };
    let history = cx.state.store.history(&cx.key);
    if cx.width >= TWO_COLUMN_MIN {
        let left_width = (cx.width / 2).clamp(40, 72);
        let right_width = cx.width.saturating_sub(left_width + 2);
        Body::Columns {
            left: facts(cx, row, left_width),
            right: graphs(history, row, right_width, cx.height),
            left_width: left_width as u16,
        }
    } else {
        let mut lines = graphs(history, row, cx.width, 0);
        lines.push(BLine::blank());
        lines.extend(facts(cx, row, cx.width));
        Body::Lines(lines)
    }
}

fn label(text: &str) -> Seg {
    Seg::new(fit(text, LABEL_W, false), theme::dim())
}

/// One overview fact before layout: wrapped text or a single styled row.
enum FactValue {
    Text(String, Style),
    Segs(Vec<Seg>),
}

struct Fact {
    name: String,
    value: FactValue,
}

impl Fact {
    fn text(name: &str, value: impl Into<String>, style: Style) -> Self {
        Self {
            name: name.into(),
            value: FactValue::Text(value.into(), style),
        }
    }
    fn segs(name: &str, segs: Vec<Seg>) -> Self {
        Self {
            name: name.into(),
            value: FactValue::Segs(segs),
        }
    }
}

/// Label column width for `facts`: the longest label plus a one-cell gap,
/// at least `LABEL_W`, and at most a third of `width` (never below
/// `LABEL_W`).
fn label_width(facts: &[Fact], width: usize) -> usize {
    let longest = facts
        .iter()
        .map(|f| bmux_tui::text_width::display_width(&f.name))
        .max()
        .unwrap_or(0);
    (longest + 1).clamp(LABEL_W, (width / 3).max(LABEL_W))
}

/// `label value` rows, wrapping text values under themselves.
fn layout_facts(facts: &[Fact], width: usize) -> Vec<BLine> {
    let label_w = label_width(facts, width);
    let head = |name: &str| {
        Seg::new(
            format!("{} ", fit(name, label_w.saturating_sub(1), false)),
            theme::dim(),
        )
    };
    let avail = width.saturating_sub(label_w).max(1);
    let mut out = Vec::new();
    for f in facts {
        match &f.value {
            FactValue::Text(value, style) => {
                for (i, part) in wrap(value, avail).into_iter().enumerate() {
                    let name = if i == 0 { f.name.as_str() } else { "" };
                    out.push(BLine::new(vec![head(name), Seg::new(part, *style)]));
                }
            }
            FactValue::Segs(segs) => {
                let mut line = vec![head(&f.name)];
                line.extend(segs.iter().cloned());
                out.push(BLine::new(line));
            }
        }
    }
    out
}

/// Detail facts the summary already shows (or the warning line under the
/// tab bar does); merging them would repeat the same fact twice.
const SUMMARY_FACTS: [&str; 12] = [
    "Executable",
    "Working dir",
    "Command",
    "User",
    "Parent",
    "State",
    "Started",
    "Threads",
    "Priority",
    "Nice",
    "Access",
    "Truncated",
];

pub fn state_style(state: ProcState) -> Style {
    match state {
        ProcState::Running => theme::ok().add_modifier(Modifier::BOLD),
        ProcState::DiskSleep => Style::new().fg(Color::Yellow),
        ProcState::Stopped | ProcState::Tracing => Style::new().fg(Color::Yellow),
        ProcState::Zombie | ProcState::Dead => theme::alert(),
        ProcState::Idle | ProcState::Unknown => theme::dim(),
        ProcState::Sleeping => theme::base(),
    }
}

pub fn user_label(row: &Row) -> String {
    let record = &row.entry.record;
    match (&record.user, record.uid) {
        (Some(user), Some(uid)) => format!("{user} (uid {uid})"),
        (Some(user), None) => user.clone(),
        (None, Some(uid)) => format!("uid {uid}"),
        (None, None) => "–".into(),
    }
}

/// Compact age such as `45s`, `3m 12s`, `2h 13m`, `4d 6h`.
pub fn age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {}s", secs / 60, secs % 60),
        3600..86_400 => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

fn local_time(secs: u64, format: &str) -> Option<String> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(i64::try_from(secs).ok()?, 0)
        .single()
        .map(|t| t.format(format).to_string())
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "–".into())
}

fn now_secs(state: &ExplorerState) -> u64 {
    let ms = state.store.acquired_at_ms();
    if ms == 0 {
        crate::procs::model::now_ms() / 1000
    } else {
        ms / 1000
    }
}

fn facts(cx: &Ctx<'_>, row: &Row, width: usize) -> Vec<BLine> {
    layout_facts(&fact_list(cx, row), width)
}

fn fact_list(cx: &Ctx<'_>, row: &Row) -> Vec<Fact> {
    let record = &row.entry.record;
    let s = &row.entry.sample;
    let base = theme::base();
    let detail_fact = |name: &str| cx.detail.and_then(|d| d.fact(name));
    let mut out = vec![Fact::text("PID", record.key.pid.to_string(), theme::bold())];
    match record.ppid {
        Some(ppid) => {
            let mut segs = Vec::new();
            match cx.state.store.by_pid(ppid) {
                Some(parent) => {
                    segs.push(Seg::link(
                        ppid.to_string(),
                        theme::accent().add_modifier(Modifier::UNDERLINE),
                        Hit::ProcessLink { key: parent.key() },
                    ));
                    segs.push(Seg::new(format!(" ({})", parent.name()), theme::dim()));
                }
                None => segs.push(Seg::new(ppid.to_string(), base)),
            }
            out.push(Fact::segs("PPID", segs));
        }
        None => out.push(Fact::text(
            "PPID",
            detail_fact("Parent").unwrap_or("–"),
            base,
        )),
    }
    let mut user = user_label(row);
    if user == "–"
        && let Some(v) = detail_fact("User")
    {
        user = v.to_string();
    }
    out.push(Fact::text("User", user, base));
    let state_text = if row.is_ghost() {
        "Exited".to_string()
    } else {
        s.state.label().to_string()
    };
    out.push(Fact::text("State", state_text, state_style(s.state)));
    let started = if record.key.start_time == 0 {
        detail_fact("Started").unwrap_or("–").to_string()
    } else {
        let when = local_time(record.key.start_time, "%Y-%m-%d %H:%M:%S").unwrap_or_default();
        let ago = now_secs(cx.state).saturating_sub(record.key.start_time);
        format!("{when} · {} ago", age(ago))
    };
    out.push(Fact::text("Started", started, base));
    let elapsed_ms = now_secs(cx.state)
        .saturating_sub(record.key.start_time)
        .saturating_mul(1000);
    let avg = if record.key.start_time > 0 && elapsed_ms > 0 {
        format!(
            " · {}% avg",
            fmt::percent(s.cpu_time_ms as f32 / elapsed_ms as f32 * 100.0)
        )
    } else {
        String::new()
    };
    out.push(Fact::segs(
        "CPU",
        vec![
            Seg::new(
                format!("{}%", fmt::percent(s.cpu_percent)),
                Style::new()
                    .fg(theme::heat(f64::from(s.cpu_percent)))
                    .add_modifier(Modifier::BOLD),
            ),
            Seg::new(format!(" now{avg}"), theme::dim()),
        ],
    ));
    out.push(Fact::text("CPU time", fmt::cpu_time(s.cpu_time_ms), base));
    let threads = s
        .threads
        .map(|t| t.to_string())
        .or_else(|| detail_fact("Threads").map(str::to_string))
        .unwrap_or_else(|| "–".into());
    out.push(Fact::text("Threads", threads, base));
    let nice = s
        .nice
        .map(|n| n.to_string())
        .or_else(|| detail_fact("Nice").map(str::to_string))
        .unwrap_or_else(|| "–".into());
    out.push(Fact::text(
        "Priority",
        format!("nice {nice} · pri {}", opt(s.priority)),
        base,
    ));
    out.push(Fact::text(
        "Memory",
        format!(
            "RSS {} · VIRT {} · {}%",
            fmt::bytes(s.mem_rss),
            fmt::bytes(s.mem_virtual),
            fmt::percent(s.mem_percent)
        ),
        base,
    ));
    out.push(Fact::text(
        "Disk",
        format!(
            "R {} · W {} · total R {} W {}",
            fmt::rate(s.disk_read_rate),
            fmt::rate(s.disk_write_rate),
            fmt::bytes(s.disk_read_total),
            fmt::bytes(s.disk_write_total)
        ),
        base,
    ));
    if let Some(net) = &row.net {
        out.push(Fact::segs(
            "Network",
            vec![
                Seg::new(
                    format!("↓ {}", fmt::rate(net.download_rate)),
                    theme::download(),
                ),
                Seg::new(" ", base),
                Seg::new(format!("↑ {}", fmt::rate(net.upload_rate)), theme::upload()),
                Seg::new(
                    format!(
                        " · total ↓ {} ↑ {}",
                        fmt::bytes(net.total_download),
                        fmt::bytes(net.total_upload)
                    ),
                    theme::dim(),
                ),
            ],
        ));
    }
    out.push(Fact::text("Open files", opt(s.open_fds), base));
    if let Some(tty) = &record.tty {
        out.push(Fact::text("TTY", tty.clone(), base));
    }
    if let Some(group) = &record.group {
        out.push(Fact::text("Group", group.clone(), base));
    }
    let exe = record
        .exe
        .as_deref()
        .or_else(|| detail_fact("Executable"))
        .unwrap_or("–");
    out.push(Fact::text("Executable", exe, base));
    let cwd = record
        .cwd
        .as_deref()
        .or_else(|| detail_fact("Working dir"))
        .unwrap_or("–");
    out.push(Fact::text("Working dir", cwd, base));
    let command = if record.cmdline.is_empty() {
        detail_fact("Command")
            .map(str::to_string)
            .unwrap_or_else(|| record.command())
    } else {
        record.command()
    };
    out.push(Fact::text("Command", command, base));
    if let Some(detail) = cx.detail {
        for (name, value) in &detail.facts {
            let shown = SUMMARY_FACTS.contains(&name.as_str())
                || (name == "Group" && record.group.is_some())
                || (name == "Terminal" && record.tty.is_some());
            if !shown {
                out.push(Fact::text(name, value.clone(), base));
            }
        }
    }
    out
}

struct Series {
    label: &'static str,
    values: Vec<f64>,
    current: String,
    max_label: String,
    max: f64,
    style: Style,
}

fn series(history: Option<&MetricHistory>, row: &Row) -> Vec<Series> {
    let s = &row.entry.sample;
    let bytes = |label, ring: Option<&crate::procs::history::Ring<u64>>, cur: u64, style| {
        let values: Vec<f64> = ring
            .map(|r| r.iter().map(|v| v as f64).collect())
            .unwrap_or_default();
        let max = ring.map(|r| r.max()).unwrap_or(0).max(cur);
        Series {
            label,
            values,
            current: fmt::rate(cur),
            max_label: fmt::rate(max),
            max: max.max(1) as f64,
            style,
        }
    };
    let cpu_values: Vec<f64> = history
        .map(|h| h.cpu.iter().map(f64::from).collect())
        .unwrap_or_default();
    let cpu_max = history
        .map(|h| h.cpu.max())
        .unwrap_or(0.0)
        .max(s.cpu_percent);
    let mem_values: Vec<f64> = history
        .map(|h| h.mem_rss.iter().map(|v| v as f64).collect())
        .unwrap_or_default();
    let mem_max = history.map(|h| h.mem_rss.max()).unwrap_or(0).max(s.mem_rss);
    let mut out = vec![
        Series {
            label: "CPU",
            values: cpu_values,
            current: format!("{}%", fmt::percent(s.cpu_percent)),
            max_label: format!("{}%", fmt::percent(cpu_max)),
            max: f64::from(cpu_max.max(1.0)),
            style: theme::accent(),
        },
        Series {
            label: "Memory",
            values: mem_values,
            current: fmt::bytes(s.mem_rss),
            max_label: fmt::bytes(mem_max),
            max: mem_max.max(1) as f64,
            style: Style::new().fg(Color::Magenta),
        },
        bytes(
            "Disk R",
            history.map(|h| &h.disk_read),
            s.disk_read_rate,
            Style::new().fg(Color::Blue),
        ),
        bytes(
            "Disk W",
            history.map(|h| &h.disk_write),
            s.disk_write_rate,
            Style::new().fg(Color::BrightMagenta),
        ),
    ];
    if let Some(net) = &row.net {
        out.push(bytes(
            "Net ↓",
            history.map(|h| &h.net_down),
            net.download_rate,
            theme::download(),
        ));
        out.push(bytes(
            "Net ↑",
            history.map(|h| &h.net_up),
            net.upload_rate,
            theme::upload(),
        ));
    }
    out
}

/// Sparklines for the overview. With `height` room for at least three rows
/// per series each gets a label row and a multi-row graph; otherwise one
/// compact row per series.
fn graphs(history: Option<&MetricHistory>, row: &Row, width: usize, height: usize) -> Vec<BLine> {
    let all = series(history, row);
    if width == 0 {
        return Vec::new();
    }
    let per = height / all.len().max(1);
    let mut out = Vec::new();
    if per >= 3 {
        let graph_h = (per - 1).min(6);
        for s in &all {
            let head = format!("{:<7}", s.label);
            out.push(BLine::new(vec![
                Seg::new(truncate(&head, width), s.style.add_modifier(Modifier::BOLD)),
                Seg::new(format!("{} ", s.current), theme::bold()),
                Seg::new(format!("max {}", s.max_label), theme::dim()),
            ]));
            for segs in graph::rows(&s.values, width, graph_h, s.max, s.style) {
                out.push(BLine::new(segs));
            }
        }
        return out;
    }
    const LABEL: usize = 7;
    const VALUES: usize = 22;
    let spark_w = width.saturating_sub(LABEL + VALUES);
    for s in &all {
        let mut segs = vec![Seg::new(
            fit(s.label, LABEL.min(width), false),
            s.style.add_modifier(Modifier::BOLD),
        )];
        if spark_w >= 4 {
            if let Some(row) = graph::rows(&s.values, spark_w, 1, s.max, s.style).pop() {
                segs.extend(row);
            }
            segs.push(Seg::new(fit(&s.current, 10, true), theme::bold()));
            segs.push(Seg::new(
                fit(&format!("max {}", s.max_label), 12, true),
                theme::dim(),
            ));
        } else {
            segs.push(Seg::new(format!(" {}", s.current), theme::bold()));
        }
        out.push(BLine::new(segs));
    }
    out
}

// ---- detail sections ---------------------------------------------------------

fn header(columns: &[(&str, usize, bool)]) -> BLine {
    BLine::new(vec![Seg::new(
        columns
            .iter()
            .map(|(t, w, r)| fit(t, *w, *r))
            .collect::<Vec<_>>()
            .join(" "),
        theme::dim().add_modifier(Modifier::BOLD | Modifier::UNDERLINE),
    )])
}

fn threads(list: &[crate::procs::detail::ThreadInfo], width: usize) -> Body {
    if list.is_empty() {
        return Body::Message("No threads reported".into());
    }
    let mut sorted: Vec<_> = list.iter().collect();
    sorted.sort_by(|a, b| {
        b.cpu_percent
            .unwrap_or(-1.0)
            .total_cmp(&a.cpu_percent.unwrap_or(-1.0))
            .then(a.tid.cmp(&b.tid))
    });
    let fixed = 8 + 11 + 7 + 10 + 5 + 5;
    let name_w = width.saturating_sub(fixed).max(4);
    let mut out = vec![header(&[
        ("TID", 8, true),
        ("NAME", name_w, false),
        ("STATE", 11, false),
        ("CPU%", 7, true),
        ("TIME", 10, true),
        ("PRI", 5, true),
    ])];
    for t in sorted {
        let cpu = t.cpu_percent.unwrap_or(0.0);
        out.push(BLine::new(vec![
            Seg::new(fit(&t.tid.to_string(), 8, true), theme::base()),
            Seg::new(" ", theme::base()),
            Seg::new(
                fit(t.name.as_deref().unwrap_or("–"), name_w, false),
                theme::base(),
            ),
            Seg::new(" ", theme::base()),
            Seg::new(fit(t.state.label(), 11, false), state_style(t.state)),
            Seg::new(" ", theme::base()),
            Seg::new(
                fit(&opt(t.cpu_percent.map(fmt::percent)), 7, true),
                Style::new().fg(theme::heat(f64::from(cpu))),
            ),
            Seg::new(" ", theme::base()),
            Seg::new(
                fit(&opt(t.cpu_time_ms.map(fmt::cpu_time)), 10, true),
                theme::base(),
            ),
            Seg::new(" ", theme::base()),
            Seg::new(fit(&opt(t.priority), 5, true), theme::dim()),
        ]));
    }
    Body::Lines(out)
}

fn file_kind_style(kind: FileKind) -> Style {
    match kind {
        FileKind::File => theme::base(),
        FileKind::Directory => Style::new().fg(Color::Blue),
        FileKind::Socket => theme::accent(),
        FileKind::Pipe => Style::new().fg(Color::Yellow),
        FileKind::Device => Style::new().fg(Color::Magenta),
        FileKind::Event | FileKind::Other => theme::dim(),
    }
}

fn files(list: &[crate::procs::detail::OpenFile], width: usize) -> Body {
    if list.is_empty() {
        return Body::Message("No open files".into());
    }
    const ORDER: [FileKind; 7] = [
        FileKind::File,
        FileKind::Directory,
        FileKind::Socket,
        FileKind::Pipe,
        FileKind::Device,
        FileKind::Event,
        FileKind::Other,
    ];
    let mut summary = vec![Seg::new(format!("{} open", list.len()), theme::bold())];
    for kind in ORDER {
        let n = list.iter().filter(|f| f.kind == kind).count();
        if n > 0 {
            summary.push(Seg::new(" · ", theme::dim()));
            summary.push(Seg::new(
                format!("{n} {}", kind.label()),
                file_kind_style(kind),
            ));
        }
    }
    let target_w = width.saturating_sub(6 + 7 + 2).max(4);
    let mut out = vec![
        BLine::new(summary),
        BLine::blank(),
        header(&[
            ("FD", 6, true),
            ("KIND", 7, false),
            ("TARGET", target_w, false),
        ]),
    ];
    let mut sorted: Vec<_> = list.iter().collect();
    sorted.sort_by_key(|f| (f.fd.is_none(), f.fd));
    for f in sorted {
        out.push(BLine::new(vec![
            Seg::new(fit(&opt(f.fd), 6, true), theme::dim()),
            Seg::new(" ", theme::base()),
            Seg::new(fit(f.kind.label(), 7, false), file_kind_style(f.kind)),
            Seg::new(" ", theme::base()),
            Seg::new(truncate(&f.target, target_w), theme::base()),
        ]));
    }
    Body::Lines(out)
}

fn env(vars: &[(String, String)], width: usize) -> Body {
    if vars.is_empty() {
        return Body::Message("Environment is empty".into());
    }
    let mut sorted = vars.to_vec();
    sorted.sort();
    let mut out = vec![
        BLine::plain(
            format!(
                "{} variables · click a row or press y to copy KEY=value",
                sorted.len()
            ),
            theme::dim(),
        ),
        BLine::blank(),
    ];
    for (i, (k, v)) in sorted.iter().enumerate() {
        let key_w = k.chars().count().min(width / 2);
        let k = truncate(k, key_w.max(1));
        let rest = width.saturating_sub(key_w + 3);
        out.push(
            BLine::new(vec![
                Seg::new(k, theme::accent()),
                Seg::new(" = ", theme::dim()),
                Seg::new(truncate(v, rest), theme::base()),
            ])
            .with_hit(Hit::DetailRow { row: i }),
        );
    }
    Body::Lines(out)
}

fn memory(maps: &[crate::procs::detail::MemoryMap], width: usize) -> Body {
    if maps.is_empty() {
        return Body::Message("No memory mappings reported".into());
    }
    let total: u64 = maps.iter().filter_map(|m| m.rss).sum();
    let range_w = 33;
    let path_w = width.saturating_sub(range_w + 5 + 8 + 3).max(4);
    let mut out = vec![
        BLine::new(vec![
            Seg::new(format!("{} mappings", maps.len()), theme::bold()),
            Seg::new(format!(" · RSS {}", fmt::bytes(total)), theme::dim()),
        ]),
        BLine::blank(),
        header(&[
            ("RANGE", range_w, false),
            ("PERM", 5, false),
            ("RSS", 8, true),
            ("PATH", path_w, false),
        ]),
    ];
    for m in maps {
        let perm_style = if m.perms.contains('x') {
            Style::new().fg(Color::Yellow)
        } else if m.perms.contains('w') {
            theme::accent()
        } else {
            theme::base()
        };
        out.push(BLine::new(vec![
            Seg::new(
                fit(&format!("{:016x}-{:016x}", m.start, m.end), range_w, false),
                theme::dim(),
            ),
            Seg::new(" ", theme::base()),
            Seg::new(fit(&m.perms, 5, false), perm_style),
            Seg::new(" ", theme::base()),
            Seg::new(fit(&opt(m.rss.map(fmt::bytes)), 8, true), theme::base()),
            Seg::new(" ", theme::base()),
            Seg::new(truncate(&m.path, path_w), theme::base()),
        ]));
    }
    Body::Lines(out)
}

fn limit_value(v: Option<u64>, unit: &str) -> String {
    match v {
        None => "unlimited".into(),
        Some(v) if unit.eq_ignore_ascii_case("bytes") => fmt::bytes(v),
        Some(v) => v.to_string(),
    }
}

/// Resource limits, followed by the process's cgroups when known.
fn limits_tab(cx: &Ctx<'_>) -> Body {
    let Some(d) = cx.detail else {
        return Body::Message("Loading…".into());
    };
    let cgroups = d.cgroups.as_ref().ok().filter(|c| !c.is_empty());
    let mut out = match &d.limits {
        Ok(list) if !list.is_empty() => limits(list, cx.width),
        Ok(_) if cgroups.is_none() => return Body::Message("No limits reported".into()),
        Err(reason) if cgroups.is_none() => {
            return Body::Message(format!("Limits unavailable\n{reason}"));
        }
        Ok(_) => vec![BLine::plain("No limits reported", theme::dim())],
        Err(reason) => vec![BLine::plain(
            truncate(&format!("Limits unavailable · {reason}"), cx.width),
            theme::dim(),
        )],
    };
    if let Some(cgroups) = cgroups {
        out.push(BLine::blank());
        out.push(header(&[("CGROUP", cx.width.max(1), false)]));
        for cgroup in cgroups {
            out.push(BLine::plain(truncate(cgroup, cx.width), theme::base()));
        }
    }
    Body::Lines(out)
}

fn limits(list: &[crate::procs::detail::ResourceLimit], width: usize) -> Vec<BLine> {
    let name_w = width.saturating_sub(12 + 12 + 10 + 3).clamp(8, 28);
    let mut out = vec![header(&[
        ("LIMIT", name_w, false),
        ("SOFT", 12, true),
        ("HARD", 12, true),
        ("UNIT", 10, false),
    ])];
    for l in list {
        let style = |v: Option<u64>| {
            if v.is_none() {
                theme::dim()
            } else {
                theme::base()
            }
        };
        out.push(BLine::new(vec![
            Seg::new(fit(&l.name, name_w, false), theme::base()),
            Seg::new(" ", theme::base()),
            Seg::new(fit(&limit_value(l.soft, &l.unit), 12, true), style(l.soft)),
            Seg::new(" ", theme::base()),
            Seg::new(fit(&limit_value(l.hard, &l.unit), 12, true), style(l.hard)),
            Seg::new(" ", theme::base()),
            Seg::new(fit(&l.unit, 10, false), theme::dim()),
        ]));
    }
    out
}

// ---- network / tree / events -------------------------------------------------

fn network(cx: &Ctx<'_>) -> Body {
    let Some(net) = cx.row.and_then(|r| r.net.as_ref()) else {
        return Body::Message(
            cx.state
                .network_note
                .clone()
                .unwrap_or_else(|| "No network activity observed".into()),
        );
    };
    let pair = |down: u64, up: u64| {
        vec![
            Seg::new(
                fit(&format!("↓ {}", fmt::rate(down)), 13, false),
                theme::download(),
            ),
            Seg::new(
                fit(&format!("↑ {}", fmt::rate(up)), 13, false),
                theme::upload(),
            ),
        ]
    };
    let mut out = Vec::new();
    let mut total = vec![label("Total")];
    total.extend(pair(net.download_rate, net.upload_rate));
    total.push(Seg::new(
        format!(
            "{} ↓ {} ↑",
            fmt::bytes(net.total_download),
            fmt::bytes(net.total_upload)
        ),
        theme::dim(),
    ));
    out.push(BLine::new(total));
    let mut internet = vec![label("Internet")];
    internet.extend(pair(net.internet_download_rate, net.internet_upload_rate));
    internet.push(Seg::new(
        format!(
            "{} ↓ {} ↑",
            fmt::bytes(net.internet_total_download),
            fmt::bytes(net.internet_total_upload)
        ),
        theme::dim(),
    ));
    out.push(BLine::new(internet));
    let mut local = vec![label("Local")];
    local.extend(pair(net.local_download_rate, net.local_upload_rate));
    local.push(Seg::new(
        format!(
            "{} ↓ {} ↑",
            fmt::bytes(net.local_total_download),
            fmt::bytes(net.local_total_upload)
        ),
        theme::dim(),
    ));
    out.push(BLine::new(local));
    if let Some(limit) = &net.throttle_limit {
        let fmt_limit = |v: Option<u64>| v.map(fmt::rate).unwrap_or_else(|| "–".into());
        out.push(BLine::new(vec![
            label("Throttle"),
            Seg::new(
                format!(
                    "↓ {} ↑ {}",
                    fmt_limit(limit.download_limit),
                    fmt_limit(limit.upload_limit)
                ),
                Style::new().fg(Color::Yellow),
            ),
        ]));
    }
    if !net.interface_stats.is_empty() {
        out.push(BLine::blank());
        out.push(header(&[
            ("INTERFACE", LABEL_W - 1, false),
            ("RATE", 26, false),
        ]));
        let mut names: Vec<&String> = net.interface_stats.keys().collect();
        names.sort();
        for name in names {
            let st = &net.interface_stats[name];
            let mut segs = vec![Seg::new(fit(name, LABEL_W, false), theme::base())];
            segs.extend(pair(st.download_rate, st.upload_rate));
            segs.push(Seg::new(
                format!(
                    "{} ↓ {} ↑",
                    fmt::bytes(st.total_download),
                    fmt::bytes(st.total_upload)
                ),
                theme::dim(),
            ));
            out.push(BLine::new(segs));
        }
    }
    out.push(BLine::blank());
    if net.connections.is_empty() {
        out.push(BLine::plain("No open connections", theme::dim()));
        return Body::Lines(out);
    }
    let addr_w = (cx.width.saturating_sub(6 + 12 + 3) / 2).clamp(8, 46);
    out.push(header(&[
        ("PROTO", 6, false),
        ("LOCAL", addr_w, false),
        ("REMOTE", addr_w, false),
        ("STATE", 12, false),
    ]));
    let endpoint = |ip: std::net::IpAddr, port: u16| match ip {
        std::net::IpAddr::V6(v6) => format!("[{v6}]:{port}"),
        std::net::IpAddr::V4(v4) => format!("{v4}:{port}"),
    };
    for c in &net.connections {
        let state_style = match c.state.as_str() {
            "ESTABLISHED" => theme::ok(),
            "LISTEN" => theme::accent(),
            "" => theme::dim(),
            _ => Style::new().fg(Color::Yellow),
        };
        out.push(BLine::new(vec![
            Seg::new(fit(&c.protocol, 6, false), theme::dim()),
            Seg::new(" ", theme::base()),
            Seg::new(
                fit(&endpoint(c.local_addr, c.local_port), addr_w, false),
                theme::base(),
            ),
            Seg::new(" ", theme::base()),
            Seg::new(
                fit(&endpoint(c.remote_addr, c.remote_port), addr_w, false),
                theme::base(),
            ),
            Seg::new(" ", theme::base()),
            Seg::new(
                fit(if c.state.is_empty() { "–" } else { &c.state }, 12, false),
                state_style,
            ),
        ]));
    }
    Body::Lines(out)
}

fn tree_line(cx: &Ctx<'_>, key: ProcessKey, prefix: String, current: bool) -> BLine {
    let row = cx.state.store.get(&key);
    let name = row
        .map(|r| r.name().to_string())
        .unwrap_or_else(|| format!("PID {}", key.pid));
    let stats = row
        .map(|r| {
            format!(
                "{:>7} {:>6}%  {:>7}",
                key.pid,
                fmt::percent(r.entry.sample.cpu_percent),
                fmt::bytes(r.entry.sample.mem_rss)
            )
        })
        .unwrap_or_default();
    let stats_w = stats.chars().count();
    let name_w = cx
        .width
        .saturating_sub(stats_w + 1 + prefix.chars().count())
        .max(1);
    let style = if current {
        theme::bold().patch(theme::accent())
    } else if row.is_some_and(Row::is_ghost) {
        theme::ghost()
    } else {
        theme::base()
    };
    let line = BLine::new(vec![
        Seg::new(prefix, theme::dim()),
        Seg::new(fit(&name, name_w, false), style),
        Seg::new(format!(" {stats}"), theme::dim()),
    ]);
    if current {
        line
    } else {
        line.with_hit(Hit::ProcessLink { key })
    }
}

fn tree(cx: &Ctx<'_>) -> Body {
    let mut chain = cx.state.store.ancestors(&cx.key);
    chain.reverse();
    let mut out = vec![BLine::plain("Click a process to inspect it", theme::dim())];
    out.push(BLine::blank());
    for (depth, key) in chain.iter().enumerate() {
        let prefix = if depth == 0 {
            String::new()
        } else {
            format!("{}└ ", "  ".repeat(depth - 1))
        };
        out.push(tree_line(cx, *key, prefix, false));
    }
    let depth = chain.len();
    let me = if depth == 0 {
        "● ".to_string()
    } else {
        format!("{}└ ● ", "  ".repeat(depth - 1))
    };
    out.push(tree_line(cx, cx.key, me, true));
    let mut children = cx.state.store.children(&cx.key);
    let cpu = |k: &ProcessKey| {
        cx.state
            .store
            .get(k)
            .map(|r| r.entry.sample.cpu_percent)
            .unwrap_or(0.0)
    };
    children.sort_by(|a, b| cpu(b).total_cmp(&cpu(a)).then(a.pid.cmp(&b.pid)));
    let indent = "  ".repeat(depth);
    let n = children.len();
    for (i, child) in children.into_iter().enumerate() {
        let branch = if i + 1 == n { "└ " } else { "├ " };
        let grand = cx.state.store.children(&child).len();
        let mut line = tree_line(cx, child, format!("{indent}{branch}"), false);
        if grand > 0 {
            line.segs
                .push(Seg::new(format!("  +{grand}"), theme::dim()));
        }
        out.push(line);
    }
    if n == 0 {
        out.push(BLine::plain(format!("{indent}(no children)"), theme::dim()));
    }
    Body::Lines(out)
}

fn kind_style(kind: EventKind) -> (Style, &'static str) {
    match kind {
        EventKind::Started => (theme::ok(), "Started"),
        EventKind::Exited => (theme::dim(), "Exited"),
        EventKind::Alert => (theme::alert(), "Alert"),
        EventKind::Action => (theme::accent(), "Action"),
        EventKind::Info => (theme::base(), "Info"),
    }
}

fn events(cx: &Ctx<'_>) -> Body {
    let mut out = Vec::new();
    for event in cx
        .state
        .events
        .iter()
        .rev()
        .filter(|e| e.key == Some(cx.key))
    {
        let (style, kind) = kind_style(event.kind);
        let when = local_time(event.at_ms / 1000, "%H:%M:%S").unwrap_or_default();
        let msg_w = cx.width.saturating_sub(9 + 8).max(1);
        out.push(BLine::new(vec![
            Seg::new(format!("{when:<8} "), theme::dim()),
            Seg::new(fit(kind, 8, false), style),
            Seg::new(truncate(&event.message, msg_w), theme::base()),
        ]));
    }
    if out.is_empty() {
        Body::Message("No events recorded for this process yet".into())
    } else {
        Body::Lines(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_are_compact() {
        assert_eq!(age(5), "5s");
        assert_eq!(age(192), "3m 12s");
        assert_eq!(age(7_980), "2h 13m");
        assert_eq!(age(367_200), "4d 6h");
    }

    #[test]
    fn limits_format_units() {
        assert_eq!(limit_value(None, "bytes"), "unlimited");
        assert_eq!(limit_value(Some(8 * 1024 * 1024), "bytes"), "8.0M");
        assert_eq!(limit_value(Some(1024), "files"), "1024");
    }
}
