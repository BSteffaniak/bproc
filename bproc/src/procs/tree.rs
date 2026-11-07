//! Process tree flattening for tree mode.
//!
//! Parents are resolved by `ppid` among the candidate rows only; a row whose
//! parent is missing, itself, or part of a cycle becomes a root. Live rows
//! win over ghosts sharing a PID, and a "parent" that started after its
//! child (PID reuse) is ignored. Everything is iterative, so arbitrarily deep
//! chains cannot overflow the stack.
//!
//! While a filter is active the view shows matching rows plus their
//! ancestors for context, and paths to matches are expanded automatically
//! (collapsed state is ignored until the filter is cleared).
use super::columns::ColumnCtx;
use super::store::{Aggregate, Row, ViewRow, ViewSpec};
use crate::process::TrafficType;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
mod tests;

/// Ancestor columns drawn before the prefix is elided with `…`.
pub const MAX_PREFIX_DEPTH: usize = 32;

const NONE: usize = usize::MAX;

/// Flatten `rows` into parent-before-children order. Siblings are ordered by
/// `compare`; collapsed nodes hide their subtree and carry an aggregate.
/// When a filter is active, matching rows keep their ancestors visible.
pub fn flatten(
    rows: &[&Row],
    spec: &ViewSpec<'_>,
    compare: &dyn Fn(&Row, &Row) -> Ordering,
) -> Vec<ViewRow> {
    let n = rows.len();
    if n == 0 {
        return Vec::new();
    }
    let parent = resolve_parents(rows);

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| compare(rows[a], rows[b]));
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots = Vec::new();
    for &i in &order {
        match parent[i] {
            NONE => roots.push(i),
            p => children[p].push(i),
        }
    }

    let filter = spec.filter.filter(|f| !f.is_empty());
    let mut highlights: Vec<Option<Vec<(usize, usize)>>> = vec![None; n];
    let keep: Option<Vec<bool>> = filter.map(|f| {
        let mut keep = vec![false; n];
        for (i, row) in rows.iter().enumerate() {
            if let Some(m) = f.matches(row, &spec.ctx) {
                highlights[i] = Some(m.highlights);
                let mut cur = i;
                while cur != NONE && !keep[cur] {
                    keep[cur] = true;
                    cur = parent[cur];
                }
            }
        }
        keep
    });
    if let Some(keep) = &keep {
        roots.retain(|&i| keep[i]);
        for list in &mut children {
            list.retain(|&i| keep[i]);
        }
    }
    let pinned: HashSet<_> = spec.pinned.iter().copied().collect();

    let mut out = Vec::with_capacity(if keep.is_some() { roots.len() } else { n });
    // (row, depth, is last sibling)
    let mut stack: Vec<(usize, usize, bool)> = Vec::new();
    for (k, &r) in roots.iter().enumerate().rev() {
        stack.push((r, 0, k + 1 == roots.len()));
    }
    // `lasts[d]` is whether the ancestor at depth `d + 1` was a last sibling.
    let mut lasts: Vec<bool> = Vec::new();
    while let Some((i, depth, last)) = stack.pop() {
        let row = rows[i];
        let key = row.key();
        let kids = &children[i];
        let has_children = !kids.is_empty();
        let collapsed = has_children && keep.is_none() && spec.collapsed.contains(&key);
        if depth > 0 {
            lasts.truncate(depth - 1);
            lasts.push(last);
        }
        out.push(ViewRow {
            key,
            depth: u16::try_from(depth).unwrap_or(u16::MAX),
            has_children,
            collapsed,
            tree_prefix: prefix(&lasts, depth),
            pinned: pinned.contains(&key),
            ghost: row.is_ghost(),
            highlights: highlights[i].take().unwrap_or_default(),
            aggregate: collapsed.then(|| aggregate(rows, &children, i, &spec.ctx)),
        });
        if !collapsed {
            for (k, &c) in kids.iter().enumerate().rev() {
                stack.push((c, depth + 1, k + 1 == kids.len()));
            }
        }
    }
    out
}

/// Parent index per row (`NONE` for roots), with cycles broken.
fn resolve_parents(rows: &[&Row]) -> Vec<usize> {
    let n = rows.len();
    let mut by_pid: HashMap<i32, usize> = HashMap::with_capacity(n);
    for (i, row) in rows.iter().enumerate() {
        let pid = row.key().pid;
        match by_pid.get(&pid) {
            Some(&j) if !rows[j].is_ghost() || row.is_ghost() => {}
            _ => {
                by_pid.insert(pid, i);
            }
        }
    }
    let mut parent: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let record = &row.entry.record;
            let Some(ppid) = record.ppid else {
                return NONE;
            };
            if ppid == record.key.pid {
                return NONE;
            }
            match by_pid.get(&ppid) {
                Some(&p) if p != i => {
                    let (ps, cs) = (rows[p].key().start_time, record.key.start_time);
                    if ps != 0 && cs != 0 && ps > cs {
                        NONE
                    } else {
                        p
                    }
                }
                _ => NONE,
            }
        })
        .collect();

    // 0 = unvisited, 1 = on the current walk, 2 = resolved.
    let mut mark = vec![0u8; n];
    let mut path = Vec::new();
    for start in 0..n {
        let mut cur = start;
        while cur != NONE && mark[cur] == 0 {
            mark[cur] = 1;
            path.push(cur);
            cur = parent[cur];
        }
        if cur != NONE && mark[cur] == 1 {
            parent[cur] = NONE;
        }
        for &p in &path {
            mark[p] = 2;
        }
        path.clear();
    }
    parent
}

/// Box-drawing prefix for a row at `depth` given its ancestors' last flags.
fn prefix(lasts: &[bool], depth: usize) -> String {
    if depth == 0 {
        return String::new();
    }
    let (own, ancestors) = lasts.split_last().unwrap_or((&true, &[]));
    let skip = ancestors.len().saturating_sub(MAX_PREFIX_DEPTH);
    let mut s = String::with_capacity((ancestors.len() - skip + 1) * 9 + 4);
    if skip > 0 {
        s.push_str("…  ");
    }
    for &last in &ancestors[skip..] {
        s.push_str(if last { "   " } else { "│  " });
    }
    s.push_str(if *own { "└─ " } else { "├─ " });
    s
}

/// Sum of the node and all descendants (ghost descendants excluded).
fn aggregate(rows: &[&Row], children: &[Vec<usize>], root: usize, ctx: &ColumnCtx) -> Aggregate {
    let mut agg = Aggregate::default();
    let mut stack = vec![root];
    while let Some(i) = stack.pop() {
        let row = rows[i];
        stack.extend(children[i].iter().copied());
        if i != root && row.is_ghost() {
            continue;
        }
        let s = &row.entry.sample;
        agg.processes += 1;
        agg.cpu_percent += s.cpu_percent;
        agg.mem_rss = agg.mem_rss.saturating_add(s.mem_rss);
        agg.disk_read_rate = agg.disk_read_rate.saturating_add(s.disk_read_rate);
        agg.disk_write_rate = agg.disk_write_rate.saturating_add(s.disk_write_rate);
        if let Some(n) = &row.net {
            let (down, up) = match ctx.traffic {
                TrafficType::All => (n.download_rate, n.upload_rate),
                TrafficType::Internet => (n.internet_download_rate, n.internet_upload_rate),
                TrafficType::Local => (n.local_download_rate, n.local_upload_rate),
            };
            agg.net_down = agg.net_down.saturating_add(down);
            agg.net_up = agg.net_up.saturating_add(up);
        }
    }
    agg
}
