//! Rules about edges as individual, addressable things: authored edge ids
//! (V121), the traversal count two edges with the same ends share (V122),
//! the order of a one-of source's edges (V123, V124), and the values an
//! `output_field` edge on a judge node can ever see (V125), and a review
//! option that only a bounded edge routes (V136).
//!
//! A "one-of" source is one whose routing picks a single edge: it has a
//! conditional or fallback outgoing edge and no unconditional, non-fallback
//! one (several of those fan out together and are not checked here).

use super::*;
use crate::judge::{DECIDED_BY, DEFAULT, JudgeQuestion, UNAVAILABLE};

/// The outgoing edges of every source, in declaration order, with the
/// sources in the order they first appear.
fn outgoing(playbook: &Playbook) -> Vec<(&str, Vec<&Edge>)> {
    let mut out: Vec<(&str, Vec<&Edge>)> = Vec::new();
    for e in &playbook.edges {
        match out.iter_mut().find(|(from, _)| *from == e.from) {
            Some((_, list)) => list.push(e),
            None => out.push((e.from.as_str(), vec![e])),
        }
    }
    out
}

/// Whether a source with these outgoing edges routes to one of them.
fn is_one_of(out: &[&Edge]) -> bool {
    out.iter().any(|e| e.condition.is_some() || e.fallback)
        && !out.iter().any(|e| e.condition.is_none() && !e.fallback)
}

/// V121 (error): an authored edge id that is malformed or carried by two
/// edges. One issue per id, naming every edge that carries it.
pub(crate) fn check_edge_ids(playbook: &Playbook, r: &mut ValidationReport) {
    let mut by_id: Vec<(&str, Vec<&Edge>)> = Vec::new();
    for e in &playbook.edges {
        let Some(id) = e.id.as_deref() else { continue };
        match by_id.iter_mut().find(|(x, _)| *x == id) {
            Some((_, list)) => list.push(e),
            None => by_id.push((id, vec![e])),
        }
    }
    for (id, edges) in by_id {
        let malformed = !crate::edge_address::is_valid_edge_id(id);
        let repeated = edges.len() > 1;
        if !malformed && !repeated {
            continue;
        }
        let mut problems = Vec::new();
        if malformed {
            problems.push(format!(
                "must match [a-z0-9][a-z0-9_-]* (at most {} characters)",
                crate::edge_address::MAX_EDGE_ID_LEN
            ));
        }
        if repeated {
            problems.push(format!(
                "is used by {} edges; an edge id must be unique",
                edges.len()
            ));
        }
        r.error_edges(
            playbook,
            "V121",
            Some(&edges[0].from),
            &edges,
            format!("edge id `{id}` {}", problems.join(" and ")),
        );
    }
}

/// V122 (warning): edges with the same two ends carry different
/// `max_traversals`. The engine counts traversals per pair of ends, so every
/// edge of the pair spends one count and each stops at its own cap.
pub(crate) fn check_pair_caps(playbook: &Playbook, r: &mut ValidationReport) {
    let mut pairs: Vec<((&str, &str), Vec<&Edge>)> = Vec::new();
    for e in &playbook.edges {
        let key = (e.from.as_str(), e.to.as_str());
        match pairs.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(e),
            None => pairs.push((key, vec![e])),
        }
    }
    for ((from, to), edges) in pairs {
        let mut caps: Vec<Option<u32>> = edges.iter().map(|e| e.max_traversals).collect();
        caps.sort_unstable();
        caps.dedup();
        if caps.len() < 2 {
            continue;
        }
        let shown: Vec<String> = caps
            .iter()
            .map(|c| c.map_or_else(|| "none".to_string(), |n| n.to_string()))
            .collect();
        r.warn_edges(
            playbook,
            "V122",
            Some(from),
            &edges,
            format!(
                "edges `{from}` -> `{to}` carry different max_traversals [{}]; the traversal count is shared by the pair, so every traversal of any of them counts, and each edge stops at its own cap against that shared count",
                shown.join(", ")
            ),
        );
    }
}

/// V123 (warning): a fallback edge of a one-of source that is not its last
/// outgoing edge. The fallback is taken only after every other edge, so its
/// position reads as an order it does not have.
///
/// V124 (warning): two unbounded edges of a one-of source with the same
/// target, condition and fallback flag; the later one is never taken. A
/// bounded edge (`max_traversals`) is left out: once it is exhausted the
/// next one with the same check is taken (V122 covers differing caps).
pub(crate) fn check_one_of_order(playbook: &Playbook, r: &mut ValidationReport) {
    for (from, out) in outgoing(playbook) {
        if !is_one_of(&out) {
            continue;
        }
        let last = out.len() - 1;
        for (i, e) in out.iter().enumerate() {
            if e.fallback && i != last {
                r.warn_edges(
                    playbook,
                    "V123",
                    Some(from),
                    &[e],
                    format!(
                        "fallback edge `{from}` -> `{}` is not the last outgoing edge of `{from}`; a fallback is taken only when no other edge matched, so declare it last",
                        e.to
                    ),
                );
            }
        }
        let mut seen: Vec<usize> = Vec::new();
        for (i, e) in out.iter().enumerate() {
            if seen.contains(&i) || e.max_traversals.is_some() {
                continue;
            }
            let same: Vec<usize> = (i..out.len())
                .filter(|&j| {
                    out[j].max_traversals.is_none()
                        && out[j].to == e.to
                        && out[j].condition == e.condition
                        && out[j].fallback == e.fallback
                })
                .collect();
            if same.len() < 2 {
                continue;
            }
            seen.extend(&same);
            let edges: Vec<&Edge> = same.iter().map(|&j| out[j]).collect();
            r.warn_edges(
                playbook,
                "V124",
                Some(from),
                &edges,
                format!(
                    "{} edges `{from}` -> `{}` have the same condition and fallback flag; only the first is ever taken",
                    edges.len(),
                    e.to
                ),
            );
        }
    }
}

/// Whether `value` is a `decided_by` a judge node can write: `default`,
/// `unavailable`, `<provider>/<model>` or `emulated:<provider>`.
fn is_decided_by(value: &str) -> bool {
    if value == DEFAULT || value == UNAVAILABLE {
        return true;
    }
    if let Some(provider) = value.strip_prefix("emulated:") {
        return !provider.is_empty();
    }
    // Split on the first `/` only: a model id may carry its own
    // (`openrouter/vendor/model-1`).
    value
        .split_once('/')
        .is_some_and(|(provider, model)| !provider.is_empty() && !model.is_empty())
}

/// V125 (warning): an `output_field` edge compares a judge node's question
/// field, or its `decided_by`, with a value the node never writes, so the
/// edge never matches.
pub(crate) fn check_judge_values(playbook: &Playbook, r: &mut ValidationReport) {
    for e in &playbook.edges {
        let Some(EdgeCondition::OutputField {
            node,
            field,
            equals,
        }) = &e.condition
        else {
            continue;
        };
        let Some((_, questions, thresholds, _)) = playbook
            .node(node)
            .and_then(|n| super::judge::judge_parts(&n.kind))
        else {
            continue;
        };
        let expected: Option<String> = if field == DECIDED_BY {
            (!is_decided_by(equals)).then(|| {
                format!(
                    "`{DEFAULT}`, `{UNAVAILABLE}`, `<provider>/<model>` or `emulated:<provider>`"
                )
            })
        } else {
            let Some(q) = questions.get(field) else {
                continue;
            };
            let t = thresholds.get(field);
            let values: Option<Vec<String>> = match q {
                JudgeQuestion::Choice { criteria, .. } => {
                    let mut v: Vec<String> = criteria.keys().map(String::from).collect();
                    if let Some(below) = t.and_then(|t| t.below.as_ref())
                        && !v.contains(below)
                    {
                        v.push(below.clone());
                    }
                    Some(v)
                }
                JudgeQuestion::Noul { .. } => Some(vec!["true".into(), "false".into()]),
                JudgeQuestion::Score { .. } => t
                    .and_then(|t| t.bands.as_ref())
                    .map(|b| b.keys().map(String::from).collect()),
            };
            match values {
                Some(v) if v.iter().any(|x| x == equals) => None,
                Some(v) => Some(
                    v.iter()
                        .map(|x| format!("`{x}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                // A score without bands writes only `<id>_score`.
                None => Some(format!(
                    "nothing: a score question without bands writes only `{field}_score`"
                )),
            }
        };
        if let Some(expected) = expected {
            r.warn_edges(
                playbook,
                "V125",
                Some(&e.from),
                &[e],
                format!(
                    "edge `{}` -> `{}` compares field `{field}` of judge node `{node}` with `{equals}`, a value it never writes (it writes {expected}), so the edge never matches",
                    e.from, e.to
                ),
            );
        }
    }
}

/// Whether edge `e` out of a `human_review` gate can be taken for decision
/// `option`, the fallback rule aside: an edge without a condition always,
/// a `review_status` edge when it names `option`, and an edge with any other
/// condition because it may match. Shared by V136 and the unattended rules.
pub(super) fn review_takes(e: &Edge, option: &str) -> bool {
    match &e.condition {
        None => true,
        Some(EdgeCondition::ReviewStatus { equals }) => equals == option,
        Some(_) => true,
    }
}

/// V136 (warning): a `human_review` option that only bounded edges route.
/// Once their cap is spent the decision has no edge, and the run fails at
/// the review (the person decided, and nothing follows). A fallback edge out
/// of the review, or an unbounded edge that the option can take, is a way
/// out; an unbounded edge with a condition other than `review_status` may
/// match too and counts as one. The usual fix is to put the cap on the edge
/// that leads back into the review, so the review itself always routes.
pub(crate) fn check_review_caps(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        let NodeKind::HumanReview { options, .. } = &node.kind else {
            continue;
        };
        let out: Vec<&Edge> = playbook
            .edges
            .iter()
            .filter(|e| e.from == node.id)
            .collect();
        if out.iter().any(|e| e.fallback) {
            continue;
        }
        for option in &crate::schema::effective_review_options(options) {
            let bounded: Vec<&Edge> = out
                .iter()
                .copied()
                .filter(|e| e.max_traversals.is_some() && review_takes(e, option))
                .collect();
            if bounded.is_empty()
                || out
                    .iter()
                    .any(|e| e.max_traversals.is_none() && review_takes(e, option))
            {
                continue;
            }
            r.warn_edges(
                playbook,
                "V136",
                Some(&node.id),
                &bounded,
                format!(
                    "human_review `{}`: decision `{option}` is routed only by a bounded edge; once its cap is spent the decision has no edge and the run fails at the review. Put the cap on the edge into the review instead, or add a fallback edge",
                    node.id
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(body: &str) -> Playbook {
        let yaml = format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: {{ profile: x }}\n{body}\n"
        );
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn found(p: &Playbook, code: &str) -> Vec<Issue> {
        validate(p, &ValidationContext::default())
            .issues
            .into_iter()
            .filter(|i| i.code == code)
            .collect()
    }

    /// The one issue of `code`, its node and edges.
    fn one(p: &Playbook, code: &str) -> (Option<String>, Vec<String>, Severity) {
        let all = found(p, code);
        assert_eq!(all.len(), 1, "{code}: {all:?}");
        let i = &all[0];
        (i.node.clone(), i.edges.clone(), i.severity)
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    const LINEAR: &str = r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
  - { id: bad, type: finish, outcome: failure }
edges:
  - { from: s, to: a }
"#;

    // --- the mapping of existing rules -------------------------------------

    #[test]
    fn v06_keeps_the_unknown_node_and_names_the_edge() {
        let p = pb(&format!("{LINEAR}  - {{ from: a, to: nowhere }}"));
        let (node, edges, _) = one(&p, "V06");
        assert_eq!(node.as_deref(), Some("nowhere"));
        assert_eq!(edges, s(&["e:a>nowhere#1"]));
    }

    #[test]
    fn v11_names_the_first_cycle_member_and_no_edge() {
        let p = pb(r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: b, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: b }
  - { from: b, to: a, condition: { type: node_status, node: b, equals: failure } }
  - { from: b, to: done, condition: { type: node_status, node: b, equals: success } }"#);
        let (node, edges, _) = one(&p, "V11");
        assert!(matches!(node.as_deref(), Some("a" | "b")), "{node:?}");
        assert!(edges.is_empty());
    }

    #[test]
    fn v30_has_no_node_and_names_the_edge() {
        let p = pb(&format!(
            "{LINEAR}  - {{ id: loop, from: a, to: done, max_traversals: 0 }}"
        ));
        let (node, edges, _) = one(&p, "V30");
        assert_eq!(node, None);
        assert_eq!(edges, s(&["id:loop"]));
    }

    #[test]
    fn v34_shadowing_names_the_unconditional_and_the_shadowed_edges() {
        let p = pb(&format!(
            "{LINEAR}  - {{ from: a, to: done }}\n  - {{ from: a, to: bad, condition: {{ type: node_status, node: a, equals: failure }} }}\n  - {{ from: a, to: bad, fallback: true }}"
        ));
        let all = found(&p, "V34");
        let shadow = all
            .iter()
            .find(|i| i.message.contains("unreachable"))
            .unwrap();
        assert_eq!(shadow.node.as_deref(), Some("a"));
        assert_eq!(shadow.edges, s(&["e:a>done#1", "e:a>bad#1"]));
    }

    #[test]
    fn v34_contradiction_names_the_edges_of_the_group() {
        let p = pb(&format!(
            "{LINEAR}  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: failure }} }}\n  - {{ from: a, to: bad, condition: {{ type: node_status, node: a, equals: failure }} }}\n  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }} }}"
        ));
        let (node, edges, _) = one(&p, "V34");
        assert_eq!(node.as_deref(), Some("a"));
        assert_eq!(edges, s(&["e:a>done#1", "e:a>bad#1"]));
    }

    const DIAMOND: &str = r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: b }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

    #[test]
    fn v36_keeps_the_target_and_names_the_edge() {
        let p = pb(&format!("{DIAMOND}  - {{ from: a, to: j, join: al }}"));
        let (node, edges, _) = one(&p, "V36");
        assert_eq!(node.as_deref(), Some("j"));
        assert_eq!(edges, s(&["e:a>j#1"]));
    }

    #[test]
    fn v79_keeps_the_target_and_names_the_edge() {
        let p = pb(r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { id: only, from: a, to: done, require: all_succeeded }"#);
        let (node, edges, _) = one(&p, "V79");
        assert_eq!(node.as_deref(), Some("done"));
        assert_eq!(edges, s(&["id:only"]));
    }

    #[test]
    fn v46_from_an_edge_names_it_and_from_a_template_names_none() {
        let p = pb(r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, outputs: { fields: [verdict] } }
  - { id: b, type: agent_task, prompt: "{{nodes.a.output.verdikt}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: b, condition: { type: output_field, node: a, field: verdikt, equals: ok } }
  - { from: a, to: done, fallback: true }
  - { from: b, to: done }"#);
        let all = found(&p, "V46");
        assert_eq!(all.len(), 2, "{all:?}");
        let edge = all.iter().find(|i| i.node.as_deref() == Some("a")).unwrap();
        assert_eq!(edge.edges, s(&["e:a>b#1"]));
        let template = all.iter().find(|i| i.node.as_deref() == Some("b")).unwrap();
        assert!(template.edges.is_empty());
    }

    // --- V121 ------------------------------------------------------------

    #[test]
    fn v121_refuses_a_malformed_or_repeated_id() {
        let ok = pb(&format!("{LINEAR}  - {{ id: to-done, from: a, to: done }}"));
        assert!(found(&ok, "V121").is_empty());
        let bad = pb(&format!("{LINEAR}  - {{ id: To_Done, from: a, to: done }}"));
        let (node, edges, sev) = one(&bad, "V121");
        assert_eq!((node.as_deref(), sev), (Some("a"), Severity::Error));
        assert_eq!(edges, s(&["id:To_Done"]));
        // An id with no address form at all: the issue carries none rather
        // than one `EdgeAddress::parse` refuses.
        let empty = pb(&format!("{LINEAR}  - {{ id: \"\", from: a, to: done }}"));
        let (_, edges, _) = one(&empty, "V121");
        assert!(edges.is_empty(), "{edges:?}");
        let twice = pb(&format!(
            "{LINEAR}  - {{ id: x, from: a, to: done, condition: {{ type: node_status, node: a, equals: success }} }}\n  - {{ id: x, from: a, to: bad, condition: {{ type: node_status, node: a, equals: failure }} }}"
        ));
        let (node, edges, _) = one(&twice, "V121");
        assert_eq!(node.as_deref(), Some("a"));
        // Both edges carry the same address; it is named once.
        assert_eq!(edges, s(&["id:x"]));
    }

    // --- V122 ------------------------------------------------------------

    #[test]
    fn v122_warns_when_one_pair_carries_different_caps() {
        let body = |second: &str| {
            pb(&format!(
                "{LINEAR}  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }}, max_traversals: 2 }}\n  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: failure }}{second} }}"
            ))
        };
        assert!(found(&body(", max_traversals: 2"), "V122").is_empty());
        let p = body(", max_traversals: 5");
        let (node, edges, sev) = one(&p, "V122");
        assert_eq!((node.as_deref(), sev), (Some("a"), Severity::Warning));
        assert_eq!(edges, s(&["e:a>done#1", "e:a>done#2"]));
        let msg = &found(&p, "V122")[0].message;
        assert!(msg.contains("shared by the pair"), "{msg}");
        assert!(msg.contains("its own cap"), "{msg}");
    }

    // --- V123 ------------------------------------------------------------

    #[test]
    fn v123_warns_on_a_fallback_that_is_not_last() {
        let last = pb(&format!(
            "{LINEAR}  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }} }}\n  - {{ from: a, to: bad, fallback: true }}"
        ));
        assert!(found(&last, "V123").is_empty());
        let first = pb(&format!(
            "{LINEAR}  - {{ from: a, to: bad, fallback: true }}\n  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }} }}"
        ));
        let (node, edges, sev) = one(&first, "V123");
        assert_eq!((node.as_deref(), sev), (Some("a"), Severity::Warning));
        assert_eq!(edges, s(&["e:a>bad#1"]));
    }

    // --- V124 ------------------------------------------------------------

    #[test]
    fn v124_warns_on_a_repeated_route_and_spares_fan_out() {
        let p = pb(&format!(
            "{LINEAR}  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }} }}\n  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }} }}\n  - {{ from: a, to: bad, fallback: true }}"
        ));
        let (node, edges, sev) = one(&p, "V124");
        assert_eq!((node.as_deref(), sev), (Some("a"), Severity::Warning));
        assert_eq!(edges, s(&["e:a>done#1", "e:a>done#2"]));
        let distinct = pb(&format!(
            "{LINEAR}  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }} }}\n  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: failure }} }}"
        ));
        assert!(found(&distinct, "V124").is_empty());
        let fan_out = pb(r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: a }
  - { from: a, to: done }"#);
        assert!(found(&fan_out, "V124").is_empty());
    }

    #[test]
    fn v124_spares_bounded_duplicates_that_take_turns() {
        let caps = |first: &str, second: &str| {
            pb(&format!(
                "{LINEAR}  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }}{first} }}\n  - {{ from: a, to: done, condition: {{ type: node_status, node: a, equals: success }}{second} }}\n  - {{ from: a, to: bad, fallback: true }}"
            ))
        };
        // Once the first is exhausted the second is taken: not a dead edge.
        let p = caps(", max_traversals: 2", ", max_traversals: 5");
        assert!(found(&p, "V124").is_empty());
        assert_eq!(found(&p, "V122").len(), 1);
        assert!(found(&caps(", max_traversals: 2", ""), "V124").is_empty());
        assert!(found(&caps("", ", max_traversals: 2"), "V124").is_empty());
        assert_eq!(found(&caps("", ""), "V124").len(), 1);
    }

    // --- V125 ------------------------------------------------------------

    const JUDGE: &str = r#"
nodes:
  - { id: s, type: start }
  - id: triage
    type: judge
    state: { out: "x" }
    questions:
      verdict:
        type: choice
        instructions: "Which?"
        criteria: { clean: "c", needs_fix: "n", unclear: "u" }
      risky: { type: noul, instructions: "Risky?" }
      effort:
        type: score
        instructions: "How much?"
        levels: ["none", "some", "much"]
      size:
        type: score
        instructions: "How big?"
        levels: ["small", "big"]
    thresholds:
      verdict: { min_confidence: 0.6, below: unclear }
      effort: { bands: { low: [0, 1], high: [1, 2] } }
    on_unavailable: fail
  - { id: done, type: finish, outcome: success }
  - { id: fix, type: finish, outcome: failure }
edges:
  - { from: s, to: triage }
  - { from: triage, to: done, fallback: true }
"#;

    fn judged(field: &str, value: &str) -> Playbook {
        pb(&format!(
            "{JUDGE}  - {{ from: triage, to: fix, condition: {{ type: output_field, node: triage, field: {field}, equals: \"{value}\" }} }}"
        ))
    }

    #[test]
    fn v125_checks_each_question_kind() {
        for (field, good, bad) in [
            ("verdict", "needs_fix", "broken"),
            ("verdict", "unclear", "other"),
            ("risky", "true", "yes"),
            ("risky", "false", "0"),
            ("effort", "high", "medium"),
        ] {
            assert!(
                found(&judged(field, good), "V125").is_empty(),
                "{field}={good}"
            );
            let p = judged(field, bad);
            let (node, edges, sev) = one(&p, "V125");
            assert_eq!((node.as_deref(), sev), (Some("triage"), Severity::Warning));
            // The fallback edge is declared before this one, so V123 fires
            // too; the edge named is the output_field one.
            assert_eq!(edges, s(&["e:triage>fix#1"]), "{field}={bad}");
        }
        // A score without bands never writes its own field.
        assert_eq!(found(&judged("size", "small"), "V125").len(), 1);
        // Fields the check does not cover stay silent.
        assert!(found(&judged("verdict_p", "x"), "V125").is_empty());
    }

    #[test]
    fn v125_checks_decided_by() {
        for good in [
            "default",
            "unavailable",
            "openai/gpt-x",
            "openrouter/vendor/model-1",
            "a/b/c",
            "emulated:local",
        ] {
            assert!(
                found(&judged("decided_by", good), "V125").is_empty(),
                "{good}"
            );
        }
        for bad in ["model", "/b", "a/", "/", "emulated:", "Default"] {
            assert_eq!(found(&judged("decided_by", bad), "V125").len(), 1, "{bad}");
        }
    }

    #[test]
    fn v53_and_v55_name_the_judge_node_and_no_edge() {
        let none = pb(&JUDGE.replace("    on_unavailable: fail\n", ""));
        let (node, edges, _) = one(&none, "V55");
        assert_eq!(node.as_deref(), Some("triage"));
        assert!(edges.is_empty());
        let unknown = pb(&JUDGE.replace("on_unavailable: fail", "on_unavailable: sometimes"));
        let (node, edges, _) = one(&unknown, "V53");
        assert_eq!(node.as_deref(), Some("triage"));
        assert!(edges.is_empty());
        let route = pb(&JUDGE.replace("on_unavailable: fail", "on_unavailable: { route: fix }"));
        let (node, edges, _) = one(&route, "V53");
        assert_eq!(node.as_deref(), Some("triage"));
        assert!(edges.is_empty());
    }

    #[test]
    fn issues_render_and_view_their_edges() {
        let p = pb(&format!("{LINEAR}  - {{ from: a, to: nowhere }}"));
        let issues = found(&p, "V06");
        let text = render_issues(&issues);
        assert!(
            text.contains("V06 error (node `nowhere`) (edges `e:a>nowhere#1`): "),
            "{text}"
        );
        let view = serde_json::to_value(IssueView::from(&issues[0])).unwrap();
        assert_eq!(view["edges"], serde_json::json!(["e:a>nowhere#1"]));
        assert_eq!(view["node"], "nowhere");
        let no_node = IssueView::from(&Issue::new::<String>(
            "V02",
            Severity::Error,
            None,
            "m".into(),
        ));
        let v = serde_json::to_value(no_node).unwrap();
        assert!(v.get("node").is_none());
        assert_eq!(v["edges"], serde_json::json!([]));
    }
}
