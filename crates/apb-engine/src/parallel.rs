//! Pure logic for parallel branches and joining. No side effects: only the
//! playbook graph + current node statuses. drive uses this to compute a
//! node's successors (a fork when there are several unconditional edges) and
//! join-node readiness. Kept separate so fork/join semantics can be tested
//! in isolation before being wired into the execution loop.

use apb_core::schema::{Edge, EdgeCondition, Playbook, StatusEq};

use crate::state::{NodeStatus, RunState};

/// Join mode (the `join` field on incoming edges). Default is `All`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMode {
    All,
    Any,
}

impl JoinMode {
    fn parse(s: &str) -> JoinMode {
        match s {
            "any" => JoinMode::Any,
            _ => JoinMode::All,
        }
    }
}

/// Readiness of a join node to execute, based on incoming branch statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinReadiness {
    /// Not all required branches have finished yet.
    NotReady,
    /// Ready, incoming branches succeeded - the node can be executed.
    ReadySuccess,
    /// Ready, but one or more branches failed - the join is considered failed (spec 8.4).
    ReadyFailure,
}

fn is_terminal(s: NodeStatus) -> bool {
    matches!(
        s,
        NodeStatus::Succeeded
            | NodeStatus::Failed
            | NodeStatus::TimedOut
            | NodeStatus::Skipped
            | NodeStatus::Cancelled
    )
}

fn succeeded(s: NodeStatus) -> bool {
    s == NodeStatus::Succeeded
}

fn status_matches(node_status: NodeStatus, equals: StatusEq) -> bool {
    match equals {
        StatusEq::Success => node_status == NodeStatus::Succeeded,
        StatusEq::Failure => matches!(node_status, NodeStatus::Failed | NodeStatus::TimedOut),
    }
}

/// Whether the edge's condition matches the current run state.
/// `from` is the edge's source node (for review_status).
pub fn edge_matches(edge: &Edge, from: &str, state: &RunState) -> bool {
    match &edge.condition {
        None => true,
        Some(EdgeCondition::NodeStatus { node, equals }) => state
            .nodes
            .get(node)
            .map(|s| status_matches(*s, *equals))
            .unwrap_or(false),
        Some(EdgeCondition::ReviewStatus { equals }) => state
            .reviews
            .get(from)
            .map(|r| &r.decision == equals)
            .unwrap_or(false),
        Some(EdgeCondition::OutputMatch { node, pattern }) => state
            .outputs
            .get(node)
            .map(|o| o.contains(pattern))
            .unwrap_or(false),
    }
}

/// Whether a bounded loop edge is still available for traversal: its folded
/// traversal count has not yet reached its `max_traversals` cap. A plain edge
/// (no cap) is always available. A bounded edge at its cap is treated as
/// NON-MATCHING during edge selection (spec 2026-07-20-run-reliability), so an
/// alternative edge (or the existing no-edge behavior) applies.
fn edge_available(edge: &Edge, state: &RunState) -> bool {
    match edge.max_traversals {
        Some(cap) => {
            let count = state
                .edge_counts
                .get(&(edge.from.clone(), edge.to.clone()))
                .copied()
                .unwrap_or(0);
            count < cap
        }
        None => true,
    }
}

/// The outgoing edges of `from` actually SELECTED for traversal, mirroring
/// [`successors`] but returning the edges rather than the target node names so
/// a caller that takes these edges can see which carry `max_traversals` and
/// journal a traversal. A bounded edge that has reached its cap is excluded
/// (treated as non-matching), exactly as it is dropped from `successors`.
pub fn selected_edges<'a>(playbook: &'a Playbook, from: &str, state: &RunState) -> Vec<&'a Edge> {
    let out: Vec<&Edge> = playbook.edges.iter().filter(|e| e.from == from).collect();
    let unconditional: Vec<&Edge> = out
        .iter()
        .copied()
        .filter(|e| e.condition.is_none() && !e.fallback && edge_available(e, state))
        .collect();
    if !unconditional.is_empty() {
        return unconditional;
    }
    if let Some(e) = out
        .iter()
        .copied()
        .find(|e| !e.fallback && edge_available(e, state) && edge_matches(e, from, state))
    {
        return vec![e];
    }
    if let Some(e) = out
        .iter()
        .copied()
        .find(|e| e.fallback && edge_available(e, state))
    {
        return vec![e];
    }
    Vec::new()
}

/// Successors of node `from`. Several UNCONDITIONAL outgoing edges = parallel
/// branches (all are returned). If there are no unconditional edges -
/// conditional routing: the first matching non-fallback edge, otherwise the
/// fallback edge (a single target is returned). An empty vector is a dead
/// end (no outgoing edges, or nothing matched and there is no fallback); the
/// caller decides what to do about that. A bounded loop edge whose traversal
/// count has reached its `max_traversals` cap is treated as non-matching.
pub fn successors(playbook: &Playbook, from: &str, state: &RunState) -> Vec<String> {
    selected_edges(playbook, from, state)
        .iter()
        .map(|e| e.to.clone())
        .collect()
}

fn incoming<'a>(playbook: &'a Playbook, node: &str) -> Vec<&'a Edge> {
    playbook.edges.iter().filter(|e| e.to == node).collect()
}

/// A join node (synchronizing) - several incoming edges AND at least one of
/// them carries a `join` field (spec 8.4). Several incoming edges without
/// `join` are an ordinary merge/loop point: the node proceeds as soon as any
/// branch arrives and does not wait (otherwise a loop like
/// `... -> check -> tick -> check` would hang at tick, which has two inputs).
pub fn is_join(playbook: &Playbook, node: &str) -> bool {
    let inc = incoming(playbook, node);
    inc.len() > 1 && inc.iter().any(|e| e.join.is_some())
}

/// The node's join mode: the first `join` set among its incoming edges (default All).
pub fn join_mode(playbook: &Playbook, node: &str) -> JoinMode {
    incoming(playbook, node)
        .iter()
        .find_map(|e| e.join.as_deref())
        .map(JoinMode::parse)
        .unwrap_or(JoinMode::All)
}

/// Readiness of a join node based on the statuses of its incoming edges'
/// source nodes.
/// All: wait until every source is terminal; success if all succeeded, otherwise failure.
/// Any: success as soon as one source succeeds; failure if all are terminal and none succeeded.
pub fn join_readiness(playbook: &Playbook, node: &str, state: &RunState) -> JoinReadiness {
    let sources: Vec<String> = incoming(playbook, node)
        .iter()
        .map(|e| e.from.clone())
        .collect();
    let status = |n: &str| state.nodes.get(n).copied().unwrap_or(NodeStatus::Pending);
    match join_mode(playbook, node) {
        JoinMode::All => {
            if !sources.iter().all(|n| is_terminal(status(n))) {
                return JoinReadiness::NotReady;
            }
            if sources.iter().all(|n| succeeded(status(n))) {
                JoinReadiness::ReadySuccess
            } else {
                JoinReadiness::ReadyFailure
            }
        }
        JoinMode::Any => {
            if sources.iter().any(|n| succeeded(status(n))) {
                JoinReadiness::ReadySuccess
            } else if sources.iter().all(|n| is_terminal(status(n))) {
                JoinReadiness::ReadyFailure
            } else {
                JoinReadiness::NotReady
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::schema::Playbook;

    const DIAMOND: &str = r#"
schema: 1
id: d
name: D
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

    fn playbook() -> Playbook {
        Playbook::from_yaml(DIAMOND).unwrap()
    }

    fn state_with(nodes: &[(&str, NodeStatus)]) -> RunState {
        let mut s = RunState::default();
        for (n, st) in nodes {
            s.nodes.insert((*n).to_string(), *st);
        }
        s
    }

    #[test]
    fn fork_returns_all_unconditional_targets() {
        let succ = successors(&playbook(), "start", &RunState::default());
        assert_eq!(succ.len(), 2);
        assert!(succ.contains(&"a".to_string()));
        assert!(succ.contains(&"b".to_string()));
    }

    #[test]
    fn linear_edge_returns_single_target() {
        assert_eq!(
            successors(&playbook(), "j", &RunState::default()),
            vec!["done".to_string()]
        );
    }

    #[test]
    fn join_detected_and_default_all() {
        assert!(is_join(&playbook(), "j"));
        assert!(!is_join(&playbook(), "a"));
        assert_eq!(join_mode(&playbook(), "j"), JoinMode::All);
    }

    #[test]
    fn join_all_not_ready_until_all_sources_terminal() {
        let s = state_with(&[("a", NodeStatus::Succeeded)]);
        assert_eq!(
            join_readiness(&playbook(), "j", &s),
            JoinReadiness::NotReady
        );
        let s = state_with(&[("a", NodeStatus::Succeeded), ("b", NodeStatus::Succeeded)]);
        assert_eq!(
            join_readiness(&playbook(), "j", &s),
            JoinReadiness::ReadySuccess
        );
    }

    #[test]
    fn join_all_fails_when_a_source_failed() {
        let s = state_with(&[("a", NodeStatus::Succeeded), ("b", NodeStatus::Failed)]);
        assert_eq!(
            join_readiness(&playbook(), "j", &s),
            JoinReadiness::ReadyFailure
        );
    }

    #[test]
    fn join_any_ready_on_first_success() {
        const ANY: &str = r#"
schema: 1
id: d
name: D
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: any }
  - { from: b, to: j, join: any }
  - { from: j, to: done }
"#;
        let playbook = Playbook::from_yaml(ANY).unwrap();
        assert_eq!(join_mode(&playbook, "j"), JoinMode::Any);
        let s = state_with(&[("a", NodeStatus::Succeeded)]);
        assert_eq!(
            join_readiness(&playbook, "j", &s),
            JoinReadiness::ReadySuccess
        );
    }
}
