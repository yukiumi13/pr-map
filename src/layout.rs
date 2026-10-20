//! Stack layout, as in the full map (python/pr_map/layout.py, which keeps the same
//! rules).
//!
//! The repository is the root of the map: its children are the base branches the stacks start
//! from (main first), and a session working in the workspace on no branch of the map stands on
//! it. Each station below it gets a lane (row) and a column (depth below the base branches); a
//! station's first child continues its lane and later children open lanes below.

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::model::{Kind, Model, Node, Session};

/// The repository's station; ':' never occurs in a branch name.
pub const ROOT: &str = ":repo";
/// Prefix of a panel cell's target that names a session's pane instead of a station, as in
/// ':pane:w1:p2'; a click there focuses that pane.
pub const PANE: &str = ":pane:";

pub fn repo_name(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

/// {parent: children} in visiting order.
pub type Kids = IndexMap<String, Vec<String>>;

/// The station a session stands on: its branch, or ROOT when it is on no branch of the map.
pub fn at<'a>(session: &'a Session, nodes: &IndexMap<String, Node>) -> &'a str {
    session
        .place
        .current
        .as_deref()
        .filter(|b| nodes.contains_key(*b))
        .unwrap_or(ROOT)
}

/// {parent: children} in map order, ROOT's being the base branches with main first.
///
/// Below a base branch: open PRs by number, then closed ones, then local branches.
pub fn children(nodes: &IndexMap<String, Node>) -> Kids {
    let mut kids = Kids::new();
    kids.insert(ROOT.into(), Vec::new());
    for n in nodes.values() {
        let parent = n
            .parent
            .as_deref()
            .filter(|p| nodes.contains_key(*p))
            .unwrap_or(ROOT);
        kids.entry(parent.to_string())
            .or_default()
            .push(n.branch.clone());
    }
    for (parent, v) in kids.iter_mut() {
        if parent == ROOT {
            v.sort_by_key(|b| (b != "main", b.clone()));
        } else {
            v.sort_by_key(|b| {
                let n = &nodes[b];
                (
                    n.kind == Kind::Local,
                    n.is_closed(),
                    n.pr.as_ref().map_or(0, |p| p.number),
                    b.clone(),
                )
            });
        }
    }
    kids
}

/// The station a branch's stack folds into: its ancestor right below a base branch.
pub fn stack_of(nodes: &IndexMap<String, Node>, branch: &str) -> String {
    let parent = |b: &str| {
        nodes
            .get(b)
            .and_then(|n| n.parent.as_deref())
            .filter(|p| nodes.contains_key(*p))
    };
    let mut b = branch;
    while let Some(p) = parent(b).filter(|p| parent(p).is_some()) {
        b = p;
    }
    b.to_string()
}

/// Stacks with a session on them.
pub fn active_stacks(model: &Model) -> HashSet<String> {
    model
        .sessions
        .iter()
        .filter_map(|s| {
            s.place
                .current
                .as_deref()
                .filter(|b| model.nodes.contains_key(*b))
        })
        .map(|b| stack_of(&model.nodes, b))
        .collect()
}

/// Cuts each stack for which `unfolded(stack)` is false down to its first station.
///
/// Returns:
///   (kids of the shown stations, {folded stack: number of stations hidden behind it}).
pub fn fold(kids: &Kids, unfolded: impl Fn(&str) -> bool) -> (Kids, IndexMap<String, usize>) {
    fn size(kids: &Kids, b: &str) -> usize {
        kids.get(b)
            .map_or(0, |ks| ks.iter().map(|k| 1 + size(kids, k)).sum())
    }
    fn visit(
        kids: &Kids,
        unfolded: &dyn Fn(&str) -> bool,
        b: &str,
        depth: usize,
        shown: &mut Kids,
        hidden: &mut IndexMap<String, usize>,
    ) {
        let mut ks = kids.get(b).cloned().unwrap_or_default();
        if depth == 2 && !ks.is_empty() && !unfolded(b) {
            hidden.insert(b.to_string(), size(kids, b));
            ks.clear();
        }
        shown.insert(b.to_string(), ks.clone());
        for k in &ks {
            visit(kids, unfolded, k, depth + 1, shown, hidden);
        }
    }
    let (mut shown, mut hidden) = (Kids::new(), IndexMap::new());
    visit(kids, &unfolded, ROOT, 0, &mut shown, &mut hidden);
    (shown, hidden)
}

/// {station: (lane, column)} for every station below ROOT; base branches are column 0.
pub fn place(kids: &Kids) -> IndexMap<String, (i64, i64)> {
    fn visit(
        kids: &Kids,
        b: &str,
        lane: i64,
        col: i64,
        pos: &mut IndexMap<String, (i64, i64)>,
    ) -> i64 {
        pos.insert(b.to_string(), (lane, col));
        let mut last = lane;
        for (i, k) in kids.get(b).into_iter().flatten().enumerate() {
            last = visit(kids, k, if i == 0 { lane } else { last + 1 }, col + 1, pos);
        }
        last
    }
    let mut pos = IndexMap::new();
    let mut lane = 0;
    for r in kids.get(ROOT).into_iter().flatten() {
        lane = visit(kids, r, lane, 0, &mut pos) + 1;
    }
    pos
}
