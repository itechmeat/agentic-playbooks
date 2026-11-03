//! Rules for approvals and person-only gates: V155 to V158.
//!
//! A gate's `approves:` binds a request to exact result snapshots, a node's
//! `requires_approval:` needs that gate's approval before it starts, and
//! `person_only` lets only a person's channel decide a gate. The rules make
//! sure each declaration means what it says on this definition.

use super::results::kind_takes_results;
use super::*;
use crate::schema::{Approves, Node, RequiresApproval, effective_review_options};

/// V155 (error): `person_only` on a node other than `human_review` or
/// `publish`. V156 (error): `person_only` with `auto_decide` or with
/// `unattended: { decide }`. V157 (error): `approves` naming an unknown
/// node, a node that does not reach the gate, or an option the gate does
/// not offer. V158 (error): `requires_approval` naming an unknown gate, a
/// node that is no `human_review` gate, a gate without `approves`, a gate that does not reach the node, an `of`
/// that is not a subset of the gate's `approves.results`, or sitting on a
/// node that does no work.
pub(crate) fn check_approvals(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        check_person_only(n, r);
        if let Some(approves) = &n.approves {
            check_approves(playbook, n, approves, r);
        }
        if let Some(required) = &n.requires_approval {
            check_requires(playbook, n, required, r);
        }
    }
}

/// Whether node `from` leads to node `to` along the playbook's edges.
fn leads_to(playbook: &Playbook, from: &str, to: &str) -> bool {
    downstream_nodes(playbook, from).contains(to)
}

/// V155 and V156.
fn check_person_only(n: &Node, r: &mut ValidationReport) {
    if !n.person_only {
        return;
    }
    let id = Some(n.id.as_str());
    if !matches!(
        n.kind,
        NodeKind::HumanReview { .. } | NodeKind::Publish { .. }
    ) {
        r.error(
            "V155",
            id,
            format!(
                "`person_only` applies to human_review and publish nodes; node `{}` is a {} node, which no person decides",
                n.id,
                n.kind.type_str()
            ),
        );
        return;
    }
    let automatic = if n.auto_decide.is_some() {
        Some("`auto_decide`")
    } else if n.unattended.as_ref().is_some_and(|u| u.decide().is_some()) {
        Some("`unattended: { decide }`")
    } else {
        None
    };
    if let Some(what) = automatic {
        r.error(
            "V156",
            id,
            format!(
                "node `{}` is `person_only`, so only a person decides it; {what} would decide it without one",
                n.id
            ),
        );
    }
}

/// V157: the gate's `approves`.
fn check_approves(playbook: &Playbook, n: &Node, approves: &Approves, r: &mut ValidationReport) {
    let id = Some(n.id.as_str());
    let NodeKind::HumanReview { options, .. } = &n.kind else {
        r.error(
            "V157",
            id,
            format!(
                "`approves` applies to human_review nodes; node `{}` is a {} node, which asks nobody",
                n.id,
                n.kind.type_str()
            ),
        );
        return;
    };
    if approves.results.is_empty() {
        r.error(
            "V157",
            id,
            format!("gate `{}`: `approves.results` names no node", n.id),
        );
    }
    for target in &approves.results {
        let problem = match playbook.node(target) {
            None => Some("is not a node of this playbook".to_string()),
            Some(t) if !kind_takes_results(&t.kind) => Some(format!(
                "is a {} node, which settles no result to show",
                t.kind.type_str()
            )),
            Some(_) if !leads_to(playbook, target, &n.id) => {
                Some("does not lead to the gate, so its result is not there to show".to_string())
            }
            Some(_) => None,
        };
        if let Some(why) = problem {
            r.error(
                "V157",
                id,
                format!(
                    "gate `{}`: `approves.results` names `{target}`, which {why}",
                    n.id
                ),
            );
        }
    }
    let offered = effective_review_options(options);
    let approving = approves.approving_options(options);
    if approving.is_empty() {
        r.error(
            "V157",
            id,
            format!("gate `{}`: `approves.options` names no option", n.id),
        );
    }
    for o in approving.iter().filter(|o| !offered.contains(o)) {
        r.error(
            "V157",
            id,
            format!(
                "gate `{}`: `approves.options` names `{o}`, which the gate does not offer ({})",
                n.id,
                offered.join(", ")
            ),
        );
    }
}

/// V158: the node's `requires_approval`.
fn check_requires(
    playbook: &Playbook,
    n: &Node,
    required: &RequiresApproval,
    r: &mut ValidationReport,
) {
    let id = Some(n.id.as_str());
    let mut fail = |why: String| {
        r.error(
            "V158",
            id,
            format!("node `{}`: `requires_approval` {why}", n.id),
        )
    };
    if !(crate::effects::node_acts(n) || matches!(n.kind, NodeKind::Publish { .. })) {
        fail(format!(
            "sits on a {} node, which does no work an approval could hold back",
            n.kind.type_str()
        ));
    }
    let gate_id = &required.gate;
    let Some(gate) = playbook.node(gate_id) else {
        fail(format!(
            "names gate `{gate_id}`, which is not a node of this playbook"
        ));
        return;
    };
    if !matches!(gate.kind, NodeKind::HumanReview { .. }) {
        fail(format!(
            "names `{gate_id}`, which is a {} node, not a human_review gate",
            gate.kind.type_str()
        ));
        return;
    }
    let Some(approves) = &gate.approves else {
        fail(format!(
            "names `{gate_id}`, which declares no `approves`, so it approves no result"
        ));
        return;
    };
    if !leads_to(playbook, gate_id, &n.id) {
        fail(format!(
            "names gate `{gate_id}`, which does not lead to this node, so its approval is never given before it"
        ));
    }
    if let Some(of) = &required.of {
        if of.is_empty() {
            fail("names no node in `of`".to_string());
        }
        for o in of.iter().filter(|o| !approves.results.contains(o)) {
            fail(format!(
                "names `{o}` in `of`, which gate `{gate_id}` does not approve ({})",
                approves.results.join(", ")
            ));
        }
    }
}
