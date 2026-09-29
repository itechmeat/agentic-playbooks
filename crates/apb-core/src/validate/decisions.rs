//! Decision-model opt-ins on a playbook (issue #165 Parts 11, 12 and 14):
//! `route`, `option_descriptions`, `auto_decide` and the enforce opt-ins.
//! Codes V70 to V74.
//!
//! The one rule with teeth is V73: an automatic review decision would remove
//! a human approval boundary, so it is refused where the playbook declares
//! `irreversible` or `secrets` effects or runs a merge, push, deploy or
//! publish step after the gate. The inferred `external` effect does not
//! count: every playbook with an agent has it. The run re-checks the same
//! rule with the effects of pinned sub-playbooks (see
//! [`auto_decide_refusal`]).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::connector::resolve::ConnectorFacts;

use super::*;
use crate::schema::{
    AUTO_DECIDE_ALLOWED, CompletionCheckSetting, DecisionOptIn, Effect, RouteSetting,
    effective_review_options,
};

/// Words that mark a step as one that ships something out of reach of a
/// later correction.
const SHIPPING_WORDS: [&str; 4] = ["merge", "push", "deploy", "publish"];

/// Whether a node looks like a merge, push, deploy or publish step by its id,
/// title or script path.
fn ships(node: &crate::schema::Node) -> bool {
    let mut names = vec![node.id.to_lowercase()];
    if let Some(t) = &node.title {
        names.push(t.to_lowercase());
    }
    if let NodeKind::Script { script, .. } = &node.kind {
        names.push(script.to_lowercase());
    }
    names.iter().any(|n| {
        n.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| SHIPPING_WORDS.iter().any(|s| w.starts_with(s)))
    })
}

/// The ids of the nodes reachable from `from` over any edge, `from` itself
/// excluded unless a loop leads back to it.
pub fn downstream_nodes(playbook: &Playbook, from: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([from]);
    while let Some(cur) = queue.pop_front() {
        for e in playbook.edges.iter().filter(|e| e.from == cur) {
            if seen.insert(e.to.clone()) {
                queue.push_back(&e.to);
            }
        }
    }
    seen
}

/// Why an automatic decision at gate `node` is refused, or `None` when it is
/// allowed: declared `irreversible` or `secrets` effects (the playbook's own
/// plus `inherited`, the pinned sub-playbooks' declared effects the run
/// knows), or a merge, push, deploy or publish step downstream of the gate.
/// `auto_decide_ok: true` on the gate overrides both. The installed
/// connectors are not consulted here; [`auto_decide_refusal_with`] does.
pub fn auto_decide_refusal(
    playbook: &Playbook,
    node: &str,
    inherited: &[Effect],
) -> Option<String> {
    auto_decide_refusal_with(playbook, node, inherited, &BTreeMap::new())
}

// --- 0.23.0 V73 by declared effects ---------------------------------------------

/// The functions of `facts` a binding grants, among `names`.
fn granted<'a>(binding: &crate::schema::ConnectorBinding, names: &'a [String]) -> Vec<&'a String> {
    use crate::schema::FunctionsAllow;
    match &binding.functions {
        FunctionsAllow::All => names.iter().collect(),
        // A `read_only` grant never reaches a write function.
        FunctionsAllow::ReadOnly => Vec::new(),
        FunctionsAllow::List(list) => names.iter().filter(|n| list.contains(n)).collect(),
    }
}

/// Why node `n` counts as a step that ships something out of reach of a
/// later correction, strongest evidence first: its own declared
/// `irreversible` or `secrets` effects, a granted connector function flagged
/// `irreversible`, then the name heuristic ([`ships`]) as the fallback.
fn shipping_reason(
    n: &crate::schema::Node,
    connectors: &BTreeMap<String, ConnectorFacts>,
) -> Option<String> {
    let declared: Vec<&str> = n
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::Irreversible => Some("irreversible"),
            Effect::Secrets => Some("secrets"),
            _ => None,
        })
        .collect();
    if !declared.is_empty() {
        return Some(format!(
            "node `{}` after the gate declares {} effects",
            n.id,
            declared.join(" and ")
        ));
    }
    for b in n.kind.connector_bindings() {
        let Some(facts) = connectors.get(&b.name) else {
            continue;
        };
        let fns = granted(b, &facts.irreversible_functions);
        if !fns.is_empty() {
            return Some(format!(
                "node `{}` after the gate may call irreversible {} function(s) {}",
                n.id,
                b.name,
                fns.iter()
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    ships(n).then(|| {
        format!(
            "node `{}` after the gate looks like a merge, push, deploy or publish step",
            n.id
        )
    })
}

/// [`auto_decide_refusal`] that also honours the connector functions a
/// downstream node is granted, from the installed connectors' `facts`.
pub fn auto_decide_refusal_with(
    playbook: &Playbook,
    node: &str,
    inherited: &[Effect],
    connectors: &BTreeMap<String, ConnectorFacts>,
) -> Option<String> {
    let gate = playbook.node(node)?;
    if gate.auto_decide_ok {
        return None;
    }
    let dangerous: BTreeSet<&'static str> = playbook
        .effects
        .iter()
        .chain(inherited)
        .filter_map(|e| match e {
            Effect::Irreversible => Some("irreversible"),
            Effect::Secrets => Some("secrets"),
            _ => None,
        })
        .collect();
    if !dangerous.is_empty() {
        return Some(format!(
            "the playbook declares {} effects",
            dangerous.into_iter().collect::<Vec<_>>().join(" and ")
        ));
    }
    let reasons: Vec<String> = downstream_nodes(playbook, node)
        .into_iter()
        .filter_map(|id| {
            playbook
                .node(&id)
                .and_then(|n| shipping_reason(n, connectors))
        })
        .collect();
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// Downstream nodes bound to a connector whose effects cannot be known: its
/// installed manifest no longer loads (`load_error`) or, when
/// `missing_is_unknown`, it is not installed at all. A `read_only` grant is
/// left out: it never reaches a write function whatever the manifest says.
/// Validation passes `false` (a context without a connector store knows
/// nothing); the run-time check passes `true`.
fn unknown_connector_effects(
    playbook: &Playbook,
    node: &str,
    connectors: &BTreeMap<String, ConnectorFacts>,
    missing_is_unknown: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for id in downstream_nodes(playbook, node) {
        let Some(n) = playbook.node(&id) else {
            continue;
        };
        for b in n.kind.connector_bindings() {
            if matches!(b.functions, crate::schema::FunctionsAllow::ReadOnly) {
                continue;
            }
            let why = match connectors.get(&b.name) {
                Some(f) if f.load_error.is_some() => "whose manifest does not load",
                None if missing_is_unknown => "which is not installed",
                _ => continue,
            };
            out.push(format!(
                "node `{id}` after the gate binds connector `{}`, {why} (effects unknown)",
                b.name
            ));
        }
    }
    out
}

/// The run-time refusal of an automatic decision: [`auto_decide_refusal_with`]
/// over the installed connectors, and fail-closed on a connector bound after
/// the gate whose effects cannot be known (its manifest does not load, or it
/// is not installed). `auto_decide_ok: true` still overrides.
pub fn auto_decide_run_refusal(
    playbook: &Playbook,
    node: &str,
    inherited: &[Effect],
    connectors: &BTreeMap<String, ConnectorFacts>,
) -> Option<String> {
    if let Some(why) = auto_decide_refusal_with(playbook, node, inherited, connectors) {
        return Some(why);
    }
    if playbook.node(node)?.auto_decide_ok {
        return None;
    }
    let unknown = unknown_connector_effects(playbook, node, connectors, true);
    (!unknown.is_empty()).then(|| unknown.join("; "))
}

/// Downstream nodes granted a connector function that is not `read_only`:
/// `(node, connector, functions)`. Not a refusal (a comment on a tracker is
/// not a deploy), but V73 names them so an author sees what an automatic
/// decision would let through.
fn downstream_writes(
    playbook: &Playbook,
    node: &str,
    connectors: &BTreeMap<String, ConnectorFacts>,
) -> Vec<String> {
    let mut out = Vec::new();
    for id in downstream_nodes(playbook, node) {
        let Some(n) = playbook.node(&id) else {
            continue;
        };
        for b in n.kind.connector_bindings() {
            let Some(facts) = connectors.get(&b.name) else {
                continue;
            };
            let fns = granted(b, &facts.write_functions);
            if !fns.is_empty() {
                out.push(format!(
                    "`{id}` ({} {})",
                    b.name,
                    fns.iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    out
}

// --- end of V73 by declared effects ----------------------------------------------

/// V70-V74.
pub(super) fn check_decision_opt_ins(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    let mut enforce_uses: Vec<&'static str> = Vec::new();
    for n in &playbook.nodes {
        let id = Some(n.id.as_str());
        let is_agent = matches!(n.kind, NodeKind::AgentTask { .. });
        // V70: tier routing on an agent_task only; a handoff node is never
        // routed.
        if n.route == Some(RouteSetting::Auto) {
            match &n.kind {
                NodeKind::AgentTask {
                    continue_session, ..
                } => {
                    if continue_session.is_some() {
                        r.warn(
                            "V70",
                            id,
                            "`route: auto` has no effect on a node with `continue_session`: a warm handoff needs the same executor, so it is never routed".into(),
                        );
                    }
                    enforce_uses.push("routing");
                }
                _ => r.error("V70", id, "`route` applies to agent_task nodes only".into()),
            }
        }
        let review_options = match &n.kind {
            NodeKind::HumanReview { options, .. } => Some(effective_review_options(options)),
            _ => None,
        };
        // V71: option descriptions name declared options.
        if !n.option_descriptions.is_empty() {
            match &review_options {
                None => r.error(
                    "V71",
                    id,
                    "`option_descriptions` applies to human_review nodes only".into(),
                ),
                Some(opts) => {
                    for (k, v) in &n.option_descriptions {
                        if !opts.contains(k) {
                            r.error(
                                "V71",
                                id,
                                format!(
                                    "`option_descriptions` names `{k}`, which is not one of the gate's options ({})",
                                    opts.join(", ")
                                ),
                            );
                        } else if v.trim().is_empty() {
                            r.error("V71", id, format!("`option_descriptions.{k}` is empty"));
                        }
                    }
                }
            }
        }
        // V72: the auto_decide block itself; V73: where it is refused.
        if let Some(ad) = &n.auto_decide {
            match &review_options {
                None => r.error(
                    "V72",
                    id,
                    "`auto_decide` applies to human_review nodes only".into(),
                ),
                Some(opts) => {
                    if ad.allow.is_empty() {
                        r.error("V72", id, "`auto_decide.allow` is empty".into());
                    }
                    for o in &ad.allow {
                        if o != AUTO_DECIDE_ALLOWED {
                            r.error(
                                "V72",
                                id,
                                format!(
                                    "`auto_decide.allow` may contain only `{AUTO_DECIDE_ALLOWED}`; `{o}` must stay a human decision"
                                ),
                            );
                        } else if !opts.contains(o) {
                            r.error(
                                "V72",
                                id,
                                format!("`auto_decide.allow` names `{o}`, which is not one of the gate's options"),
                            );
                        }
                    }
                    if !(ad.min_confidence > 0.0 && ad.min_confidence <= 1.0) {
                        r.error(
                            "V72",
                            id,
                            "`auto_decide.min_confidence` must be in (0, 1]".into(),
                        );
                    }
                    if let Some(why) =
                        auto_decide_refusal_with(playbook, &n.id, &[], &ctx.connectors)
                    {
                        r.error(
                            "V73",
                            id,
                            format!(
                                "`auto_decide` is refused: {why}; an automatic decision must not remove this approval boundary (set `auto_decide_ok: true` to override)"
                            ),
                        );
                    } else if !n.auto_decide_ok {
                        let unknown =
                            unknown_connector_effects(playbook, &n.id, &ctx.connectors, false);
                        if !unknown.is_empty() {
                            r.warn(
                                "V73",
                                id,
                                format!(
                                    "`auto_decide` cannot tell what nodes after the gate may do: {}; a run refuses the automatic decision until the manifest loads again",
                                    unknown.join("; ")
                                ),
                            );
                        }
                        let writes = downstream_writes(playbook, &n.id, &ctx.connectors);
                        if !writes.is_empty() {
                            r.warn(
                                "V73",
                                id,
                                format!(
                                    "`auto_decide` lets nodes after the gate write through connectors: {}; flag a function that cannot be taken back `irreversible: true` in its connector, or declare `effects: [irreversible]` on the node, to keep a person at this gate",
                                    writes.join(", ")
                                ),
                            );
                        }
                    }
                    enforce_uses.push("review_triage");
                }
            }
        }
        // V74: completion_check is an agent_task setting.
        if let Some(cc) = n.completion_check {
            if !is_agent {
                r.warn(
                    "V74",
                    id,
                    "`completion_check` has no effect outside agent_task nodes".into(),
                );
            } else if cc == CompletionCheckSetting::Enforce {
                enforce_uses.push("completion_check");
            }
        }
    }
    if playbook.defaults.retry_advice == Some(DecisionOptIn::Enforce) {
        enforce_uses.push("retry_advice");
    }
    if playbook
        .supervisor
        .as_ref()
        .and_then(|s| s.pre_triage)
        .is_some_and(|m| m == DecisionOptIn::Enforce)
    {
        enforce_uses.push("supervisor_triage");
    }
    // V74 (informational): an opt-in acts only where the machine enables it.
    enforce_uses.sort_unstable();
    enforce_uses.dedup();
    if !enforce_uses.is_empty() {
        r.warn(
            "V74",
            None,
            format!(
                "decision-model opt-ins ({}) act only on a machine whose decisions.yaml puts the use in enforce and has a stored threshold for its model; elsewhere the playbook runs as without them",
                enforce_uses.join(", ")
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Severity, ValidationContext, validate};
    use super::auto_decide_run_refusal;
    use crate::schema::Playbook;
    use std::collections::BTreeMap;

    fn issues(yaml: &str) -> Vec<(String, Severity)> {
        let p = Playbook::from_yaml(yaml).unwrap();
        validate(&p, &ValidationContext::default())
            .issues
            .into_iter()
            .filter(|i| i.code.starts_with("V7"))
            .map(|i| (i.code.to_string(), i.severity))
            .collect()
    }

    fn gate(extra: &str, effects: &str, after: &str) -> String {
        format!(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\n{effects}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: x, profile: m }}\n  - {{ id: g, type: human_review, options: [approve, needs_changes]{extra} }}\n  - {{ id: {after}, type: agent_task, prompt: y, profile: m }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: g }}\n  - {{ from: g, to: {after} }}\n  - {{ from: {after}, to: done }}\n"
        )
    }

    const AUTO: &str = ", auto_decide: { allow: [needs_changes] }";

    #[test]
    fn auto_decide_is_allowed_on_a_plain_gate_with_an_informational_note() {
        let got = issues(&gate(AUTO, "", "fix"));
        assert_eq!(got, vec![("V74".to_string(), Severity::Warning)]);
    }

    #[test]
    fn auto_decide_is_refused_per_declared_effect_and_for_shipping_steps() {
        for (effects, after) in [
            ("effects: [irreversible]\n", "fix"),
            ("effects: [secrets]\n", "fix"),
            ("", "merge_pr"),
            ("", "git-push"),
            ("", "deploy"),
            ("", "publish_release"),
        ] {
            let got = issues(&gate(AUTO, effects, after));
            assert!(
                got.contains(&("V73".to_string(), Severity::Error)),
                "{effects} {after}: {got:?}"
            );
        }
        // The inferred `external` (every agent playbook has it) and a
        // declared `network` do not refuse.
        let got = issues(&gate(AUTO, "effects: [external, network]\n", "fix"));
        assert!(!got.iter().any(|(c, _)| c == "V73"), "{got:?}");
        // The explicit override.
        let got = issues(&gate(
            &format!("{AUTO}, auto_decide_ok: true"),
            "effects: [irreversible]\n",
            "merge",
        ));
        assert!(!got.iter().any(|(c, _)| c == "V73"), "{got:?}");
    }

    /// The downstream node, by its `after` id with extra fields.
    fn gate_with(extra_after: &str, after: &str) -> String {
        gate(AUTO, "", after).replace(
            &format!("{{ id: {after}, type: agent_task, prompt: y, profile: m }}"),
            &format!("{{ id: {after}, type: agent_task, prompt: y, profile: m{extra_after} }}"),
        )
    }

    fn issues_with(yaml: &str, ctx: &ValidationContext) -> Vec<(String, Severity, String)> {
        let p = Playbook::from_yaml(yaml).unwrap();
        validate(&p, ctx)
            .issues
            .into_iter()
            .filter(|i| i.code == "V73")
            .map(|i| (i.code.to_string(), i.severity, i.message))
            .collect()
    }

    fn tracker() -> ValidationContext {
        let facts = crate::connector::resolve::ConnectorFacts {
            irreversible_functions: vec!["merge_request".into()],
            write_functions: vec!["merge_request".into(), "add_comment".into()],
            ..Default::default()
        };
        ValidationContext {
            connectors: BTreeMap::from([("tracker".to_string(), facts)]),
            ..Default::default()
        }
    }

    /// V73 honours what a node declares, not only what it is called: a
    /// neutral id with `effects: [irreversible]` refuses, and so does a
    /// granted connector function flagged `irreversible`; a `read_only`
    /// grant or a list without it does not, and a plain write function is a
    /// warning. `auto_decide_ok` still overrides.
    #[test]
    fn auto_decide_is_refused_by_declared_node_effects_and_irreversible_functions() {
        let none = ValidationContext::default();
        let declared = gate_with(", effects: [irreversible]", "finalize");
        let got = issues_with(&declared, &none);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].1, Severity::Error);
        assert!(
            got[0]
                .2
                .contains("`finalize` after the gate declares irreversible"),
            "{}",
            got[0].2
        );
        // A neutral name without a declaration stays allowed.
        assert!(issues_with(&gate_with("", "finalize"), &none).is_empty());

        let all = gate_with(", connectors: [tracker]", "finalize");
        let got = issues_with(&all, &tracker());
        assert_eq!(got[0].1, Severity::Error, "{got:?}");
        assert!(
            got[0]
                .2
                .contains("irreversible tracker function(s) `merge_request`"),
            "{}",
            got[0].2
        );
        // Without the installed connector's facts nothing is known.
        assert!(issues_with(&all, &none).is_empty());

        let read_only = gate_with(
            ", connectors: [{ name: tracker, functions: read_only }]",
            "finalize",
        );
        assert!(issues_with(&read_only, &tracker()).is_empty());

        let comment = gate_with(
            ", connectors: [{ name: tracker, functions: [add_comment] }]",
            "finalize",
        );
        let got = issues_with(&comment, &tracker());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].1, Severity::Warning);
        assert!(
            got[0].2.contains("`finalize` (tracker add_comment)"),
            "{}",
            got[0].2
        );

        let overridden = all.replace(AUTO, &format!("{AUTO}, auto_decide_ok: true"));
        assert!(issues_with(&overridden, &tracker()).is_empty());
    }

    /// A connector bound after the gate whose manifest no longer loads (or
    /// that is not installed at run time) has unknown effects: the run
    /// refuses the automatic decision, validation warns. A `read_only` grant
    /// never reaches a write function, so it stays allowed.
    #[test]
    fn auto_decide_is_refused_at_run_time_for_a_connector_with_unknown_effects() {
        let broken = ValidationContext {
            connectors: BTreeMap::from([(
                "t".to_string(),
                crate::connector::resolve::ConnectorFacts {
                    load_error: Some("bad manifest".into()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let yaml = gate_with(", connectors: [t]", "finalize");
        let p = Playbook::from_yaml(&yaml).unwrap();
        let why = auto_decide_run_refusal(&p, "g", &[], &broken.connectors).unwrap();
        assert!(
            why.contains("`finalize`") && why.contains("effects unknown"),
            "{why}"
        );
        // Not installed at run time: unknown too.
        let why = auto_decide_run_refusal(&p, "g", &[], &BTreeMap::new()).unwrap();
        assert!(why.contains("not installed"), "{why}");
        let got = issues_with(&yaml, &broken);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].1, Severity::Warning);
        assert!(got[0].2.contains("effects unknown"), "{}", got[0].2);
        // Validation without facts knows nothing and says nothing.
        assert!(issues_with(&yaml, &ValidationContext::default()).is_empty());
        let read_only = gate_with(
            ", connectors: [{ name: t, functions: read_only }]",
            "finalize",
        );
        let p = Playbook::from_yaml(&read_only).unwrap();
        assert!(auto_decide_run_refusal(&p, "g", &[], &broken.connectors).is_none());
        let overridden = yaml.replace(AUTO, &format!("{AUTO}, auto_decide_ok: true"));
        let p = Playbook::from_yaml(&overridden).unwrap();
        assert!(auto_decide_run_refusal(&p, "g", &[], &broken.connectors).is_none());
    }

    #[test]
    fn auto_decide_allows_needs_changes_only() {
        let got = issues(&gate(
            ", auto_decide: { allow: [approve, needs_changes] }",
            "",
            "fix",
        ));
        assert!(got.contains(&("V72".to_string(), Severity::Error)));
        let got = issues(&gate(
            ", auto_decide: { allow: [needs_changes], min_confidence: 1.5 }",
            "",
            "fix",
        ));
        assert!(got.contains(&("V72".to_string(), Severity::Error)));
        // `needs_changes` must be a declared option.
        let yaml = gate(AUTO, "", "fix").replace(
            "options: [approve, needs_changes]",
            "options: [approve, reject]",
        );
        assert!(issues(&yaml).contains(&("V72".to_string(), Severity::Error)));
    }

    #[test]
    fn option_descriptions_name_declared_options() {
        let ok = issues(&gate(
            ", option_descriptions: { approve: \"Tests pass.\", needs_changes: \"Fixes first.\" }",
            "",
            "fix",
        ));
        assert!(ok.is_empty(), "{ok:?}");
        let bad = issues(&gate(
            ", option_descriptions: { reject: \"Wrong.\" }",
            "",
            "fix",
        ));
        assert_eq!(bad, vec![("V71".to_string(), Severity::Error)]);
        // Default options count as declared.
        let yaml = gate(", option_descriptions: { reject: \"Wrong.\" }", "", "fix")
            .replace("options: [approve, needs_changes], ", "");
        assert!(issues(&yaml).is_empty());
    }

    #[test]
    fn route_is_an_agent_task_setting_and_never_applies_to_a_handoff() {
        let yaml = gate("", "", "fix").replace(
            "prompt: y, profile: m",
            "prompt: y, profile: m, route: auto, continue_session: w",
        );
        let got = issues(&yaml);
        assert!(
            got.contains(&("V70".to_string(), Severity::Warning)),
            "{got:?}"
        );
        let yaml = gate(", route: auto", "", "fix");
        assert!(issues(&yaml).contains(&("V70".to_string(), Severity::Error)));
    }
}
