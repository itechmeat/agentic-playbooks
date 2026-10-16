//! The state a drive weighs joins against when it is about to execute a node
//! again (issue #203): a `--from-node` resume, an interrupted or parallel
//! fallback restart, a supervisor retry of a failed batch member.
//!
//! The journal fold still reads the re-run node as finished (typically
//! `failed`, routed into a failure edge), and every node downstream of it keeps
//! the routing of the old pass. Weighing a join against that fold writes the
//! re-run node's branch off as dead, so the join starts together with the
//! re-run node, and an old hop of a downstream node (`j -> done`) re-offers a
//! finish node that ends the run before the join re-runs on the new input.
//! The revived view removes both: those nodes read as not yet run. The drive
//! journals it as `nodes_revived`, and the fold applies it
//! (`RunState::revive`), so every later readiness decision sees it.

use std::collections::{BTreeSet, HashMap, VecDeque};

use apb_core::schema::Playbook;

use crate::state::{NodeStatus, RunState};

/// The nodes a drive re-executes when it executes every node of `starts`
/// again, per start and as a whole. Built once per rebuild: the forward and
/// reverse adjacency are computed once, not once per question.
#[derive(Debug, Clone, Default)]
pub struct Revival {
    by_start: Vec<(String, BTreeSet<String>)>,
    all: BTreeSet<String>,
}

impl Revival {
    /// The revival of `starts` against `state`, the fold it is applied to:
    /// the state decides which `defaults.on_failure` hops are real (see
    /// below).
    pub fn new(playbook: &Playbook, state: &RunState, starts: &[String]) -> Self {
        let mut forward = apb_core::graphutil::adjacency(playbook);
        let mut reverse: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in &playbook.edges {
            reverse
                .entry(e.to.as_str())
                .or_default()
                .push(e.from.as_str());
        }
        // A `defaults.on_failure` handler is a real hop, with no declared
        // edge, out of a node whose failure took it (a journaled policy hop),
        // and out of a start that failed (its re-run may fail into it again).
        // Without it the handler would stay outside the revival while its own
        // edges are weighed against the revived nodes, and a fallback edge it
        // never took would become a stale head. Not out of every covered
        // node: one that succeeded never routed there, and a join fed by the
        // handler would wait on a handler nobody routes to again. Forward
        // only: the ancestor walk stays on declared edges, or a handler with
        // a rework path back upstream would make every node an ancestor and
        // shrink the revival to the start alone.
        for n in &playbook.nodes {
            let Some(handler) = playbook.defaults.on_failure.target_for(&n.id) else {
                continue;
            };
            let routed = state
                .journaled_hops
                .contains(&(n.id.clone(), handler.to_string()));
            // A failed start reaches the handler only when none of its own
            // declared edges takes the failure.
            let failed_start = starts.contains(&n.id)
                && state
                    .nodes
                    .get(&n.id)
                    .is_some_and(|st| matches!(st, NodeStatus::Failed | NodeStatus::TimedOut))
                && super::selected_edges(playbook, &n.id, state).is_empty();
            if routed || failed_start {
                forward.entry(n.id.as_str()).or_default().push(handler);
            }
        }
        let mut out = Revival::default();
        for start in starts {
            let nodes = downstream_only(&forward, &reverse, start);
            out.all.extend(nodes.iter().cloned());
            out.by_start.push((start.clone(), nodes));
        }
        out
    }

    /// Adds `node` to the revival of `start` (a start of this revival).
    pub fn add(&mut self, start: &str, node: &str) {
        if let Some((_, nodes)) = self.by_start.iter_mut().find(|(s, _)| s == start) {
            nodes.insert(node.to_string());
            self.all.insert(node.to_string());
        }
    }

    /// Every revived node, of every start.
    pub fn nodes(&self) -> &BTreeSet<String> {
        &self.all
    }

    pub fn contains(&self, node: &str) -> bool {
        self.all.contains(node)
    }

    /// Each start with the nodes its re-execution revives, in start order.
    pub fn per_start(&self) -> impl Iterator<Item = (&str, &BTreeSet<String>)> {
        self.by_start.iter().map(|(s, n)| (s.as_str(), n))
    }

    /// The first start whose re-execution revives `node`.
    pub fn start_of(&self, node: &str) -> Option<&str> {
        self.by_start
            .iter()
            .find(|(_, nodes)| nodes.contains(node))
            .map(|(start, _)| start.as_str())
    }
}

/// `start` itself and every node only downstream of it: the forward walk from
/// `start` that never enters an ancestor of `start` (a node that can reach
/// it).
///
/// Inside a loop the walk stops where the cycle leads back upstream. What lies
/// beyond an ancestor is downstream of that ancestor rather than of `start`,
/// and reviving it would drop the routing that keeps a never-started sibling a
/// pending head, or re-run a sibling that already completed (`a -> {x, y}`,
/// `x -> a`: re-running `x` revives neither `a` nor `y`). The cycle keeps its
/// old routing and the next loop pass re-runs it as usual.
fn downstream_only(
    forward: &HashMap<&str, Vec<&str>>,
    reverse: &HashMap<&str, Vec<&str>>,
    start: &str,
) -> BTreeSet<String> {
    let ancestors = walk(reverse, start, &BTreeSet::new());
    let mut out = walk(forward, start, &ancestors);
    out.insert(start.to_string());
    out
}

/// The nodes reachable from `from` over `adj` without entering `stop`,
/// `from` itself excluded unless a cycle leads back to it.
fn walk(adj: &HashMap<&str, Vec<&str>>, from: &str, stop: &BTreeSet<String>) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([from]);
    while let Some(id) = queue.pop_front() {
        for next in adj.get(id).into_iter().flatten() {
            if !stop.contains(*next) && seen.insert((*next).to_string()) {
                queue.push_back(next);
            }
        }
    }
    seen.remove(from);
    seen
}

/// The nodes a re-execution of `start` runs again (see [`Revival`]).
pub fn revived_nodes(playbook: &Playbook, state: &RunState, start: &str) -> BTreeSet<String> {
    Revival::new(playbook, state, &[start.to_string()]).all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::{
        JoinReadiness, dead_inputs, join_readiness, pending_heads, pending_inputs,
    };
    use crate::state::{NodeStatus, RunState};

    /// `state` with the revival of `starts` applied, as the fold applies a
    /// `nodes_revived` record.
    fn revived(p: &Playbook, state: &RunState, starts: &[String]) -> RunState {
        let mut s = state.clone();
        s.revive(Revival::new(p, state, starts).nodes());
        s
    }

    /// The issue's shape: `b` delivered into the implicit join `j`, `c` failed
    /// into `failed`, so `c2` never ran and `j` was weighed without it.
    const REPORT: &str = r#"
schema: 2
id: r
name: R
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: c, type: prompt, prompt: "c" }
  - { id: c2, type: prompt, prompt: "c2" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: j }
  - { from: c, to: c2, condition: { type: node_status, node: c, equals: success } }
  - { from: c, to: failed, condition: { type: node_status, node: c, equals: failure } }
  - { from: c2, to: j }
  - { from: j, to: done }
"#;

    /// A loop with a fork inside: `a` forks into `x` and `y`, `x` loops back
    /// into `a`. `y` never started.
    const LOOP_FORK: &str = r#"
schema: 2
id: l
name: L
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: x, type: prompt, prompt: "x" }
  - { id: y, type: prompt, prompt: "y" }
edges:
  - { from: start, to: a }
  - { from: a, to: x }
  - { from: a, to: y }
  - { from: x, to: a, max_traversals: 2 }
"#;

    fn pb(yaml: &str) -> Playbook {
        Playbook::from_yaml(yaml).unwrap()
    }

    fn state(nodes: &[(&str, NodeStatus)], hops: &[(&str, &str)]) -> RunState {
        let mut s = RunState::default();
        for (n, st) in nodes {
            s.nodes.insert((*n).to_string(), *st);
        }
        for (f, t) in hops {
            s.journaled_hops
                .insert(((*f).to_string(), (*t).to_string()));
        }
        s
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    /// The fold of the report's first pass: `c` failed into `failed`.
    fn report_failed_pass() -> RunState {
        state(
            &[
                ("start", NodeStatus::Succeeded),
                ("a", NodeStatus::Succeeded),
                ("b", NodeStatus::Succeeded),
                ("c", NodeStatus::Failed),
                ("failed", NodeStatus::Succeeded),
            ],
            &[
                ("start", "a"),
                ("a", "b"),
                ("a", "c"),
                ("b", "j"),
                ("c", "failed"),
            ],
        )
    }

    #[test]
    fn revived_nodes_is_the_downstream_closure_without_ancestors() {
        let p = pb(REPORT);
        assert_eq!(
            revived_nodes(&p, &RunState::default(), "c")
                .into_iter()
                .collect::<Vec<_>>(),
            names(&["c", "c2", "done", "failed", "j"])
        );
    }

    #[test]
    fn rerun_node_keeps_its_branch_alive_for_the_join() {
        let p = pb(REPORT);
        let old = report_failed_pass();
        let active = names(&["c", "j"]);
        // Against the old fold the join is ready without `c2`: the bug.
        assert_eq!(
            join_readiness(&p, "j", &old, &active),
            JoinReadiness::ReadySuccess
        );
        assert_eq!(dead_inputs(&p, "j", &old, &active), names(&["c2"]));

        let new = revived(&p, &old, &names(&["c"]));
        assert_eq!(
            join_readiness(&p, "j", &new, &active),
            JoinReadiness::NotReady
        );
        assert_eq!(pending_inputs(&p, "j", &new, &active), names(&["c2"]));
        assert!(dead_inputs(&p, "j", &new, &active).is_empty());
        // The old failure route is not re-offered, the join still is (it never
        // finished); the re-run node itself is the drive's current.
        assert_eq!(pending_heads(&p, &new), names(&["j"]));
    }

    #[test]
    fn stale_hops_of_downstream_nodes_are_dropped() {
        // R3 shape: the join `j` ran on the first pass and routed into `done`.
        let p = pb(REPORT);
        let mut old = report_failed_pass();
        old.nodes.insert("j".into(), NodeStatus::Succeeded);
        old.journaled_hops.insert(("j".into(), "done".into()));
        assert!(pending_heads(&p, &old).contains(&"done".to_string()));

        let new = revived(&p, &old, &names(&["c"]));
        // Neither the stale `done` nor the join that already ran is a head:
        // the join runs again when `c`'s branch delivers into it.
        assert!(pending_heads(&p, &new).is_empty());
        assert_eq!(new.current_status("j"), NodeStatus::Pending);
        // The history keeps the earlier result.
        assert_eq!(new.nodes.get("j"), Some(&NodeStatus::Succeeded));
        // Upstream of the rerun node nothing changes.
        assert_eq!(new.nodes.get("b"), Some(&NodeStatus::Succeeded));
        assert!(new.routes("b", "j"));
        // The revived join's old hop stays in the history, but does not route.
        assert!(new.journaled_hops.contains(&("j".into(), "done".into())));
        assert!(!new.routes("j", "done"));
    }

    #[test]
    fn a_loop_keeps_the_routing_of_its_upstream_fan_out() {
        let p = pb(LOOP_FORK);
        // Inside the cycle `a` is an ancestor of `x`: the walk stops there,
        // so neither `a` nor `y` (downstream only through `a`) is revived.
        assert_eq!(
            revived_nodes(&p, &RunState::default(), "x")
                .into_iter()
                .collect::<Vec<_>>(),
            names(&["x"])
        );
        let old = state(
            &[
                ("start", NodeStatus::Succeeded),
                ("a", NodeStatus::Succeeded),
                ("x", NodeStatus::Failed),
            ],
            &[("start", "a"), ("a", "x"), ("a", "y")],
        );
        let new = revived(&p, &old, &names(&["x"]));
        // `a`'s hops survive, so the never-started sibling stays a head.
        assert_eq!(pending_heads(&p, &new), names(&["y"]));
        assert_eq!(new.nodes.get("a"), Some(&NodeStatus::Succeeded));
    }

    /// Review finding: a sibling that already completed is not re-run by a
    /// re-run of the other sibling inside the loop.
    #[test]
    fn a_loop_does_not_revive_a_completed_sibling() {
        let p = pb(LOOP_FORK);
        let old = state(
            &[
                ("start", NodeStatus::Succeeded),
                ("a", NodeStatus::Succeeded),
                ("x", NodeStatus::Failed),
                ("y", NodeStatus::Succeeded),
            ],
            &[("start", "a"), ("a", "x"), ("a", "y")],
        );
        let new = revived(&p, &old, &names(&["x"]));
        assert_eq!(new.current_status("y"), NodeStatus::Succeeded);
        assert!(pending_heads(&p, &new).is_empty());
    }

    /// A join inside the cycle (`a -> {x, y}`, `x -> j`, `y -> j`, `j -> a`):
    /// every other node of the cycle can reach `x`, so a re-run of `x` revives
    /// `x` alone, and `j` keeps whatever the old pass left it.
    #[test]
    fn a_join_inside_the_cycle_is_not_revived() {
        let p = pb(r#"
schema: 2
id: l
name: L
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: x, type: prompt, prompt: "x" }
  - { id: y, type: prompt, prompt: "y" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: x }
  - { from: a, to: y }
  - { from: x, to: j }
  - { from: y, to: j }
  - { from: j, to: a, max_traversals: 1 }
  - { from: j, to: done, fallback: true }
"#);
        assert_eq!(
            revived_nodes(&p, &RunState::default(), "x")
                .into_iter()
                .collect::<Vec<_>>(),
            names(&["x"])
        );
        // From `j` the walk stops at `a`: `done` is the only node beyond it.
        assert_eq!(
            revived_nodes(&p, &RunState::default(), "j")
                .into_iter()
                .collect::<Vec<_>>(),
            names(&["done", "j"])
        );
    }

    const HANDLED: &str = r#"
schema: 2
id: h
name: H
version: 1.0.0
defaults: { on_failure: handler }
nodes:
  - { id: start, type: start }
  - { id: c, type: prompt, prompt: "c" }
  - { id: d, type: prompt, prompt: "d" }
  - { id: handler, type: prompt, prompt: "handler" }
  - { id: notify, type: prompt, prompt: "notify" }
  - { id: other, type: prompt, prompt: "other" }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: c }
  - { from: c, to: d }
  - { from: d, to: done }
  - { from: handler, to: notify, condition: { type: node_status, node: c, equals: failure } }
  - { from: handler, to: other, fallback: true }
  - { from: notify, to: failed }
  - { from: other, to: failed }
"#;

    /// Round 4: a `defaults.on_failure` handler the re-run node's failure
    /// took is downstream of it, so a re-run revives it; its never-taken
    /// fallback edge is no stale head.
    #[test]
    fn a_failure_policy_handler_is_revived_with_the_node() {
        let p = pb(HANDLED);
        let old = state(
            &[
                ("start", NodeStatus::Succeeded),
                ("c", NodeStatus::Failed),
                ("handler", NodeStatus::Succeeded),
            ],
            &[("start", "c"), ("c", "handler"), ("handler", "notify")],
        );
        let set = revived_nodes(&p, &old, "c");
        assert!(set.contains("handler") && set.contains("notify"), "{set:?}");
        let new = revived(&p, &old, &names(&["c"]));
        assert!(!pending_heads(&p, &new).contains(&"other".to_string()));
    }

    /// Round 6: only a node whose own result routed into the handler (or a
    /// failed start) leads there. `c` succeeded and `d` failed into the
    /// handler: re-running `c` revives `d` and through it the handler;
    /// re-running a succeeded `c` whose closure never failed does not.
    #[test]
    fn a_failure_policy_handler_is_revived_only_from_a_real_route() {
        let p = pb(HANDLED);
        let clean = state(
            &[
                ("start", NodeStatus::Succeeded),
                ("c", NodeStatus::Succeeded),
                ("d", NodeStatus::Succeeded),
            ],
            &[("start", "c"), ("c", "d"), ("d", "done")],
        );
        let set = revived_nodes(&p, &clean, "c");
        assert!(!set.contains("handler"), "{set:?}");
        let d_failed = state(
            &[
                ("start", NodeStatus::Succeeded),
                ("c", NodeStatus::Succeeded),
                ("d", NodeStatus::Failed),
                ("handler", NodeStatus::Succeeded),
            ],
            &[("start", "c"), ("c", "d"), ("d", "handler")],
        );
        assert!(revived_nodes(&p, &d_failed, "c").contains("handler"));
    }

    /// Round 7 item 6: a failed start whose own declared failure edge takes
    /// the failure never reaches the `defaults.on_failure` handler.
    #[test]
    fn a_failed_start_with_its_own_failure_edge_skips_the_handler() {
        let with_edge = HANDLED.replace(
            "  - { from: c, to: d }\n",
            "  - { from: c, to: d, condition: { type: node_status, node: c, equals: success } }\n  - { from: c, to: notify, condition: { type: node_status, node: c, equals: failure } }\n",
        );
        let failed = state(
            &[("start", NodeStatus::Succeeded), ("c", NodeStatus::Failed)],
            &[("start", "c")],
        );
        assert!(!revived_nodes(&pb(&with_edge), &failed, "c").contains("handler"));
        assert!(revived_nodes(&pb(HANDLED.replace("  - { from: c, to: d }\n", "  - { from: c, to: d, condition: { type: node_status, node: c, equals: success } }\n").as_str()), &failed, "c").contains("handler"));
    }

    #[test]
    fn revival_names_the_start_that_revives_a_node() {
        let p = pb(REPORT);
        let r = Revival::new(&p, &RunState::default(), &names(&["b", "c"]));
        assert_eq!(r.start_of("c2"), Some("c"));
        assert_eq!(r.start_of("j"), Some("b"));
        assert_eq!(r.start_of("a"), None);
        assert!(r.contains("done") && !r.contains("a"));
    }

    /// Policy marks of a revived node stay in the history and do not matter
    /// while it is revived: its own edges are never weighed then, and its
    /// next start clears them.
    #[test]
    fn policy_marks_of_revived_nodes_do_not_route() {
        let p = pb(REPORT);
        let mut old = report_failed_pass();
        old.route_suppressed.insert("c".into());
        let new = revived(&p, &old, &names(&["c"]));
        assert_eq!(new.current_status("c"), NodeStatus::Pending);
        // `c` counts as live: the join waits for its branch.
        assert_eq!(
            join_readiness(&p, "j", &new, &names(&["c"])),
            JoinReadiness::NotReady
        );
    }

    #[test]
    fn pending_inputs_is_empty_for_a_node_that_does_not_join() {
        let p = pb(REPORT);
        let s = revived(&p, &report_failed_pass(), &names(&["c"]));
        assert!(pending_inputs(&p, "c2", &s, &names(&["c"])).is_empty());
    }
}
