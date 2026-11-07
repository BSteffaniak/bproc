//! Process tree flattening for tree mode.
use super::store::{Row, ViewRow, ViewSpec};
use std::cmp::Ordering;

/// Flatten `rows` into parent-before-children order. Siblings are ordered by
/// `compare`; collapsed nodes hide their subtree and carry an aggregate.
/// When a filter is active, matching rows keep their ancestors visible.
pub fn flatten(
    rows: &[&Row],
    spec: &ViewSpec<'_>,
    compare: &dyn Fn(&Row, &Row) -> Ordering,
) -> Vec<ViewRow> {
    let mut sorted: Vec<&Row> = rows.to_vec();
    sorted.sort_by(|a, b| compare(a, b));
    sorted
        .into_iter()
        .map(|r| ViewRow {
            key: r.key(),
            depth: 0,
            has_children: false,
            collapsed: spec.collapsed.contains(&r.key()),
            tree_prefix: String::new(),
            pinned: false,
            ghost: r.is_ghost(),
            highlights: Vec::new(),
            aggregate: None,
        })
        .collect()
}
