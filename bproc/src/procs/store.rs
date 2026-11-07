//! Joined process table: collector snapshots + network attribution + history.
//! The UI reads views from here; it never touches collectors directly.
use super::columns::{ColumnCtx, ColumnId, tiebreak};
use super::filter::Filter;
use super::history::{MetricHistory, SystemHistory};
use super::model::{ProcSnapshot, ProcessEntry, ProcessKey, SystemSnapshot};
use crate::process::{ProcessInfo, ProcessMap};
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

impl ProcessStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_snapshot(&mut self, snapshot: ProcSnapshot) {
        let now = snapshot.acquired_at_ms;
        let mut ghosts: Vec<Row> = self
            .rows
            .drain(..)
            .filter(|r| {
                r.exited_at_ms
                    .is_some_and(|t| now.saturating_sub(t) < GHOST_MS)
            })
            .collect();
        for exited in snapshot.exited {
            ghosts.push(Row {
                entry: ProcessEntry {
                    record: exited.record,
                    sample: exited.last_sample,
                },
                net: None,
                exited_at_ms: Some(now),
            });
        }
        let mut rows: Vec<Row> = snapshot
            .processes
            .into_iter()
            .map(|entry| Row {
                net: self.network.get(&entry.record.key.pid).cloned(),
                entry,
                exited_at_ms: None,
            })
            .collect();
        let live: HashSet<ProcessKey> = rows.iter().map(Row::key).collect();
        rows.extend(ghosts.into_iter().filter(|g| !live.contains(&g.key())));
        for row in rows.iter().filter(|r| !r.is_ghost()) {
            self.history
                .entry(row.key())
                .or_default()
                .push(&row.entry.sample, row.net.as_deref());
        }
        self.history.retain(|key, _| live.contains(key));
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
            if !row.is_ghost() {
                self.by_pid.insert(key.pid, key);
            }
        }
        for row in &self.rows {
            if let Some(ppid) = row.entry.record.ppid
                && !row.is_ghost()
            {
                self.children.entry(ppid).or_default().push(row.key());
            }
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
        self.index.get(key).map(|&i| &self.rows[i])
    }
    pub fn by_pid(&self, pid: i32) -> Option<&Row> {
        self.by_pid.get(&pid).and_then(|k| self.get(k))
    }
    pub fn history(&self, key: &ProcessKey) -> Option<&MetricHistory> {
        self.history.get(key)
    }
    /// Direct live children.
    pub fn children(&self, key: &ProcessKey) -> Vec<ProcessKey> {
        self.children.get(&key.pid).cloned().unwrap_or_default()
    }
    /// All live descendants, depth first.
    pub fn descendants(&self, key: &ProcessKey) -> Vec<ProcessKey> {
        let mut out = Vec::new();
        let mut stack = self.children(key);
        let mut seen = HashSet::new();
        while let Some(k) = stack.pop() {
            if seen.insert(k) {
                out.push(k);
                stack.extend(self.children(&k));
            }
        }
        out
    }
    /// Ancestors from the immediate parent to the root.
    pub fn ancestors(&self, key: &ProcessKey) -> Vec<ProcessKey> {
        let mut out = Vec::new();
        let mut current = self.get(key).and_then(|r| r.entry.record.ppid);
        while let Some(ppid) = current {
            let Some(row) = self.by_pid(ppid) else { break };
            if out.contains(&row.key()) || row.key() == *key {
                break;
            }
            out.push(row.key());
            current = row.entry.record.ppid;
        }
        out
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
                n.download_rate + n.upload_rate + n.total_download + n.total_upload > 0
            })
        {
            return false;
        }
        true
    }

    /// Build the ordered rows for the table.
    pub fn view(&self, spec: &ViewSpec<'_>) -> Vec<ViewRow> {
        let candidates: Vec<&Row> = self.rows.iter().filter(|r| self.visible(r, spec)).collect();
        let compare = |a: &Row, b: &Row| {
            let ord = spec.sort.column.compare(a, b, &spec.ctx);
            let ord = if spec.sort.descending {
                ord.reverse()
            } else {
                ord
            };
            ord.then_with(|| tiebreak(&a.key(), &b.key()))
        };
        if spec.tree {
            return super::tree::flatten(&candidates, spec, &compare);
        }
        let mut matched: Vec<(&Row, Vec<(usize, usize)>)> = candidates
            .into_iter()
            .filter_map(|r| match spec.filter {
                Some(f) if !f.is_empty() => f.matches(r, &spec.ctx).map(|m| (r, m.highlights)),
                _ => Some((r, Vec::new())),
            })
            .collect();
        if let Some(order) = spec.frozen_order {
            let pos: HashMap<ProcessKey, usize> =
                order.iter().enumerate().map(|(i, k)| (*k, i)).collect();
            matched.sort_by(
                |(a, _), (b, _)| match (pos.get(&a.key()), pos.get(&b.key())) {
                    (Some(x), Some(y)) => x.cmp(y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => compare(a, b),
                },
            );
        } else {
            matched.sort_by(|(a, _), (b, _)| compare(a, b));
        }
        let pinned: HashMap<ProcessKey, usize> = spec
            .pinned
            .iter()
            .enumerate()
            .map(|(i, k)| (*k, i))
            .collect();
        matched.sort_by_key(|(r, _)| pinned.get(&r.key()).copied().unwrap_or(usize::MAX));
        matched
            .into_iter()
            .map(|(r, highlights)| ViewRow {
                key: r.key(),
                depth: 0,
                has_children: false,
                collapsed: false,
                tree_prefix: String::new(),
                pinned: pinned.contains_key(&r.key()),
                ghost: r.is_ghost(),
                highlights,
                aggregate: None,
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
