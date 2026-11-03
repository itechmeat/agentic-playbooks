//! Rules for a `human_review` gate's `unattended` outcome: V141 to V143.
//!
//! An unattended run decides a gate only with an option the author declared
//! (`unattended: { decide: <option> }`). That decision removes a person from
//! the route the option takes, so it is refused where that route can reach a
//! step V73 counts as shipping (declared or implied `irreversible` or
//! `secrets` effects, a granted irreversible connector function, a merge,
//! push, deploy or publish step by name) before a person is asked again, at
//! the next `human_review` or `publish`. The walk starts at the chosen
//! option's edges only, unlike V73's walk over every edge after the gate,
//! and then follows every route the scheduler can take: edges and the
//! failure policies' handlers ([`unattended_reach`]).
//! The run rechecks the same rule with the pinned sub-playbooks' effects
//! ([`unattended_decide_refusal`]).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::decisions::{shipping_reason, unknown_connector_effects_in};
use super::review::review_takes;
use super::*;
use crate::connector::resolve::ConnectorFacts;
use crate::schema::{BranchFailurePolicy, Edge, Effect, Node, effective_review_options};

/// Whether the walk stops at `n`: a person is asked again there. A publish
/// step counts: its push needs a person's consent, which an unattended run
/// never gives, so the run fails there rather than ship.
fn asks_a_person(n: &Node) -> bool {
    matches!(
        n.kind,
        NodeKind::HumanReview { .. } | NodeKind::Publish { .. }
    )
}

/// The edges out of gate `gate` that decision `option` can take. A fallback
/// edge counts unless an unbounded edge always takes the option (no
/// condition, or a `review_status` naming it): a bounded edge may be spent
/// and a condition of another kind may not match.
fn option_edges<'a>(playbook: &'a Playbook, gate: &str, option: &str) -> Vec<&'a Edge> {
    let out: Vec<&Edge> = playbook.edges.iter().filter(|e| e.from == gate).collect();
    let always = out.iter().any(|e| {
        !e.fallback
            && e.max_traversals.is_none()
            && match &e.condition {
                None => true,
                Some(EdgeCondition::ReviewStatus { equals }) => equals == option,
                Some(_) => false,
            }
    });
    out.into_iter()
        .filter(|e| match e.fallback {
            true => !always,
            false => review_takes(e, option),
        })
        .collect()
}

/// The nodes decision `option` of gate `gate` can reach before a person is
/// asked again: the targets of [`option_edges`] and every node after them,
/// stopping at (and leaving out) a `human_review` or a `publish` node.
/// "After" follows every way the scheduler leaves a node: its edges (judge
/// edges included), the `defaults.on_failure` handler (any node in reach
/// can fail; the handler's own failure does not route to it again), and the
/// `fork.on_failure` target of every `fail_fast` fork whose branches hold a
/// node in reach. A fork's branches are the ones the engine cancels and
/// routes on a branch failure ([`crate::fork::fork_region`], the same
/// region `enclosing_forks` hands the drive), so a node after the fork's
/// join is not one: its failure never takes the fork's `on_failure`. One
/// walk serves the validator (V142) and the run's recheck.
pub fn unattended_reach(playbook: &Playbook, gate: &str, option: &str) -> BTreeSet<String> {
    // Each `fail_fast` fork with an `on_failure` target, with its branches:
    // a failure of any of them routes there (directly, or by escalating
    // from a fork nested inside, which these branches hold too).
    let forks: Vec<(&str, crate::fork::ForkRegion)> = playbook
        .nodes
        .iter()
        .filter_map(|n| {
            let spec = n.fork.as_ref()?;
            if spec.on_branch_failure != BranchFailurePolicy::FailFast {
                return None;
            }
            let target = spec.on_failure.as_deref()?;
            Some((target, crate::fork::fork_region(playbook, &n.id)?))
        })
        .collect();
    let mut seen = BTreeSet::new();
    let mut queue: VecDeque<&str> = option_edges(playbook, gate, option)
        .into_iter()
        .map(|e| e.to.as_str())
        .collect();
    while let Some(cur) = queue.pop_front() {
        let Some(n) = playbook.node(cur) else {
            continue;
        };
        if asks_a_person(n) || !seen.insert(cur.to_string()) {
            continue;
        }
        queue.extend(
            playbook
                .edges
                .iter()
                .filter(|e| e.from == cur)
                .map(|e| e.to.as_str()),
        );
        queue.extend(playbook.defaults.on_failure.target_for(cur));
        queue.extend(
            forks
                .iter()
                .filter(|(_, region)| region.contains(cur))
                .map(|(target, _)| *target),
        );
    }
    seen
}

/// Why an unattended run may not decide gate `gate` with `option`, or `None`
/// when it may: the playbook's own declared `irreversible` or `secrets`
/// effects, then those of `inherited` (the effects of the pinned
/// sub-playbooks in reach, which only the run knows), then a shipping step ([`shipping_reason`]) in
/// [`unattended_reach`]. With `unknown_fails` (the run), a node in reach
/// bound to a connector whose effects cannot be known (its manifest does not
/// load, or it is not installed) refuses too. `auto_decide_ok` does not
/// apply: it overrides the model's decision, not this one.
pub fn unattended_decide_refusal(
    playbook: &Playbook,
    gate: &str,
    option: &str,
    inherited: &[Effect],
    connectors: &BTreeMap<String, ConnectorFacts>,
    unknown_fails: bool,
) -> Option<String> {
    let named = |effects: &mut dyn Iterator<Item = &Effect>| -> Option<String> {
        let found: BTreeSet<&'static str> = effects
            .filter_map(|e| match e {
                Effect::Irreversible => Some("irreversible"),
                Effect::Secrets => Some("secrets"),
                _ => None,
            })
            .collect();
        (!found.is_empty()).then(|| found.into_iter().collect::<Vec<_>>().join(" and "))
    };
    if let Some(d) = named(&mut playbook.effects.iter()) {
        return Some(format!("the playbook declares {d} effects"));
    }
    if let Some(d) = named(&mut inherited.iter()) {
        return Some(format!("a sub-playbook in reach declares {d} effects"));
    }
    let reach = unattended_reach(playbook, gate, option);
    let mut reasons: Vec<String> = reach
        .iter()
        .filter_map(|id| {
            playbook
                .node(id)
                .and_then(|n| shipping_reason(n, connectors))
        })
        .collect();
    if unknown_fails {
        reasons.extend(unknown_connector_effects_in(
            playbook, reach, connectors, true,
        ));
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// V141 (error): `unattended` on a kind other than `human_review` (a
/// publish step's consent is never given unattended), or `decide` naming an
/// option the gate does not declare. V142 (error): see
/// [`unattended_decide_refusal`]. V143 (error): no unbounded edge routes the
/// option out of the gate, so the unattended decision has no way on once a
/// bounded edge is spent (V136 only warns for a person's decision).
pub(crate) fn check_unattended(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    for n in &playbook.nodes {
        let Some(unattended) = &n.unattended else {
            continue;
        };
        let id = Some(n.id.as_str());
        let options = match &n.kind {
            NodeKind::HumanReview { options, .. } => effective_review_options(options),
            NodeKind::Publish { .. } => {
                r.error(
                    "V141",
                    id,
                    "`unattended` is not allowed on a publish node: a publish step pushes only with a person's consent, so an unattended run fails there".into(),
                );
                continue;
            }
            kind => {
                r.error(
                    "V141",
                    id,
                    format!(
                        "`unattended` applies to human_review nodes only; node `{}` is a {} node",
                        n.id,
                        kind.type_str()
                    ),
                );
                continue;
            }
        };
        let Some(decide) = unattended.decide() else {
            continue;
        };
        let option = decide.decide.as_str();
        if !options.iter().any(|o| o == option) {
            r.error(
                "V141",
                id,
                format!(
                    "`unattended.decide` names `{option}`, which is not one of the gate's options ({})",
                    options.join(", ")
                ),
            );
            continue;
        }
        if let Some(why) =
            unattended_decide_refusal(playbook, &n.id, option, &[], &ctx.connectors, false)
        {
            r.error(
                "V142",
                id,
                format!(
                    "`unattended: {{ decide: {option} }}` is refused: {why}; an unattended run must not pass this gate without a person before such a step (add a human_review before it, or use `unattended: fail`)"
                ),
            );
        }
        check_exit(playbook, n, option, r);
    }
}

/// V143: the edges out of gate `n` that can carry `option` include an
/// unbounded one (a fallback, or an edge [`review_takes`] for the option).
fn check_exit(playbook: &Playbook, n: &Node, option: &str, r: &mut ValidationReport) {
    let out: Vec<&Edge> = playbook
        .edges
        .iter()
        .filter(|e| e.from == n.id && (e.fallback || review_takes(e, option)))
        .collect();
    if out.iter().any(|e| e.max_traversals.is_none()) {
        return;
    }
    let what = match out.is_empty() {
        true => format!(
            "no edge routes `{option}` out of the gate, so the run would end there; add an edge for it or a fallback edge"
        ),
        false => format!(
            "only bounded edges route `{option}` out of the gate; once their cap is spent the decision has no edge and the run fails there. Put the cap on the edge into the gate instead, or add a fallback edge"
        ),
    };
    r.error_edges(
        playbook,
        "V143",
        Some(&n.id),
        &out,
        format!("`unattended: {{ decide: {option} }}` is refused: {what}"),
    );
}

/// The run-time half of the `decide` rule: what the run rechecks that
/// `apb validate` cannot know (the pinned sub-playbooks' effects, a
/// connector whose effects are unknown on this machine). The validator's
/// own rules (V137 to V143) are proven through `apb validate` in the CLI
/// suite.
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::connector::resolve::ConnectorFacts;
    use crate::schema::{Effect, Playbook};

    /// `start -> gate`; `approve` goes to `post`, which binds connector
    /// `tracker`, then to `gate2` and `late`, which binds `crm`.
    const GATED: &str = r#"schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approve, needs_changes], unattended: { decide: approve } }
  - { id: post, type: agent_task, prompt: post it, connectors: [tracker] }
  - { id: gate2, type: human_review }
  - { id: late, type: agent_task, prompt: sync, connectors: [crm] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: post, condition: { type: review_status, equals: approve } }
  - { from: gate, to: done, fallback: true }
  - { from: post, to: gate2 }
  - { from: gate2, to: late }
  - { from: late, to: done }
"#;

    fn refusal(
        inherited: &[Effect],
        connectors: &BTreeMap<String, ConnectorFacts>,
        unknown_fails: bool,
    ) -> Option<String> {
        let p = Playbook::from_yaml(GATED).unwrap();
        super::unattended_decide_refusal(
            &p,
            "gate",
            "approve",
            inherited,
            connectors,
            unknown_fails,
        )
    }

    fn installed(names: &[&str]) -> BTreeMap<String, ConnectorFacts> {
        names
            .iter()
            .map(|n| (n.to_string(), ConnectorFacts::default()))
            .collect()
    }

    #[test]
    fn a_pinned_child_that_declares_secrets_refuses_the_decision() {
        let both = installed(&["tracker", "crm"]);
        assert_eq!(refusal(&[], &both, true), None);
        let why = refusal(&[Effect::Network, Effect::Secrets], &both, true).unwrap();
        assert_eq!(why, "a sub-playbook in reach declares secrets effects");
    }

    #[test]
    fn a_connector_with_unknown_effects_in_reach_refuses_only_at_run_time() {
        // `tracker` is in reach and not installed: the run fails closed, the
        // validator (which may run without a connector store) does not.
        let crm_only = installed(&["crm"]);
        let why = refusal(&[], &crm_only, true).unwrap();
        assert!(
            why.contains(
                "node `post` after the gate binds connector `tracker`, which is not installed"
            ),
            "{why}"
        );
        assert_eq!(refusal(&[], &crm_only, false), None);
        // `crm` sits after the second gate: out of reach either way.
        assert_eq!(refusal(&[], &installed(&["tracker"]), true), None);
    }

    #[test]
    fn an_irreversible_function_granted_in_reach_refuses_the_decision() {
        let mut facts = installed(&["crm"]);
        facts.insert(
            "tracker".into(),
            ConnectorFacts {
                irreversible_functions: vec!["close_issue".into()],
                ..ConnectorFacts::default()
            },
        );
        let why = refusal(&[], &facts, false).unwrap();
        assert!(
            why.contains(
                "node `post` after the gate may call irreversible tracker function(s) `close_issue`"
            ),
            "{why}"
        );
    }
}
