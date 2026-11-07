//! Headless subcommands: `ps`, `tree`, `signal` and `watch`.
use crate::procs::actions::{self, ActionRequest, Signal};
use crate::procs::columns::{ColumnCtx, ColumnId};
use crate::procs::export::{self, ExportFormat};
use crate::procs::filter::Filter;
use crate::procs::model::now_ms;
use crate::procs::platform::create_sampler;
use crate::procs::store::{ProcessStore, SortSpec, UserScope, ViewRow, ViewSpec};
use crate::procs::watch::{
    Comparison, WatchAction, WatchEngine, WatchMetric, WatchRule, WatchTarget,
};
use crate::procs::{ProcSnapshot, ProcessKey};
use anyhow::{Context, Result, anyhow, bail};
use clap::{ArgGroup, Args, Subcommand};
use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub enum Command {
    /// List processes with bproc's columns, sorting and filter language
    Ps(PsArgs),
    /// Show the process tree (same options as `ps`)
    Tree(PsArgs),
    /// Send a signal to a process after verifying its identity
    Signal(SignalArgs),
    /// Watch processes headlessly and print an alert line whenever a rule fires
    Watch(WatchArgs),
}

#[derive(Args, Debug, Clone, PartialEq)]
pub struct PsArgs {
    /// Sort column: cpu, mem, rss, pid, name, time, read, write or any filter key
    #[arg(long, short, value_name = "COLUMN", default_value = "cpu", value_parser = parse_column)]
    pub sort: ColumnId,
    /// Sort ascending instead of the column's natural order
    #[arg(long)]
    pub asc: bool,
    /// Show at most N processes
    #[arg(long, short = 'n', value_name = "N")]
    pub limit: Option<usize>,
    /// Filter query, e.g. "node cpu>5 -user:root"
    #[arg(long, short, value_name = "QUERY")]
    pub filter: Option<String>,
    /// Comma-separated columns, e.g. pid,user,cpu,mem,command
    #[arg(long, short, value_name = "LIST", value_delimiter = ',', value_parser = parse_column)]
    pub columns: Option<Vec<ColumnId>>,
    /// Show the process hierarchy
    #[arg(long, short)]
    pub tree: bool,
    /// Output JSON
    #[arg(long, conflicts_with = "csv")]
    pub json: bool,
    /// Output CSV
    #[arg(long)]
    pub csv: bool,
    /// Include kernel threads
    #[arg(long, short)]
    pub all: bool,
    /// Never truncate the command column
    #[arg(long, short)]
    pub wide: bool,
}

#[derive(Args, Debug, Clone, PartialEq)]
pub struct SignalArgs {
    /// Target process ID
    pub pid: i32,
    /// Signal: term, kill, hup, int, quit, stop, cont, usr1, usr2 (or SIGTERM, 15, ...)
    #[arg(long, short, value_name = "SIGNAL", default_value = "term", value_parser = parse_signal)]
    pub signal: Signal,
    /// Also signal every descendant of the process
    #[arg(long)]
    pub tree: bool,
}

#[derive(Args, Debug, Clone, PartialEq)]
#[command(group(ArgGroup::new("target").required(true).args(["name", "command", "pid", "user", "any"])))]
#[command(group(ArgGroup::new("threshold").args(["above", "below"])))]
pub struct WatchArgs {
    /// Process name glob, e.g. "node*"
    #[arg(long, value_name = "GLOB")]
    pub name: Option<String>,
    /// Substring of the command line
    #[arg(long, value_name = "SUBSTR")]
    pub command: Option<String>,
    /// One specific process
    #[arg(long, value_name = "PID")]
    pub pid: Option<i32>,
    /// Processes owned by this user
    #[arg(long, value_name = "USER")]
    pub user: Option<String>,
    /// Every process
    #[arg(long)]
    pub any: bool,
    /// cpu, mem, rss, read, write, down, up, threads, fds, exits or starts
    #[arg(long, short, value_name = "METRIC", value_parser = parse_metric)]
    pub metric: WatchMetric,
    /// Alert when the metric is above this value (sizes like 500M accepted)
    #[arg(long, value_name = "X")]
    pub above: Option<String>,
    /// Alert when the metric is below this value
    #[arg(long, value_name = "X")]
    pub below: Option<String>,
    /// Condition must hold this long, e.g. 30s, 5m
    #[arg(long = "for", value_name = "DURATION", default_value = "0", value_parser = parse_duration)]
    pub sustain: Duration,
    /// Minimum time between repeated alerts for the same process
    #[arg(long, value_name = "DURATION", default_value = "60s", value_parser = parse_duration)]
    pub cooldown: Duration,
    /// Sampling interval, e.g. 1.5 or 500ms
    #[arg(long, short, value_name = "SECONDS", default_value = "1.5", value_parser = parse_duration)]
    pub interval: Duration,
    /// notify, log, or a signal to send (term, kill, stop, ...)
    #[arg(long, short, value_name = "ACTION", default_value = "notify", value_parser = parse_action)]
    pub action: WatchAction,
    /// Exit after N alerts
    #[arg(long, value_name = "N")]
    pub count: Option<u64>,
}

/// Run a subcommand to completion.
pub async fn run(command: Command) -> Result<()> {
    match command {
        Command::Ps(args) => ps(&args),
        Command::Tree(args) => ps(&PsArgs { tree: true, ..args }),
        Command::Signal(args) => signal(&args),
        Command::Watch(args) => watch(&args).await,
    }
}

// ---- parsing helpers -------------------------------------------------------------

/// "30s", "5m", "1h", "1h30m", "250ms", "2d" or plain (fractional) seconds.
pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let s = input.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err("empty duration".into());
    }
    if let Ok(secs) = s.parse::<f64>() {
        return secs_to_duration(secs, input);
    }
    let mut total = 0.0;
    let mut rest = s.as_str();
    while !rest.is_empty() {
        let split = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .ok_or_else(|| format!("missing unit in \"{input}\""))?;
        let (num, tail) = rest.split_at(split);
        let value: f64 = num
            .parse()
            .map_err(|_| format!("invalid duration \"{input}\" (try 30s, 5m, 1h)"))?;
        let unit_len = tail
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(tail.len());
        let (unit, next) = tail.split_at(unit_len);
        let scale = match unit {
            "ms" => 0.001,
            "s" | "sec" | "secs" => 1.0,
            "m" | "min" | "mins" => 60.0,
            "h" | "hr" | "hrs" => 3600.0,
            "d" | "day" | "days" => 86_400.0,
            _ => {
                return Err(format!(
                    "unknown unit \"{unit}\" in \"{input}\" (use ms, s, m, h, d)"
                ));
            }
        };
        total += value * scale;
        rest = next;
    }
    secs_to_duration(total, input)
}

fn secs_to_duration(secs: f64, input: &str) -> Result<Duration, String> {
    if (0.0..1e12).contains(&secs) {
        Ok(Duration::from_secs_f64(secs))
    } else {
        Err(format!("invalid duration \"{input}\""))
    }
}

/// "500M", "1.5G", "10K", "2MiB", "512" (bytes). Units are binary (1K = 1024).
pub fn parse_size(input: &str) -> Result<u64, String> {
    let s = input.trim().to_ascii_uppercase();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num
        .parse()
        .map_err(|_| format!("invalid size \"{input}\" (try 500M, 1.5G)"))?;
    let unit = unit.trim();
    let unit = unit
        .strip_suffix("IB")
        .or_else(|| unit.strip_suffix('B'))
        .unwrap_or(unit);
    let power = match unit {
        "" => 0,
        "K" => 1,
        "M" => 2,
        "G" => 3,
        "T" => 4,
        "P" => 5,
        _ => return Err(format!("unknown size unit in \"{input}\" (use K, M, G, T)")),
    };
    let bytes = value * 1024f64.powi(power);
    if (0.0..u64::MAX as f64).contains(&bytes) {
        Ok(bytes as u64)
    } else {
        Err(format!("size out of range: \"{input}\""))
    }
}

/// Signal by short name, full name or number: term, SIGTERM, 15.
pub fn parse_signal(input: &str) -> Result<Signal, String> {
    let s = input.trim().to_ascii_uppercase();
    let bare = s.strip_prefix("SIG").unwrap_or(&s);
    let by_name = Signal::ALL
        .into_iter()
        .find(|sig| sig.name().strip_prefix("SIG") == Some(bare));
    #[cfg(unix)]
    let by_number = bare
        .parse::<i32>()
        .ok()
        .and_then(|n| Signal::ALL.into_iter().find(|sig| sig.number() == n));
    #[cfg(not(unix))]
    let by_number = None;
    by_name.or(by_number).ok_or_else(|| {
        format!("unknown signal \"{input}\" (term, kill, hup, int, quit, stop, cont, usr1, usr2)")
    })
}

/// Column by filter key, header title or a common alias.
pub fn parse_column(input: &str) -> Result<ColumnId, String> {
    let key = input.trim();
    let alias = match key.to_ascii_lowercase().as_str() {
        "command" | "args" => Some(ColumnId::Command),
        "comm" => Some(ColumnId::Name),
        "res" | "resident" => Some(ColumnId::Rss),
        "memory" | "%mem" => Some(ColumnId::Mem),
        "%cpu" => Some(ColumnId::Cpu),
        "cputime" => Some(ColumnId::CpuTime),
        "start" | "started" => Some(ColumnId::Started),
        "thr" | "nlwp" => Some(ColumnId::Threads),
        "stat" | "s" => Some(ColumnId::State),
        "ni" => Some(ColumnId::Nice),
        _ => None,
    };
    alias
        .or_else(|| ColumnId::from_filter_key(key))
        .or_else(|| {
            crate::procs::columns::ALL_COLUMNS
                .iter()
                .copied()
                .find(|c| c.title().eq_ignore_ascii_case(key))
        })
        .ok_or_else(|| {
            format!("unknown column \"{input}\" (try pid, user, cpu, mem, rss, time, command)")
        })
}

pub fn parse_metric(input: &str) -> Result<WatchMetric, String> {
    WatchMetric::from_key(input).ok_or_else(|| {
        format!("unknown metric \"{input}\" (cpu, mem, rss, read, write, down, up, threads, fds, exits, starts)")
    })
}

pub fn parse_action(input: &str) -> Result<WatchAction, String> {
    match input.trim().to_ascii_lowercase().as_str() {
        "notify" | "print" => Ok(WatchAction::Notify),
        "log" => Ok(WatchAction::Log),
        other => parse_signal(other).map(WatchAction::Signal).map_err(|_| {
            format!("unknown action \"{input}\" (notify, log, term, kill, stop, ...)")
        }),
    }
}

/// Threshold in the metric's unit; byte metrics accept sizes like "500M".
pub fn parse_threshold(metric: WatchMetric, input: &str) -> Result<f64, String> {
    let trimmed = input.trim().trim_end_matches('%').trim_end_matches("/s");
    if metric.is_bytes() {
        return parse_size(trimmed).map(|b| b as f64);
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("invalid threshold \"{input}\" for {}", metric.short_label()))
}

// ---- ps / tree -------------------------------------------------------------------

const DEFAULT_COLUMNS: &[ColumnId] = &[
    ColumnId::Pid,
    ColumnId::User,
    ColumnId::Cpu,
    ColumnId::Mem,
    ColumnId::Rss,
    ColumnId::State,
    ColumnId::CpuTime,
    ColumnId::Command,
];

/// Take two samples ~500ms apart so CPU% and rates are meaningful.
fn sample_store(settle: Option<Duration>) -> Result<ProcessStore> {
    let mut sampler = create_sampler();
    if let Some(wait) = settle {
        sampler.sample().context("sampling processes")?;
        std::thread::sleep(wait);
    }
    let raw = sampler.sample().context("sampling processes")?;
    let mut store = ProcessStore::new();
    store.apply_snapshot(ProcSnapshot {
        sequence: 1,
        acquired_at_ms: now_ms(),
        interval_secs: settle.map(|d| d.as_secs_f32()).unwrap_or(0.0),
        system: raw.system,
        processes: raw.processes,
        permission_limited: raw.permission_limited,
        ..Default::default()
    });
    Ok(store)
}

fn column_ctx(store: &ProcessStore) -> ColumnCtx {
    ColumnCtx {
        now_ms: store.acquired_at_ms(),
        mem_total: store.system().mem_total,
        cpu_count: store.system().cpus.len().max(1),
        ..ColumnCtx::default()
    }
}

/// Ordered, filtered, limited rows for `ps`.
fn ps_rows(store: &ProcessStore, args: &PsArgs, filter: Option<&Filter>) -> Vec<ViewRow> {
    let collapsed = HashSet::new();
    let spec = ViewSpec {
        sort: SortSpec {
            column: args.sort,
            descending: if args.asc {
                false
            } else {
                args.sort.default_descending()
            },
        },
        filter,
        tree: args.tree,
        collapsed: &collapsed,
        hide_kernel_threads: !args.all,
        users: UserScope::All,
        net_active_only: false,
        pinned: &[],
        frozen_order: None,
        ctx: column_ctx(store),
    };
    let mut rows = store.view(&spec);
    if let Some(limit) = args.limit {
        rows.truncate(limit);
    }
    rows
}

fn ps(args: &PsArgs) -> Result<()> {
    let filter = match args.filter.as_deref().map(str::trim) {
        Some(q) if !q.is_empty() => Some(Filter::parse(q).map_err(|e| {
            anyhow!(
                "invalid filter: {} (at \"{}\")",
                e.message,
                q.get(e.span.0..e.span.1).unwrap_or(q)
            )
        })?),
        _ => None,
    };
    let store = sample_store(Some(Duration::from_millis(500)))?;
    let rows = ps_rows(&store, args, filter.as_ref());
    let columns = args
        .columns
        .clone()
        .unwrap_or_else(|| DEFAULT_COLUMNS.to_vec());
    let ctx = column_ctx(&store);
    let output = if args.json || args.csv {
        let format = if args.json {
            ExportFormat::Json
        } else {
            ExportFormat::Csv
        };
        export::render(&store, &rows, &columns, &ctx, format)
    } else {
        let width = if args.wide { None } else { output_width() };
        render_table(&store, &rows, &columns, &ctx, width)
    };
    let mut out = std::io::stdout().lock();
    out.write_all(output.as_bytes())
        .and_then(|()| {
            if output.ends_with('\n') {
                Ok(())
            } else {
                out.write_all(b"\n")
            }
        })
        .or_else(ignore_broken_pipe)?;
    Ok(())
}

fn ignore_broken_pipe(error: std::io::Error) -> std::io::Result<()> {
    if error.kind() == std::io::ErrorKind::BrokenPipe {
        Ok(())
    } else {
        Err(error)
    }
}

/// Width budget: `COLUMNS`, else the terminal size, else unlimited when piped.
fn output_width() -> Option<usize> {
    if let Some(cols) = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|&c| c > 0)
    {
        return Some(cols);
    }
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return None;
    }
    Some(terminal_width().unwrap_or(120))
}

#[cfg(unix)]
fn terminal_width() -> Option<usize> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    (ok && size.ws_col > 0).then_some(size.ws_col as usize)
}

#[cfg(not(unix))]
fn terminal_width() -> Option<usize> {
    None
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// Aligned plain-text table. Flexible columns (command, exe) absorb the
/// remaining `width`; `None` never truncates.
pub fn render_table(
    store: &ProcessStore,
    rows: &[ViewRow],
    columns: &[ColumnId],
    ctx: &ColumnCtx,
    width: Option<usize>,
) -> String {
    const MAX_FIXED: usize = 32;
    if columns.is_empty() {
        return String::new();
    }
    let tree_column = columns
        .iter()
        .position(|c| *c == ColumnId::Command)
        .or_else(|| columns.iter().position(|c| *c == ColumnId::Name));
    let cells: Vec<Vec<String>> = rows
        .iter()
        .filter_map(|view| {
            let row = store.get(&view.key)?;
            Some(
                columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let text = c.text(row, ctx);
                        if Some(i) == tree_column && !view.tree_prefix.is_empty() {
                            format!("{}{text}", view.tree_prefix)
                        } else {
                            text
                        }
                    })
                    .collect(),
            )
        })
        .collect();
    let mut widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let natural = cells
                .iter()
                .map(|r| r[i].chars().count())
                .chain([c.title().chars().count()])
                .max()
                .unwrap_or(0);
            if c.is_flexible() {
                natural
            } else {
                natural.min(MAX_FIXED)
            }
        })
        .collect();
    if let Some(budget) = width {
        let gaps = columns.len().saturating_sub(1);
        let fixed: usize = columns
            .iter()
            .zip(&widths)
            .filter(|(c, _)| !c.is_flexible())
            .map(|(_, w)| *w)
            .sum();
        let flexible: Vec<usize> = (0..columns.len())
            .filter(|&i| columns[i].is_flexible())
            .collect();
        if !flexible.is_empty() {
            let share = budget.saturating_sub(fixed + gaps) / flexible.len();
            for i in flexible {
                widths[i] = widths[i].min(share.max(columns[i].title().chars().count()).max(10));
            }
        }
    }
    let last = columns.len() - 1;
    let line = |texts: &mut dyn Iterator<Item = (usize, String)>| {
        let mut out = String::new();
        for (i, text) in texts {
            let text = truncate(&text, widths[i]);
            let pad = widths[i].saturating_sub(text.chars().count());
            if i > 0 {
                out.push(' ');
            }
            match columns[i].align() {
                crate::procs::columns::Align::Right => {
                    out.push_str(&" ".repeat(pad));
                    out.push_str(&text);
                }
                crate::procs::columns::Align::Left => {
                    out.push_str(&text);
                    if i != last {
                        out.push_str(&" ".repeat(pad));
                    }
                }
            }
        }
        out.truncate(out.trim_end().len());
        out.push('\n');
        out
    };
    let mut out = line(&mut columns.iter().map(|c| c.title().to_string()).enumerate());
    for row in cells {
        out.push_str(&line(&mut row.into_iter().enumerate()));
    }
    out
}

// ---- signal ----------------------------------------------------------------------

fn signal(args: &SignalArgs) -> Result<()> {
    let start = actions::current_start_time(args.pid)
        .ok_or_else(|| anyhow!("no process with PID {}", args.pid))?;
    let store = sample_store(None).ok();
    let key = store
        .as_ref()
        .and_then(|s| s.by_pid(args.pid))
        .map(|r| r.key())
        .unwrap_or(ProcessKey::new(args.pid, start));
    let mut targets = vec![key];
    if args.tree
        && let Some(store) = &store
    {
        targets.extend(store.descendants(&key));
    }
    let request = ActionRequest::Signal {
        targets,
        signal: args.signal,
    };
    let names = |k: &ProcessKey| {
        store
            .as_ref()
            .and_then(|s| s.get(k))
            .map(|r| r.name().to_string())
            .unwrap_or_else(|| format!("PID {}", k.pid))
    };
    let report = actions::execute(&request, &names);
    if report.outcomes.len() > 1 {
        for outcome in &report.outcomes {
            match &outcome.result {
                Ok(()) => println!("  {} ({}) ok", outcome.name, outcome.key.pid),
                Err(e) => eprintln!("  {} ({}) failed: {e}", outcome.name, outcome.key.pid),
            }
        }
    }
    let failed = report.failed().count();
    if failed == 0 {
        println!("{}", report.summary());
        Ok(())
    } else {
        bail!("{}", report.summary())
    }
}

// ---- watch -----------------------------------------------------------------------

/// Build the single rule a `watch` invocation describes.
pub fn watch_rule(
    args: &WatchArgs,
    resolve: &dyn Fn(i32) -> Option<ProcessKey>,
) -> Result<WatchRule> {
    let target = if let Some(pid) = args.pid {
        let key = resolve(pid).ok_or_else(|| anyhow!("no process with PID {pid}"))?;
        WatchTarget::Process {
            key,
            name: format!("PID {pid}"),
        }
    } else if let Some(name) = &args.name {
        WatchTarget::Name(name.clone())
    } else if let Some(command) = &args.command {
        WatchTarget::Command(command.clone())
    } else if let Some(user) = &args.user {
        WatchTarget::User(user.clone())
    } else {
        WatchTarget::Any
    };
    let (comparison, threshold) = match (&args.above, &args.below) {
        (Some(x), _) => (
            Comparison::Above,
            parse_threshold(args.metric, x).map_err(|e| anyhow!(e))?,
        ),
        (None, Some(x)) => (
            Comparison::Below,
            parse_threshold(args.metric, x).map_err(|e| anyhow!(e))?,
        ),
        (None, None) if args.metric.is_event() => (Comparison::Above, 0.0),
        (None, None) => bail!(
            "--above or --below is required for --metric {}",
            args.metric.key()
        ),
    };
    Ok(
        WatchRule::new(1, "watch", target, args.metric, comparison, threshold)
            .with_sustain(args.sustain.as_secs())
            .with_cooldown(args.cooldown.as_secs())
            .with_action(args.action),
    )
}

fn timestamp(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

async fn watch(args: &WatchArgs) -> Result<()> {
    let mut rule = watch_rule(args, &|pid| {
        actions::current_start_time(pid).map(|start| ProcessKey::new(pid, start))
    })?;
    let watched_pid = args.pid;
    let (collector, mut snapshots) = crate::procs::collector::spawn(args.interval);
    let mut store = ProcessStore::new();
    let mut engine = WatchEngine::default();
    let mut fired = 0u64;
    let mut announced = false;
    let mut result = Ok(());
    loop {
        let snapshot = tokio::select! {
            snapshot = snapshots.recv() => match snapshot {
                Some(snapshot) => snapshot,
                None => {
                    result = Err(anyhow!("process collector stopped"));
                    break;
                }
            },
            _ = tokio::signal::ctrl_c() => break,
        };
        let evaluate = snapshot.clone();
        store.apply_snapshot(snapshot);
        if !announced {
            // Re-key a --pid target with the collector's identity and real name.
            if let Some(pid) = watched_pid {
                match store.by_pid(pid) {
                    Some(row) => {
                        rule.target = WatchTarget::Process {
                            key: row.key(),
                            name: row.name().to_string(),
                        }
                    }
                    None => {
                        result = Err(anyhow!("no process with PID {pid}"));
                        break;
                    }
                }
            }
            eprintln!(
                "Watching: {} (every {:.1}s, Ctrl+C to stop)",
                rule.describe(),
                args.interval.as_secs_f64()
            );
            engine.upsert(rule.clone());
            announced = true;
        }
        for firing in engine.evaluate(&evaluate, &store) {
            println!(
                "[{}] {}",
                timestamp(evaluate.acquired_at_ms),
                firing.message
            );
            if let WatchAction::Signal(signal) = firing.action {
                let request = ActionRequest::Signal {
                    targets: vec![firing.key],
                    signal,
                };
                let report = actions::execute(&request, &|_| firing.name.clone());
                println!("[{}]   {}", timestamp(now_ms()), report.summary());
            }
            fired += 1;
        }
        let _ = std::io::stdout().flush();
        if args.count.is_some_and(|n| fired >= n) {
            break;
        }
        if let WatchTarget::Process { key, name } = &rule.target
            && store.get(key).is_none_or(|r| r.is_ghost())
        {
            eprintln!("{name} ({}) exited; stopping", key.pid);
            break;
        }
    }
    drop(snapshots);
    collector.shutdown();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Args as Cli;
    use crate::procs::model::{ProcessEntry, ProcessRecord, ProcessSample, SystemSnapshot};
    use clap::Parser;
    use std::sync::Arc;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("bproc").chain(args.iter().copied()))
    }

    fn command(args: &[&str]) -> Command {
        parse(args).expect("parses").command.expect("subcommand")
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_duration("1h30m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse_duration("45"), Ok(Duration::from_secs(45)));
        assert_eq!(parse_duration("1.5"), Ok(Duration::from_millis(1500)));
        assert_eq!(parse_duration("250ms"), Ok(Duration::from_millis(250)));
        assert_eq!(parse_duration(" 2D "), Ok(Duration::from_secs(172_800)));
        assert_eq!(parse_duration("0"), Ok(Duration::ZERO));
        for bad in ["", "abc", "5x", "-3", "1e99", "s", "1.2.3s"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("512"), Ok(512));
        assert_eq!(parse_size("10K"), Ok(10 * 1024));
        assert_eq!(parse_size("500M"), Ok(500 * 1024 * 1024));
        assert_eq!(parse_size("500mb"), Ok(500 * 1024 * 1024));
        assert_eq!(parse_size("2MiB"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_size("1.5G"), Ok(1536 * 1024 * 1024));
        assert_eq!(parse_size("1 T"), Ok(1 << 40));
        for bad in ["", "M", "12Q", "-5M", "1e30P"] {
            assert!(parse_size(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn signals_and_actions() {
        assert_eq!(parse_signal("term"), Ok(Signal::Term));
        assert_eq!(parse_signal("SIGKILL"), Ok(Signal::Kill));
        assert_eq!(parse_signal("Usr2"), Ok(Signal::Usr2));
        assert_eq!(parse_signal("9"), Ok(Signal::Kill));
        assert!(parse_signal("sigfoo").is_err());
        assert_eq!(parse_action("notify"), Ok(WatchAction::Notify));
        assert_eq!(parse_action("LOG"), Ok(WatchAction::Log));
        assert_eq!(parse_action("stop"), Ok(WatchAction::Signal(Signal::Stop)));
        assert!(parse_action("explode").is_err());
    }

    #[test]
    fn columns_and_thresholds() {
        assert_eq!(parse_column("cpu"), Ok(ColumnId::Cpu));
        assert_eq!(parse_column("COMMAND"), Ok(ColumnId::Command));
        assert_eq!(parse_column("time"), Ok(ColumnId::CpuTime));
        assert_eq!(parse_column("TIME+"), Ok(ColumnId::CpuTime));
        assert_eq!(parse_column("read"), Ok(ColumnId::DiskRead));
        assert_eq!(parse_column("RES"), Ok(ColumnId::Rss));
        assert!(parse_column("bogus").is_err());
        assert_eq!(parse_threshold(WatchMetric::CpuPercent, "80%"), Ok(80.0));
        assert_eq!(
            parse_threshold(WatchMetric::MemRss, "1G"),
            Ok((1u64 << 30) as f64)
        );
        assert_eq!(
            parse_threshold(WatchMetric::DiskRead, "10M/s"),
            Ok(10.0 * 1024.0 * 1024.0)
        );
        assert!(parse_threshold(WatchMetric::Threads, "lots").is_err());
    }

    #[test]
    fn legacy_flags_still_parse() {
        let args = parse(&["--pid", "123", "--download-limit", "1M"]).unwrap();
        assert_eq!(args.pid, Some(123));
        assert_eq!(args.download_limit.as_deref(), Some("1M"));
        assert!(args.command.is_none());
        assert!(parse(&["--list-backends"]).unwrap().list_backends);
        assert!(parse(&[]).unwrap().command.is_none());
    }

    #[test]
    fn ps_and_tree_parse() {
        let Command::Ps(ps) = command(&[
            "ps",
            "--sort",
            "mem",
            "--asc",
            "--limit",
            "5",
            "--filter",
            "node cpu>1",
            "--columns",
            "pid,user,cpu,command",
            "--csv",
            "--all",
        ]) else {
            panic!("expected ps");
        };
        assert_eq!(ps.sort, ColumnId::Mem);
        assert!(ps.asc && ps.csv && ps.all && !ps.json && !ps.tree);
        assert_eq!(ps.limit, Some(5));
        assert_eq!(ps.filter.as_deref(), Some("node cpu>1"));
        assert_eq!(
            ps.columns,
            Some(vec![
                ColumnId::Pid,
                ColumnId::User,
                ColumnId::Cpu,
                ColumnId::Command
            ])
        );
        let Command::Ps(defaults) = command(&["ps"]) else {
            panic!("expected ps");
        };
        assert_eq!(defaults.sort, ColumnId::Cpu);
        assert!(defaults.columns.is_none());
        assert!(matches!(
            command(&["tree", "-n", "3"]),
            Command::Tree(PsArgs { limit: Some(3), .. })
        ));
        assert!(parse(&["ps", "--json", "--csv"]).is_err());
        assert!(parse(&["ps", "--sort", "nonsense"]).is_err());
    }

    #[test]
    fn signal_parses() {
        assert_eq!(
            command(&["signal", "4242"]),
            Command::Signal(SignalArgs {
                pid: 4242,
                signal: Signal::Term,
                tree: false
            })
        );
        assert_eq!(
            command(&["signal", "7", "--signal", "kill", "--tree"]),
            Command::Signal(SignalArgs {
                pid: 7,
                signal: Signal::Kill,
                tree: true
            })
        );
        assert!(parse(&["signal"]).is_err());
        assert!(parse(&["signal", "1", "--signal", "nope"]).is_err());
    }

    #[test]
    fn watch_parses_and_builds_rules() {
        let Command::Watch(w) = command(&[
            "watch",
            "--name",
            "node*",
            "--metric",
            "cpu",
            "--above",
            "80",
            "--for",
            "30s",
            "--action",
            "term",
            "--count",
            "2",
            "--interval",
            "2",
        ]) else {
            panic!("expected watch");
        };
        assert_eq!(w.sustain, Duration::from_secs(30));
        assert_eq!(w.interval, Duration::from_secs(2));
        assert_eq!(w.count, Some(2));
        let rule = watch_rule(&w, &|_| None).unwrap();
        assert_eq!(rule.target, WatchTarget::Name("node*".into()));
        assert_eq!(rule.threshold, 80.0);
        assert_eq!(rule.sustain_secs, 30);
        assert_eq!(rule.action, WatchAction::Signal(Signal::Term));
        assert_eq!(
            rule.describe(),
            "CPU % above 80 for 30s · name node* → send SIGTERM"
        );

        let Command::Watch(w) =
            command(&["watch", "--pid", "9", "--metric", "rss", "--below", "500M"])
        else {
            panic!("expected watch");
        };
        let rule = watch_rule(&w, &|pid| Some(ProcessKey::new(pid, 77))).unwrap();
        assert!(
            matches!(rule.target, WatchTarget::Process { key, .. } if key == ProcessKey::new(9, 77))
        );
        assert_eq!(rule.comparison, Comparison::Below);
        assert_eq!(rule.threshold, (500u64 << 20) as f64);
        assert_eq!(rule.sustain_secs, 0);
        assert_eq!(rule.cooldown_secs, 60);
        assert!(
            watch_rule(&w, &|_| None).is_err(),
            "missing pid is an error"
        );

        let Command::Watch(w) = command(&["watch", "--command", "worker", "--metric", "exits"])
        else {
            panic!("expected watch");
        };
        assert_eq!(
            watch_rule(&w, &|_| None).unwrap().metric,
            WatchMetric::Exits
        );
        let Command::Watch(w) = command(&["watch", "--any", "--metric", "fds"]) else {
            panic!("expected watch");
        };
        assert!(
            watch_rule(&w, &|_| None).is_err(),
            "threshold metrics need a bound"
        );

        assert!(
            parse(&["watch", "--metric", "cpu", "--above", "1"]).is_err(),
            "target required"
        );
        assert!(
            parse(&[
                "watch", "--name", "a", "--pid", "1", "--metric", "cpu", "--above", "1"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "watch", "--name", "a", "--metric", "cpu", "--above", "1", "--below", "2"
            ])
            .is_err()
        );
        assert!(parse(&["watch", "--name", "a", "--metric", "volts", "--above", "1"]).is_err());
    }

    fn fixture() -> ProcessStore {
        let entry = |pid: i32, ppid: i32, name: &str, cmd: &str, cpu: f32| ProcessEntry {
            record: Arc::new(ProcessRecord {
                key: ProcessKey::new(pid, 1000),
                ppid: Some(ppid),
                name: name.into(),
                cmdline: cmd.split(' ').map(String::from).collect(),
                user: Some("alice".into()),
                kernel_thread: name == "kworker",
                ..Default::default()
            }),
            sample: ProcessSample {
                cpu_percent: cpu,
                mem_rss: pid as u64 * 1024 * 1024,
                ..Default::default()
            },
        };
        ProcessStore::from_entries(
            vec![
                entry(1, 0, "init", "/sbin/init", 0.5),
                entry(
                    20,
                    1,
                    "node",
                    "/usr/local/bin/node /srv/app/server.js --port 8080",
                    42.0,
                ),
                entry(300, 1, "zsh", "-zsh", 3.0),
                entry(2, 0, "kworker", "", 0.0),
            ],
            SystemSnapshot::default(),
        )
    }

    fn ps_args(extra: &[&str]) -> PsArgs {
        let mut argv = vec!["ps"];
        argv.extend_from_slice(extra);
        match command(&argv) {
            Command::Ps(a) => a,
            _ => unreachable!(),
        }
    }

    #[test]
    fn ps_rows_sort_limit_and_kernel_threads() {
        let store = fixture();
        let pids = |rows: Vec<ViewRow>| rows.iter().map(|r| r.key.pid).collect::<Vec<_>>();
        assert_eq!(pids(ps_rows(&store, &ps_args(&[]), None)), vec![20, 300, 1]);
        assert_eq!(
            pids(ps_rows(&store, &ps_args(&["--all"]), None)),
            vec![20, 300, 1, 2]
        );
        assert_eq!(
            pids(ps_rows(&store, &ps_args(&["--sort", "pid"]), None)),
            vec![1, 20, 300]
        );
        assert_eq!(
            pids(ps_rows(&store, &ps_args(&["--sort", "cpu", "--asc"]), None)),
            vec![1, 300, 20]
        );
        assert_eq!(
            pids(ps_rows(&store, &ps_args(&["-n", "1"]), None)),
            vec![20]
        );
        let filter = Filter::parse("node").unwrap();
        assert_eq!(
            pids(ps_rows(&store, &ps_args(&[]), Some(&filter))),
            vec![20]
        );
    }

    #[test]
    fn table_is_aligned_and_truncated() {
        let store = fixture();
        let rows = ps_rows(&store, &ps_args(&["--sort", "pid"]), None);
        let columns = [
            ColumnId::Pid,
            ColumnId::User,
            ColumnId::Cpu,
            ColumnId::Command,
        ];
        let ctx = column_ctx(&store);
        let wide = render_table(&store, &rows, &columns, &ctx, None);
        let lines: Vec<&str> = wide.lines().collect();
        assert_eq!(lines[0], "PID USER  CPU% COMMAND");
        assert_eq!(lines[1], "  1 alice  0.5 /sbin/init");
        assert_eq!(
            lines[2],
            " 20 alice 42.0 /usr/local/bin/node /srv/app/server.js --port 8080"
        );
        assert_eq!(lines.len(), 4);

        let narrow = render_table(&store, &rows, &columns, &ctx, Some(40));
        for line in narrow.lines() {
            assert!(line.chars().count() <= 40, "{line:?} exceeds 40");
        }
        assert!(narrow.lines().nth(2).unwrap().ends_with('…'));

        let tiny = render_table(&store, &rows, &columns, &ctx, Some(1));
        assert!(tiny.lines().nth(2).unwrap().contains("/usr/loca…"));
        assert_eq!(render_table(&store, &rows, &[], &ctx, Some(80)), "");
        assert_eq!(
            render_table(&store, &[], &columns, &ctx, Some(80)),
            "PID USER CPU% COMMAND\n"
        );
    }

    #[test]
    fn table_puts_tree_prefix_on_command() {
        let store = fixture();
        let rows = vec![ViewRow {
            key: ProcessKey::new(300, 1000),
            depth: 1,
            has_children: false,
            collapsed: false,
            tree_prefix: "└─ ".into(),
            pinned: false,
            ghost: false,
            highlights: Vec::new(),
            aggregate: None,
        }];
        let out = render_table(
            &store,
            &rows,
            &[ColumnId::Pid, ColumnId::Name],
            &column_ctx(&store),
            None,
        );
        assert_eq!(out.lines().nth(1), Some("300 └─ zsh"));
    }

    #[test]
    fn truncation_handles_edges() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("hello", 1), "…");
        assert_eq!(truncate("hello", 0), "");
    }
}
