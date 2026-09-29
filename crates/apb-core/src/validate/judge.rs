//! Judge rules (issue #165 Parts 5 and 7): the `judge` node's questions,
//! thresholds, fallback and state, and the `judge` edge condition.
//!
//! Errors are what the engine could not run or would run wrongly; warnings
//! are the authoring advice a decision model needs to answer well (an
//! `unclear` option, levels in words, a small state, a declared fallback).

use super::templates::template_refs;
use super::*;
use crate::judge::{
    DECIDED_BY, ESCAPE_OPTIONS, JudgeFallback, JudgeQuestion, JudgeQuestions, JudgeThreshold,
    MAX_LEVELS, MAX_OPTIONS, MAX_QUESTIONS, MIN_LEVELS, OrderedMap, REASON, UNAVAILABLE,
};

/// The provider cap the questions are checked against: the lowest state
/// budget across the known gateways (32k tokens), at the same conservative
/// three bytes per token the client-side limit check uses.
const PROVIDER_CAP_BYTES: usize = 32_000 * 3;
/// The default `privacy.max_state_bytes` a rendered state is clipped to.
const STATE_BUDGET_BYTES: usize = 24_000;
/// What a reference to the whole run context is assumed to render to.
const CONTEXT_ESTIMATE_BYTES: usize = 24_000;
/// What any other template reference is assumed to render to.
const REF_ESTIMATE_BYTES: usize = 1_000;
/// Judge edges of one node above this many make one oversized request.
const MAX_JUDGE_EDGES: usize = 8;

/// A question id or state field name: `[a-z][a-z0-9_]*`, at most 64 chars.
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

type JudgeParts<'a> = (
    &'a OrderedMap<String>,
    &'a JudgeQuestions,
    &'a OrderedMap<JudgeThreshold>,
    Option<&'a JudgeFallback>,
);

fn judge_parts(kind: &NodeKind) -> Option<JudgeParts<'_>> {
    match kind {
        NodeKind::Judge {
            state,
            questions,
            thresholds,
            on_unavailable,
            ..
        } => Some((state, questions, thresholds, on_unavailable.as_ref())),
        _ => None,
    }
}

/// V50-V58 and V61 on every judge node.
pub(crate) fn check_judge_nodes(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        let Some((state, questions, thresholds, fallback)) = judge_parts(&node.kind) else {
            continue;
        };
        let id = node.id.as_str();
        check_questions(id, questions, r);
        check_field_collisions(id, questions, thresholds, r);
        check_thresholds(id, questions, thresholds, r);
        check_fallback(playbook, id, questions, thresholds, fallback, r);
        check_state(playbook, id, state, questions, r);
        if let NodeKind::Judge {
            profile: Some(_), ..
        } = &node.kind
            && !matches!(fallback, Some(JudgeFallback::Emulate))
        {
            r.warn(
                "V61",
                Some(id),
                "judge `profile` is only used by `on_unavailable: emulate`; it has no effect here"
                    .into(),
            );
        }
    }
}

/// V50: no two questions write the same output field (`risk` and `risk_p`
/// would both set `risk_p`, one answer silently replacing the other).
fn check_field_collisions(
    id: &str,
    questions: &JudgeQuestions,
    thresholds: &crate::judge::OrderedMap<crate::judge::JudgeThreshold>,
    r: &mut ValidationReport,
) {
    let mut owner: std::collections::BTreeMap<String, &str> = Default::default();
    for (qid, q) in questions.iter() {
        let one = crate::judge::JudgeQuestions {
            entries: vec![(qid.to_string(), q.clone())],
            ..Default::default()
        };
        for field in crate::judge::output_fields(&one, thresholds) {
            if field == DECIDED_BY || field == REASON {
                continue;
            }
            if let Some(other) = owner.insert(field.clone(), qid) {
                r.error(
                    "V50",
                    Some(id),
                    format!(
                        "questions `{other}` and `{qid}` both write the output field `{field}`; rename one"
                    ),
                );
            }
        }
    }
}

/// V50: a non-empty map of at most 32 questions with valid ids. V51: each
/// question's own shape. V56, V57: the advice warnings.
fn check_questions(id: &str, questions: &JudgeQuestions, r: &mut ValidationReport) {
    if questions.list_shaped {
        r.error(
            "V50",
            Some(id),
            "judge `questions` must be a map of question id to question, not a list".into(),
        );
    }
    if questions.is_empty() {
        r.error("V50", Some(id), "judge node asks no questions".into());
    }
    if questions.entries.len() > MAX_QUESTIONS {
        r.error(
            "V50",
            Some(id),
            format!(
                "judge node asks {} questions; at most {MAX_QUESTIONS} fit one request",
                questions.entries.len()
            ),
        );
    }
    for (qid, q) in questions.iter() {
        if !valid_name(qid) || qid == DECIDED_BY || qid == REASON {
            r.error(
                "V50",
                Some(id),
                format!(
                    "question id `{qid}` must match [a-z][a-z0-9_]* (at most 64 characters) and must not be `{DECIDED_BY}` or `{REASON}`"
                ),
            );
        }
        if q.instructions().trim().is_empty() {
            r.error(
                "V51",
                Some(id),
                format!("question `{qid}` has no instructions"),
            );
        }
        match q {
            JudgeQuestion::Choice { criteria, .. } => {
                if !(2..=MAX_OPTIONS).contains(&criteria.len()) {
                    r.error(
                        "V51",
                        Some(id),
                        format!(
                            "choice question `{qid}` needs 2 to {MAX_OPTIONS} criteria (options), got {}",
                            criteria.len()
                        ),
                    );
                }
                if !criteria.is_empty() && !criteria.keys().any(|k| ESCAPE_OPTIONS.contains(&k)) {
                    r.warn(
                        "V56",
                        Some(id),
                        format!(
                            "choice question `{qid}` has no `unclear`, `other` or `none` option: without an honest way out the model must force a pick on material it cannot judge"
                        ),
                    );
                }
            }
            JudgeQuestion::Score { levels, .. } => {
                if !(MIN_LEVELS..=MAX_LEVELS).contains(&levels.len()) {
                    r.error(
                        "V51",
                        Some(id),
                        format!(
                            "score question `{qid}` needs {MIN_LEVELS} to {MAX_LEVELS} levels, got {}",
                            levels.len()
                        ),
                    );
                }
                if levels.iter().any(|l| l.chars().any(|c| c.is_ascii_digit())) {
                    r.warn(
                        "V57",
                        Some(id),
                        format!(
                            "score question `{qid}` has digits in its level labels; the model reads the words, never the numbers, so describe each level as a situation"
                        ),
                    );
                }
            }
            JudgeQuestion::Noul { criteria, .. } => {
                if let Some(c) = criteria
                    && let Some(bad) = c.keys().find(|k| *k != "true" && *k != "false")
                {
                    r.error(
                        "V51",
                        Some(id),
                        format!(
                            "noul question `{qid}` criteria may only name `true` and `false`, not `{bad}`"
                        ),
                    );
                }
            }
        }
    }
}

/// The option names of a `choice`, the level count of a `score`.
fn options(q: &JudgeQuestion) -> Vec<&str> {
    match q {
        JudgeQuestion::Choice { criteria, .. } => criteria.keys().collect(),
        _ => Vec::new(),
    }
}

/// V52: every threshold names a real question and uses only the keys its
/// type takes, with values in range.
fn check_thresholds(
    id: &str,
    questions: &JudgeQuestions,
    thresholds: &OrderedMap<JudgeThreshold>,
    r: &mut ValidationReport,
) {
    let mut err = |msg: String| r.error("V52", Some(id), msg);
    for (qid, t) in thresholds.iter() {
        let Some(q) = questions.get(qid) else {
            err(format!("thresholds name unknown question `{qid}`"));
            continue;
        };
        let choice_keys = t.min_confidence.is_some() || t.below.is_some();
        match q {
            JudgeQuestion::Choice { .. } => {
                if t.yes_at.is_some() || t.bands.is_some() {
                    err(format!(
                        "threshold of choice question `{qid}` takes only min_confidence and below"
                    ));
                }
                match (&t.min_confidence, &t.below) {
                    (Some(c), Some(below)) => {
                        if !(0.0..=1.0).contains(c) {
                            err(format!(
                                "min_confidence of `{qid}` must be between 0 and 1, got {c}"
                            ));
                        }
                        if !options(q).contains(&below.as_str()) {
                            err(format!(
                                "below `{below}` of `{qid}` is not one of its options [{}]",
                                options(q).join(", ")
                            ));
                        }
                    }
                    (Some(_), None) => err(format!(
                        "min_confidence of `{qid}` needs `below`, the safe option taken under it"
                    )),
                    (None, Some(_)) => err(format!(
                        "below of `{qid}` needs `min_confidence`, the confidence under which it applies"
                    )),
                    (None, None) => err(format!("threshold of `{qid}` sets nothing")),
                }
            }
            JudgeQuestion::Noul { .. } => {
                if choice_keys || t.bands.is_some() {
                    err(format!(
                        "threshold of noul question `{qid}` takes only yes_at"
                    ));
                }
                match t.yes_at {
                    Some(p) if p > 0.0 && p < 1.0 => {}
                    Some(p) => err(format!(
                        "yes_at of `{qid}` must be strictly between 0 and 1, got {p}"
                    )),
                    None => err(format!("threshold of `{qid}` sets nothing")),
                }
            }
            JudgeQuestion::Score { levels, .. } => {
                if choice_keys || t.yes_at.is_some() {
                    err(format!(
                        "threshold of score question `{qid}` takes only bands"
                    ));
                }
                let Some(bands) = &t.bands else {
                    err(format!("threshold of `{qid}` sets nothing"));
                    continue;
                };
                let top = levels.len().saturating_sub(1) as f64;
                let mut ranges: Vec<(f64, f64, &str)> = Vec::new();
                for (name, [lo, hi]) in bands.iter() {
                    if name.trim().is_empty() {
                        err(format!("a band of `{qid}` has an empty name"));
                    }
                    if lo.partial_cmp(hi) != Some(std::cmp::Ordering::Less)
                        || *lo < 0.0
                        || *hi > top
                    {
                        err(format!(
                            "band `{name}` of `{qid}` must be [lo, hi] with 0 <= lo < hi <= {top} (level indexes, lowest 0)"
                        ));
                    }
                    ranges.push((*lo, *hi, name));
                }
                if bands.is_empty() {
                    err(format!("bands of `{qid}` are empty"));
                }
                ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
                for w in ranges.windows(2) {
                    if w[1].0 < w[0].1 {
                        err(format!(
                            "bands `{}` and `{}` of `{qid}` overlap",
                            w[0].2, w[1].2
                        ));
                    }
                }
            }
        }
    }
}

/// Whether `value` is a declared output value of question `q`.
fn default_fits(q: &JudgeQuestion, t: Option<&JudgeThreshold>, value: &serde_json::Value) -> bool {
    match q {
        JudgeQuestion::Choice { .. } => value.as_str().is_some_and(|v| options(q).contains(&v)),
        JudgeQuestion::Noul { .. } => value.is_boolean(),
        JudgeQuestion::Score { levels, .. } => match t.and_then(|t| t.bands.as_ref()) {
            Some(bands) => value.as_str().is_some_and(|v| bands.get(v).is_some()),
            None => value
                .as_f64()
                .is_some_and(|x| (0.0..=(levels.len().saturating_sub(1)) as f64).contains(&x)),
        },
    }
}

/// V53: `on_unavailable` names a known form; a route has a target and the
/// `output_field` edge that takes it; defaults fit their questions. V55:
/// the warning when it is absent.
fn check_fallback(
    playbook: &Playbook,
    id: &str,
    questions: &JudgeQuestions,
    thresholds: &OrderedMap<JudgeThreshold>,
    fallback: Option<&JudgeFallback>,
    r: &mut ValidationReport,
) {
    match fallback {
        None => r.warn(
            "V55",
            Some(id),
            "judge node declares no on_unavailable, so it fails whenever no usable answer comes back (for example on a machine without decisions.yaml); declare route, default, fail or emulate".into(),
        ),
        Some(JudgeFallback::Unknown(word)) => r.error(
            "V53",
            Some(id),
            format!(
                "on_unavailable `{word}` is not one of fail, emulate, {{ route: <node> }}, {{ default: {{ ... }} }}"
            ),
        ),
        Some(JudgeFallback::Fail) | Some(JudgeFallback::Emulate) => {}
        Some(JudgeFallback::Route(target)) => {
            if target == id || playbook.node(target).is_none() {
                r.error(
                    "V53",
                    Some(id),
                    format!("on_unavailable routes to unknown node `{target}`"),
                );
                return;
            }
            let routed = playbook.edges.iter().any(|e| {
                e.from == id
                    && e.to == *target
                    && !e.fallback
                    && matches!(
                        &e.condition,
                        Some(EdgeCondition::OutputField { node, field, equals })
                            if node == id && field == DECIDED_BY && equals == UNAVAILABLE
                    )
            });
            if !routed {
                r.error(
                    "V53",
                    Some(id),
                    format!(
                        "on_unavailable routes to `{target}`, so an edge `{id}` -> `{target}` with condition {{ type: output_field, node: {id}, field: {DECIDED_BY}, equals: {UNAVAILABLE} }} must take that route"
                    ),
                );
            }
        }
        Some(JudgeFallback::Default(values)) => {
            if values.is_empty() {
                r.error(
                    "V53",
                    Some(id),
                    "on_unavailable default declares no values".into(),
                );
            }
            for (qid, value) in values {
                match questions.get(qid) {
                    None => r.error(
                        "V53",
                        Some(id),
                        format!("on_unavailable default names unknown question `{qid}`"),
                    ),
                    Some(q) if !default_fits(q, thresholds.get(qid), value) => r.error(
                        "V53",
                        Some(id),
                        format!(
                            "on_unavailable default `{qid}: {value}` is not a value question `{qid}` can have"
                        ),
                    ),
                    Some(_) => {}
                }
            }
        }
    }
}

/// V54: the state names at least one valid field, and no question alone
/// exceeds the provider cap (questions are never clipped). V58: the rendered
/// state is likely to exceed the default state budget and be clipped.
fn check_state(
    playbook: &Playbook,
    id: &str,
    state: &OrderedMap<String>,
    questions: &JudgeQuestions,
    r: &mut ValidationReport,
) {
    if state.is_empty() {
        r.error(
            "V54",
            Some(id),
            "judge node has no state: name at least one field the questions can cite".into(),
        );
    }
    for (name, _) in state.iter() {
        if !valid_name(name) || name == "meta" {
            r.error(
                "V54",
                Some(id),
                format!("state field `{name}` must match [a-z][a-z0-9_]* (at most 64 characters) and must not be `meta`"),
            );
        }
    }
    for (qid, q) in questions.iter() {
        let size = serde_json::to_string(q).map_or(0, |s| s.len());
        if size > PROVIDER_CAP_BYTES / 2 {
            r.error(
                "V54",
                Some(id),
                format!(
                    "question `{qid}` is about {size} bytes; with the state it would not fit the provider limit"
                ),
            );
        }
    }
    let estimate: usize = state
        .iter()
        .map(|(_, text)| {
            let refs = template_refs(text);
            let literal = refs
                .iter()
                .fold(text.len(), |n, r| n.saturating_sub(r.len() + 4));
            literal
                + refs
                    .iter()
                    .map(|r| match r.as_str() {
                        "run.context" => CONTEXT_ESTIMATE_BYTES,
                        _ => REF_ESTIMATE_BYTES,
                    })
                    .sum::<usize>()
        })
        .sum();
    if estimate > STATE_BUDGET_BYTES {
        r.warn(
            "V58",
            Some(id),
            format!(
                "judge state renders to about {estimate} bytes, over the default {STATE_BUDGET_BYTES}-byte state budget, so it will be clipped; cite small named values (one field of a node output) instead of whole transcripts"
            ),
        );
    }
    let _ = playbook;
}

/// V59 (error): a judge edge needs a question, a `min_p` strictly between 0
/// and 1 and an explicit `on_unavailable`. V60 (warning): more than eight
/// judge edges on one node.
pub(crate) fn check_judge_edges(playbook: &Playbook, r: &mut ValidationReport) {
    let mut per_node: HashMap<&str, usize> = HashMap::new();
    for e in &playbook.edges {
        let Some(EdgeCondition::Judge {
            question,
            min_p,
            on_unavailable,
        }) = &e.condition
        else {
            continue;
        };
        *per_node.entry(e.from.as_str()).or_default() += 1;
        let at = format!("judge edge `{}` -> `{}`", e.from, e.to);
        if question.trim().is_empty() {
            r.error("V59", Some(&e.from), format!("{at} has an empty question"));
        }
        if !(min_p.0 > 0.0 && min_p.0 < 1.0) {
            r.error(
                "V59",
                Some(&e.from),
                format!(
                    "{at} has min_p {}, which must be strictly between 0 and 1",
                    min_p.0
                ),
            );
        }
        if on_unavailable.is_none() {
            r.error(
                "V59",
                Some(&e.from),
                format!(
                    "{at} must declare on_unavailable: true or false, the route taken when no answer comes back"
                ),
            );
        }
    }
    let mut crowded: Vec<(&str, usize)> = per_node
        .into_iter()
        .filter(|(_, n)| *n > MAX_JUDGE_EDGES)
        .collect();
    crowded.sort_unstable();
    for (node, n) in crowded {
        r.warn(
            "V60",
            Some(node),
            format!(
                "node `{node}` has {n} judge edges; they go in one request, so above {MAX_JUDGE_EDGES} consider a judge node with a choice question"
            ),
        );
    }
}

/// The output fields a judge node publishes, for the V46 check of edges and
/// templates that read it: its declared `outputs.fields` when it has any,
/// else the fields its questions produce.
pub(crate) fn judge_output_fields(node: &crate::schema::Node) -> Option<Vec<String>> {
    let (_, questions, thresholds, _) = judge_parts(&node.kind)?;
    Some(crate::judge::output_fields(questions, thresholds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(judge: &str, extra_nodes: &str, edges: &str) -> Playbook {
        let yaml = format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: {{ profile: x }}\nnodes:\n  - {{ id: start, type: start }}\n  - id: review\n    type: agent_task\n    prompt: review it\n  - id: triage\n    type: judge\n{judge}{extra_nodes}  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: fix, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: review }}\n  - {{ from: review, to: triage }}\n{edges}"
        );
        Playbook::from_yaml(&yaml).unwrap()
    }

    const GOOD: &str = r#"    state:
      request: "{{run.instruction}}"
      review_output: "{{nodes.review.output}}"
    questions:
      verdict:
        type: choice
        instructions: "Which outcome does `review_output` report?"
        criteria:
          clean: "No blocking findings."
          needs_fix: "At least one defect."
          unclear: "Cut off or empty."
      risky: { type: noul, instructions: "Does `review_output` mention deleted tests?" }
      effort:
        type: score
        instructions: "How much work do the findings imply?"
        levels: ["nothing to do", "a one-line fix", "a local change", "a redesign"]
    thresholds:
      verdict: { min_confidence: 0.6, below: unclear }
      risky: { yes_at: 0.7 }
      effort: { bands: { small: [0, 1.5], large: [1.5, 3] } }
    on_unavailable: { default: { verdict: unclear, risky: true } }
"#;

    const EDGES: &str = "  - { from: triage, to: fix, condition: { type: output_field, node: triage, field: verdict, equals: needs_fix } }\n  - { from: triage, to: done, fallback: true }\n";

    fn issues(p: &Playbook) -> Vec<(&'static str, Severity, String)> {
        validate(p, &ValidationContext::default())
            .issues
            .into_iter()
            .map(|i| (i.code, i.severity, i.message))
            .collect()
    }

    fn has(p: &Playbook, code: &str, severity: Severity) -> bool {
        issues(p)
            .iter()
            .any(|(c, s, _)| *c == code && *s == severity)
    }

    fn judge_codes(p: &Playbook) -> Vec<&'static str> {
        issues(p)
            .into_iter()
            .map(|(c, _, _)| c)
            .filter(|c| c[1..].parse::<u32>().is_ok_and(|n| (50..=61).contains(&n)))
            .collect()
    }

    #[test]
    fn a_complete_judge_node_validates_clean() {
        let p = pb(GOOD, "", EDGES);
        assert!(judge_codes(&p).is_empty(), "{:?}", issues(&p));
        assert!(validate(&p, &ValidationContext::default()).is_valid());
    }

    #[test]
    fn list_shaped_questions_are_refused() {
        let body = GOOD.replace(
            "    questions:\n      verdict:\n        type: choice",
            "    questions:\n      - id: verdict\n        type: choice",
        );
        let body = body
            .replace(
                "      risky: { type: noul",
                "      - { id: risky, type: noul",
            )
            .replace(
                "      effort:\n        type: score",
                "      - id: effort\n        type: score",
            );
        let p = pb(&body, "", EDGES);
        let found = issues(&p);
        assert!(
            found
                .iter()
                .any(|(c, s, m)| *c == "V50" && *s == Severity::Error && m.contains("not a list")),
            "{found:?}"
        );
    }

    #[test]
    fn a_score_needs_two_levels_and_a_choice_two_options() {
        let p = pb(
            &GOOD.replace(
                r#"levels: ["nothing to do", "a one-line fix", "a local change", "a redesign"]"#,
                r#"levels: ["only one"]"#,
            )
            .replace("      effort: { bands: { small: [0, 1.5], large: [1.5, 3] } }\n", ""),
            "",
            EDGES,
        );
        assert!(has(&p, "V51", Severity::Error), "{:?}", issues(&p));
        let p = pb(
            &GOOD.replace(
                "          needs_fix: \"At least one defect.\"\n          unclear: \"Cut off or empty.\"\n",
                "",
            )
            .replace("{ min_confidence: 0.6, below: unclear }", "{ min_confidence: 0.6, below: clean }")
            .replace("verdict: unclear, ", ""),
            "",
            EDGES,
        );
        assert!(has(&p, "V51", Severity::Error), "{:?}", issues(&p));
    }

    #[test]
    fn thresholds_must_name_real_questions_and_options() {
        for (from, to) in [
            ("below: unclear }", "below: maybe }"),
            (
                "      risky: { yes_at: 0.7 }",
                "      riskyy: { yes_at: 0.7 }",
            ),
            ("{ yes_at: 0.7 }", "{ yes_at: 1.0 }"),
            (
                "{ min_confidence: 0.6, below: unclear }",
                "{ min_confidence: 0.6 }",
            ),
            ("large: [1.5, 3]", "large: [1.0, 3]"),
            ("large: [1.5, 3]", "large: [1.5, 7]"),
        ] {
            let p = pb(&GOOD.replace(from, to), "", EDGES);
            assert!(has(&p, "V52", Severity::Error), "{to}: {:?}", issues(&p));
        }
    }

    #[test]
    fn fallbacks_are_checked_and_their_absence_is_a_warning() {
        let without = GOOD.replace(
            "    on_unavailable: { default: { verdict: unclear, risky: true } }\n",
            "",
        );
        let p = pb(&without, "", EDGES);
        assert!(has(&p, "V55", Severity::Warning));
        assert!(validate(&p, &ValidationContext::default()).is_valid());

        for bad in [
            "{ default: { verdict: maybe } }",
            "{ default: { risky: 0.3 } }",
            "{ default: { nothing: true } }",
            "{ route: nowhere }",
            "sometimes",
        ] {
            let p = pb(
                &GOOD.replace("{ default: { verdict: unclear, risky: true } }", bad),
                "",
                EDGES,
            );
            assert!(has(&p, "V53", Severity::Error), "{bad}: {:?}", issues(&p));
        }

        // A route needs the output_field edge on decided_by that takes it.
        let route = GOOD.replace(
            "{ default: { verdict: unclear, risky: true } }",
            "{ route: human }",
        );
        let human = "  - { id: human, type: human_review }\n";
        let p = pb(
            &route,
            human,
            &format!("{EDGES}  - {{ from: human, to: done }}\n"),
        );
        assert!(has(&p, "V53", Severity::Error), "{:?}", issues(&p));
        let edge = "  - { from: triage, to: human, condition: { type: output_field, node: triage, field: decided_by, equals: unavailable } }\n";
        let p = pb(
            &route,
            human,
            &format!("{edge}{EDGES}  - {{ from: human, to: done }}\n"),
        );
        assert!(judge_codes(&p).is_empty(), "{:?}", issues(&p));
    }

    #[test]
    fn emulate_needs_a_profile() {
        let yaml = |defaults: &str| {
            format!(
                "schema: 2\nid: p\nname: p\nversion: 1.0.0\n{defaults}nodes:\n  - {{ id: start, type: start }}\n  - id: triage\n    type: judge\n    state: {{ s: x }}\n    questions: {{ q: {{ type: noul, instructions: i }} }}\n    on_unavailable: emulate\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: triage }}\n  - {{ from: triage, to: done }}\n"
            )
        };
        let p = Playbook::from_yaml(&yaml("")).unwrap();
        assert!(has(&p, "V18", Severity::Error), "{:?}", issues(&p));
        let p = Playbook::from_yaml(&yaml("defaults: { profile: light }\n")).unwrap();
        assert!(validate(&p, &ValidationContext::default()).is_valid());
    }

    #[test]
    fn advice_warnings_fire_on_the_shapes_they_describe() {
        let p = pb(
            &GOOD
                .replace("          unclear: \"Cut off or empty.\"\n", "")
                .replace("below: unclear", "below: clean")
                .replace("verdict: unclear", "verdict: clean")
                .replace("\"a one-line fix\"", "\"level 1\""),
            "",
            EDGES,
        );
        assert!(has(&p, "V56", Severity::Warning), "{:?}", issues(&p));
        assert!(has(&p, "V57", Severity::Warning), "{:?}", issues(&p));
        let p = pb(
            &GOOD.replace(
                "\"{{run.instruction}}\"",
                "\"{{run.context}} {{run.context}}\"",
            ),
            "",
            EDGES,
        );
        assert!(has(&p, "V58", Severity::Warning), "{:?}", issues(&p));
    }

    #[test]
    fn judge_edges_need_a_question_a_probability_and_a_fallback() {
        let edges = |cond: &str| {
            format!(
                "  - {{ from: triage, to: fix, condition: {cond} }}\n  - {{ from: triage, to: done, fallback: true }}\n"
            )
        };
        let ok = edges(
            "{ type: judge, question: \"Is it broken?\", min_p: 0.7, on_unavailable: false }",
        );
        assert!(judge_codes(&pb(GOOD, "", &ok)).is_empty());
        for bad in [
            "{ type: judge, question: \"Is it broken?\", min_p: 0.7 }",
            "{ type: judge, question: \"Is it broken?\", min_p: 1.5, on_unavailable: true }",
            "{ type: judge, question: \" \", min_p: 0.5, on_unavailable: true }",
        ] {
            let p = pb(GOOD, "", &edges(bad));
            assert!(has(&p, "V59", Severity::Error), "{bad}: {:?}", issues(&p));
        }
    }

    #[test]
    fn an_edge_reading_a_field_the_judge_never_writes_is_a_v46_warning() {
        let p = pb(GOOD, "", &EDGES.replace("field: verdict", "field: verdikt"));
        assert!(has(&p, "V46", Severity::Warning), "{:?}", issues(&p));
        assert!(!has(&pb(GOOD, "", EDGES), "V46", Severity::Warning));
    }

    #[test]
    fn the_repository_judge_playbooks_validate_clean() {
        for yaml in [
            include_str!("../../../../examples/playbooks/review-triage.yaml"),
            include_str!("../../../../.apb/playbooks/apb-task-implement/1.15.0/playbook.yaml"),
            include_str!("../../../../.apb/playbooks/apb-task-implement/1.16.0/playbook.yaml"),
            include_str!("../../../../.apb/playbooks/apb-task-implement/1.17.0/playbook.yaml"),
        ] {
            let p = Playbook::from_yaml(yaml).unwrap();
            assert!(judge_codes(&p).is_empty(), "{:?}", issues(&p));
            let ctx = ValidationContext {
                profiles: vec!["developer".into(), "janitor".into()],
                ..Default::default()
            };
            assert!(validate(&p, &ctx).is_valid(), "{:?}", issues(&p));
        }
    }

    /// 0.24.0: the current implement playbook pushes a branch, opens a PR
    /// (`create_pull`), pushes review fixes and deletes the merged branch on
    /// origin, so those steps declare `irreversible` and a run of it needs
    /// the person's consent at start.
    #[test]
    fn the_repository_implement_playbook_declares_its_irreversible_steps() {
        let p = Playbook::from_yaml(include_str!(
            "../../../../.apb/playbooks/apb-task-implement/1.17.0/playbook.yaml"
        ))
        .unwrap();
        let declared: Vec<&str> = p
            .nodes
            .iter()
            .filter(|n| n.effects.contains(&crate::schema::Effect::Irreversible))
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(declared, ["pr", "post_pr", "finalize"]);
        assert!(crate::effects::effective(&p).contains(&crate::schema::Effect::Irreversible));
        assert_eq!(
            include_str!("../../../../.apb/playbooks/apb-task-implement/current").trim(),
            "1.17.0"
        );
    }

    #[test]
    fn two_questions_writing_the_same_field_are_refused() {
        let body = GOOD.replace(
            "      risky: { type: noul,",
            "      risky_p: { type: noul, instructions: \"Is it?\" }\n      risky: { type: noul,",
        );
        let p = pb(&body, "", EDGES);
        assert!(
            issues(&p)
                .iter()
                .any(|(c, s, m)| *c == "V50" && *s == Severity::Error && m.contains("`risky_p`")),
            "{:?}",
            issues(&p)
        );
    }

    #[test]
    fn an_empty_state_or_a_bad_state_name_is_v54() {
        let empty = GOOD.replace(
            "    state:\n      request: \"{{run.instruction}}\"\n      review_output: \"{{nodes.review.output}}\"\n",
            "    state: {}\n",
        );
        assert!(has(&pb(&empty, "", EDGES), "V54", Severity::Error));
        let meta = GOOD.replace("      request:", "      meta:");
        assert!(has(&pb(&meta, "", EDGES), "V54", Severity::Error));
        let huge = GOOD.replace(
            "Does `review_output` mention deleted tests?",
            &"x".repeat(PROVIDER_CAP_BYTES / 2 + 10),
        );
        assert!(has(&pb(&huge, "", EDGES), "V54", Severity::Error));
    }

    #[test]
    fn a_profile_without_emulate_is_v61() {
        let body = GOOD.replace("    questions:\n", "    profile: x\n    questions:\n");
        assert!(has(&pb(&body, "", EDGES), "V61", Severity::Warning));
        let emulate = body.replace(
            "    on_unavailable: { default: { verdict: unclear, risky: true } }\n",
            "    on_unavailable: emulate\n",
        );
        assert!(!has(&pb(&emulate, "", EDGES), "V61", Severity::Warning));
    }

    #[test]
    fn more_than_eight_judge_edges_on_one_node_is_v60() {
        let mut edges = String::from(EDGES);
        for i in 0..=MAX_JUDGE_EDGES {
            edges.push_str(&format!(
                "  - {{ from: review, to: done, condition: {{ type: judge, question: \"Is it {i}?\", min_p: 0.5 }} }}\n"
            ));
        }
        assert!(has(&pb(GOOD, "", &edges), "V60", Severity::Warning));
    }
}
