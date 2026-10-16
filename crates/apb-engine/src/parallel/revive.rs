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
//! The revived view removes both: those nodes read as not yet run.

use std::collections::{BTreeSet, VecDeque};

use apb_core::schema::Playbook;

use crate::state::RunState;

/// The nodes a re-execution of `start` runs again: `start` itself and every
/// node only downstream of it.
///
/// "Only downstream" excludes the ancestors of `start` (the nodes that can
/// reach it). In a cycle the forward closure of `start` contains the fan-out
/// points upstream of it; reviving those would drop the hops that keep a
/// never-started sibling a pending head, so inside a loop the cycle keeps its
/// old routing, which errs toward waiting.
pub fn revived_nodes(playbook: &Playbook, start: &str) -> BTreeSet<String> {
    let ancestors = reaching(playbook, start);
    let mut out: BTreeSet<String> = apb_core::graphutil::reachable(playbook, &[start])
        .into_iter()
        .filter(|n| !ancestors.contains(n))
        .collect();
    out.insert(start.to_string());
    out
}

/// Every node that has a path to `target`, `target` excluded.
fn reaching(playbook: &Playbook, target: &str) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([target]);
    while let Some(id) = queue.pop_front() {
        for e in playbook.edges.iter().filter(|e| e.to == id) {
            if e.from != target && seen.insert(e.from.clone()) {
                queue.push_back(e.from.as_str());
            }
        }
    }
    seen
}

/// `state` as the drive weighs joins and rebuilds its frontier when it is
/// about to execute every node of `starts` again: each revived node (see
/// [`revived_nodes`]) reads as not yet run, its journaled hops are dropped and
/// its fork or join policy marks are cleared, because this pass re-executes it.
///
/// Outputs, edge counts, finished counts and judge answers stay: edge
/// selection consults them only for finished nodes, which revived ones are
/// not, and loop budgets must keep counting.
pub fn revived(playbook: &Playbook, state: &RunState, starts: &[String]) -> RunState {
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    for start in starts {
        nodes.extend(revived_nodes(playbook, start));
    }
    let mut s = state.clone();
    for n in &nodes {
        s.nodes.remove(n);
        s.route_suppressed.remove(n);
        s.refused_joins.remove(n);
    }
    s.journaled_hops.retain(|(from, _)| !nodes.contains(from));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::{
        JoinReadiness, dead_inputs, join_readiness, pending_heads, pending_inputs,
    };
    use crate::state::NodeStatus;

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
            revived_nodes(&p, "c").into_iter().collect::<Vec<_>>(),
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
        // The old failure route is not re-offered, the join still is.
        assert_eq!(pending_heads(&p, &new), names(&["c", "j"]));
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
        assert!(!pending_heads(&p, &new).contains(&"done".to_string()));
        assert!(!new.nodes.contains_key("j"));
        // Upstream of the rerun node nothing changes.
        assert_eq!(new.nodes.get("b"), Some(&NodeStatus::Succeeded));
        assert!(new.journaled_hops.contains(&("b".into(), "j".into())));
    }

    #[test]
    fn a_loop_keeps_the_routing_of_its_upstream_fan_out() {
        let p = pb(LOOP_FORK);
        // Inside the cycle `a` is an ancestor of `x`, so it is not revived.
        assert_eq!(
            revived_nodes(&p, "x").into_iter().collect::<Vec<_>>(),
            names(&["x", "y"])
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
        assert_eq!(pending_heads(&p, &new), names(&["x", "y"]));
        assert_eq!(new.nodes.get("a"), Some(&NodeStatus::Succeeded));
    }

    #[test]
    fn policy_marks_of_revived_nodes_are_cleared() {
        let p = pb(REPORT);
        let mut old = report_failed_pass();
        old.route_suppressed.insert("c".into());
        old.refused_joins.insert("j".into());
        old.route_suppressed.insert("b".into());
        let new = revived(&p, &old, &names(&["c"]));
        assert!(!new.route_suppressed.contains("c"));
        assert!(!new.refused_joins.contains("j"));
        assert!(new.route_suppressed.contains("b"));
    }

    #[test]
    fn pending_inputs_is_empty_for_a_node_that_does_not_join() {
        let p = pb(REPORT);
        let s = revived(&p, &report_failed_pass(), &names(&["c"]));
        assert!(pending_inputs(&p, "c2", &s, &names(&["c"])).is_empty());
    }
}
