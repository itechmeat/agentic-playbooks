//! Fork and join failure options (issue #195): `fork` on a forking node
//! (V77, V78) and `require` on a join edge (V79).

use super::*;
use crate::fork::{fork_heads, fork_region};
use crate::graphutil::{adjacency, is_acyclic_fan_in};
use crate::schema::{BranchFailurePolicy, JoinMode};

pub(crate) fn check_forks(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        let Some(spec) = &node.fork else { continue };
        let id = node.id.as_str();
        if fork_heads(playbook, id).len() < 2 {
            r.error(
                "V77",
                Some(id),
                format!(
                    "node `{id}` declares `fork` but does not fork: it needs two or more \
                     unconditional outgoing edges"
                ),
            );
            continue;
        }
        let policy = spec.on_branch_failure;
        match (&spec.on_failure, policy) {
            (None, BranchFailurePolicy::FailFast) => r.error(
                "V78",
                Some(id),
                format!(
                    "fork `{id}` uses `on_branch_failure: fail_fast` without `on_failure`: \
                     name the node the run goes to when a branch fails"
                ),
            ),
            (Some(target), BranchFailurePolicy::Wait | BranchFailurePolicy::CancelSiblings) => r
                .error(
                    "V78",
                    Some(id),
                    format!(
                        "fork `{id}` names `on_failure: {target}`, which only `on_branch_failure: \
                         fail_fast` uses; with `{}` the failed node follows its own routing",
                        policy.as_str()
                    ),
                ),
            (Some(target), BranchFailurePolicy::FailFast) => {
                check_failure_target(playbook, id, target, r)
            }
            (None, _) => {}
        }
    }
}

fn check_failure_target(playbook: &Playbook, fork: &str, target: &str, r: &mut ValidationReport) {
    if playbook.node(target).is_none() {
        r.error(
            "V78",
            Some(fork),
            format!("fork `{fork}` routes branch failures to unknown node `{target}`"),
        );
        return;
    }
    if target == fork {
        r.error(
            "V78",
            Some(fork),
            format!("fork `{fork}` routes branch failures back to itself"),
        );
        return;
    }
    if playbook
        .node(target)
        .is_some_and(|n| matches!(n.kind, crate::schema::NodeKind::Start))
    {
        r.error(
            "V78",
            Some(fork),
            format!("fork `{fork}` routes branch failures to the start node `{target}`"),
        );
        return;
    }
    let Some(region) = fork_region(playbook, fork) else {
        return;
    };
    if region.contains(target) {
        r.error(
            "V78",
            Some(fork),
            format!(
                "fork `{fork}` routes branch failures to `{target}`, a node inside its own \
                 branches, which a failure cancels"
            ),
        );
    } else if region.joins.contains(target) {
        r.error(
            "V78",
            Some(fork),
            format!(
                "fork `{fork}` routes branch failures to `{target}`, one of its own joins, \
                 which `fail_fast` never runs"
            ),
        );
    }
    // `fail_fast` ends the fork at the first failure, while a `join: any`
    // waits for the first success: the two contradict each other.
    for join in &region.joins {
        let any = playbook.edges.iter().any(|e| {
            &e.to == join && e.join.as_deref().and_then(JoinMode::parse) == Some(JoinMode::Any)
        });
        if any {
            r.error(
                "V78",
                Some(fork),
                format!(
                    "fork `{fork}` uses `fail_fast`, but its join `{join}` is a `join: any`, \
                     which proceeds on the first success; use `cancel_siblings` or `wait`"
                ),
            );
        }
    }
}

pub(crate) fn check_join_requires(playbook: &Playbook, r: &mut ValidationReport) {
    let adj = adjacency(playbook);
    for e in playbook.edges.iter().filter(|e| e.require.is_some()) {
        let incoming: Vec<&crate::schema::Edge> =
            playbook.edges.iter().filter(|x| x.to == e.to).collect();
        let mode = incoming
            .iter()
            .find_map(|x| x.join.as_deref())
            .and_then(JoinMode::parse);
        let sources: Vec<&str> = incoming.iter().map(|x| x.from.as_str()).collect();
        let synchronizes = incoming.len() >= 2
            && (mode.is_some() || is_acyclic_fan_in(&adj, e.to.as_str(), &sources));
        if !synchronizes {
            r.error(
                "V79",
                Some(&e.to),
                format!(
                    "edge `{}` -> `{}` has `require: all_succeeded`, but `{}` is not a join: \
                     it needs two or more incoming edges outside its own cycle, or a `join`",
                    e.from, e.to, e.to
                ),
            );
        } else if mode == Some(JoinMode::Any) {
            r.error(
                "V79",
                Some(&e.to),
                format!(
                    "edge `{}` -> `{}` has `require: all_succeeded` on a `join: any`: the first \
                     arrival cannot also wait for every branch to succeed",
                    e.from, e.to
                ),
            );
        }
    }
}
