//! `route_evaluated`: what each outgoing edge of a one-of routing decision,
//! a dead end or a `max_loops` exit evaluated to, written from the very
//! evaluation that decided (never a second evaluation). Fan-out decisions
//! write nothing.

use std::path::Path;

use apb_core::edge_address::edge_addresses;
use apb_core::schema::{EdgeCondition, Playbook, output_field_value};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::event::EventLog;
use crate::parallel::{CheckResult, RouteEvaluation, RouteMode};
use crate::state::RunState;

use super::writer::{Record, Scrub, write_with};

/// The record kind.
pub const KIND: &str = "route_evaluated";

/// The longest observed text a check keeps, after scrubbing.
pub const OBSERVED_MAX: usize = 64;

/// One edge of a routing decision.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteCheck {
    /// The edge's rendered address.
    pub edge: String,
    pub rank: usize,
    /// `unconditional`, `fallback`, or the condition's type.
    pub kind: String,
    /// `matched`, `not_matched`, `exhausted`, `not_reached`,
    /// `judge_unavailable` or `shadowed`.
    pub result: String,
    /// The smallest fact that explains the result: a status, a review
    /// decision, an output field value (scrubbed, then clipped), a judge
    /// edge's `p` and `applied`. Absent for an edge that was not checked
    /// and for `output_match`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub observed: Option<Value>,
}

/// A `route_evaluated` record as the reader returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteEvaluated {
    pub after_seq: u64,
    pub node: String,
    /// The source's execution (its finish count) this decision routed.
    pub execution: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// `one_of`, `dead_end` or `loop_exhausted`.
    pub mode: String,
    pub checks: Vec<RouteCheck>,
    /// The addresses of the edges taken.
    pub chosen: Vec<String>,
}

fn result_name(r: CheckResult) -> &'static str {
    match r {
        CheckResult::Matched => "matched",
        CheckResult::NotMatched => "not_matched",
        CheckResult::Exhausted => "exhausted",
        CheckResult::NotReached => "not_reached",
        CheckResult::JudgeUnavailable => "judge_unavailable",
        CheckResult::Shadowed => "shadowed",
    }
}

fn kind_name(edge: &apb_core::schema::Edge) -> &'static str {
    if edge.fallback {
        return "fallback";
    }
    match &edge.condition {
        None => "unconditional",
        Some(EdgeCondition::NodeStatus { .. }) => "node_status",
        Some(EdgeCondition::ReviewStatus { .. }) => "review_status",
        Some(EdgeCondition::OutputMatch { .. }) => "output_match",
        Some(EdgeCondition::OutputField { .. }) => "output_field",
        Some(EdgeCondition::Judge { .. }) => "judge",
        Some(EdgeCondition::NoNewItems { .. }) => "no_new_items",
    }
}

/// The fact behind a checked condition.
fn observed(
    edge: &apb_core::schema::Edge,
    rank: usize,
    from: &str,
    state: &RunState,
    scrub: &Scrub<'_>,
) -> Option<Value> {
    match edge.condition.as_ref()? {
        EdgeCondition::NodeStatus { node, .. } => {
            Some(Value::from(state.current_status(node).as_str()))
        }
        EdgeCondition::ReviewStatus { .. } => state
            .reviews
            .get(from)
            .map(|r| Value::from(scrub.clip(&r.decision, OBSERVED_MAX))),
        EdgeCondition::OutputMatch { .. } => None,
        cond @ EdgeCondition::OutputField { node, field, .. } => {
            let o = state.outputs.get(node)?;
            // An `equals` keeps today's clipped text; a threshold names the
            // text it read and the number it made of it (either may be null).
            if matches!(
                cond.field_compare(),
                Some(apb_core::schema::FieldCompare::Equals(_))
            ) {
                output_field_value(o, field).map(|v| Value::from(scrub.clip(&v, OBSERVED_MAX)))
            } else {
                // The number is read from the raw value, the way routing
                // reads it, never from the clipped text; and it is left out
                // when the scrubber touched the text, so a numeric secret
                // never leaves as its re-serialized number (`123456.000`
                // as `123456.0`).
                let raw = output_field_value(o, field);
                let value = raw.as_deref().map(|v| scrub.clip(v, OBSERVED_MAX));
                let number = apb_core::schema::output_field_number(o, field)
                    .filter(|_| !raw.as_deref().is_some_and(|v| scrub.alters(v)));
                Some(serde_json::json!({ "value": value, "number": number }))
            }
        }
        EdgeCondition::Judge { .. } => {
            let answers = state.judge_edges.get(from)?;
            if state.finished_counts.get(from) != Some(&answers.execution) {
                return None;
            }
            let p = answers.p.get(&format!("edge_{rank}"))?;
            Some(serde_json::json!({ "p": p, "applied": answers.applied }))
        }
        // The loop fold itself (spec loops-and-graphs D2): what the node's
        // successful visits read out, whatever the edge decided from.
        EdgeCondition::NoNewItems {
            node, field, key, ..
        } => {
            let items = apb_core::loop_items::evaluate(
                state
                    .success_outputs
                    .get(node)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                field,
                key.as_deref(),
            );
            Some(serde_json::json!({
                "visits": items.visits,
                "streak": items.streak,
                "new_in_last": items.new_in_last,
            }))
        }
    }
}

fn mode_name(mode: RouteMode) -> Option<&'static str> {
    match mode {
        RouteMode::OneOf => Some("one_of"),
        RouteMode::DeadEnd => Some("dead_end"),
        RouteMode::FanOut | RouteMode::Suppressed => None,
    }
}

/// Writes the record of a routing decision `advance_frontier` just made out
/// of `node`, from the evaluation that decided. A fan-out (or a route a fork
/// policy suppressed) writes nothing.
pub(crate) fn record_decision(
    log: &EventLog,
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    evaluation: &RouteEvaluation,
) {
    let Some(mode) = mode_name(evaluation.mode) else {
        return;
    };
    let Some(run_dir) = log.run_dir() else {
        return;
    };
    let after_seq = log.peek_next_seq().saturating_sub(1);
    let execution = state.finished_counts.get(node).copied().unwrap_or(0);
    write_with(&run_dir, |scrub| {
        let addresses = edge_addresses(playbook);
        let checks = evaluation
            .checks
            .iter()
            .map(|c| {
                let edge = &playbook.edges[c.index];
                let checked = matches!(
                    c.result,
                    CheckResult::Matched | CheckResult::NotMatched | CheckResult::Shadowed
                );
                RouteCheck {
                    edge: addresses[c.index].rendered.clone(),
                    rank: c.rank,
                    kind: kind_name(edge).into(),
                    result: result_name(c.result).into(),
                    observed: checked
                        .then(|| observed(edge, c.rank, node, state, scrub))
                        .flatten(),
                }
            })
            .collect();
        let chosen = evaluation
            .chosen
            .iter()
            .map(|i| addresses[*i].rendered.clone())
            .collect();
        record(node, execution, mode, after_seq, checks, chosen)
    });
}

/// Writes the record of a `max_loops` exit: the condition node at its cap
/// takes its first fallback edge (`fallback`, an index into the playbook's
/// edges) without consulting any other edge.
pub(crate) fn record_loop_exit(log: &EventLog, playbook: &Playbook, node: &str, fallback: usize) {
    let Some(run_dir) = log.run_dir() else {
        return;
    };
    let after_seq = log.peek_next_seq().saturating_sub(1);
    write_with(&run_dir, |_| {
        // Read only when the writer is on: the journal read is the record's.
        let execution = execution_of(&run_dir, node);
        let addresses = edge_addresses(playbook);
        let checks = addresses
            .iter()
            .filter(|a| playbook.edges[a.index].from == node)
            .map(|a| RouteCheck {
                edge: a.rendered.clone(),
                rank: a.rank,
                kind: kind_name(&playbook.edges[a.index]).into(),
                result: match a.index == fallback {
                    true => "matched",
                    false => "not_reached",
                }
                .into(),
                observed: None,
            })
            .collect();
        let chosen = vec![addresses[fallback].rendered.clone()];
        record(node, execution, "loop_exhausted", after_seq, checks, chosen)
    });
}

/// The finish count of `node` in the run's journal (0 when it cannot be read).
fn execution_of(run_dir: &Path, node: &str) -> u32 {
    crate::event::read_all(run_dir)
        .map(|events| crate::scheduler::node_finished_count(&events, node) as u32)
        .unwrap_or(0)
}

fn record(
    node: &str,
    execution: u32,
    mode: &str,
    after_seq: u64,
    checks: Vec<RouteCheck>,
    chosen: Vec<String>,
) -> Option<Record> {
    let body = RouteEvaluated {
        after_seq,
        node: node.to_string(),
        execution,
        item: None,
        mode: mode.to_string(),
        checks,
        chosen,
    };
    let Value::Object(mut fields) = serde_json::to_value(&body).ok()? else {
        return None;
    };
    fields.remove("after_seq");
    Some(Record {
        kind: KIND,
        after_seq,
        key: format!("{node}|{execution}"),
        fields: fields.into_iter().collect::<Map<String, Value>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::evaluate_live_route;
    use crate::state::{JudgeEdgeAnswers, ReviewDecision};

    const JUDGED: &str = "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: s, type: prompt, prompt: s }\n  - { id: a, type: finish, outcome: success }\n  - { id: z, type: finish, outcome: success }\nedges:\n  - { from: start, to: s }\n  - { from: s, to: a, condition: { type: judge, question: q, min_p: 0.7, on_unavailable: false } }\n  - { from: s, to: z, fallback: true, max_traversals: 1 }\n";

    fn state(answers: Option<bool>) -> RunState {
        let mut s = RunState::default();
        s.nodes
            .insert("s".into(), crate::state::NodeStatus::Succeeded);
        s.finished_counts.insert("s".into(), 1);
        if let Some(applied) = answers {
            s.judge_edges.insert(
                "s".into(),
                JudgeEdgeAnswers {
                    execution: 1,
                    applied,
                    p: [("edge_0".to_string(), 0.4)].into(),
                },
            );
        }
        s
    }

    /// The record `record_decision` writes for `state` (a fresh run dir).
    fn recorded(pb: &Playbook, state: &RunState) -> RouteEvaluated {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run).unwrap();
        let log = EventLog::create(&run).unwrap();
        let evaluation = evaluate_live_route(pb, "s", state);
        record_decision(&log, pb, "s", state, &evaluation);
        super::super::read(&run)
            .into_iter()
            .find_map(|d| match d {
                super::super::Diagnostic::Route(r) => Some(r),
                _ => None,
            })
            .expect("a route record")
    }

    fn results(r: &RouteEvaluated) -> Vec<&str> {
        r.checks.iter().map(|c| c.result.as_str()).collect()
    }

    #[test]
    fn a_shadow_answer_reads_shadowed_with_its_p_and_the_fallback_is_taken() {
        let pb = Playbook::from_yaml(JUDGED).unwrap();
        let r = recorded(&pb, &state(Some(false)));
        assert_eq!(r.mode, "one_of");
        assert_eq!(results(&r), ["shadowed", "matched"]);
        assert_eq!(
            r.checks[0].observed,
            Some(serde_json::json!({"p": 0.4, "applied": false}))
        );
        assert_eq!(r.chosen, ["e:s>z#1"]);
        // An applied answer below min_p is a plain miss, with its p.
        let r = recorded(&pb, &state(Some(true)));
        assert_eq!(results(&r), ["not_matched", "matched"]);
        assert_eq!(
            r.checks[0].observed,
            Some(serde_json::json!({"p": 0.4, "applied": true}))
        );
    }

    #[test]
    fn no_answer_reads_judge_unavailable_and_an_exhausted_fallback_is_a_dead_end() {
        let pb = Playbook::from_yaml(JUDGED).unwrap();
        let mut s = state(None);
        s.edge_counts.insert(("s".into(), "z".into()), 1);
        let r = recorded(&pb, &s);
        assert_eq!(r.mode, "dead_end");
        assert_eq!(results(&r), ["judge_unavailable", "exhausted"]);
        assert_eq!(r.checks[0].observed, None);
        assert!(r.chosen.is_empty());
    }

    #[test]
    fn a_review_decision_is_observed_scrubbed_whole_then_clipped() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run).unwrap();
        let secret = "quietharbor".repeat(12);
        crate::run_config::write_run_config(
            &run,
            &crate::run_config::RunConfig {
                params: [("note".to_string(), secret.clone())].into(),
                ..Default::default()
            },
        )
        .unwrap();
        let yaml = "schema: 2\nid: g\nname: g\nversion: 1.0.0\nparams:\n  - { name: note, type: text, secret: true }\nnodes:\n  - { id: start, type: start }\n  - { id: s, type: human_review, options: [approved, rejected] }\n  - { id: a, type: finish, outcome: success }\n  - { id: z, type: finish, outcome: success }\nedges:\n  - { from: start, to: s }\n  - { from: s, to: a, condition: { type: review_status, equals: approved } }\n  - { from: s, to: z, fallback: true }\n";
        std::fs::write(run.join("playbook.yaml"), yaml).unwrap();
        let pb = Playbook::from_yaml(yaml).unwrap();
        let mut s = state(None);
        s.reviews.insert(
            "s".into(),
            ReviewDecision {
                decision: secret.clone(),
                note: String::new(),
            },
        );
        let log = EventLog::create(&run).unwrap();
        record_decision(&log, &pb, "s", &s, &evaluate_live_route(&pb, "s", &s));
        let text = std::fs::read_to_string(run.join(super::super::FILE)).unwrap();
        assert!(text.contains("review_status"), "{text}");
        for start in 0..secret.len() - 16 {
            assert!(
                !text.contains(&secret[start..start + 16]),
                "a fragment leaked"
            );
        }
    }

    /// A threshold's observed number is left out when the scrubber touched
    /// the field's text: a numeric secret (`123456.000`) would leave as its
    /// re-serialized number (`123456.0`), which no scrub recognizes. A plain
    /// field keeps its number.
    #[test]
    fn a_threshold_never_records_the_number_of_a_redacted_text() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run).unwrap();
        let secret = "123456.000";
        crate::run_config::write_run_config(
            &run,
            &crate::run_config::RunConfig {
                params: [("pin".to_string(), secret.to_string())].into(),
                ..Default::default()
            },
        )
        .unwrap();
        let yaml = "schema: 2\nid: g\nname: g\nversion: 1.0.0\nparams:\n  - { name: pin, type: text, secret: true }\nnodes:\n  - { id: start, type: start }\n  - { id: s, type: prompt, prompt: s }\n  - { id: a, type: finish, outcome: success }\n  - { id: z, type: finish, outcome: success }\nedges:\n  - { from: start, to: s }\n  - { from: s, to: a, condition: { type: output_field, node: s, field: f, gte: 1 } }\n  - { from: s, to: z, fallback: true }\n";
        std::fs::write(run.join("playbook.yaml"), yaml).unwrap();
        let pb = Playbook::from_yaml(yaml).unwrap();
        let observed = |value: &str| {
            let mut s = state(None);
            s.outputs
                .insert("s".into(), serde_json::json!({ "f": value }).to_string());
            let _ = std::fs::remove_file(run.join(super::super::FILE));
            let log = EventLog::create(&run).unwrap();
            record_decision(&log, &pb, "s", &s, &evaluate_live_route(&pb, "s", &s));
            std::fs::read_to_string(run.join(super::super::FILE)).unwrap()
        };
        let text = observed(secret);
        assert!(!text.contains("123456"), "{text}");
        assert!(text.contains("\"number\":null"), "{text}");
        let plain = observed("80");
        assert!(plain.contains("\"number\":80.0"), "{plain}");
    }
}
