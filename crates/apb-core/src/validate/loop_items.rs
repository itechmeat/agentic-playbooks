//! Rules for the `no_new_items` edge condition and the seen template:
//! V164 a malformed condition, V165 a `{{nodes.<id>.seen.<field>}}`
//! template with no condition behind it or over two disagreeing ones, and
//! V166 (warning) a condition whose node and edge source share no cycle.

use super::templates::{cache_key_texts, template_refs, template_texts, workdir_texts};
use super::*;

/// V164, V165 and V166, one pass over the edges and one over the template
/// texts. V164: a malformed `no_new_items` condition. V165: a seen template
/// with no `no_new_items` condition on its node and field, or over two
/// conditions that disagree on `key`. V166 (warning): the condition's node
/// and the edge source share no cycle, so the run never comes round to the
/// repeated visits the condition compares.
pub(crate) fn check_loop_items(playbook: &Playbook, r: &mut ValidationReport) {
    // V165: the conditions grouped by (node, field), in edge order, each
    // group holding its `key` (or the absence of one).
    let mut keys: HashMap<(&str, &str), Vec<Option<&str>>> = HashMap::new();
    // V166: the cyclic components, computed once for the whole playbook. A
    // one-node component is cyclic exactly when the node carries a self-loop
    // edge, the same test check_cycles applies.
    let self_loops: HashSet<&str> = playbook
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| e.from.as_str())
        .collect();
    let components = crate::graphutil::sccs(playbook);
    let cyclic: Vec<HashSet<&str>> = components
        .iter()
        .filter(|c| c.len() > 1 || self_loops.contains(&c[0].as_str()))
        .map(|c| c.iter().map(String::as_str).collect())
        .collect();

    for e in &playbook.edges {
        let Some(cond) = e.condition.as_ref() else {
            continue;
        };
        let EdgeCondition::NoNewItems {
            node,
            field,
            key,
            rounds,
        } = cond
        else {
            continue;
        };
        keys.entry((node.as_str(), field.as_str()))
            .or_default()
            .push(key.as_deref());
        let from = e.from.as_str();
        // V164: the condition itself must be well formed. Each rule reports
        // once against the edge's source and moves on, the way V10 does.
        let Some(n) = playbook.node(node) else {
            r.error_edges(
                playbook,
                "V164",
                Some(from),
                &[e],
                format!("no_new_items names node `{node}`, which does not exist"),
            );
            continue;
        };
        if !matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. }) {
            r.error_edges(
                playbook,
                "V164",
                Some(from),
                &[e],
                format!(
                    "no_new_items names node `{node}`, a {} node; only an agent_task or a script node can be visited round after round",
                    n.kind.type_str()
                ),
            );
            continue;
        }
        if n.list().is_some() {
            r.error_edges(
                playbook,
                "V164",
                Some(from),
                &[e],
                format!(
                    "no_new_items names node `{node}`, a list node; a list node runs its items in one visit and is never revisited"
                ),
            );
            continue;
        }
        let plain = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c != '.' && c != '}' && !c.is_whitespace())
        };
        if !plain(field) {
            r.error_edges(
                playbook,
                "V164",
                Some(from),
                &[e],
                format!(
                    "no_new_items `field` must be a plain field name; empty, a dot, whitespace or a closing brace not allowed, got `{field}`"
                ),
            );
            continue;
        }
        if let Some(k) = key.as_deref().filter(|k| !plain(k)) {
            r.error_edges(
                playbook,
                "V164",
                Some(from),
                &[e],
                format!(
                    "no_new_items `key` must be a plain field name; empty, a dot, whitespace or a closing brace not allowed, got `{k}`"
                ),
            );
            continue;
        }
        if !(1..=10).contains(rounds) {
            r.error_edges(
                playbook,
                "V164",
                Some(from),
                &[e],
                format!("no_new_items `rounds` must be between 1 and 10, got {rounds}"),
            );
            continue;
        }
        // V166 (warning): the watched node and the edge source must sit on
        // one cycle, or the run never returns to the node for the next
        // visit. A node is on a cycle exactly when it sits in a cyclic
        // component; two different nodes share a cycle when one cyclic
        // component holds both.
        let on_one_cycle = if from == node.as_str() {
            cyclic.iter().any(|c| c.contains(node.as_str()))
        } else {
            cyclic
                .iter()
                .any(|c| c.contains(from) && c.contains(node.as_str()))
        };
        if !on_one_cycle {
            let pair = if from == node.as_str() {
                format!("`{node}` is on no cycle")
            } else {
                format!("`{from}` and `{node}` share no cycle")
            };
            r.warn_edges(
                playbook,
                "V166",
                Some(from),
                &[e],
                format!(
                    "no_new_items names node `{node}`, but {pair}; the condition never sees the repeated visits it compares"
                ),
            );
        }
    }

    // V165: every seen template must be backed by the condition it reads,
    // in a prompt, a workdir or a cache key alike (an unbacked one renders
    // empty, so a cache key over it never moves). A template naming an
    // unknown node or carrying an empty field stays V13's, the same
    // deferral V38 makes; this loop consults only the templates the
    // grammar accepts.
    let texts = template_texts(playbook)
        .into_iter()
        .chain(workdir_texts(playbook))
        .chain(cache_key_texts(playbook));
    for (owner, text) in texts {
        for cap in template_refs(text) {
            let parts: Vec<&str> = cap.split('.').collect();
            let ["nodes", nid, "seen", field] = parts.as_slice() else {
                continue;
            };
            if playbook.node(nid).is_none() || field.trim().is_empty() {
                continue;
            }
            let message = match keys.get(&(*nid, *field)) {
                None => Some(format!(
                    "template {{{{nodes.{nid}.seen.{field}}}}} has no no_new_items condition on `{nid}` and `{field}`; add the condition to the edge that exits the loop"
                )),
                Some(ks) if ks.windows(2).any(|pair| pair[0] != pair[1]) => Some(format!(
                    "template {{{{nodes.{nid}.seen.{field}}}}} is backed by no_new_items conditions on `{nid}` and `{field}` that disagree on `key`; the seen text is then ambiguous"
                )),
                _ => None,
            };
            if let Some(message) = message {
                r.error("V165", Some(owner), message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(yaml: &str) -> Vec<(&'static str, Severity)> {
        let p = Playbook::from_yaml(yaml).unwrap();
        validate(&p, &ValidationContext::default())
            .issues
            .iter()
            .map(|i| (i.code, i.severity))
            .collect()
    }

    /// The clean loop playbook: `s -> hunt`, the exit edge `hunt -> f`
    /// carrying the `no_new_items` condition, the conditional self loop with
    /// a traversal cap, and a fallback route to the finish, the order the
    /// pattern prescribes (exit first, loop conditional, fallback last).
    const CLEAN: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: hunt, type: agent_task, prompt: "find items", expected_duration: 5m }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: hunt }
  - { from: hunt, to: f, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }
  - { from: hunt, to: hunt, condition: { type: node_status, node: hunt, equals: success }, max_traversals: 6 }
  - { from: hunt, to: f, fallback: true }
"#;

    /// The exit condition's inner text, the substring every condition
    /// mutation rewrites.
    const EXIT: &str = "{ type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 }";
    /// The loop edge's line without the leading dash, for edge order swaps.
    const LOOP_LINE: &str = "- { from: hunt, to: hunt, condition: { type: node_status, node: hunt, equals: success }, max_traversals: 6 }";

    /// CLEAN with the prompt reading the seen keys of `hunt` through the
    /// template the condition backs.
    fn clean_with_seen_template() -> String {
        CLEAN.replace(
            "prompt: \"find items\"",
            "prompt: \"revisit with {{nodes.hunt.seen.findings}}\"",
        )
    }

    #[test]
    fn a_clean_loop_playbook_is_fully_clean() {
        let c = codes(CLEAN);
        assert!(c.is_empty(), "expected a clean loop playbook, got {c:?}");
    }

    #[test]
    fn v164_unknown_node() {
        let yaml = CLEAN.replace(
            EXIT,
            "{ type: no_new_items, node: nope, field: findings, key: id, rounds: 2 }",
        );
        assert!(
            codes(&yaml).contains(&("V164", Severity::Error)),
            "an unknown watched node must be V164"
        );
    }

    #[test]
    fn v164_node_that_is_not_an_agent_task_or_script() {
        // The start node exists but cannot be visited round after round.
        let yaml = CLEAN.replace(
            EXIT,
            "{ type: no_new_items, node: s, field: findings, key: id, rounds: 2 }",
        );
        let c = codes(&yaml);
        assert!(c.contains(&("V164", Severity::Error)), "got {c:?}");
        let report = validate(
            &Playbook::from_yaml(&yaml).unwrap(),
            &ValidationContext::default(),
        );
        let issue = report
            .issues
            .iter()
            .find(|i| i.code == "V164")
            .expect("expected a V164 error");
        assert!(
            issue.message.contains("start"),
            "V164 must name the node kind it rejects, got: {}",
            issue.message
        );
    }

    #[test]
    fn v164_list_node() {
        let yaml = CLEAN.replace(
            "- { id: hunt, type: agent_task, prompt: \"find items\", expected_duration: 5m }",
            "- { id: hunt, type: agent_task, prompt: \"find items\", expected_duration: 5m, list: { items: { inline: [ { id: a } ] } } }",
        );
        assert!(
            codes(&yaml).contains(&("V164", Severity::Error)),
            "a list node must be V164"
        );
    }

    #[test]
    fn v164_bad_field_values() {
        for bad in ["", " ", "find.ings", "find ings", "findings}"] {
            let yaml = CLEAN.replace("field: findings", &format!("field: \"{bad}\""));
            let c = codes(&yaml);
            assert!(
                c.contains(&("V164", Severity::Error)),
                "field `{bad}` must be V164, got {c:?}"
            );
        }
    }

    #[test]
    fn v164_bad_key_values() {
        for bad in ["", " ", "it.em", "it em", "item}"] {
            let yaml = CLEAN.replace("key: id", &format!("key: \"{bad}\""));
            let c = codes(&yaml);
            assert!(
                c.contains(&("V164", Severity::Error)),
                "key `{bad}` must be V164, got {c:?}"
            );
        }
    }

    #[test]
    fn v164_rounds_outside_1_to_10() {
        for rounds in ["0", "11"] {
            let yaml = CLEAN.replace("rounds: 2", &format!("rounds: {rounds}"));
            let c = codes(&yaml);
            assert!(
                c.contains(&("V164", Severity::Error)),
                "rounds {rounds} must be V164, got {c:?}"
            );
        }
    }

    #[test]
    fn v164_rounds_boundaries_are_valid() {
        for rounds in ["1", "10"] {
            let yaml = CLEAN.replace("rounds: 2", &format!("rounds: {rounds}"));
            let c = codes(&yaml);
            assert!(
                !c.iter().any(|(code, _)| *code == "V164"),
                "rounds {rounds} is inside the range, got {c:?}"
            );
        }
    }

    /// The seen template the exit condition backs: no V13 and no V165.
    #[test]
    fn v13_silent_for_a_seen_template_backed_by_its_condition() {
        let c = codes(&clean_with_seen_template());
        assert!(
            !c.iter().any(|(code, _)| *code == "V13"),
            "a seen template with its condition must resolve, got {c:?}"
        );
        assert!(!c.iter().any(|(code, _)| *code == "V165"), "got {c:?}");
    }

    /// Same node and field, but the condition is gone (the exit edge routes
    /// on `failure` instead, so the graph shape stays legal): V165, and V13
    /// stays out of it because the grammar arm accepts the reference.
    #[test]
    fn v165_seen_template_without_a_condition() {
        let yaml = clean_with_seen_template()
            .replace(EXIT, "{ type: node_status, node: hunt, equals: failure }");
        let c = codes(&yaml);
        assert!(c.contains(&("V165", Severity::Error)), "got {c:?}");
        assert!(
            !c.iter().any(|(code, _)| *code == "V13"),
            "the seen grammar arm keeps the reference resolvable, got {c:?}"
        );
    }

    /// A second condition over the same node and field with another `key`
    /// (here: no key at all) makes the seen text ambiguous.
    #[test]
    fn v165_two_conditions_disagreeing_on_key() {
        let yaml = clean_with_seen_template().replace(
            "- { from: hunt, to: f, fallback: true }",
            "- { from: hunt, to: f, condition: { type: no_new_items, node: hunt, field: findings, rounds: 2 } }\n  - { from: hunt, to: f, fallback: true }",
        );
        let c = codes(&yaml);
        assert!(c.contains(&("V165", Severity::Error)), "got {c:?}");
    }

    /// Two conditions over the same node, field AND key agree, whatever
    /// their `rounds`.
    #[test]
    fn v165_two_conditions_on_one_key_are_clean() {
        let yaml = clean_with_seen_template().replace(
            "- { from: hunt, to: f, fallback: true }",
            "- { from: hunt, to: f, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 3 } }\n  - { from: hunt, to: f, fallback: true }",
        );
        let c = codes(&yaml);
        assert!(
            !c.iter().any(|(code, _)| *code == "V165"),
            "agreeing keys must not be V165, got {c:?}"
        );
    }

    /// `prep` runs once before the loop; the exit edge off `hunt` watching
    /// `prep` can never see the repeated visits the condition compares.
    #[test]
    fn v166_watched_node_on_no_cycle_warns() {
        let yaml = CLEAN
            .replace(
                EXIT,
                "{ type: no_new_items, node: prep, field: findings, key: id, rounds: 2 }",
            )
            .replace(
                "- { id: hunt, type: agent_task, prompt: \"find items\", expected_duration: 5m }",
                "- { id: prep, type: agent_task, prompt: \"prepare\", expected_duration: 5m }\n  - { id: hunt, type: agent_task, prompt: \"find items\", expected_duration: 5m }",
            )
            .replace(
                "- { from: s, to: hunt }",
                "- { from: s, to: prep }\n  - { from: prep, to: hunt }",
            );
        let c = codes(&yaml);
        assert!(c.contains(&("V166", Severity::Warning)), "got {c:?}");
    }

    /// The loop passes through `review` and `gate`; the exit edge off
    /// `gate` watches `hunt`, and one cycle runs around both.
    #[test]
    fn v166_two_nodes_on_one_cycle_are_clean() {
        let yaml = CLEAN
            .replace(
                "- { from: hunt, to: f, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }",
                "- { from: gate, to: f, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }",
            )
            .replace(
                LOOP_LINE,
                "- { from: hunt, to: review }\n  - { from: review, to: gate }\n  - { from: gate, to: hunt, condition: { type: node_status, node: gate, equals: success }, max_traversals: 6 }",
            )
            .replace(
                "- { from: hunt, to: f, fallback: true }",
                "- { from: gate, to: f, fallback: true }",
            )
            .replace(
                "- { id: hunt, type: agent_task, prompt: \"find items\", expected_duration: 5m }",
                "- { id: hunt, type: agent_task, prompt: \"find items\", expected_duration: 5m }\n  - { id: review, type: agent_task, prompt: \"review\", expected_duration: 5m }\n  - { id: gate, type: agent_task, prompt: \"gate\", expected_duration: 5m }",
            );
        let c = codes(&yaml);
        assert!(c.is_empty(), "expected a clean three-node loop, got {c:?}");
    }

    /// The loop edge taken unconditionally shadows the conditional exit
    /// edge, whichever of the two is declared first (V34, already in place
    /// for the other condition kinds, must fire for this one too).
    #[test]
    fn v34_unconditional_loop_edge_after_the_exit_edge() {
        let yaml = CLEAN.replace(LOOP_LINE, "- { from: hunt, to: hunt }");
        let c = codes(&yaml);
        assert!(c.contains(&("V34", Severity::Error)), "got {c:?}");
    }

    #[test]
    fn v34_unconditional_loop_edge_before_the_exit_edge() {
        let yaml = CLEAN
            .replace(
                "- { from: hunt, to: f, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }",
                "- { from: hunt, to: hunt }\n  - { from: hunt, to: f, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }",
            )
            .replace(
                &format!("  {LOOP_LINE}\n"),
                "",
            );
        let c = codes(&yaml);
        assert!(c.contains(&("V34", Severity::Error)), "got {c:?}");
    }

    /// V165 reads every template the run renders: a seen reference in a
    /// cache key or a workdir with no condition behind it is refused like
    /// one in a prompt.
    #[test]
    fn v165_seen_template_in_a_cache_key_or_workdir_without_a_condition() {
        for extra in [
            ", cache: { key: \"{{nodes.hunt.seen.findings}}\" }",
            ", workdir: \"wd/{{nodes.hunt.seen.findings}}\"",
        ] {
            let yaml = CLEAN
                .replace(EXIT, "{ type: node_status, node: hunt, equals: failure }")
                .replace(
                    "expected_duration: 5m }",
                    &format!("expected_duration: 5m{extra} }}"),
                );
            let c = codes(&yaml);
            assert!(c.contains(&("V165", Severity::Error)), "{extra}: got {c:?}");
        }
    }

    /// V46 reads the seen template like an output template: a node that
    /// declares its fields without the one a seen reference names is
    /// warned at the reading node, not only at the condition's edge.
    #[test]
    fn v46_seen_template_of_an_undeclared_field() {
        let yaml = clean_with_seen_template()
            .replace(
                "  - { id: f, type: finish, outcome: success }",
                "  - { id: f, type: finish, outcome: success, prompt: \"all of {{nodes.hunt.seen.findings}}\" }",
            )
            .replace(
                "expected_duration: 5m }",
                "expected_duration: 5m, outputs: { fields: [summary] } }",
            );
        let report = validate(
            &Playbook::from_yaml(&yaml).unwrap(),
            &ValidationContext::default(),
        );
        let owners: Vec<Option<&str>> = report
            .issues
            .iter()
            .filter(|i| i.code == "V46")
            .map(|i| i.node.as_deref())
            .collect();
        assert!(
            owners.contains(&Some("f")),
            "the finish prompt's seen reference must be V46, got {owners:?}"
        );
    }
}
