use super::*;
use crate::process::ProcessInfo;
use crate::procs::filter::Filter;
use crate::procs::model::{ProcessEntry, ProcessKey, ProcessRecord, ProcessSample};
use crate::procs::store::{SortSpec, UserScope};
use std::sync::Arc;

fn row(pid: i32, ppid: Option<i32>, name: &str) -> Row {
    Row {
        entry: ProcessEntry {
            record: Arc::new(ProcessRecord {
                key: ProcessKey::new(pid, 100 + pid as u64),
                ppid,
                name: name.into(),
                ..Default::default()
            }),
            sample: ProcessSample {
                cpu_percent: pid as f32,
                mem_rss: 1000,
                disk_read_rate: 10,
                disk_write_rate: 1,
                ..Default::default()
            },
        },
        net: None,
        exited_at_ms: None,
    }
}

struct Fixture {
    rows: Vec<Row>,
    collapsed: HashSet<ProcessKey>,
    pinned: Vec<ProcessKey>,
    filter: Option<Filter>,
    ctx: ColumnCtx,
}

impl Fixture {
    fn new(rows: Vec<Row>) -> Self {
        Self {
            rows,
            collapsed: HashSet::new(),
            pinned: Vec::new(),
            filter: None,
            ctx: ColumnCtx::default(),
        }
    }

    fn key(&self, pid: i32) -> ProcessKey {
        self.rows.iter().find(|r| r.key().pid == pid).unwrap().key()
    }

    fn collapse(mut self, pid: i32) -> Self {
        let key = self.key(pid);
        self.collapsed.insert(key);
        self
    }

    fn filter(mut self, q: &str) -> Self {
        self.filter = Some(Filter::parse(q).unwrap());
        self
    }

    fn flatten_by(&self, compare: &dyn Fn(&Row, &Row) -> Ordering) -> Vec<ViewRow> {
        let refs: Vec<&Row> = self.rows.iter().collect();
        let spec = ViewSpec {
            sort: SortSpec::default(),
            filter: self.filter.as_ref(),
            tree: true,
            collapsed: &self.collapsed,
            hide_kernel_threads: false,
            users: UserScope::All,
            net_active_only: false,
            pinned: &self.pinned,
            frozen_order: None,
            ctx: self.ctx,
        };
        flatten(&refs, &spec, compare)
    }

    fn flatten(&self) -> Vec<ViewRow> {
        self.flatten_by(&|a, b| a.key().pid.cmp(&b.key().pid))
    }
}

fn pids(view: &[ViewRow]) -> Vec<i32> {
    view.iter().map(|v| v.key.pid).collect()
}

fn lines(view: &[ViewRow], fx: &Fixture) -> Vec<String> {
    view.iter()
        .map(|v| {
            let name = fx.rows.iter().find(|r| r.key() == v.key).unwrap().name();
            format!("{}{}", v.tree_prefix, name)
        })
        .collect()
}

/// 1 init
/// ├─ 2 a
/// │  ├─ 4 c
/// │  └─ 5 d
/// │     └─ 7 f
/// └─ 3 b
///    └─ 6 e
fn sample() -> Vec<Row> {
    vec![
        row(6, Some(3), "e"),
        row(1, Some(0), "init"),
        row(5, Some(2), "d"),
        row(3, Some(1), "b"),
        row(7, Some(5), "f"),
        row(2, Some(1), "a"),
        row(4, Some(2), "c"),
    ]
}

#[test]
fn empty_input() {
    assert!(Fixture::new(Vec::new()).flatten().is_empty());
}

#[test]
fn builds_pstree_style_prefixes() {
    let fx = Fixture::new(sample());
    let view = fx.flatten();
    assert_eq!(
        lines(&view, &fx),
        vec![
            "init",
            "├─ a",
            "│  ├─ c",
            "│  └─ d",
            "│     └─ f",
            "└─ b",
            "   └─ e",
        ]
    );
    assert_eq!(pids(&view), vec![1, 2, 4, 5, 7, 3, 6]);
    let depths: Vec<u16> = view.iter().map(|v| v.depth).collect();
    assert_eq!(depths, vec![0, 1, 2, 2, 3, 1, 2]);
    let parents: Vec<bool> = view.iter().map(|v| v.has_children).collect();
    assert_eq!(parents, vec![true, true, false, true, false, true, false]);
    assert!(view.iter().all(|v| !v.collapsed && v.aggregate.is_none()));
    assert!(view.iter().all(|v| v.highlights.is_empty()));
}

#[test]
fn siblings_follow_compare() {
    let fx = Fixture::new(sample());
    let view = fx.flatten_by(&|a, b| b.key().pid.cmp(&a.key().pid));
    assert_eq!(pids(&view), vec![1, 3, 6, 2, 5, 7, 4]);
    assert_eq!(view[1].tree_prefix, "├─ ");
    assert_eq!(view[3].tree_prefix, "└─ ");
}

#[test]
fn multiple_roots_and_missing_parents() {
    let fx = Fixture::new(vec![
        row(10, Some(999), "orphan"),
        row(11, None, "noparent"),
        row(12, Some(12), "selfparent"),
        row(13, Some(10), "child"),
    ]);
    let view = fx.flatten();
    assert_eq!(pids(&view), vec![10, 13, 11, 12]);
    assert_eq!(view[0].tree_prefix, "");
    assert_eq!(view[1].tree_prefix, "└─ ");
    assert_eq!(view[2].depth, 0);
    assert_eq!(view[3].depth, 0);
}

#[test]
fn cycles_are_broken() {
    let fx = Fixture::new(vec![
        row(1, Some(3), "a"),
        row(2, Some(1), "b"),
        row(3, Some(2), "c"),
        row(4, Some(4), "d"),
        row(5, Some(6), "e"),
        row(6, Some(5), "f"),
    ]);
    let view = fx.flatten();
    assert_eq!(view.len(), 6);
    let mut seen = pids(&view);
    seen.sort();
    assert_eq!(seen, vec![1, 2, 3, 4, 5, 6]);
    assert!(view.iter().filter(|v| v.depth == 0).count() >= 3);
}

#[test]
fn pid_reuse_does_not_adopt_older_children() {
    let mut parent = row(5, None, "newer");
    Arc::make_mut(&mut parent.entry.record).key.start_time = 10_000;
    let child = row(6, Some(5), "older");
    let fx = Fixture::new(vec![parent, child]);
    let view = fx.flatten();
    assert!(view.iter().all(|v| v.depth == 0));
}

#[test]
fn collapsed_nodes_hide_subtree_and_aggregate() {
    let mut rows = sample();
    rows[4].net = Some(Arc::new({
        let mut n = ProcessInfo::new(7, "f".into());
        n.download_rate = 300;
        n.upload_rate = 30;
        n.internet_download_rate = 3;
        n
    }));
    let fx = Fixture::new(rows).collapse(2);
    let view = fx.flatten();
    assert_eq!(pids(&view), vec![1, 2, 3, 6]);
    let a = &view[1];
    assert!(a.collapsed && a.has_children);
    let agg = a.aggregate.unwrap();
    assert_eq!(agg.processes, 4);
    assert_eq!(agg.cpu_percent, 2.0 + 4.0 + 5.0 + 7.0);
    assert_eq!(agg.mem_rss, 4000);
    assert_eq!(agg.disk_read_rate, 40);
    assert_eq!(agg.disk_write_rate, 4);
    assert_eq!((agg.net_down, agg.net_up), (300, 30));
    // The sibling after the collapsed node still draws as the last child.
    assert_eq!(view[2].tree_prefix, "└─ ");

    let mut fx = fx;
    fx.ctx.traffic = TrafficType::Internet;
    let agg = fx.flatten()[1].aggregate.unwrap();
    assert_eq!((agg.net_down, agg.net_up), (3, 0));
}

#[test]
fn collapsing_a_leaf_is_a_no_op() {
    let fx = Fixture::new(sample()).collapse(4);
    let view = fx.flatten();
    assert_eq!(view.len(), 7);
    let c = view.iter().find(|v| v.key.pid == 4).unwrap();
    assert!(!c.collapsed && c.aggregate.is_none());
}

#[test]
fn collapsed_root_and_nested_collapse() {
    let fx = Fixture::new(sample()).collapse(1).collapse(2);
    let view = fx.flatten();
    assert_eq!(pids(&view), vec![1]);
    assert_eq!(view[0].aggregate.unwrap().processes, 7);
}

#[test]
fn filter_keeps_matches_and_ancestors() {
    let fx = Fixture::new(sample()).filter("name:f");
    let view = fx.flatten();
    assert_eq!(pids(&view), vec![1, 2, 5, 7]);
    assert_eq!(
        lines(&view, &fx),
        vec!["init", "└─ a", "   └─ d", "      └─ f"]
    );
    // Only the match carries highlights; context ancestors don't.
    assert_eq!(view[3].highlights, vec![(0, 1)]);
    assert!(view[..3].iter().all(|v| v.highlights.is_empty()));
    assert!(!view[3].has_children);
}

#[test]
fn filter_auto_expands_collapsed_paths() {
    let fx = Fixture::new(sample()).collapse(2).filter("name:f");
    let view = fx.flatten();
    assert_eq!(pids(&view), vec![1, 2, 5, 7]);
    assert!(view.iter().all(|v| !v.collapsed && v.aggregate.is_none()));
}

#[test]
fn filter_with_matching_ancestor_and_descendant() {
    let fx = Fixture::new(sample()).filter("a | e");
    let view = fx.flatten();
    // `init` contains neither; `a` matches; `b` is context for `e`.
    assert_eq!(pids(&view), vec![1, 2, 3, 6]);
    assert!(view[0].highlights.is_empty());
    assert!(!view[1].highlights.is_empty());
    assert!(view[2].highlights.is_empty());
    assert!(!view[3].highlights.is_empty());
}

#[test]
fn filter_matching_nothing_yields_nothing() {
    let fx = Fixture::new(sample()).filter("zzz");
    assert!(fx.flatten().is_empty());
}

#[test]
fn empty_filter_is_ignored() {
    let fx = Fixture::new(sample()).filter("   ");
    assert_eq!(fx.flatten().len(), 7);
}

#[test]
fn pinned_and_ghost_flags() {
    let mut rows = sample();
    rows[4].exited_at_ms = Some(1);
    let mut fx = Fixture::new(rows);
    fx.pinned = vec![fx.key(3)];
    let view = fx.flatten();
    let f = view.iter().find(|v| v.key.pid == 7).unwrap();
    assert!(f.ghost);
    assert_eq!(f.depth, 3, "ghost stays nested under its parent");
    assert!(view.iter().find(|v| v.key.pid == 3).unwrap().pinned);
    assert_eq!(view.iter().filter(|v| v.pinned).count(), 1);
}

#[test]
fn live_row_wins_over_ghost_with_same_pid() {
    let mut ghost = row(2, Some(1), "old");
    Arc::make_mut(&mut ghost.entry.record).key.start_time = 50;
    ghost.exited_at_ms = Some(1);
    let live = row(2, Some(1), "new");
    let child = row(9, Some(2), "kid");
    let fx = Fixture::new(vec![ghost, row(1, None, "init"), child, live]);
    let view = fx.flatten();
    let kid = view.iter().find(|v| v.key.pid == 9).unwrap();
    let parent_idx = view.iter().position(|v| v.key.pid == 9).unwrap() - 1;
    assert_eq!(kid.depth, 2);
    assert!(!view[parent_idx].ghost);
}

#[test]
fn ghost_descendants_are_excluded_from_aggregates() {
    let mut rows = sample();
    rows[4].exited_at_ms = Some(1); // f
    let fx = Fixture::new(rows).collapse(5);
    let d = fx.flatten().into_iter().find(|v| v.key.pid == 5).unwrap();
    assert_eq!(d.aggregate.unwrap().processes, 1);
}

#[test]
fn deep_chain_does_not_overflow() {
    let n = 20_000;
    let rows: Vec<Row> = (1..=n).map(|pid| row(pid, Some(pid - 1), "x")).collect();
    let fx = Fixture::new(rows).collapse(1);
    let view = fx.flatten();
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].aggregate.unwrap().processes, n as usize);

    let rows: Vec<Row> = (1..=3_000)
        .map(|pid| row(pid, Some(pid - 1), "x"))
        .collect();
    let fx = Fixture::new(rows);
    let view = fx.flatten();
    assert_eq!(view.len(), 3_000);
    assert_eq!(view[2_999].depth, 2_999);
    // Prefixes are bounded: elided ancestors plus MAX_PREFIX_DEPTH columns.
    let last = &view[2_999].tree_prefix;
    assert!(last.starts_with("…  "));
    assert_eq!(last.chars().count(), 3 * (MAX_PREFIX_DEPTH + 2));
}

#[test]
fn depth_saturates() {
    let rows: Vec<Row> = (1..=70_000)
        .map(|pid| row(pid, Some(pid - 1), "x"))
        .collect();
    let fx = Fixture::new(rows);
    let view = fx.flatten();
    assert_eq!(view.last().unwrap().depth, u16::MAX);
}

#[test]
fn wide_tree_is_fast() {
    let mut rows = vec![row(1, None, "root")];
    for pid in 2..2_500 {
        rows.push(row(pid, Some(1 + (pid - 2) / 5), "worker"));
    }
    let fx = Fixture::new(rows).filter("work");
    let start = std::time::Instant::now();
    for _ in 0..5 {
        assert_eq!(fx.flatten().len(), 2_499);
    }
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn prefix_helper() {
    assert_eq!(prefix(&[], 0), "");
    assert_eq!(prefix(&[true], 1), "└─ ");
    assert_eq!(prefix(&[false, true, false], 3), "│     ├─ ");
}
