//! The structure of a fork (issue #195): which nodes make up its branches and
//! where those branches merge again. Shared by the validator (V77, V78) and the
//! engine's branch-failure handling, so both answer "is this node inside the
//! fork" the same way.
//!
//! A fork is a node with two or more unconditional, non-fallback outgoing edges;
//! their targets are the branch heads. Everything is computed on the structural
//! graph, never through the fork node itself, so a loop that comes back to the
//! fork (`fork -> a -> join -> check -> fork`) does not make every node part of
//! every branch.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::schema::{BranchFailurePolicy, Playbook};

/// The branches of one fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkRegion {
    /// The fork node itself.
    pub fork: String,
    /// The branch heads, in edge declaration order.
    pub heads: Vec<String>,
    /// The nodes of the fork's branches: reachable from a head without passing
    /// through the fork, not reachable from every head, and either a head or on
    /// a path into a merge point. A node a single branch leaves through (a
    /// failure sink only that branch feeds) is outside the branches.
    pub branches: BTreeSet<String>,
    /// The merge points: nodes every head reaches with an incoming edge from a
    /// branch node. The fork's joins.
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

fn adjacency_without<'a>(playbook: &'a Playbook, skip: &str) -> HashMap<&'a str, Vec<&'a str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &playbook.edges {
        if e.from != skip && e.to != skip {
            adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
        }
    }
    adj
}

fn reach<'a>(adj: &HashMap<&'a str, Vec<&'a str>>, from: &'a str) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut q = VecDeque::from([from]);
    while let Some(id) = q.pop_front() {
        if seen.insert(id) {
            for next in adj.get(id).into_iter().flatten() {
                q.push_back(next);
            }
        }
    }
    seen
}

/// The branches and joins of `fork`, or `None` when the node does not fork.
pub fn fork_region(playbook: &Playbook, fork: &str) -> Option<ForkRegion> {
    let heads = fork_heads(playbook, fork);
    if heads.len() < 2 {
        return None;
    }
    let adj = adjacency_without(playbook, fork);
    let reached: Vec<HashSet<&str>> = heads.iter().map(|h| reach(&adj, h.as_str())).collect();
    let union: HashSet<&str> = reached.iter().flatten().copied().collect();
    let common: HashSet<&str> = union
        .iter()
        .copied()
        .filter(|n| reached.iter().all(|r| r.contains(n)))
        .collect();
    let candidates: HashSet<&str> = union.difference(&common).copied().collect();
    let joins: BTreeSet<String> = playbook
        .edges
        .iter()
        .filter(|e| candidates.contains(e.from.as_str()) && common.contains(e.to.as_str()))
        .map(|e| e.to.clone())
        .collect();
    let branches: BTreeSet<String> = candidates
        .iter()
        .filter(|n| {
            heads.iter().any(|h| h == *n)
                || joins.is_empty()
                || reach(&adj, n).iter().any(|r| joins.contains(*r))
        })
        .map(|n| n.to_string())
        .collect();
    Some(ForkRegion {
        fork: fork.to_string(),
        heads,
        branches,
        joins,
    })
}

/// The fork whose branch-failure policy governs a failure of `node`: among the
/// forks declaring a policy other than `wait` whose branches contain `node`,
/// the innermost one (the fewest branch nodes; declaration order breaks a tie).
pub fn governing_fork(
    playbook: &Playbook,
    node: &str,
) -> Option<(ForkRegion, BranchFailurePolicy)> {
    playbook
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
        .min_by_key(|(region, _)| region.branches.len())
}
