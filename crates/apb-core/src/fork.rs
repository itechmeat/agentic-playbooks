//! The structure of a fork (issue #195): which nodes make up its branches and
//! where those branches merge again. Shared by the validator (V77, V78) and the
//! engine's branch-failure handling, so both answer "is this node inside the
//! fork" the same way.
//!
//! A fork is a node with two or more unconditional, non-fallback outgoing edges;
//! their targets are the branch heads. A node belongs to the branch of head `h`
//! when `h` dominates it in the graph rooted at the fork: every path from the
//! fork to the node passes through `h`. Edges back into the fork are ignored.
//! Dominance is what keeps a rework loop from below the merge
//! (`assemble -> review -> design`) from pulling the merge, or the reworked
//! head, out of place: the review step is reached through either branch, so no
//! head dominates it, while `design` is still dominated by itself. A node only
//! one branch reaches, a dead end or a failure sink only that branch feeds, is
//! part of that branch and is cancelled with it.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::schema::{BranchFailurePolicy, Edge, EdgeCondition, Playbook, StatusEq};

/// The branches of one fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkRegion {
    /// The fork node itself.
    pub fork: String,
    /// The branch heads, in edge declaration order.
    pub heads: Vec<String>,
    /// The nodes of the fork's branches: the nodes some head dominates.
    pub branches: BTreeSet<String>,
    /// The fork's joins: nodes outside the branches that a branch node reaches
    /// along its normal path (an edge that is not a failure route). A shared
    /// failure sink that only failure edges and `fallback` edges lead into is
    /// not a join.
    pub joins: BTreeSet<String>,
}

impl ForkRegion {
    /// Whether `node` is one of the fork's branch nodes.
    pub fn contains(&self, node: &str) -> bool {
        self.branches.contains(node)
    }
}

/// The branch heads of `fork`: the targets of its unconditional, non-fallback
/// outgoing edges, deduplicated in declaration order. Fewer than two means the
/// node does not fork.
pub fn fork_heads(playbook: &Playbook, fork: &str) -> Vec<String> {
    let mut heads: Vec<String> = Vec::new();
    for e in playbook.edges.iter().filter(|e| e.from == fork) {
        if e.condition.is_none() && !e.fallback && e.to != fork && !heads.contains(&e.to) {
            heads.push(e.to.clone());
        }
    }
    heads
}

/// Whether an edge only carries its source's failure: a `fallback` edge, or a
/// `node_status` condition on the source equal to `failure`.
fn is_failure_route(e: &Edge) -> bool {
    e.fallback
        || matches!(&e.condition, Some(EdgeCondition::NodeStatus { node, equals: StatusEq::Failure }) if node == &e.from)
}

/// The dominator sets of every node reachable from `root`, edges into `root`
/// ignored. Iterative data-flow form: graphs here have tens of nodes.
fn dominators<'a>(playbook: &'a Playbook, root: &'a str) -> HashMap<&'a str, HashSet<&'a str>> {
    let mut succ: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut pred: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in playbook.edges.iter().filter(|e| e.to != root) {
        succ.entry(e.from.as_str()).or_default().push(e.to.as_str());
        pred.entry(e.to.as_str()).or_default().push(e.from.as_str());
    }
    let mut order: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut q = VecDeque::from([root]);
    while let Some(n) = q.pop_front() {
        if seen.insert(n) {
            order.push(n);
            for m in succ.get(n).into_iter().flatten() {
                q.push_back(m);
            }
        }
    }
    let all: HashSet<&str> = seen.clone();
    let mut dom: HashMap<&str, HashSet<&str>> = order
        .iter()
        .map(|n| match *n == root {
            true => (*n, HashSet::from([root])),
            false => (*n, all.clone()),
        })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for n in order.iter().skip(1) {
            let mut next: Option<HashSet<&str>> = None;
            for p in pred
                .get(n)
                .into_iter()
                .flatten()
                .filter(|p| seen.contains(*p))
            {
                let d = &dom[p];
                next = Some(match next {
                    None => d.clone(),
                    Some(acc) => acc.intersection(d).copied().collect(),
                });
            }
            let mut next = next.unwrap_or_default();
            next.insert(n);
            if dom[n] != next {
                dom.insert(n, next);
                changed = true;
            }
        }
    }
    dom
}

/// The branches and joins of `fork`, or `None` when the node does not fork.
pub fn fork_region(playbook: &Playbook, fork: &str) -> Option<ForkRegion> {
    let heads = fork_heads(playbook, fork);
    if heads.len() < 2 {
        return None;
    }
    let dom = dominators(playbook, fork);
    let branches: BTreeSet<String> = dom
        .iter()
        .filter(|(n, d)| **n != fork && heads.iter().any(|h| d.contains(h.as_str())))
        .map(|(n, _)| n.to_string())
        .collect();
    let joins: BTreeSet<String> = playbook
        .edges
        .iter()
        .filter(|e| {
            branches.contains(&e.from)
                && !branches.contains(&e.to)
                && e.to != fork
                && !is_failure_route(e)
        })
        .map(|e| e.to.clone())
        .collect();
    Some(ForkRegion {
        fork: fork.to_string(),
        heads,
        branches,
        joins,
    })
}

/// The forks with a policy other than `wait` whose branches contain `node`,
/// innermost first (the fewest branch nodes; declaration order breaks a tie).
/// The first one governs a failure of `node`; the engine escalates to the next
/// one when the failure's routing leaves that fork's branches.
pub fn enclosing_forks(playbook: &Playbook, node: &str) -> Vec<(ForkRegion, BranchFailurePolicy)> {
    let mut forks: Vec<(ForkRegion, BranchFailurePolicy)> = playbook
        .nodes
        .iter()
        .filter_map(|n| {
            let spec = n.fork.as_ref()?;
            if spec.on_branch_failure == BranchFailurePolicy::Wait {
                return None;
            }
            let region = fork_region(playbook, &n.id)?;
            region
                .contains(node)
                .then_some((region, spec.on_branch_failure))
        })
        .collect();
    forks.sort_by_key(|(region, _)| region.branches.len());
    forks
}
