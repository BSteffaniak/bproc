//! Joined process table: collector snapshots + network attribution + history.
//! The UI reads views from here; it never touches collectors directly.
use super::columns::{ColumnCtx, ColumnId, SortKey, tiebreak};
use super::filter::Filter;
use super::history::{MetricHistory, SystemHistory};
use super::model::{ProcSnapshot, ProcessEntry, ProcessKey, SystemSnapshot};
use crate::process::{ProcessInfo, ProcessMap};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// How long exited processes linger as greyed "ghost" rows.
pub const GHOST_MS: u64 = 4_000;

#[derive(Debug, Clone)]
pub struct Row {
    pub entry: ProcessEntry,
    /// Network attribution from the packet monitor, matched by PID.
    pub net: Option<Arc<ProcessInfo>>,
    /// Set when the process has exited; the row is shown greyed until expiry.
    pub exited_at_ms: Option<u64>,
}

impl Row {
    pub fn key(&self) -> ProcessKey {
        self.entry.record.key
    }
    pub fn is_ghost(&self) -> bool {
        self.exited_at_ms.is_some()
    }
    pub fn name(&self) -> &str {
        &self.entry.record.name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    pub column: ColumnId,
    pub descending: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            column: ColumnId::Cpu,
            descending: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UserScope {
    #[default]
    All,
    /// Only processes owned by this uid.
    Uid(u32),
}

/// Everything that shapes a table view. Cheap to build every frame.
pub struct ViewSpec<'a> {
    pub sort: SortSpec,
    pub filter: Option<&'a Filter>,
    pub tree: bool,
    pub collapsed: &'a HashSet<ProcessKey>,
    pub hide_kernel_threads: bool,
    pub users: UserScope,
    pub net_active_only: bool,
    /// Pinned processes float to the top (flat mode) in pin order.
    pub pinned: &'a [ProcessKey],
    /// When frozen, keep this order and append newcomers at the end.
    pub frozen_order: Option<&'a [ProcessKey]>,
    pub ctx: ColumnCtx,
}

/// Summed usage of a collapsed subtree.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Aggregate {
    pub processes: usize,
    pub cpu_percent: f32,
    pub mem_rss: u64,
    pub disk_read_rate: u64,
    pub disk_write_rate: u64,
    pub net_down: u64,
    pub net_up: u64,
}

impl Aggregate {
    fn add(&mut self, row: &Row) {
        let s = &row.entry.sample;
        self.processes += 1;
        self.cpu_percent += finite(s.cpu_percent);
        self.mem_rss = self.mem_rss.saturating_add(s.mem_rss);
        self.disk_read_rate = self.disk_read_rate.saturating_add(s.disk_read_rate);
        self.disk_write_rate = self.disk_write_rate.saturating_add(s.disk_write_rate);
        if let Some(n) = &row.net {
            self.net_down = self.net_down.saturating_add(n.download_rate);
            self.net_up = self.net_up.saturating_add(n.upload_rate);
        }
    }
}

/// Whole-machine sums over live processes (ghosts excluded).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Totals {
    pub processes: usize,
    /// Sum of thread counts; processes with an unknown count contribute one.
    pub threads: u64,
    /// Sum of per-process CPU (percent of one core).
    pub cpu_percent: f32,
    pub mem_rss: u64,
    pub disk_read_rate: u64,
    pub disk_write_rate: u64,
    /// All-traffic network rates from the packet monitor.
    pub net_down: u64,
    pub net_up: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewRow {
    pub key: ProcessKey,
    pub depth: u16,
    pub has_children: bool,
    pub collapsed: bool,
    /// Box-drawing prefix for tree mode, e.g. "│  ├─ ".
    pub tree_prefix: String,
    pub pinned: bool,
    pub ghost: bool,
    /// Char ranges in the Name/Command text matched by the filter's text terms.
    pub highlights: Vec<(usize, usize)>,
    /// Present for collapsed tree nodes with children.
    pub aggregate: Option<Aggregate>,
}

#[derive(Default)]
pub struct ProcessStore {
    rows: Vec<Row>,
    index: HashMap<ProcessKey, usize>,
    by_pid: HashMap<i32, ProcessKey>,
    children: HashMap<i32, Vec<ProcessKey>>,
    system: SystemSnapshot,
    system_history: SystemHistory,
    history: HashMap<ProcessKey, MetricHistory>,
    network: HashMap<i32, Arc<ProcessInfo>>,
    sequence: u64,
    acquired_at_ms: u64,
    permission_limited: usize,
}

fn finite(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// First position of every key in `keys` (later duplicates are ignored).
fn positions(keys: &[ProcessKey]) -> HashMap<ProcessKey, usize> {
    let mut map = HashMap::with_capacity(keys.len());
    for (i, key) in keys.iter().enumerate() {
        map.entry(*key).or_insert(i);
    }
    map
}

impl ProcessStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the live table with `snapshot`. Exited processes become ghost
    /// rows (keeping their last network attribution and history) for
    /// [`GHOST_MS`]; expired ghosts and their history are dropped.
    pub fn apply_snapshot(&mut self, snapshot: ProcSnapshot) {
        let now = snapshot.acquired_at_ms;
        let mut last_net: HashMap<ProcessKey, Arc<ProcessInfo>> = HashMap::new();
        let mut ghosts: Vec<Row> = Vec::new();
        for row in self.rows.drain(..) {
            match row.exited_at_ms {
                Some(t) if now.saturating_sub(t) < GHOST_MS => ghosts.push(row),
                Some(_) => {}
                None => {
                    if let Some(net) = &row.net {
                        last_net.insert(row.key(), net.clone());
                    }
                }
            }
        }
        let mut ghost_keys: HashSet<ProcessKey> = ghosts.iter().map(Row::key).collect();
        for exited in snapshot.exited {
            let key = exited.record.key;
            if !ghost_keys.insert(key) {
                continue;
            }
            ghosts.push(Row {
                net: last_net.remove(&key),
                entry: ProcessEntry {
                    record: exited.record,
                    sample: exited.last_sample,
                },
                exited_at_ms: Some(now),
            });
        }
        let mut live = HashSet::with_capacity(snapshot.processes.len());
        let mut rows: Vec<Row> = Vec::with_capacity(snapshot.processes.len() + ghosts.len());
        for entry in snapshot.processes {
            if !live.insert(entry.record.key) {
                continue;
            }
            rows.push(Row {
                net: self.network.get(&entry.record.key.pid).cloned(),
                entry,
                exited_at_ms: None,
            });
        }
        rows.extend(ghosts.into_iter().filter(|g| !live.contains(&g.key())));
        for row in rows.iter().filter(|r| !r.is_ghost()) {
            self.history
                .entry(row.key())
                .or_default()
                .push(&row.entry.sample, row.net.as_deref());
        }
        let retained: HashSet<ProcessKey> = rows.iter().map(Row::key).collect();
        self.history.retain(|key, _| retained.contains(key));
        self.system_history.push(&snapshot.system, &rows);
        self.rows = rows;
        self.system = snapshot.system;
        self.sequence = snapshot.sequence;
        self.acquired_at_ms = now;
        self.permission_limited = snapshot.permission_limited;
        self.reindex();
    }

    /// Attach the latest packet-monitor attribution (keyed by PID).
    pub fn merge_network(&mut self, map: &ProcessMap) {
        self.network = map
            .iter()
            .filter(|(_, p)| !p.is_terminated)
            .map(|(pid, p)| (*pid, Arc::new(p.clone())))
            .collect();
        for row in self.rows.iter_mut().filter(|r| !r.is_ghost()) {
            row.net = self.network.get(&row.entry.record.key.pid).cloned();
        }
    }

    fn reindex(&mut self) {
        self.index.clear();
        self.by_pid.clear();
        self.children.clear();
        for (i, row) in self.rows.iter().enumerate() {
            let key = row.key();
            self.index.insert(key, i);
            if row.is_ghost() {
                continue;
            }
            self.by_pid.insert(key.pid, key);
            if let Some(ppid) = row.entry.record.ppid
                && ppid != key.pid
            {
                self.children.entry(ppid).or_default().push(key);
            }
        }
        for kids in self.children.values_mut() {
            kids.sort_unstable_by(tiebreak);
        }
    }

    pub fn system(&self) -> &SystemSnapshot {
        &self.system
    }
    pub fn system_history(&self) -> &SystemHistory {
        &self.system_history
    }
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
    pub fn len_live(&self) -> usize {
        self.by_pid.len()
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn acquired_at_ms(&self) -> u64 {
        self.acquired_at_ms
    }
    pub fn permission_limited(&self) -> usize {
        self.permission_limited
    }
    pub fn has_network(&self) -> bool {
        !self.network.is_empty()
    }
    pub fn get(&self, key: &ProcessKey) -> Option<&Row> {
        self.index.get(key).and_then(|&i| self.rows.get(i))
    }
    pub fn by_pid(&self, pid: i32) -> Option<&Row> {
        self.by_pid.get(&pid).and_then(|k| self.get(k))
    }
    /// Metric history for a live process, or a ghost until it expires.
    pub fn history(&self, key: &ProcessKey) -> Option<&MetricHistory> {
        self.history.get(key)
    }
    /// Direct live children, ordered by PID.
    pub fn children(&self, key: &ProcessKey) -> Vec<ProcessKey> {
        self.children.get(&key.pid).cloned().unwrap_or_default()
    }
    /// All live descendants, depth first (pre-order, siblings by PID).
    /// Never includes `key` itself, even when the parent links form a cycle.
    pub fn descendants(&self, key: &ProcessKey) -> Vec<ProcessKey> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([*key]);
        let mut stack: Vec<ProcessKey> = self.children(key).into_iter().rev().collect();
        while let Some(k) = stack.pop() {
            if seen.insert(k) {
                out.push(k);
                if let Some(kids) = self.children.get(&k.pid) {
                    stack.extend(kids.iter().rev().filter(|c| !seen.contains(c)));
                }
            }
        }
        out
    }
    /// Live ancestors from the immediate parent to the root. Stops at a
    /// missing parent, a self-parented process or a cycle.
    pub fn ancestors(&self, key: &ProcessKey) -> Vec<ProcessKey> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([*key]);
        let mut current = self
            .get(key)
            .and_then(|r| r.entry.record.ppid)
            .filter(|ppid| *ppid != key.pid);
        while let Some(ppid) = current {
            let Some(row) = self.by_pid(ppid) else { break };
            if !seen.insert(row.key()) {
                break;
            }
            out.push(row.key());
            current = row.entry.record.ppid.filter(|p| *p != ppid);
        }
        out
    }

    /// Usage of `key` plus all its live descendants.
    pub fn subtree_aggregate(&self, key: &ProcessKey) -> Aggregate {
        let mut agg = Aggregate::default();
        if let Some(row) = self.get(key) {
            agg.add(row);
        }
        for k in self.descendants(key) {
            if let Some(row) = self.get(&k) {
                agg.add(row);
            }
        }
        agg
    }

    /// Sums over all live processes for headers and status lines.
    pub fn totals(&self) -> Totals {
        let mut agg = Aggregate::default();
        let mut threads = 0u64;
        for row in self.rows.iter().filter(|r| !r.is_ghost()) {
            agg.add(row);
            threads = threads.saturating_add(row.entry.sample.threads.unwrap_or(1) as u64);
        }
        Totals {
            processes: agg.processes,
            threads,
            cpu_percent: agg.cpu_percent,
            mem_rss: agg.mem_rss,
            disk_read_rate: agg.disk_read_rate,
            disk_write_rate: agg.disk_write_rate,
            net_down: agg.net_down,
            net_up: agg.net_up,
        }
    }

    /// Distinct owners of live processes (user name, else numeric uid),
    /// sorted case-insensitively.
    pub fn users(&self) -> Vec<String> {
        let mut users: Vec<String> = self
            .rows
            .iter()
            .filter(|r| !r.is_ghost())
            .filter_map(|r| {
                let rec = &r.entry.record;
                rec.user.clone().or_else(|| rec.uid.map(|u| u.to_string()))
            })
            .filter(|u| !u.is_empty())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        users.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
        users
    }

    /// Live processes whose name equals `name` (case-insensitive), by PID.
    pub fn find_by_name(&self, name: &str) -> Vec<ProcessKey> {
        let needle = name.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut keys: Vec<ProcessKey> = self
            .rows
            .iter()
            .filter(|r| !r.is_ghost() && r.name().to_lowercase() == needle)
            .map(Row::key)
            .collect();
        keys.sort_unstable_by(tiebreak);
        keys
    }

    /// The `n` live processes ranking highest by `column` in its natural
    /// direction (largest first for numbers, A→Z for text); missing values last.
    pub fn top_by(&self, column: ColumnId, n: usize, ctx: &ColumnCtx) -> Vec<ProcessKey> {
        if n == 0 {
            return Vec::new();
        }
        let live: Vec<&Row> = self.rows.iter().filter(|r| !r.is_ghost()).collect();
        let keys: Vec<SortKey> = live.iter().map(|r| column.sort_key(r, ctx)).collect();
        let descending = column.default_descending();
        let mut order: Vec<usize> = (0..live.len()).collect();
        order.sort_unstable_by(|&a, &b| {
            keys[a]
                .cmp_directed(&keys[b], descending)
                .then_with(|| tiebreak(&live[a].key(), &live[b].key()))
        });
        order.into_iter().take(n).map(|i| live[i].key()).collect()
    }

    fn visible(&self, row: &Row, spec: &ViewSpec<'_>) -> bool {
        if spec.hide_kernel_threads && row.entry.record.kernel_thread {
            return false;
        }
        if let UserScope::Uid(uid) = spec.users
            && row.entry.record.uid != Some(uid)
        {
            return false;
        }
        if spec.net_active_only
            && !row.net.as_ref().is_some_and(|n| {
                n.download_rate > 0
                    || n.upload_rate > 0
                    || n.total_download > 0
                    || n.total_upload > 0
            })
        {
            return false;
        }
        true
    }

    /// Candidate indices in display order, ignoring pins: frozen keys first in
    /// frozen order, then everything else by the sort column. Each sort value
    /// is computed once, so this stays cheap for thousands of rows.
    fn order(candidates: &[&Row], spec: &ViewSpec<'_>) -> Vec<usize> {
        let keys: Vec<SortKey> = candidates
            .iter()
            .map(|r| spec.sort.column.sort_key(r, &spec.ctx))
            .collect();
        let frozen: Vec<Option<usize>> = match spec.frozen_order {
            Some(order) => {
                let pos = positions(order);
                candidates
                    .iter()
                    .map(|r| pos.get(&r.key()).copied())
                    .collect()
            }
            None => vec![None; candidates.len()],
        };
        let descending = spec.sort.descending;
        let mut order: Vec<usize> = (0..candidates.len()).collect();
        order.sort_unstable_by(|&a, &b| match (frozen[a], frozen[b]) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => keys[a]
                .cmp_directed(&keys[b], descending)
                .then_with(|| tiebreak(&candidates[a].key(), &candidates[b].key())),
        });
        order
    }

    /// Build the ordered rows for the table.
    pub fn view(&self, spec: &ViewSpec<'_>) -> Vec<ViewRow> {
        let candidates: Vec<&Row> = self.rows.iter().filter(|r| self.visible(r, spec)).collect();
        let order = Self::order(&candidates, spec);
        let mut rank = vec![0usize; candidates.len()];
        for (position, &i) in order.iter().enumerate() {
            rank[i] = position;
        }
        let pins = positions(spec.pinned);
        if spec.tree {
            let by_key: HashMap<ProcessKey, usize> = candidates
                .iter()
                .enumerate()
                .map(|(i, r)| (r.key(), rank[i]))
                .collect();
            let descending = spec.sort.descending;
            let compare = |a: &Row, b: &Row| match (by_key.get(&a.key()), by_key.get(&b.key())) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => spec
                    .sort
                    .column
                    .sort_key(a, &spec.ctx)
                    .cmp_directed(&spec.sort.column.sort_key(b, &spec.ctx), descending)
                    .then_with(|| tiebreak(&a.key(), &b.key())),
            };
            let mut rows = super::tree::flatten(&candidates, spec, &compare);
            for row in &mut rows {
                row.pinned = pins.contains_key(&row.key);
            }
            return rows;
        }
        let filter = spec.filter.filter(|f| !f.is_empty());
        let mut matched: Vec<(usize, Vec<(usize, usize)>)> = candidates
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match filter {
                Some(f) => f.matches(r, &spec.ctx).map(|m| (i, m.highlights)),
                None => Some((i, Vec::new())),
            })
            .collect();
        matched.sort_unstable_by_key(|(i, _)| {
            let pin = pins
                .get(&candidates[*i].key())
                .copied()
                .unwrap_or(usize::MAX);
            (pin, rank[*i])
        });
        matched
            .into_iter()
            .map(|(i, highlights)| {
                let r = candidates[i];
                ViewRow {
                    key: r.key(),
                    depth: 0,
                    has_children: false,
                    collapsed: false,
                    tree_prefix: String::new(),
                    pinned: pins.contains_key(&r.key()),
                    ghost: r.is_ghost(),
                    highlights,
                    aggregate: None,
                }
            })
            .collect()
    }

    /// Test/fixture helper: replace contents directly.
    #[cfg(test)]
    pub fn from_entries(entries: Vec<ProcessEntry>, system: SystemSnapshot) -> Self {
        let mut store = Self::new();
        store.apply_snapshot(ProcSnapshot {
            sequence: 1,
            acquired_at_ms: super::model::now_ms(),
            system,
            processes: entries,
            ..Default::default()
        });
        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::model::{ExitedProcess, ProcessRecord, ProcessSample};

    fn entry(pid: i32, ppid: Option<i32>, name: &str, cpu: f32) -> ProcessEntry {
        ProcessEntry {
            record: Arc::new(ProcessRecord {
                key: ProcessKey::new(pid, 1_000 + pid as u64),
                ppid,
                name: name.into(),
                cmdline: vec![format!("/usr/bin/{name}"), "--flag".into()],
                uid: Some(if pid % 2 == 0 { 0 } else { 501 }),
                user: Some(if pid % 2 == 0 { "root" } else { "alice" }.into()),
                ..Default::default()
            }),
            sample: ProcessSample {
                cpu_percent: cpu,
                mem_rss: pid as u64 * 1024,
                threads: Some(2),
                ..Default::default()
            },
        }
    }

    fn key(pid: i32) -> ProcessKey {
        ProcessKey::new(pid, 1_000 + pid as u64)
    }

    fn snapshot(at: u64, processes: Vec<ProcessEntry>, exited: Vec<ProcessEntry>) -> ProcSnapshot {
        ProcSnapshot {
            sequence: at,
            acquired_at_ms: at,
            processes,
            exited: exited
                .into_iter()
                .map(|e| ExitedProcess {
                    record: e.record,
                    last_sample: e.sample,
                })
                .collect(),
            ..Default::default()
        }
    }

    struct Spec {
        sort: SortSpec,
        tree: bool,
        hide_kernel_threads: bool,
        users: UserScope,
        net_active_only: bool,
        pinned: Vec<ProcessKey>,
        frozen: Option<Vec<ProcessKey>>,
        filter: Option<Filter>,
        collapsed: HashSet<ProcessKey>,
    }

    impl Default for Spec {
        fn default() -> Self {
            Self {
                sort: SortSpec::default(),
                tree: false,
                hide_kernel_threads: false,
                users: UserScope::All,
                net_active_only: false,
                pinned: Vec::new(),
                frozen: None,
                filter: None,
                collapsed: HashSet::new(),
            }
        }
    }

    impl Spec {
        fn sorted(column: ColumnId, descending: bool) -> Self {
            Self {
                sort: SortSpec { column, descending },
                ..Self::default()
            }
        }
        fn run(&self, store: &ProcessStore) -> Vec<ViewRow> {
            store.view(&ViewSpec {
                sort: self.sort,
                filter: self.filter.as_ref(),
                tree: self.tree,
                collapsed: &self.collapsed,
                hide_kernel_threads: self.hide_kernel_threads,
                users: self.users.clone(),
                net_active_only: self.net_active_only,
                pinned: &self.pinned,
                frozen_order: self.frozen.as_deref(),
                ctx: ColumnCtx::default(),
            })
        }
        fn pids(&self, store: &ProcessStore) -> Vec<i32> {
            self.run(store).iter().map(|r| r.key.pid).collect()
        }
    }

    fn store(entries: Vec<ProcessEntry>) -> ProcessStore {
        ProcessStore::from_entries(entries, SystemSnapshot::default())
    }

    fn sample_store() -> ProcessStore {
        store(vec![
            entry(1, None, "launchd", 1.0),
            entry(20, Some(1), "Zsh", 5.0),
            entry(3, Some(1), "bash", 5.0),
            entry(40, Some(3), "vim", 0.5),
            entry(5, Some(3), "Cargo", 50.0),
        ])
    }

    #[test]
    fn numeric_sort_both_directions_with_pid_tiebreak() {
        let s = sample_store();
        assert_eq!(
            Spec::sorted(ColumnId::Cpu, true).pids(&s),
            vec![5, 3, 20, 1, 40]
        );
        assert_eq!(
            Spec::sorted(ColumnId::Cpu, false).pids(&s),
            vec![40, 1, 3, 20, 5]
        );
        assert_eq!(
            Spec::sorted(ColumnId::Pid, false).pids(&s),
            vec![1, 3, 5, 20, 40]
        );
    }

    #[test]
    fn text_sort_is_case_insensitive() {
        let s = sample_store();
        assert_eq!(
            Spec::sorted(ColumnId::Name, false).pids(&s),
            vec![3, 5, 1, 40, 20]
        );
        assert_eq!(
            Spec::sorted(ColumnId::Name, true).pids(&s),
            vec![20, 40, 1, 5, 3]
        );
        // Equal users fall back to PID order in both directions.
        assert_eq!(
            Spec::sorted(ColumnId::User, false).pids(&s),
            vec![1, 3, 5, 20, 40]
        );
        assert_eq!(
            Spec::sorted(ColumnId::User, true).pids(&s),
            vec![20, 40, 1, 3, 5]
        );
    }

    #[test]
    fn missing_values_sort_last_in_both_directions() {
        let mut a = entry(1, None, "a", 0.0);
        a.sample.threads = None;
        let mut b = entry(2, None, "b", 0.0);
        b.sample.threads = Some(9);
        let mut c = entry(3, None, "c", 0.0);
        c.sample.threads = Some(4);
        let mut d = entry(4, None, "d", 0.0);
        d.sample.threads = None;
        let s = store(vec![a, b, c, d]);
        assert_eq!(
            Spec::sorted(ColumnId::Threads, true).pids(&s),
            vec![2, 3, 1, 4]
        );
        assert_eq!(
            Spec::sorted(ColumnId::Threads, false).pids(&s),
            vec![3, 2, 1, 4]
        );
        // Missing text (no TTY) also sinks.
        let mut t = entry(9, None, "t", 0.0);
        Arc::make_mut(&mut t.record).tty = Some("ttys001".into());
        let s = store(vec![entry(8, None, "u", 0.0), t]);
        assert_eq!(Spec::sorted(ColumnId::Tty, true).pids(&s), vec![9, 8]);
        assert_eq!(Spec::sorted(ColumnId::Tty, false).pids(&s), vec![9, 8]);
    }

    #[test]
    fn non_finite_values_do_not_break_sorting() {
        let s = store(vec![
            entry(1, None, "a", f32::NAN),
            entry(2, None, "b", 3.0),
            entry(3, None, "c", f32::INFINITY),
        ]);
        assert_eq!(Spec::sorted(ColumnId::Cpu, true).pids(&s), vec![3, 2, 1]);
        assert_eq!(Spec::sorted(ColumnId::Cpu, false).pids(&s), vec![2, 3, 1]);
    }

    #[test]
    fn pinned_rows_float_in_pin_order() {
        let s = sample_store();
        let spec = Spec {
            pinned: vec![key(40), key(1), key(999), key(40)],
            ..Spec::sorted(ColumnId::Cpu, true)
        };
        let rows = spec.run(&s);
        let pids: Vec<i32> = rows.iter().map(|r| r.key.pid).collect();
        assert_eq!(pids, vec![40, 1, 5, 3, 20]);
        let pinned: Vec<bool> = rows.iter().map(|r| r.pinned).collect();
        assert_eq!(pinned, vec![true, true, false, false, false]);
    }

    #[test]
    fn tree_mode_delegates_and_marks_pins() {
        let s = sample_store();
        let spec = Spec {
            tree: true,
            pinned: vec![key(5)],
            ..Spec::default()
        };
        let rows = spec.run(&s);
        let mut pids: Vec<i32> = rows.iter().map(|r| r.key.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![1, 3, 5, 20, 40]);
        for row in rows {
            assert_eq!(row.pinned, row.key.pid == 5);
        }
    }

    #[test]
    fn frozen_order_keeps_positions_and_appends_newcomers() {
        let s = sample_store();
        let spec = Spec {
            frozen: Some(vec![key(40), key(1), key(77), key(20)]),
            ..Spec::sorted(ColumnId::Cpu, true)
        };
        assert_eq!(spec.pids(&s), vec![40, 1, 20, 5, 3]);
        let pinned = Spec {
            pinned: vec![key(3)],
            ..spec
        };
        assert_eq!(pinned.pids(&s), vec![3, 40, 1, 20, 5]);
    }

    #[test]
    fn scope_filters() {
        let mut kernel = entry(2, Some(0), "kthreadd", 0.0);
        Arc::make_mut(&mut kernel.record).kernel_thread = true;
        let mut s = store(vec![
            entry(1, None, "init", 0.0),
            kernel,
            entry(3, None, "sshd", 0.0),
        ]);
        let base = Spec::sorted(ColumnId::Pid, false);
        assert_eq!(base.pids(&s), vec![1, 2, 3]);
        let hide = Spec {
            hide_kernel_threads: true,
            ..Spec::sorted(ColumnId::Pid, false)
        };
        assert_eq!(hide.pids(&s), vec![1, 3]);
        let mine = Spec {
            users: UserScope::Uid(0),
            ..Spec::sorted(ColumnId::Pid, false)
        };
        assert_eq!(mine.pids(&s), vec![2]);
        let net = Spec {
            net_active_only: true,
            ..Spec::sorted(ColumnId::Pid, false)
        };
        assert!(net.pids(&s).is_empty());
        let mut map = ProcessMap::new();
        let mut info = ProcessInfo::new(3, "sshd".into());
        info.upload_rate = 10;
        map.insert(3, info);
        map.insert(1, ProcessInfo::new(1, "init".into()));
        let mut dead = ProcessInfo::new(2, "kthreadd".into());
        dead.total_download = 5;
        dead.is_terminated = true;
        map.insert(2, dead);
        s.merge_network(&map);
        assert!(s.has_network());
        assert_eq!(net.pids(&s), vec![3]);
    }

    #[test]
    fn empty_filter_matches_everything() {
        let s = sample_store();
        let spec = Spec {
            filter: Some(Filter::parse("").unwrap()),
            ..Spec::default()
        };
        assert_eq!(spec.run(&s).len(), 5);
        assert!(Spec::default().run(&ProcessStore::new()).is_empty());
    }

    #[test]
    fn ghosts_linger_then_expire_with_history() {
        let mut s = ProcessStore::new();
        let mut net = ProcessMap::new();
        let mut info = ProcessInfo::new(7, "worker".into());
        info.download_rate = 99;
        net.insert(7, info);
        s.merge_network(&net);
        s.apply_snapshot(snapshot(
            1_000,
            vec![
                entry(1, None, "init", 0.0),
                entry(7, Some(1), "worker", 3.0),
            ],
            vec![],
        ));
        assert_eq!(s.children(&key(1)), vec![key(7)]);
        assert_eq!(s.history(&key(7)).map(|h| h.cpu.len()), Some(1));

        let worker = entry(7, Some(1), "worker", 3.0);
        s.apply_snapshot(snapshot(
            2_000,
            vec![entry(1, None, "init", 0.0)],
            vec![worker],
        ));
        let ghost = s.get(&key(7)).expect("ghost row");
        assert!(ghost.is_ghost());
        assert_eq!(ghost.net.as_ref().map(|n| n.download_rate), Some(99));
        assert!(s.by_pid(7).is_none());
        assert!(s.children(&key(1)).is_empty());
        assert_eq!(s.len_live(), 1);
        assert_eq!(s.history(&key(7)).map(|h| h.cpu.len()), Some(1));
        let rows = Spec::default().run(&s);
        assert!(rows.iter().any(|r| r.key == key(7) && r.ghost));

        s.apply_snapshot(snapshot(
            2_000 + GHOST_MS - 1,
            vec![entry(1, None, "init", 0.0)],
            vec![],
        ));
        assert!(s.get(&key(7)).is_some());
        s.apply_snapshot(snapshot(
            2_000 + GHOST_MS,
            vec![entry(1, None, "init", 0.0)],
            vec![],
        ));
        assert!(s.get(&key(7)).is_none());
        assert!(s.history(&key(7)).is_none());
        assert_eq!(s.history(&key(1)).map(|h| h.cpu.len()), Some(4));
    }

    #[test]
    fn reused_pid_is_live_alongside_its_ghost() {
        let mut s = ProcessStore::new();
        let old = entry(7, None, "old", 0.0);
        s.apply_snapshot(snapshot(1_000, vec![old.clone()], vec![]));
        let mut new = entry(7, None, "new", 0.0);
        Arc::make_mut(&mut new.record).key = ProcessKey::new(7, 5_000);
        s.apply_snapshot(snapshot(2_000, vec![new], vec![old]));
        assert_eq!(s.rows().len(), 2);
        assert_eq!(s.by_pid(7).map(Row::name), Some("new"));
        assert!(s.get(&key(7)).is_some_and(Row::is_ghost));
    }

    #[test]
    fn relatives_are_robust_to_cycles_and_self_parents() {
        let s = store(vec![
            entry(0, Some(0), "kernel_task", 0.0),
            entry(1, Some(0), "launchd", 0.0),
            entry(10, Some(1), "a", 0.0),
            entry(11, Some(10), "b", 0.0),
            entry(12, Some(10), "c", 0.0),
            // 20 <-> 21 form a cycle.
            entry(20, Some(21), "x", 0.0),
            entry(21, Some(20), "y", 0.0),
        ]);
        assert!(s.ancestors(&key(0)).is_empty());
        assert!(!s.children(&key(0)).contains(&key(0)));
        assert_eq!(s.ancestors(&key(11)), vec![key(10), key(1), key(0)]);
        assert_eq!(s.descendants(&key(1)), vec![key(10), key(11), key(12)]);
        assert_eq!(
            s.descendants(&key(0)),
            vec![key(1), key(10), key(11), key(12)]
        );
        assert_eq!(s.ancestors(&key(20)), vec![key(21)]);
        assert_eq!(s.descendants(&key(20)), vec![key(21)]);
        assert!(s.ancestors(&key(999)).is_empty());
        assert!(s.descendants(&key(999)).is_empty());
        let agg = s.subtree_aggregate(&key(10));
        assert_eq!(agg.processes, 3);
        assert_eq!(agg.mem_rss, (10 + 11 + 12) * 1024);
    }

    #[test]
    fn queries() {
        let mut s = sample_store();
        let mut map = ProcessMap::new();
        let mut info = ProcessInfo::new(5, "Cargo".into());
        info.download_rate = 100;
        info.upload_rate = 7;
        map.insert(5, info);
        s.merge_network(&map);
        let t = s.totals();
        assert_eq!(t.processes, 5);
        assert_eq!(t.threads, 10);
        assert!((t.cpu_percent - 61.5).abs() < 1e-4);
        assert_eq!(t.mem_rss, (1 + 20 + 3 + 40 + 5) * 1024);
        assert_eq!((t.net_down, t.net_up), (100, 7));
        assert_eq!(s.users(), vec!["alice".to_string(), "root".to_string()]);
        assert_eq!(s.find_by_name("cargo"), vec![key(5)]);
        assert!(s.find_by_name("  ").is_empty());
        assert!(s.find_by_name("carg").is_empty());
        let ctx = ColumnCtx::default();
        assert_eq!(
            s.top_by(ColumnId::Cpu, 3, &ctx),
            vec![key(5), key(3), key(20)]
        );
        assert_eq!(s.top_by(ColumnId::Name, 2, &ctx), vec![key(3), key(5)]);
        assert_eq!(s.top_by(ColumnId::NetDown, 1, &ctx), vec![key(5)]);
        assert!(s.top_by(ColumnId::Cpu, 0, &ctx).is_empty());
        assert_eq!(s.top_by(ColumnId::Cpu, 99, &ctx).len(), 5);
        assert_eq!(ProcessStore::new().totals(), Totals::default());
    }

    #[test]
    fn large_views_stay_sorted() {
        let entries: Vec<ProcessEntry> = (1..=5_000)
            .map(|pid| entry(pid, Some(pid / 2), "p", ((pid * 7919) % 1000) as f32 / 10.0))
            .collect();
        let s = store(entries);
        let start = std::time::Instant::now();
        let rows = Spec::sorted(ColumnId::Cpu, true).run(&s);
        let elapsed = start.elapsed();
        assert_eq!(rows.len(), 5_000);
        let cpu = |k: &ProcessKey| s.get(k).unwrap().entry.sample.cpu_percent;
        assert!(rows.windows(2).all(|w| {
            let (a, b) = (cpu(&w[0].key), cpu(&w[1].key));
            a > b || (a == b && w[0].key.pid < w[1].key.pid)
        }));
        assert!(elapsed.as_secs() < 2, "view took {elapsed:?}");
        let cmd = Spec::sorted(ColumnId::Command, false).run(&s);
        assert_eq!(cmd.len(), 5_000);
    }

    #[test]
    fn duplicate_entries_in_a_snapshot_are_collapsed() {
        let s = store(vec![entry(1, None, "a", 0.0), entry(1, None, "a", 0.0)]);
        assert_eq!(s.rows().len(), 1);
        assert_eq!(s.history(&key(1)).map(|h| h.cpu.len()), Some(1));
    }
}
