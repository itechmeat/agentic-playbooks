//! Readiness: whether a playbook is fit to run alone. The deterministic
//! check a node needs, the boundaries an acting node has per effect
//! domain, and the irreversible steps a run reaches without a gate; the
//! pure half, shared by the validator, the fit report and the start check.

use std::collections::BTreeSet;

use crate::effects;
use crate::schema::{Effect, Node, NodeKind, Playbook};

pub use crate::unattended_policy::{MAX_PROBATION, UnattendedPolicy};

/// Whether `node` has a check apb runs itself and whose failure fails
/// the node or the run: a `success_check` of the script forms, a `script`
/// list item check under `on_needs_look: fail`, or a `script` goal
/// criterion under `goal.enforce: true` (which checks the whole run).
pub fn has_deterministic_check(playbook: &Playbook, node: &Node) -> bool {
    // A script success check rejects the attempt; the node fails when its
    // retries run out. A marker reads the agent's own text: not proof.
    if matches!(
        node.success_check,
        Some(crate::schema::SuccessCheck::Script(_))
            | Some(crate::schema::SuccessCheck::Scoped { .. })
    ) {
        return true;
    }
    // A script item check under `on_needs_look: fail`: an item the script
    // rejects makes the node fail. Under the default the node succeeds
    // with items that need a look, so the check proves nothing.
    if let Some(list) = node.list()
        && list.on_needs_look == crate::list::OnNeedsLook::Fail
        && list
            .check
            .as_ref()
            .and_then(|c| c.source())
            .is_some_and(|s| matches!(s, crate::list::CheckSource::Script(_)))
    {
        return true;
    }
    // An enforced `script` goal criterion checks the whole run, so it
    // counts for every node.
    if let Some(goal) = &playbook.goal
        && goal.enforce
        && goal
            .criteria
            .iter()
            .any(|c| matches!(c.check, crate::schema::GoalCheck::Script { .. }))
    {
        return true;
    }
    false
}

/// The acting `agent_task` nodes (`effects::node_acts`) without one, in
/// definition order.
pub fn unchecked_actors(playbook: &Playbook) -> Vec<&str> {
    playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::AgentTask { .. }))
        .filter(|n| effects::node_acts(n) && !has_deterministic_check(playbook, n))
        .map(|n| n.id.as_str())
        .collect()
}

/// An acting node and the effect domain it has no boundary in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unbounded<'a> {
    pub node: &'a str,
    pub domain: BoundaryDomain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryDomain {
    /// The working tree the node runs in.
    Tree,
    /// One connector the node binds (its name).
    Connector(String),
    /// The network and external services outside the node's connector
    /// bindings: the `network` or `external` capability apb infers for
    /// it (`effects::node_inferred`) or that it declares
    /// (`Node::own_effects`).
    Network,
    /// A `finish` node with a prompt: it runs an agent, and the format
    /// lets it declare no local boundary.
    FinishAgent,
}

/// What this machine enforces of one node's isolation contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Enforced {
    /// The contract is allowed and runs in the sandbox.
    pub tree: bool,
    /// Also: the sandbox closes the network (`network: none` holds).
    pub network_closed: bool,
}

/// Whether the node runs an agent through a prompt, so no local boundary
/// can be declared for it: a prompted `finish`.
fn is_prompted_finish(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Finish {
            prompt: Some(_),
            ..
        }
    )
}

/// The acting nodes without a boundary, per effect domain, in definition
/// order (a node may appear once per domain). `isolation(id)` says what
/// this machine enforces of node `id`'s isolation contract; the engine
/// answers it, the rule stays pure.
pub fn unbounded_actors<'a>(
    playbook: &'a Playbook,
    isolation: &dyn Fn(&str) -> Enforced,
) -> Vec<Unbounded<'a>> {
    let mut out = Vec::new();
    for node in &playbook.nodes {
        if !effects::node_acts(node) {
            continue;
        }
        // A sub-playbook is judged by its own readiness, not here, and a
        // publish node is not an actor: its push leaves only through the
        // consent of the exact plan.
        if matches!(
            node.kind,
            NodeKind::Playbook { .. } | NodeKind::Publish { .. }
        ) {
            continue;
        }
        let enforced = isolation(&node.id);
        // A prompted finish runs an agent and cannot declare a local
        // boundary (V137 and V170 refuse read_only and writes on it, and
        // it takes no isolation): always listed.
        if is_prompted_finish(node) {
            out.push(Unbounded {
                node: &node.id,
                domain: BoundaryDomain::FinishAgent,
            });
            continue;
        }
        // The working tree: `read_only`, `writes`, or an enforced
        // isolation contract. `max_changed_files` caps a count, not what
        // is written: not a boundary.
        let tree_bounded = node.read_only || node.writes.is_some() || enforced.tree;
        if !tree_bounded {
            out.push(Unbounded {
                node: &node.id,
                domain: BoundaryDomain::Tree,
            });
        }
        // Each connector binding is bounded only by what the playbook
        // declares for it: read-only functions, or named functions with
        // `max_calls`. `read_only`, `writes` and isolation never bound
        // this domain.
        for binding in node.kind.connector_bindings() {
            let bounded = match &binding.functions {
                crate::schema::FunctionsAllow::ReadOnly => true,
                crate::schema::FunctionsAllow::List(_) => binding.max_calls.is_some(),
                crate::schema::FunctionsAllow::All => false,
            };
            if !bounded {
                out.push(Unbounded {
                    node: &node.id,
                    domain: BoundaryDomain::Connector(binding.name.clone()),
                });
            }
        }
        // The network: an actor that can reach it is bounded only by an
        // enforced isolation whose sandbox closes the network, whatever
        // its read_only, writes or connector bindings. A script takes no
        // isolation, so every acting script is unbounded here.
        let reaches = effects::node_inferred(node)
            .iter()
            .any(|e| matches!(e, Effect::Network | Effect::External))
            || node
                .own_effects()
                .iter()
                .any(|e| matches!(e, Effect::Network | Effect::External));
        if reaches && !enforced.network_closed {
            out.push(Unbounded {
                node: &node.id,
                domain: BoundaryDomain::Network,
            });
        }
    }
    out
}

/// Whether the node's execution has a failure status a failure route can
/// be an entry path from: every kind but `start` can fail (a step, a
/// judge, a wait that times out, a gate that times out, a publish whose
/// push fails, a finish whose agent fails, a condition that cannot be
/// read).
fn can_fail(node: &Node) -> bool {
    !matches!(node.kind, NodeKind::Start)
}

/// The nodes `defaults.on_failure` and a `fail_fast` fork's `on_failure`
/// route a failure of `u` to, in this order. Conservative: a node whose
/// failure an edge of its own handles still gets the `defaults.on_failure`
/// transition, which can only list more, never less.
fn failure_targets<'a>(playbook: &'a Playbook, u: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    if let Some(t) = playbook
        .defaults
        .on_failure
        .target_for(u)
        .filter(|t| playbook.node(t).is_some())
    {
        out.push(t);
    }
    for node in &playbook.nodes {
        let Some(fork) = &node.fork else {
            continue;
        };
        if fork.on_branch_failure != crate::schema::BranchFailurePolicy::FailFast {
            continue;
        }
        let Some(t) = fork.on_failure.as_deref() else {
            continue;
        };
        let Some(region) = crate::fork::fork_region(playbook, &node.id) else {
            continue;
        };
        if region.branches.contains(u) && playbook.node(t).is_some() {
            out.push(t);
        }
    }
    out
}

/// Nodes whose own effects include `irreversible` and that a run can
/// reach without passing a `human_review` or `publish` node, along edges
/// and along the failure transitions of the nodes it reaches. Each state
/// `(node, gated)` is visited once; a node is listed once, in definition
/// order. `graphutil` is not changed: V07 and the run graph keep
/// `reachable_with_policies` as it is.
pub fn ungated_irreversible(playbook: &Playbook) -> Vec<&str> {
    use std::collections::VecDeque;
    // A playbook without exactly one start node lists nothing (V03
    // refuses it).
    let starts: Vec<&Node> = playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Start))
        .collect();
    if starts.len() != 1 {
        return Vec::new();
    }
    let adj = crate::graphutil::adjacency(playbook);
    let is_gate = |id: &str| {
        playbook.node(id).is_some_and(|n| match n.kind {
            // A gate a decision model may decide on its own is not a
            // gate here.
            NodeKind::HumanReview { .. } => !(n.auto_decide.is_some() && n.auto_decide_ok),
            NodeKind::Publish { .. } => true,
            _ => false,
        })
    };
    let mut seen: BTreeSet<(&str, bool)> = BTreeSet::new();
    let mut queue = VecDeque::new();
    queue.push_back((starts[0].id.as_str(), false));
    seen.insert((starts[0].id.as_str(), false));
    let mut ungated: BTreeSet<&str> = BTreeSet::new();
    while let Some((u, gated)) = queue.pop_front() {
        let node = playbook.node(u);
        if let Some(node) = node
            && node.own_effects().contains(&Effect::Irreversible)
            && !gated
            && !matches!(node.kind, NodeKind::Publish { .. })
        {
            ungated.insert(u);
        }
        // Leaving a gate by any of its edges passes it.
        let next_gated = gated || is_gate(u);
        for v in adj.get(u).map(|vs| vs.as_slice()).unwrap_or(&[]) {
            if seen.insert((v, next_gated)) {
                queue.push_back((v, next_gated));
            }
        }
        // A failure route is an entry path only from a node that can fail
        // and that a run reaches; it carries the `gated` it was reached
        // with (a node that fails has not passed itself).
        if node.is_some_and(can_fail) {
            for t in failure_targets(playbook, u) {
                if seen.insert((t, gated)) {
                    queue.push_back((t, gated));
                }
            }
        }
    }
    // In definition order.
    playbook
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| ungated.contains(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Playbook;

    const START: &str = "  - { id: start, type: start }\n";
    const FINISH: &str = "  - { id: done, type: finish, outcome: success }\n";

    fn playbook(extra: &str, nodes: &str, edges: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\n{extra}nodes:\n{nodes}edges:\n{edges}"
        ))
        .unwrap()
    }

    fn edge(from: &str, to: &str) -> String {
        format!("  - {{ from: {from}, to: {to} }}\n")
    }

    /// A script success check, and an enforced script goal criterion: both
    /// count. A marker success check does not.
    #[test]
    fn a_script_check_and_an_enforced_script_goal_count() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: w, type: script, script: s.sh, runner: sh, success_check: {{ script: check.sh }} }}\n{FINISH}"
            ),
            &(edge("start", "w") + &edge("w", "done")),
        );
        let w = pb.node("w").unwrap();
        assert!(has_deterministic_check(&pb, w));
        let enforced = playbook(
            "goal:\n  statement: done\n  enforce: true\n  criteria:\n    - { description: ok, check: { type: script, path: goal.sh } }\n",
            &format!("{START}  - {{ id: w, type: script, script: s.sh, runner: sh }}\n{FINISH}"),
            &(edge("start", "w") + &edge("w", "done")),
        );
        let w = enforced.node("w").unwrap();
        assert!(
            has_deterministic_check(&enforced, w),
            "an enforced script goal criterion checks the whole run"
        );
    }

    /// A completion marker reads the agent's own text: not proof.
    #[test]
    fn a_marker_is_not_a_deterministic_check() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: w, type: agent_task, prompt: P, profile: x, success_check: {{ marker: done }} }}\n{FINISH}"
            ),
            &(edge("start", "w") + &edge("w", "done")),
        );
        assert!(!has_deterministic_check(&pb, pb.node("w").unwrap()));
    }

    /// An output item check compares the agent's own output: not proof.
    #[test]
    fn an_output_item_check_is_not_proof() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - id: w\n    type: agent_task\n    prompt: P\n    profile: x\n    list:\n      items: {{ inline: [a] }}\n      check: {{ output: {{ field: ok, equals: \"1\" }} }}\n{FINISH}"
            ),
            &(edge("start", "w") + &edge("w", "done")),
        );
        assert!(!has_deterministic_check(&pb, pb.node("w").unwrap()));
    }

    /// A judge item check is a model's verdict: not proof.
    #[test]
    fn a_judge_item_check_does_not_count() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - id: w\n    type: agent_task\n    prompt: P\n    profile: x\n    list:\n      items: {{ inline: [a] }}\n      check:\n        judge:\n          state: {{ subject: \"{{{{item}}}}\" }}\n          instructions: fine?\n          criteria: {{ accept: fine, reject: not }}\n          min_confidence: 0.6\n          on_unavailable: reject\n{FINISH}"
            ),
            &(edge("start", "w") + &edge("w", "done")),
        );
        assert!(!has_deterministic_check(&pb, pb.node("w").unwrap()));
    }

    /// A script item check counts only under `on_needs_look: fail`: under
    /// the default the node succeeds with items that need a look.
    #[test]
    fn a_script_item_check_counts_only_when_needs_look_fails_the_node() {
        let list = |on: &str| {
            playbook(
                "",
                &format!(
                    "{START}  - id: w\n    type: agent_task\n    prompt: P\n    profile: x\n    list:\n      items: {{ inline: [a] }}\n      check: {{ script: c.sh }}\n      on_needs_look: {on}\n{FINISH}"
                ),
                &(edge("start", "w") + &edge("w", "done")),
            )
        };
        let failing = list("fail");
        assert!(has_deterministic_check(
            &failing,
            failing.node("w").unwrap()
        ));
        let continuing = list("continue");
        assert!(!has_deterministic_check(
            &continuing,
            continuing.node("w").unwrap()
        ));
    }

    /// A goal criterion counts only when enforced: a `script` criterion
    /// without `enforce` and a `list` criterion with it count nothing.
    #[test]
    fn a_goal_counts_only_when_enforced() {
        let script_not_enforced = playbook(
            "goal:\n  statement: done\n  criteria:\n    - { description: ok, check: { type: script, path: goal.sh } }\n",
            &format!("{START}  - {{ id: w, type: script, script: s.sh, runner: sh }}\n{FINISH}"),
            &(edge("start", "w") + &edge("w", "done")),
        );
        assert!(!has_deterministic_check(
            &script_not_enforced,
            script_not_enforced.node("w").unwrap()
        ));
        let list_enforced = playbook(
            "goal:\n  statement: done\n  enforce: true\n  criteria:\n    - { description: ok, check: { type: list, node: w } }\n",
            &format!("{START}  - {{ id: w, type: script, script: s.sh, runner: sh }}\n{FINISH}"),
            &(edge("start", "w") + &edge("w", "done")),
        );
        assert!(!has_deterministic_check(
            &list_enforced,
            list_enforced.node("w").unwrap()
        ));
    }

    /// `unchecked_actors` names the acting agent tasks without a check, in
    /// definition order.
    #[test]
    fn unchecked_actors_names_the_acting_agents_without_a_check() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: a, type: script, script: s.sh, runner: sh, success_check: {{ script: c.sh }} }}\n  - {{ id: b, type: agent_task, prompt: P, profile: x }}\n  - {{ id: c, type: agent_task, prompt: P, profile: x, success_check: {{ script: c.sh }} }}\n{FINISH}"
            ),
            &(edge("start", "a") + &edge("a", "b") + &edge("b", "c") + &edge("c", "done")),
        );
        assert_eq!(unchecked_actors(&pb), vec!["b"]);
    }

    /// A read-only agent with an open connector is unbounded in that
    /// connector only: the tree is bounded.
    #[test]
    fn a_read_only_node_with_an_open_connector_is_unbounded() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: w, type: agent_task, prompt: P, profile: x, read_only: true, connectors: [ {{ name: crm }} ] }}\n{FINISH}"
            ),
            &(edge("start", "w") + &edge("w", "done")),
        );
        let got = unbounded_actors(&pb, &|_| Enforced::default());
        assert!(
            got.contains(&Unbounded {
                node: "w",
                domain: BoundaryDomain::Connector("crm".into()),
            }),
            "{got:?}"
        );
        assert!(
            !got.iter().any(|u| u.domain == BoundaryDomain::Tree),
            "read_only bounds the tree: {got:?}"
        );
    }

    /// A connector binding bounded by read-only functions, or by named
    /// functions with `max_calls`: bounded. `functions: all`, or names
    /// without `max_calls`: unbounded.
    #[test]
    fn a_bounded_connector_names_functions_and_max_calls() {
        let bound = |decl: &str| {
            playbook(
                "",
                &format!(
                    "{START}  - {{ id: w, type: agent_task, prompt: P, profile: x, connectors: [{decl}] }}\n{FINISH}"
                ),
                &(edge("start", "w") + &edge("w", "done")),
            )
        };
        let no_connector = |got: &[Unbounded<'_>]| {
            !got.iter()
                .any(|u| matches!(u.domain, BoundaryDomain::Connector(_)))
        };
        let pb = bound(" { name: crm, functions: read_only } ");
        assert!(no_connector(&unbounded_actors(
            &pb,
            &|_| Enforced::default()
        )));
        let pb = bound(" { name: crm, functions: [get, put], max_calls: 3 } ");
        assert!(no_connector(&unbounded_actors(
            &pb,
            &|_| Enforced::default()
        )));
        let pb = bound(" { name: crm } ");
        assert_eq!(
            unbounded_actors(&pb, &|_| Enforced::default())
                .into_iter()
                .find(|u| matches!(u.domain, BoundaryDomain::Connector(_)))
                .map(|u| u.domain),
            Some(BoundaryDomain::Connector("crm".into()))
        );
        let pb = bound(" { name: crm, functions: [get, put] } ");
        assert_eq!(
            unbounded_actors(&pb, &|_| Enforced::default())
                .into_iter()
                .find(|u| matches!(u.domain, BoundaryDomain::Connector(_)))
                .map(|u| u.domain),
            Some(BoundaryDomain::Connector("crm".into()))
        );
    }

    /// A declared `external` effect is judged in the network domain.
    #[test]
    fn a_declared_external_effect_is_judged_in_the_network_domain() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: w, type: agent_task, prompt: P, profile: x, effects: [external] }}\n{FINISH}"
            ),
            &(edge("start", "w") + &edge("w", "done")),
        );
        let got = unbounded_actors(&pb, &|_| Enforced {
            tree: true,
            ..Enforced::default()
        });
        assert_eq!(
            got,
            vec![Unbounded {
                node: "w",
                domain: BoundaryDomain::Network,
            }],
            "{got:?}"
        );
    }

    /// A prompted finish runs an agent and cannot declare a local
    /// boundary: always listed with the domain FinishAgent.
    #[test]
    fn a_prompted_finish_is_an_actor_without_a_local_boundary() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: end, type: finish, outcome: success, prompt: summarize }}\n  - {{ id: no, type: finish, outcome: failure }}\n"
            ),
            &(edge("start", "end") + &edge("end", "no")),
        );
        let got = unbounded_actors(&pb, &|_| Enforced {
            tree: true,
            network_closed: true,
        });
        assert_eq!(
            got,
            vec![Unbounded {
                node: "end",
                domain: BoundaryDomain::FinishAgent,
            }],
            "{got:?}"
        );
    }

    /// The tree and network domains against a stand-in `isolation`
    /// answering per node id.
    #[test]
    fn unbounded_actors_lack_a_tree_boundary() {
        let pb = playbook(
            "",
            &format!("{START}  - {{ id: w, type: agent_task, prompt: P, profile: x }}\n{FINISH}"),
            &(edge("start", "w") + &edge("w", "done")),
        );
        let got = unbounded_actors(&pb, &|id| {
            if id == "w" {
                Enforced {
                    tree: true,
                    network_closed: false,
                }
            } else {
                Enforced::default()
            }
        });
        assert_eq!(
            got,
            vec![Unbounded {
                node: "w",
                domain: BoundaryDomain::Network,
            }],
            "{got:?}"
        );
        let got = unbounded_actors(&pb, &|id| {
            if id == "w" {
                Enforced {
                    tree: true,
                    network_closed: true,
                }
            } else {
                Enforced::default()
            }
        });
        assert!(got.is_empty(), "{got:?}");
    }

    const WORK: &str = "  - { id: work, type: script, script: s.sh, runner: sh }\n";
    const GATE: &str = "  - { id: gate, type: human_review, options: [approve, reject] }\n";
    const DESTRUCTIVE: &str = "  - { id: destructive, type: script, script: s.sh, runner: sh, effects: [irreversible] }\n  - { id: no, type: finish, outcome: failure }\n";

    /// `start -> work -> gate -> done`, with `defaults.on_failure` naming
    /// the irreversible node: `work` can fail before the gate, so the
    /// target is listed.
    #[test]
    fn a_failure_before_the_gate_is_ungated() {
        let pb = playbook(
            "defaults: { on_failure: destructive }\n",
            &format!("{START}{WORK}{GATE}{DESTRUCTIVE}{FINISH}"),
            &(edge("start", "work")
                + &edge("work", "gate")
                + &edge("gate", "done")
                + &edge("destructive", "no")),
        );
        assert_eq!(ungated_irreversible(&pb), vec!["destructive"]);
    }

    /// `start -> gate -> work`: every failure of `work` reaches the
    /// handler after the review. The gate itself can time out before it
    /// is passed (every kind but `start` can fail), so its own failure
    /// route lists the handler; without a timeout route through the gate
    /// (a fork's handler, scoped to the branches behind it) nothing is.
    #[test]
    fn a_failure_after_the_gate_is_gated() {
        let pb = playbook(
            "",
            &format!(
                "{START}{GATE}  - {{ id: fork, type: agent_task, prompt: P, profile: x, fork: {{ on_branch_failure: fail_fast, on_failure: destructive }} }}\n  - {{ id: b1, type: script, script: s.sh, runner: sh }}\n  - {{ id: b2, type: script, script: s.sh, runner: sh }}\n{DESTRUCTIVE}{FINISH}"
            ),
            &(edge("start", "gate")
                + &edge("gate", "fork")
                + &edge("fork", "b1")
                + &edge("fork", "b2")
                + &edge("b1", "done")
                + &edge("b2", "done")
                + &edge("destructive", "no")),
        );
        assert!(ungated_irreversible(&pb).is_empty(), "after the gate");
    }

    /// A gate that can fail (a timeout) before it is passed routes to the
    /// `defaults.on_failure` handler ungated: the handler is listed.
    #[test]
    fn a_gate_that_fails_is_not_passed() {
        let pb = playbook(
            "defaults: { on_failure: destructive }\n",
            &format!("{START}{GATE}{WORK}{DESTRUCTIVE}{FINISH}"),
            &(edge("start", "gate")
                + &edge("gate", "work")
                + &edge("work", "done")
                + &edge("destructive", "no")),
        );
        assert_eq!(ungated_irreversible(&pb), vec!["destructive"]);
    }

    /// A node reachable around the gate is listed, even though another
    /// path to it passes the gate.
    #[test]
    fn a_node_reachable_around_a_gate_is_listed() {
        let pb = playbook(
            "",
            &format!("{START}{WORK}{GATE}{DESTRUCTIVE}{FINISH}"),
            &(edge("start", "work")
                + &edge("work", "gate")
                + &edge("gate", "destructive")
                + &edge("work", "destructive")
                + &edge("destructive", "done")),
        );
        assert_eq!(ungated_irreversible(&pb), vec!["destructive"]);
    }

    /// A publish node is never listed: it is a gate itself.
    #[test]
    fn a_publish_node_is_never_listed() {
        let pb = playbook(
            "",
            &format!(
                "{START}{WORK}{GATE}  - {{ id: destructive, type: publish, from: work, remote: origin }}\n  - {{ id: no, type: finish, outcome: failure }}\n{FINISH}"
            ),
            &(edge("start", "work")
                + &edge("work", "gate")
                + &edge("work", "destructive")
                + &edge("gate", "done")
                + &edge("destructive", "no")),
        );
        assert!(
            ungated_irreversible(&pb).is_empty(),
            "{:?}",
            ungated_irreversible(&pb)
        );
    }

    /// A review a decision model may decide on its own (`auto_decide`
    /// with `auto_decide_ok`) is not a gate in the walk; without
    /// `auto_decide_ok` it is.
    #[test]
    fn a_gate_a_model_may_decide_is_not_a_gate() {
        let with = |ok: &str| {
            playbook(
                "",
                &format!(
                    "{START}  - {{ id: gate, type: human_review, options: [approve, needs_changes], auto_decide: {{ allow: [needs_changes] }}{ok} }}\n  - {{ id: destructive, type: script, script: s.sh, runner: sh, effects: [irreversible] }}\n{FINISH}"
                ),
                &(edge("start", "gate")
                    + &edge("gate", "destructive")
                    + &edge("destructive", "done")),
            )
        };
        assert_eq!(
            ungated_irreversible(&with(", auto_decide_ok: true")),
            vec!["destructive"]
        );
        assert!(ungated_irreversible(&with("")).is_empty());
    }

    /// A sub-playbook node is judged by its own readiness and a publish
    /// node is not an actor: neither is listed as unbounded.
    #[test]
    fn sub_playbook_and_publish_nodes_are_not_judged_as_actors() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: child, type: playbook, playbook: kid }}\n  - {{ id: work, type: script, script: s.sh, runner: sh, read_only: true }}\n  - {{ id: ship, type: publish, from: work, remote: origin }}\n{FINISH}"
            ),
            &(edge("start", "child")
                + &edge("child", "work")
                + &edge("work", "ship")
                + &edge("ship", "done")),
        );
        let got: Vec<&str> = unbounded_actors(&pb, &|_| Enforced::default())
            .into_iter()
            .map(|u| u.node)
            .filter(|n| *n != "work")
            .collect();
        assert!(got.is_empty(), "{got:?}");
    }

    /// The fail-fast fork routes its failures from its branches: the
    /// `on_failure` target is listed when the branches start before any
    /// gate, and not when the fork sits behind one.
    #[test]
    fn a_fail_fast_fork_routes_from_its_branches() {
        let fork = |gate: &str| {
            let gate_node = if gate.is_empty() {
                String::new()
            } else {
                format!("  - {{ id: {gate}, type: human_review, options: [approve, reject] }}\n")
            };
            let nodes = format!(
                "{START}{gate_node}  - {{ id: fork, type: agent_task, prompt: P, profile: x, fork: {{ on_branch_failure: fail_fast, on_failure: destructive }} }}\n  - {{ id: b1, type: script, script: s.sh, runner: sh }}\n  - {{ id: b2, type: script, script: s.sh, runner: sh }}\n  - {{ id: destructive, type: script, script: s.sh, runner: sh, effects: [irreversible] }}\n  - {{ id: no, type: finish, outcome: failure }}\n{FINISH}"
            );
            let edges = if gate.is_empty() {
                edge("start", "fork")
                    + &edge("fork", "b1")
                    + &edge("fork", "b2")
                    + &edge("b1", "done")
                    + &edge("b2", "done")
                    + &edge("destructive", "no")
            } else {
                edge("start", "gate")
                    + &edge("gate", "fork")
                    + &edge("fork", "b1")
                    + &edge("fork", "b2")
                    + &edge("b1", "done")
                    + &edge("b2", "done")
                    + &edge("destructive", "no")
            };
            playbook("", &nodes, &edges)
        };
        let pb = fork("");
        assert_eq!(ungated_irreversible(&pb), vec!["destructive"]);
        let pb = fork("gate");
        assert!(
            ungated_irreversible(&pb).is_empty(),
            "the fork sits behind the gate"
        );
    }

    /// The network domain: a read-only script is bounded in its tree and
    /// unbounded externally; an isolated agent with `network: none`
    /// enforced is bounded there.
    #[test]
    fn a_read_only_script_is_not_bounded_externally() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: w, type: script, script: s.sh, runner: sh, read_only: true }}\n{FINISH}"
            ),
            &(edge("start", "w") + &edge("w", "done")),
        );
        let got = unbounded_actors(&pb, &|_| Enforced::default());
        assert_eq!(
            got,
            vec![Unbounded {
                node: "w",
                domain: BoundaryDomain::Network,
            }],
            "{got:?}"
        );
    }

    #[test]
    fn an_isolated_agent_without_network_is_bounded_externally() {
        let pb = playbook(
            "",
            &format!(
                "{START}  - {{ id: w, type: agent_task, prompt: P, profile: x, isolation: {{ level: full, network: none }} }}\n{FINISH}"
            ),
            &(edge("start", "w") + &edge("w", "done")),
        );
        // The stand-in answers `network_closed` only for w.
        let got = unbounded_actors(&pb, &|id| {
            if id == "w" {
                Enforced {
                    tree: true,
                    network_closed: true,
                }
            } else {
                Enforced::default()
            }
        });
        assert!(got.is_empty(), "{got:?}");
        // `tree` alone leaves the node in the network domain.
        let got = unbounded_actors(&pb, &|id| {
            if id == "w" {
                Enforced {
                    tree: true,
                    ..Enforced::default()
                }
            } else {
                Enforced::default()
            }
        });
        assert_eq!(got[0].domain, BoundaryDomain::Network);
    }
}
