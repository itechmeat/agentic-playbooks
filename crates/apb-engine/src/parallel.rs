//! Pure logic for parallel branches and joining. No side effects: only the
//! playbook graph + current node statuses. drive uses this to compute a
//! node's successors (a fork when there are several unconditional edges) and
//! join-node readiness. Kept separate so fork/join semantics can be tested
//! in isolation before being wired into the execution loop.

use std::collections::BTreeSet;

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

/// A join node (synchronizing) - several incoming edges, and either
///
///   * at least one of them carries a `join` field (an explicit join, spec
///     8.4), or
///   * every one of them originates outside the node's own strongly connected
///     component: the fan-in is acyclic, so the node is an IMPLICIT all-join
///     and waits for its inputs.
///
/// A merge point INSIDE its own cycle (`... -> check -> tick -> check`, where
/// tick has two inputs and one of them is the back edge) keeps first-arrival
/// semantics: a wait-for-all barrier there would never fire, because the back
/// edge's source has not run in this pass.
pub fn is_join(playbook: &Playbook, node: &str) -> bool {
    let inc = incoming(playbook, node);
    if inc.len() < 2 {
        return false;
    }
    if inc.iter().any(|e| e.join.is_some()) {
        return true;
    }
    let component = apb_core::graphutil::component_of(playbook, node);
    inc.iter().all(|e| !component.contains(&e.from))
}

/// The node's join mode: the first `join` set among its incoming edges (default All).
pub fn join_mode(playbook: &Playbook, node: &str) -> JoinMode {
    incoming(playbook, node)
        .iter()
        .find_map(|e| e.join.as_deref())
        .map(JoinMode::parse)
        .unwrap_or(JoinMode::All)
}

/// What one incoming branch of a join contributes to its readiness.
enum Arrival {
    /// The branch can still arrive: the join has to wait for it.
    Pending,
    /// The branch arrived, with this terminal status.
    Delivered(NodeStatus),
    /// The branch can never arrive: its source is unreachable from the active
    /// nodes, or the source finished and routed somewhere else. Waiting for it
    /// would deadlock, so it is ignored entirely.
    Dead,
}

/// How one incoming edge's source stands. `live` is the forward-reachable
/// region of the active node set (see [`join_readiness`]).
fn arrival(
    playbook: &Playbook,
    node: &str,
    source: &str,
    state: &RunState,
    live: &BTreeSet<String>,
) -> Arrival {
    let status = state
        .nodes
        .get(source)
        .copied()
        .unwrap_or(NodeStatus::Pending);
    if !is_terminal(status) {
        // Still to run, or dead: nothing that can still execute leads here.
        return match live.contains(source) {
            true => Arrival::Pending,
            false => Arrival::Dead,
        };
    }
    // The source is done. It only delivered into this join if its own routing
    // actually selected the edge; an unmatched condition or an exhausted
    // bounded edge means this input never arrives (an either-or merge has one
    // such source by construction).
    match successors(playbook, source, state)
        .iter()
        .any(|s| s == node)
    {
        true => Arrival::Delivered(status),
        false => Arrival::Dead,
    }
}

/// Readiness of a join node based on the statuses of its incoming edges'
/// source nodes.
/// All: wait until every source has arrived or died; success if every arrival
/// succeeded, otherwise failure.
/// Any: success as soon as one source arrives succeeded; failure once no source
/// can still arrive and none succeeded.
///
/// `active` is the set of nodes the run can still execute (the node being
/// advanced past, the other frontier heads, and the members of a running
/// batch). A source outside its forward-reachable region can never arrive and
/// is treated as satisfied, which is what keeps an either-or conditional merge
/// from waiting forever under wait-for-all.
pub fn join_readiness(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    active: &[String],
) -> JoinReadiness {
    let seeds: Vec<&str> = active.iter().map(String::as_str).collect();
    let live = apb_core::graphutil::reachable(playbook, &seeds);
    let arrivals: Vec<Arrival> = incoming(playbook, node)
        .iter()
        .map(|e| arrival(playbook, node, &e.from, state, &live))
        .collect();
    let waiting = arrivals.iter().any(|a| matches!(a, Arrival::Pending));
    let delivered = || {
        arrivals.iter().filter_map(|a| match a {
            Arrival::Delivered(s) => Some(*s),
            _ => None,
        })
    };
    match join_mode(playbook, node) {
        JoinMode::All => {
            if waiting {
                JoinReadiness::NotReady
            } else if delivered().all(succeeded) {
                JoinReadiness::ReadySuccess
            } else {
                JoinReadiness::ReadyFailure
            }
        }
        JoinMode::Any => {
            if delivered().any(succeeded) {
                JoinReadiness::ReadySuccess
            } else if waiting {
                JoinReadiness::NotReady
            } else {
                JoinReadiness::ReadyFailure
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

    /// The same diamond WITHOUT any `join` field: the fan-in of `j` is acyclic,
    /// so it is an implicit all-join.
    const IMPLICIT_DIAMOND: &str = r#"
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
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }
"#;

    /// A bounded loop: `check` has two incoming edges, but one of them comes
    /// from inside its own cycle (`check -> tick -> check`), so it stays a
    /// first-arrival merge point.
    const LOOP_MERGE: &str = r#"
schema: 1
id: d
name: D
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: check, type: condition, max_loops: 2 }
  - { id: tick, type: prompt, prompt: "tick" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: check }
  - { from: check, to: tick, condition: { type: node_status, node: check, equals: failure } }
  - { from: tick, to: check }
  - { from: check, to: done, condition: { type: node_status, node: check, equals: success } }
"#;

    /// An either-or fork: exactly one of `a`/`b` is selected, and both feed the
    /// same merge node `m`.
    const EITHER_OR: &str = r#"
schema: 1
id: d
name: D
version: 1.0.0
nodes:
  - { id: start, type: condition }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: m, type: prompt, prompt: "m" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a, condition: { type: node_status, node: start, equals: success } }
  - { from: start, to: b, condition: { type: node_status, node: start, equals: failure } }
  - { from: a, to: m }
  - { from: b, to: m }
  - { from: m, to: done }
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

    fn active(nodes: &[&str]) -> Vec<String> {
        nodes.iter().map(|n| (*n).to_string()).collect()
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
            join_readiness(&playbook(), "j", &s, &active(&["a", "b"])),
            JoinReadiness::NotReady
        );
        let s = state_with(&[("a", NodeStatus::Succeeded), ("b", NodeStatus::Succeeded)]);
        assert_eq!(
            join_readiness(&playbook(), "j", &s, &active(&["a", "b"])),
            JoinReadiness::ReadySuccess
        );
    }

    #[test]
    fn join_all_fails_when_a_source_failed() {
        let s = state_with(&[("a", NodeStatus::Succeeded), ("b", NodeStatus::Failed)]);
        assert_eq!(
            join_readiness(&playbook(), "j", &s, &active(&["a", "b"])),
            JoinReadiness::ReadyFailure
        );
    }

    #[test]
    fn acyclic_fan_in_without_join_field_is_an_implicit_join() {
        let pb = Playbook::from_yaml(IMPLICIT_DIAMOND).unwrap();
        assert!(is_join(&pb, "j"), "a barrier-less diamond merge waits");
        assert_eq!(join_mode(&pb, "j"), JoinMode::All);
        assert!(!is_join(&pb, "a"), "a single-input node is never a join");
    }

    #[test]
    fn cycle_merge_point_stays_first_arrival() {
        let pb = Playbook::from_yaml(LOOP_MERGE).unwrap();
        assert!(
            !is_join(&pb, "check"),
            "a merge point inside its own cycle must not wait for the back edge"
        );
    }

    #[test]
    fn join_all_ready_when_the_other_source_is_dead() {
        // `a` ran and routed into `m`; `b` was never selected and is no longer
        // reachable from the active set, so waiting for it would deadlock.
        let pb = Playbook::from_yaml(EITHER_OR).unwrap();
        let s = state_with(&[
            ("start", NodeStatus::Succeeded),
            ("a", NodeStatus::Succeeded),
        ]);
        assert_eq!(
            join_readiness(&pb, "m", &s, &active(&["a"])),
            JoinReadiness::ReadySuccess
        );
    }

    #[test]
    fn join_all_waits_while_the_other_source_is_still_reachable() {
        let pb = Playbook::from_yaml(EITHER_OR).unwrap();
        let s = state_with(&[
            ("start", NodeStatus::Succeeded),
            ("a", NodeStatus::Succeeded),
        ]);
        assert_eq!(
            join_readiness(&pb, "m", &s, &active(&["a", "b"])),
            JoinReadiness::NotReady
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
            join_readiness(&playbook, "j", &s, &active(&["a", "b"])),
            JoinReadiness::ReadySuccess
        );
    }
}
