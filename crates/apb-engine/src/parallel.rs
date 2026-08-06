//! Pure logic for parallel branches and joining. No side effects: only the
//! playbook graph + current node statuses. drive uses this to compute a
//! node's successors (a fork when there are several unconditional edges) and
//! join-node readiness. Kept separate so fork/join semantics can be tested
//! in isolation before being wired into the execution loop.

use std::collections::{BTreeSet, VecDeque};

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

/// One top-level field of a node output that parses as a JSON object, as the
/// string an `output_field` condition compares against (spec 2026-08-05 section
/// 2.5). `None` for every shape that cannot be read as one unambiguous string:
/// output that is not JSON, JSON that is not an object, an absent field, and a
/// value that is null, an array or an object. A string is taken verbatim; a bool
/// and a number take their JSON textual form (`true`, `3`, `3.5`).
///
/// Total by construction, because a routing decision must never panic on
/// whatever an agent happened to print: an unreadable output simply means the
/// edge does not apply, and the graph's fallback (or the no-edge behavior)
/// decides, exactly as it does for a node with no output at all.
fn output_field_value(output: &str, field: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(output).ok()?;
    match parsed.as_object()?.get(field)? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
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
        Some(EdgeCondition::OutputField {
            node,
            field,
            equals,
        }) => state
            .outputs
            .get(node)
            .and_then(|o| output_field_value(o, field))
            .is_some_and(|value| value == *equals),
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

/// Why a node synchronizes at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    /// The author declared a `join` on an incoming edge (spec 8.4): the full
    /// contract, including failing the node when a delivered input failed.
    Explicit(JoinMode),
    /// Inferred from an acyclic fan-in. It ONLY synchronizes: it waits for its
    /// inputs and then executes. It never fails the node, because a fan-in the
    /// author drew without a `join` is very often a shared failure sink or error
    /// handler, and a failure edge into such a node exists precisely to deliver
    /// a failure into something that must run.
    Implicit,
}

/// Why `node` synchronizes, or `None` when it does not.
///
/// Several incoming edges are needed either way, plus either an explicit `join`
/// field, or a fan-in where no source lies inside the node's own strongly
/// connected component (the fan-in is acyclic).
///
/// A merge point INSIDE its own cycle (`... -> check -> tick -> check`, where
/// tick has two inputs and one of them is the back edge) keeps first-arrival
/// semantics: a wait-for-all barrier there would never fire, because the back
/// edge's source has not run in this pass.
///
/// The same-component test is one forward walk rather than a full SCC pass:
/// there is already an edge `source -> node`, so the two share a component
/// exactly when `source` is reachable from `node`.
pub fn join_kind(playbook: &Playbook, node: &str) -> Option<JoinKind> {
    let inc = incoming(playbook, node);
    if inc.len() < 2 {
        return None;
    }
    if inc.iter().any(|e| e.join.is_some()) {
        return Some(JoinKind::Explicit(join_mode(playbook, node)));
    }
    // The acyclic-fan-in test itself lives in `apb_core::graphutil` so the
    // validator's `waits_for_all_inputs` and this classification can never
    // disagree about what an implicit barrier is.
    let adj = apb_core::graphutil::adjacency(playbook);
    let sources: Vec<&str> = inc.iter().map(|e| e.from.as_str()).collect();
    match apb_core::graphutil::is_acyclic_fan_in(&adj, node, &sources) {
        true => Some(JoinKind::Implicit),
        false => None,
    }
}

/// Whether `node` synchronizes (see [`join_kind`]).
pub fn is_join(playbook: &Playbook, node: &str) -> bool {
    join_kind(playbook, node).is_some()
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

fn status_of(state: &RunState, node: &str) -> NodeStatus {
    state
        .nodes
        .get(node)
        .copied()
        .unwrap_or(NodeStatus::Pending)
}

/// The nodes a run can still execute, walked forward from `active` over the
/// RESIDUAL graph: out of a node that has already finished only the edges its
/// own routing selected are followed, because the edges it did not take can
/// never be traversed now. Out of a node that has not finished, every outgoing
/// edge is followed - any of them could still be taken.
///
/// Walking the STRUCTURAL graph instead would keep whole branches "live" that
/// the run has already routed away from, which is what makes a shared failure
/// sink (`w1 -> aborted` taken, `w1 -> w2 -> aborted` abandoned) wait forever
/// for an input that can no longer arrive.
fn live_nodes(playbook: &Playbook, state: &RunState, active: &[String]) -> BTreeSet<String> {
    let mut live: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for n in active {
        if live.insert(n.clone()) {
            queue.push_back(n.clone());
        }
    }
    while let Some(id) = queue.pop_front() {
        let next: BTreeSet<String> = match is_terminal(status_of(state, &id)) {
            true => routed_targets(playbook, &id, state),
            false => playbook
                .edges
                .iter()
                .filter(|e| e.from == id)
                .map(|e| e.to.clone())
                .collect(),
        };
        for n in next {
            if live.insert(n.clone()) {
                queue.push_back(n);
            }
        }
    }
    live
}

/// The targets a node that has already finished actually routed into: the edges
/// its routing selects against the current state, PLUS every hop out of it the
/// journal records (a counted edge traversal, or a `defaults.on_failure` policy
/// route).
///
/// The journal half is not redundant. A bounded edge that has reached its
/// `max_traversals` cap stops being selectable, so re-deriving the routing alone
/// forgets a branch it demonstrably took - the target would vanish from
/// liveness, from the rebuilt [`pending_heads`], and from an [`arrival`]'s
/// delivery proof, and a join would fire without it. The policy route is the
/// same problem for a hop that never had an edge at all. This is the single
/// definition of "which way did this node actually go", shared by all three.
///
/// The journal half is filtered against the nodes the CURRENT playbook declares:
/// a supervisor patch can delete a node the journal still carries a hop into,
/// and re-offering it would hand the drive loop a node it cannot resolve. The
/// filter is deliberately on the target NODE, not on a declared edge - a policy
/// route is a real hop with no declared edge, and a hop the run demonstrably
/// took is still outstanding work when a patch merely removes its edge.
fn routed_targets(playbook: &Playbook, node: &str, state: &RunState) -> BTreeSet<String> {
    let mut targets: BTreeSet<String> = successors(playbook, node, state).into_iter().collect();
    let journaled = state
        .edge_counts
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|((from, to), _)| (from, to))
        .chain(state.policy_routes.iter().map(|(from, to)| (from, to)));
    for (from, to) in journaled {
        if from == node && playbook.node(to).is_some() {
            targets.insert(to.clone());
        }
    }
    targets
}

/// The branch heads a run still has outstanding, rebuilt from the journal fold
/// alone: every target a finished node routed into that has not finished itself.
///
/// The drive loop keeps this set in memory as the frontier, so it dies with its
/// driver. Any drive over an existing run dir rebuilds it from here, otherwise a
/// sibling branch that never started is not in the active set, gets written off
/// as dead, and a join fires without it - silently completing a run with a branch
/// missing.
///
/// A handler the `defaults.on_failure` policy routes to is included: the drive
/// journals that route as `EdgeTraversed { via_policy: true }` when it takes it,
/// so [`routed_targets`] sees it without this function re-deriving the
/// failure-policy predicate (which must not drift from the drive loop's copy).
pub fn pending_heads(playbook: &Playbook, state: &RunState) -> Vec<String> {
    let mut heads: BTreeSet<String> = BTreeSet::new();
    for (node, status) in &state.nodes {
        if !is_terminal(*status) {
            continue;
        }
        for s in routed_targets(playbook, node, state) {
            if !is_terminal(status_of(state, &s)) {
                heads.insert(s);
            }
        }
    }
    heads.into_iter().collect()
}

/// How one incoming edge's source stands. `live` is the residual-reachable
/// region of the active node set (see [`live_nodes`]).
fn arrival(
    playbook: &Playbook,
    node: &str,
    source: &str,
    state: &RunState,
    live: &BTreeSet<String>,
) -> Arrival {
    let status = status_of(state, source);
    if !is_terminal(status) {
        // Still to run, or dead: nothing that can still execute leads here.
        return match live.contains(source) {
            true => Arrival::Pending,
            false => Arrival::Dead,
        };
    }
    // The source is done, so it either delivered into this join or routed
    // elsewhere (an unmatched condition; an either-or merge has one such source
    // by construction).
    match routed_targets(playbook, source, state).contains(node) {
        true => Arrival::Delivered(status),
        false => Arrival::Dead,
    }
}

/// Readiness of a join node, from what each of its incoming branches delivered.
///
/// Explicit `all`: wait until every input has arrived or died, then succeed if
/// every arrival succeeded and fail otherwise. Explicit `any`: succeed on the
/// first succeeded arrival, fail once nothing can still arrive and none did.
/// Implicit: wait until nothing can still arrive, then ALWAYS execute - the node
/// is only being synchronized, and failure propagation stays with the node's own
/// failure handling and the author's failure edges.
///
/// `active` is the set of nodes the run can still execute (the node being
/// advanced past, the other frontier heads, the members of a running batch, and
/// on a resume the rebuilt [`pending_heads`]). A source outside the residual
/// region reachable from it can never arrive and is treated as satisfied, which
/// is what keeps a conditional merge from waiting forever under wait-for-all.
pub fn join_readiness(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    active: &[String],
) -> JoinReadiness {
    let Some(kind) = join_kind(playbook, node) else {
        // Not a synchronizing node: first arrival runs it.
        return JoinReadiness::ReadySuccess;
    };
    let live = live_nodes(playbook, state, active);
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
    // Nothing arrived at all, so there is nothing to judge the node by. This is
    // reachable through `defaults.on_failure: <node>`, which pushes a handler
    // onto the frontier without consulting any edge. The node executes rather
    // than being failed by a barrier with no input to fail on - stated here for
    // both modes so the arms below cannot drift apart on it.
    if delivered().next().is_none() && !waiting {
        return JoinReadiness::ReadySuccess;
    }
    match kind {
        JoinKind::Implicit => match waiting {
            true => JoinReadiness::NotReady,
            false => JoinReadiness::ReadySuccess,
        },
        JoinKind::Explicit(JoinMode::All) => {
            if waiting {
                JoinReadiness::NotReady
            } else if delivered().all(succeeded) {
                JoinReadiness::ReadySuccess
            } else {
                JoinReadiness::ReadyFailure
            }
        }
        JoinKind::Explicit(JoinMode::Any) => {
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

/// The incoming sources of `node` that can never arrive, in incoming-edge order:
/// the inputs a join readiness verdict wrote off (see [`arrival`]'s `Dead`).
///
/// Empty for a node that does not synchronize, and empty for a join every input
/// of which either arrived or can still arrive. [`join_readiness`] computes the
/// same arrivals; this function exists so the scheduler can JOURNAL the write-off
/// at the point it acts on the verdict, without a journal handle reaching into
/// this pure module. Both answers come from one implementation, so the event and
/// the verdict can never disagree.
pub fn dead_inputs(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    active: &[String],
) -> Vec<String> {
    if join_kind(playbook, node).is_none() {
        return Vec::new();
    }
    let live = live_nodes(playbook, state, active);
    incoming(playbook, node)
        .iter()
        .filter(|e| {
            matches!(
                arrival(playbook, node, &e.from, state, &live),
                Arrival::Dead
            )
        })
        .map(|e| e.from.clone())
        .collect()
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

    /// A linear chain whose every step routes its own failure into one shared
    /// failure finish node - the most common failure topology in real playbooks.
    const SHARED_FAILURE_SINK: &str = r#"
schema: 1
id: d
name: D
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w1, type: prompt, prompt: "w1" }
  - { id: w2, type: prompt, prompt: "w2" }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: start, to: w1 }
  - { from: w1, to: w2, condition: { type: node_status, node: w1, equals: success } }
  - { from: w1, to: aborted, condition: { type: node_status, node: w1, equals: failure } }
  - { from: w2, to: done, condition: { type: node_status, node: w2, equals: success } }
  - { from: w2, to: aborted, condition: { type: node_status, node: w2, equals: failure } }
"#;

    /// An explicit `join: all` whose `a` input arrives over a bounded edge: once
    /// that edge reaches its cap it stops showing up in `successors`, but it did
    /// deliver.
    const BOUNDED_INPUT_JOIN: &str = r#"
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
  - { from: a, to: j, join: all, max_traversals: 1 }
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
    fn liveness_follows_only_the_edges_a_finished_node_selected() {
        // `w1` failed and routed into the shared failure sink. `w2` sits on the
        // success path `w1` did NOT take, so it can never arrive - even though
        // the structural graph still has an edge from `w1` towards it.
        let pb = Playbook::from_yaml(SHARED_FAILURE_SINK).unwrap();
        let s = state_with(&[("start", NodeStatus::Succeeded), ("w1", NodeStatus::Failed)]);
        let live = live_nodes(&pb, &s, &active(&["w1"]));
        assert!(live.contains("aborted"), "the selected route stays live");
        assert!(
            !live.contains("w2"),
            "a route a finished node did not select is not live"
        );
    }

    #[test]
    fn an_implicit_join_never_fails_the_node_on_a_failed_input() {
        // A failure edge into a shared sink or handler exists precisely to
        // deliver a failure into a node that must run. An implicit barrier only
        // synchronizes; it must not fail the node it is guarding.
        let pb = Playbook::from_yaml(SHARED_FAILURE_SINK).unwrap();
        let s = state_with(&[("start", NodeStatus::Succeeded), ("w1", NodeStatus::Failed)]);
        assert_eq!(
            join_readiness(&pb, "aborted", &s, &active(&["w1"])),
            JoinReadiness::ReadySuccess
        );
    }

    #[test]
    fn an_explicit_join_still_fails_the_node_on_a_failed_input() {
        // The `join:` contract is unchanged: a delivered failure fails the join.
        let pb = Playbook::from_yaml(DIAMOND).unwrap();
        let s = state_with(&[("a", NodeStatus::Succeeded), ("b", NodeStatus::Failed)]);
        assert_eq!(
            join_readiness(&pb, "j", &s, &active(&["a", "b"])),
            JoinReadiness::ReadyFailure
        );
    }

    #[test]
    fn a_delivered_input_survives_its_bounded_edge_reaching_the_cap() {
        // `a -> j` delivered once and thereby hit its cap, so `successors(a)`
        // no longer offers it. The journaled traversal count is proof it did
        // arrive, so the join must still see the failure rather than treat the
        // input as dead and pass vacuously.
        let pb = Playbook::from_yaml(BOUNDED_INPUT_JOIN).unwrap();
        let mut s = state_with(&[("a", NodeStatus::Failed), ("b", NodeStatus::Succeeded)]);
        s.edge_counts.insert(("a".into(), "j".into()), 1);
        assert_eq!(
            join_readiness(&pb, "j", &s, &active(&["a", "b"])),
            JoinReadiness::ReadyFailure
        );
    }

    #[test]
    fn a_join_with_no_arrivals_at_all_executes() {
        // Reachable through `defaults.on_failure`, which pushes a handler onto
        // the frontier without consulting any edge. Nothing arrived, so there is
        // nothing to fail on: the node runs. Stated for EVERY kind so the arms
        // cannot drift apart.
        let s = state_with(&[("start", NodeStatus::Succeeded)]);
        let with_join = |value: &str| {
            let yaml = SHARED_FAILURE_SINK.replace(
                "{ from: w1, to: aborted, condition:",
                &format!("{{ from: w1, to: aborted, join: {value}, condition:"),
            );
            Playbook::from_yaml(&yaml).unwrap()
        };

        let implicit = Playbook::from_yaml(SHARED_FAILURE_SINK).unwrap();
        assert_eq!(join_kind(&implicit, "aborted"), Some(JoinKind::Implicit));
        assert_eq!(
            join_readiness(&implicit, "aborted", &s, &active(&["aborted"])),
            JoinReadiness::ReadySuccess
        );

        let all = with_join("all");
        assert_eq!(
            join_kind(&all, "aborted"),
            Some(JoinKind::Explicit(JoinMode::All))
        );
        assert_eq!(
            join_readiness(&all, "aborted", &s, &active(&["aborted"])),
            JoinReadiness::ReadySuccess
        );

        let any = with_join("any");
        assert_eq!(
            join_kind(&any, "aborted"),
            Some(JoinKind::Explicit(JoinMode::Any))
        );
        assert_eq!(
            join_readiness(&any, "aborted", &s, &active(&["aborted"])),
            JoinReadiness::ReadySuccess
        );
    }

    #[test]
    fn pending_heads_rebuilds_the_branch_heads_a_resume_lost() {
        // A resume starts with an empty frontier. Without rebuilding the heads
        // the run has outstanding, an unstarted sibling branch would be judged
        // dead and the join would fire without it.
        let pb = playbook();
        let s = state_with(&[
            ("start", NodeStatus::Succeeded),
            ("a", NodeStatus::Succeeded),
        ]);
        assert_eq!(
            pending_heads(&pb, &s),
            vec!["b".to_string(), "j".to_string()]
        );
        assert_eq!(
            join_readiness(&pb, "j", &s, &active(&["a", "b", "j"])),
            JoinReadiness::NotReady
        );
    }

    /// A handler reached through `defaults.on_failure` traverses no declared
    /// edge, so before the drive journaled the route this reconstruction could
    /// not see it and a driver death lost the handler entirely (Task 1 handover
    /// note 3).
    #[test]
    fn a_policy_routed_handler_shows_up_as_a_pending_head() {
        const POLICY_HANDLER: &str = r#"
schema: 1
id: d
name: D
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: prompt, prompt: "work" }
  - { id: handler, type: prompt, prompt: "recover" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: handler, to: done }
"#;
        let pb = Playbook::from_yaml(POLICY_HANDLER).unwrap();
        let mut s = state_with(&[
            ("start", NodeStatus::Succeeded),
            ("work", NodeStatus::Failed),
        ]);
        // What the drive journals when it takes the policy route.
        s.policy_routes
            .insert(("work".to_string(), "handler".to_string()));
        assert_eq!(pending_heads(&pb, &s), vec!["handler".to_string()]);
    }

    /// Patch resurrection: a supervisor patch can delete a node the journal
    /// still carries a traversal into. Re-offering it would hand the drive loop
    /// a node the current playbook does not declare.
    #[test]
    fn a_traversal_into_a_node_the_playbook_dropped_is_not_resurrected() {
        let pb = playbook();
        let mut s = state_with(&[
            ("start", NodeStatus::Succeeded),
            ("a", NodeStatus::Succeeded),
        ]);
        s.edge_counts.insert(("a".into(), "ghost".into()), 1);
        s.policy_routes
            .insert(("a".to_string(), "phantom".to_string()));
        let heads = pending_heads(&pb, &s);
        assert!(
            !heads.contains(&"ghost".to_string()) && !heads.contains(&"phantom".to_string()),
            "a head the playbook no longer declares must be dropped, got: {heads:?}"
        );
        assert!(
            heads.contains(&"b".to_string()),
            "the real outstanding head survives the filter, got: {heads:?}"
        );
    }

    /// The write-off the scheduler journals is computed here, so the pure
    /// readiness verdict and the observable event can never disagree.
    #[test]
    fn dead_inputs_names_the_source_that_can_never_arrive() {
        let pb = Playbook::from_yaml(EITHER_OR).unwrap();
        let s = state_with(&[
            ("start", NodeStatus::Succeeded),
            ("a", NodeStatus::Succeeded),
        ]);
        assert_eq!(dead_inputs(&pb, "m", &s, &active(&["a"])), vec!["b"]);
        assert!(
            dead_inputs(&pb, "m", &s, &active(&["a", "b"])).is_empty(),
            "a source that can still arrive is not written off"
        );
        assert!(
            dead_inputs(&pb, "a", &s, &active(&["a"])).is_empty(),
            "a node that is not a join writes nothing off"
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

    // --- output_field (spec 2026-08-05 section 2.5) ---
    //
    // The condition reads ONE top-level field of a source output that parses as
    // a JSON object. Every shape it cannot read is a NON-match, never a panic:
    // routing must degrade to "this edge does not apply" so the graph's fallback
    // (or the no-edge behavior) decides, exactly as an unset output does.

    /// An `output_field` edge from `w` reading `verdict`, against an output.
    fn output_field_matches(output: Option<&str>, equals: &str) -> bool {
        let edge = Edge {
            from: "w".into(),
            to: "next".into(),
            condition: Some(EdgeCondition::OutputField {
                node: "w".into(),
                field: "verdict".into(),
                equals: equals.into(),
            }),
            fallback: false,
            join: None,
            max_traversals: None,
        };
        let mut state = RunState::default();
        if let Some(text) = output {
            state.outputs.insert("w".into(), text.to_string());
        }
        edge_matches(&edge, "w", &state)
    }

    #[test]
    fn output_field_matches_a_top_level_string_field() {
        assert!(output_field_matches(
            Some(r#"{"verdict":"failed","note":"x"}"#),
            "failed"
        ));
        assert!(!output_field_matches(Some(r#"{"verdict":"ok"}"#), "failed"));
    }

    #[test]
    fn output_field_stringifies_a_bool_or_number_field() {
        assert!(output_field_matches(Some(r#"{"verdict":true}"#), "true"));
        assert!(output_field_matches(Some(r#"{"verdict":3}"#), "3"));
        assert!(!output_field_matches(Some(r#"{"verdict":3}"#), "3.0"));
    }

    #[test]
    fn output_field_never_matches_what_it_cannot_read() {
        // No output recorded at all.
        assert!(!output_field_matches(None, "failed"));
        // Output that is not JSON.
        assert!(!output_field_matches(Some("all checks failed"), "failed"));
        // Valid JSON that is not an object.
        assert!(!output_field_matches(Some(r#"["failed"]"#), "failed"));
        assert!(!output_field_matches(Some(r#""failed""#), "failed"));
        // An object without the field.
        assert!(!output_field_matches(
            Some(r#"{"other":"failed"}"#),
            "failed"
        ));
        // A field whose value has no unambiguous string form.
        assert!(!output_field_matches(Some(r#"{"verdict":null}"#), "null"));
        assert!(!output_field_matches(
            Some(r#"{"verdict":["failed"]}"#),
            "failed"
        ));
        assert!(!output_field_matches(
            Some(r#"{"verdict":{"v":"failed"}}"#),
            "failed"
        ));
        // Empty output.
        assert!(!output_field_matches(Some(""), "failed"));
    }

    #[test]
    fn output_field_comparison_is_exact() {
        // Not a substring test (that is what output_match is for), and not
        // case-insensitive.
        assert!(!output_field_matches(
            Some(r#"{"verdict":"failed"}"#),
            "fail"
        ));
        assert!(!output_field_matches(
            Some(r#"{"verdict":"FAILED"}"#),
            "failed"
        ));
        // Surrounding whitespace in the OUTPUT is JSON insignificance, so it is
        // tolerated; whitespace inside the compared value is not.
        assert!(output_field_matches(
            Some("  {\"verdict\":\"failed\"}\n"),
            "failed"
        ));
        assert!(!output_field_matches(
            Some(r#"{"verdict":" failed"}"#),
            "failed"
        ));
    }
}
