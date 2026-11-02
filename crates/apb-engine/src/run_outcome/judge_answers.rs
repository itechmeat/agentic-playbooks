//! Judge answers on the run graph: what each judge node execution was told
//! and what it routed, and each judge edge's `p` against its `min_p`.
//!
//! A judge node's view is built from its `node_finished` output even when no
//! decision was journaled (an unconfigured or switched-off judge takes its
//! `on_unavailable` route or default directly). Each question compares the
//! measure its type routes on: a choice's confidence with `min_confidence`,
//! a noul's `p` with `yes_at`, a score's value with its bands. The model's
//! answer and the routed output are kept apart, and a native answer and an
//! emulation answer of the same execution are kept as separate evidence.

use std::collections::BTreeMap;

use apb_core::edge_address::{EdgeAddress, edge_addresses};
use apb_core::judge::{DEFAULT_YES_AT, JudgeQuestion, JudgeThreshold};
use apb_core::schema::{EdgeCondition, NodeKind};
use serde::Serialize;
use serde_json::Value;

use super::route_evidence::RouteEvidence;
use crate::event::{DecisionAnswer, EventPayload};

/// One decision a judge node execution journaled.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeEvidence {
    pub seq: u64,
    /// `native` (the judge's own decision model) or `emulation` (its
    /// declared `emulate` fallback).
    pub source: String,
    /// The use's mode when it was asked (`off`, `shadow`, `advise`,
    /// `enforce`).
    pub mode: String,
    pub applied: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// The error kind when nothing usable came back.
    pub error: Option<String>,
}

/// How an answer compares with the threshold its question routes on.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeComparison {
    /// `confidence` (choice), `p` (noul) or `value` (score).
    pub measure: String,
    pub observed: f64,
    /// Whether the answer passed its threshold; `None` for a score, which
    /// lands in a band or in none.
    pub passed: Option<bool>,
    /// The band a score fell into.
    pub band: Option<String>,
    /// One line for people, such as `confidence 0.30 < 0.70: takes below`.
    pub label: String,
}

/// One evidence's answer to one question.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeAnswerView {
    /// `native` or `emulation`, as in [`JudgeEvidence::source`].
    pub source: String,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub value: Option<Value>,
    pub p: Option<f64>,
    pub confidence: Option<f64>,
    pub invalid: Option<String>,
    pub comparison: Option<JudgeComparison>,
}

/// One question of a judge node execution.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeQuestionView {
    pub id: String,
    /// `choice`, `noul` or `score`.
    pub kind: String,
    pub answers: Vec<JudgeAnswerView>,
    /// The value the node's output carries for the question (what routing
    /// read), apart from what the model said.
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub routed_value: Option<Value>,
}

/// One execution of a judge node.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeNodeAnswers {
    pub node: String,
    pub execution: u32,
    pub segment: Option<usize>,
    /// The status the execution ended with; `None` while open.
    pub status: Option<String>,
    /// From the output: `<provider>/<model>`, `default`, `unavailable` or
    /// `emulated:<provider>`.
    pub decided_by: Option<String>,
    /// From the output: why a fallback answered.
    pub reason: Option<String>,
    /// The declared `on_unavailable` form: `route`, `default`, `fail`,
    /// `emulate`, or `fail` when absent.
    pub fallback: Option<String>,
    pub evidence: Vec<JudgeEvidence>,
    pub questions: Vec<JudgeQuestionView>,
}

/// One judge edge's answer for one execution of its source.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeEdgeAnswer {
    pub node: String,
    pub execution: u32,
    pub seq: u64,
    /// The edge, when the decision named it or its rank resolves in a known
    /// definition; `None` reads as unknown.
    pub edge: Option<EdgeAddress>,
    pub rendered: Option<String>,
    pub p: Option<f64>,
    pub min_p: Option<f64>,
    pub mode: String,
    pub applied: bool,
    /// Whether routing took the edge, when that is established.
    pub taken: Option<bool>,
    pub label: String,
}

/// Every execution of every judge node of the run's known definitions.
pub(crate) fn judge_nodes(ev: &RouteEvidence<'_>) -> Vec<JudgeNodeAnswers> {
    let mut out = Vec::new();
    for (node, execs) in ev.index.nodes() {
        for x in execs.iter().filter(|x| !x.cancelled) {
            let segment = ev.execution_segment(node, x.ordinal);
            let Some(pb) = segment.and_then(|s| ev.playbook(s)) else {
                continue;
            };
            let Some(NodeKind::Judge {
                questions,
                thresholds,
                on_unavailable,
                ..
            }) = pb.node(node).map(|n| &n.kind)
            else {
                continue;
            };
            let decisions: Vec<(u64, &EventPayload)> = ev
                .events
                .iter()
                .filter(|e| {
                    matches!(&e.payload, EventPayload::DecisionMade { use_site, node: Some(n), .. }
                        if use_site == "judge_node" && n == node)
                        && ev
                            .index
                            .containing(node, e.seq)
                            .is_some_and(|c| c.ordinal == x.ordinal)
                })
                .map(|e| (e.seq, &e.payload))
                .collect();
            let evidence: Vec<JudgeEvidence> = decisions
                .iter()
                .enumerate()
                .filter_map(|(i, (seq, p))| evidence_of(i, *seq, p))
                .collect();
            let output = x.end_seq.and_then(|end| output_at(ev, end));
            let field = |k: &str| {
                output
                    .as_ref()
                    .and_then(|o| o.get(k))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            let questions = questions
                .iter()
                .map(|(id, q)| JudgeQuestionView {
                    id: id.to_string(),
                    kind: q.type_str().to_string(),
                    answers: decisions
                        .iter()
                        .zip(&evidence)
                        .filter_map(|((_, p), e)| {
                            let EventPayload::DecisionMade { answers, .. } = p else {
                                return None;
                            };
                            let a = answers.get(id)?;
                            Some(answer_view(&e.source, q, thresholds.get(id), a))
                        })
                        .collect(),
                    routed_value: output.as_ref().and_then(|o| o.get(id)).cloned(),
                })
                .collect();
            out.push(JudgeNodeAnswers {
                node: node.to_string(),
                execution: x.ordinal,
                segment,
                status: x.status.clone(),
                decided_by: field(apb_core::judge::DECIDED_BY),
                reason: field(apb_core::judge::REASON),
                fallback: Some(fallback_word(on_unavailable.as_ref()).to_string()),
                evidence,
                questions,
            });
        }
    }
    out
}

fn fallback_word(f: Option<&apb_core::judge::JudgeFallback>) -> &'static str {
    use apb_core::judge::JudgeFallback;
    match f {
        Some(JudgeFallback::Route(_)) => "route",
        Some(JudgeFallback::Default(_)) => "default",
        Some(JudgeFallback::Emulate) => "emulate",
        Some(JudgeFallback::Fail) | None => "fail",
        Some(JudgeFallback::Unknown(_)) => "unknown",
    }
}

/// The `i`-th judge-node decision of one execution: the first one is the
/// native ask unless its mode was `off` (only an emulation asks then);
/// every later one is the emulation fallback.
fn evidence_of(i: usize, seq: u64, p: &EventPayload) -> Option<JudgeEvidence> {
    let EventPayload::DecisionMade {
        mode,
        applied,
        provider,
        model,
        error,
        ..
    } = p
    else {
        return None;
    };
    let native = i == 0 && mode != "off";
    Some(JudgeEvidence {
        seq,
        source: if native { "native" } else { "emulation" }.into(),
        mode: mode.clone(),
        applied: *applied,
        provider: provider.clone(),
        model: model.clone(),
        error: error.clone(),
    })
}

fn output_at(ev: &RouteEvidence<'_>, seq: u64) -> Option<serde_json::Map<String, Value>> {
    ev.events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeFinished { output, .. } if e.seq == seq => {
            serde_json::from_str::<Value>(output)
                .ok()
                .and_then(|v| v.as_object().cloned())
        }
        _ => None,
    })
}

fn answer_view(
    source: &str,
    q: &JudgeQuestion,
    t: Option<&JudgeThreshold>,
    a: &DecisionAnswer,
) -> JudgeAnswerView {
    JudgeAnswerView {
        source: source.to_string(),
        value: a.value.clone(),
        p: a.p,
        confidence: a.confidence,
        invalid: a.invalid.clone(),
        comparison: a.invalid.is_none().then(|| comparison(q, t, a)).flatten(),
    }
}

/// The comparison a question's type routes on.
pub fn comparison(
    q: &JudgeQuestion,
    t: Option<&JudgeThreshold>,
    a: &DecisionAnswer,
) -> Option<JudgeComparison> {
    match q {
        JudgeQuestion::Choice { .. } => {
            let min = t?.min_confidence?;
            let below = t.and_then(|t| t.below.as_deref());
            let c = a.confidence.unwrap_or(0.0);
            let passed = c >= min;
            let label = match (passed, below) {
                (true, _) => format!("confidence {c:.2} >= {min:.2}"),
                (false, Some(b)) => format!("confidence {c:.2} < {min:.2}: takes `{b}`"),
                (false, None) => format!("confidence {c:.2} < {min:.2}"),
            };
            Some(JudgeComparison {
                measure: "confidence".into(),
                observed: c,
                passed: Some(passed),
                band: None,
                label,
            })
        }
        JudgeQuestion::Noul { .. } => {
            let p = a.p?;
            let yes_at = t.and_then(|t| t.yes_at).unwrap_or(DEFAULT_YES_AT);
            let passed = p >= yes_at;
            let op = if passed { ">=" } else { "<" };
            Some(JudgeComparison {
                measure: "p".into(),
                observed: p,
                passed: Some(passed),
                band: None,
                label: format!("p {p:.2} {op} {yes_at:.2}: {passed}"),
            })
        }
        JudgeQuestion::Score { .. } => {
            let v = a.value.as_ref().and_then(Value::as_f64)?;
            let Some(bands) = t.and_then(|t| t.bands.as_ref()) else {
                return Some(JudgeComparison {
                    measure: "value".into(),
                    observed: v,
                    passed: None,
                    band: None,
                    label: format!("value {v:.2}, no bands: only the score is written"),
                });
            };
            let band = crate::decision::judge::band(bands, v).map(str::to_string);
            let label = match &band {
                Some(b) => format!("value {v:.2} in band `{b}`"),
                None => format!("value {v:.2} falls in no band: no value is written"),
            };
            Some(JudgeComparison {
                measure: "value".into(),
                observed: v,
                passed: None,
                band,
                label,
            })
        }
    }
}

/// Every judge-edge answer of the run, attributed to the execution of its
/// source whose finish count it carries.
pub(crate) fn judge_edges(ev: &RouteEvidence<'_>) -> Vec<JudgeEdgeAnswer> {
    let mut out = Vec::new();
    for e in ev.events {
        let EventPayload::DecisionMade {
            use_site,
            node: Some(node),
            attempt: Some(attempt),
            answers,
            applied,
            mode,
            edge_addresses: named,
            ..
        } = &e.payload
        else {
            continue;
        };
        if use_site != "judge_edge" {
            continue;
        }
        let Some(x) = ev.index.judge_edge_execution(node, e.seq, *attempt) else {
            continue;
        };
        let segment = ev.execution_segment(node, x.ordinal);
        let pb = segment.and_then(|s| ev.playbook(s));
        let episode = ev.episodes.get(&(node.clone(), x.ordinal));
        let out_edges: Vec<usize> = pb.map_or_else(Vec::new, |pb| {
            pb.edges
                .iter()
                .enumerate()
                .filter(|(_, ed)| &ed.from == node)
                .map(|(i, _)| i)
                .collect()
        });
        for (key, a) in answers {
            let Some(rank) = key
                .strip_prefix("edge_")
                .and_then(|r| r.parse::<usize>().ok())
            else {
                continue;
            };
            let from_name = named.get(key).and_then(|s| EdgeAddress::parse(s).ok());
            let index = match (&from_name, pb) {
                (Some(addr), Some(pb)) => apb_core::edge_address::resolve(pb, addr),
                (None, Some(_)) => out_edges.get(rank).copied(),
                _ => None,
            };
            let edge = from_name.clone().or_else(|| {
                let pb = pb?;
                edge_addresses(pb)
                    .into_iter()
                    .nth(index?)
                    .map(|a| a.address)
            });
            let min_p = index.and_then(|i| match &pb?.edges[i].condition {
                Some(EdgeCondition::Judge { min_p, .. }) => Some(min_p.0),
                _ => None,
            });
            let taken = index.and_then(|i| match episode {
                Some(ep) if ep.complete => Some(ep.chosen.contains(&i)),
                _ => (ev.deliveries.iter().any(|d| {
                    d.edge == Some(i) && d.from == *node && d.execution == Some(x.ordinal)
                }))
                .then_some(true),
            });
            let label = match (a.p, min_p) {
                (Some(p), Some(m)) => {
                    let op = if p >= m { ">=" } else { "<" };
                    format!("p {p:.2} {op} {m:.2}")
                }
                (Some(p), None) => format!("p {p:.2}"),
                (None, _) => "unknown".to_string(),
            };
            out.push(JudgeEdgeAnswer {
                node: node.clone(),
                execution: x.ordinal,
                seq: e.seq,
                rendered: edge.as_ref().map(EdgeAddress::render),
                edge,
                p: a.p,
                min_p,
                mode: mode.clone(),
                applied: *applied,
                taken,
                label,
            });
        }
    }
    out
}

/// The answers of one execution keyed by question id, for tests and the
/// text surfaces.
pub fn questions_by_id(n: &JudgeNodeAnswers) -> BTreeMap<&str, &JudgeQuestionView> {
    n.questions.iter().map(|q| (q.id.as_str(), q)).collect()
}
