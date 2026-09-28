//! The judge uses' pure half (issue #165 Parts 5 and 7): the questions a
//! judge node or judge edge sends, and the code that turns answers into the
//! node's output. Thresholds are applied here, never by the model: a
//! `choice` below `min_confidence` becomes the declared safe option, a
//! `noul` is `p >= yes_at`, a `score` falls into its `[lo, hi)` band (the
//! last band closed).

use std::collections::BTreeMap;

use apb_core::judge::{
    DECIDED_BY, DEFAULT_YES_AT, JudgeFallback, JudgeQuestion, JudgeQuestions, JudgeThreshold,
    OrderedMap, REASON,
};
use apb_decide::{ChoiceCriteria, NoulCriteria, Question};
use serde_json::{Map, Value, json};

use crate::event::DecisionAnswer;

/// A judge node's questions on the wire.
pub(crate) fn node_questions(questions: &JudgeQuestions) -> BTreeMap<String, Question> {
    questions
        .iter()
        .map(|(id, q)| {
            let wire = match q {
                JudgeQuestion::Choice {
                    instructions,
                    criteria,
                } => Question::Choice {
                    instructions: json!(instructions),
                    criteria: criteria
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.as_ref().map(|d| json!(d))))
                        .collect::<ChoiceCriteria>(),
                },
                JudgeQuestion::Score {
                    instructions,
                    levels,
                } => Question::Score {
                    instructions: json!(instructions),
                    levels: levels.iter().map(|l| json!(l)).collect(),
                },
                JudgeQuestion::Noul {
                    instructions,
                    criteria,
                } => Question::Noul {
                    instructions: json!(instructions),
                    criteria: criteria.as_ref().map(|c| NoulCriteria {
                        yes: c.get("true").map_or(Value::Null, |d| json!(d)),
                        no: c.get("false").map_or(Value::Null, |d| json!(d)),
                    }),
                },
            };
            (id.to_string(), wire)
        })
        .collect()
}

/// The band `value` falls into: `[lo, hi)`, the last band (highest `hi`)
/// closed at its top.
fn band(bands: &OrderedMap<[f64; 2]>, value: f64) -> Option<&str> {
    let top = bands
        .iter()
        .map(|(_, [_, hi])| *hi)
        .fold(f64::MIN, f64::max);
    bands
        .iter()
        .find(|(_, [lo, hi])| value >= *lo && (value < *hi || (*hi == top && value <= *hi)))
        .map(|(name, _)| name)
}

/// The node output for a set of answers, or the reason it cannot be built
/// (a missing or invalid answer). `decided_by` names who answered.
pub(crate) fn output_from_answers(
    questions: &JudgeQuestions,
    thresholds: &OrderedMap<JudgeThreshold>,
    answers: &BTreeMap<String, DecisionAnswer>,
    decided_by: &str,
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (id, q) in questions.iter() {
        let a = answers
            .get(id)
            .filter(|a| a.invalid.is_none())
            .ok_or_else(|| format!("no valid answer for `{id}`"))?;
        let t = thresholds.get(id);
        let missing = || format!("no valid answer for `{id}`");
        match q {
            JudgeQuestion::Choice { .. } => {
                let mut value = a
                    .value
                    .as_ref()
                    .and_then(Value::as_str)
                    .ok_or_else(missing)?
                    .to_string();
                let confidence = a.confidence.unwrap_or(0.0);
                if let Some(JudgeThreshold {
                    min_confidence: Some(min),
                    below: Some(below),
                    ..
                }) = t
                    && confidence < *min
                {
                    value = below.clone();
                }
                out.insert(id.to_string(), json!(value));
                out.insert(format!("{id}_p"), json!(a.p));
                out.insert(format!("{id}_confidence"), json!(a.confidence));
            }
            JudgeQuestion::Noul { .. } => {
                let p = a.p.ok_or_else(missing)?;
                let yes_at = t.and_then(|t| t.yes_at).unwrap_or(DEFAULT_YES_AT);
                out.insert(id.to_string(), json!(p >= yes_at));
                out.insert(format!("{id}_p"), json!(p));
            }
            JudgeQuestion::Score { .. } => {
                let value = a
                    .value
                    .as_ref()
                    .and_then(Value::as_f64)
                    .ok_or_else(missing)?;
                if let Some(bands) = t.and_then(|t| t.bands.as_ref())
                    && let Some(name) = band(bands, value)
                {
                    out.insert(id.to_string(), json!(name));
                }
                out.insert(format!("{id}_score"), json!(value));
            }
        }
    }
    out.insert(DECIDED_BY.to_string(), json!(decided_by));
    Ok(out)
}

/// The output of a `route` or `default` fallback, `None` for the forms
/// that do not succeed with an output of their own (`fail`, `emulate`).
pub(crate) fn fallback_output(
    fallback: Option<&JudgeFallback>,
    reason: &str,
) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    match fallback {
        Some(JudgeFallback::Route(_)) => {
            out.insert(DECIDED_BY.into(), json!(apb_core::judge::UNAVAILABLE));
        }
        Some(JudgeFallback::Default(values)) => {
            for (k, v) in values {
                out.insert(k.clone(), v.clone());
            }
            out.insert(DECIDED_BY.into(), json!(apb_core::judge::DEFAULT));
        }
        _ => return None,
    }
    out.insert(REASON.into(), json!(reason));
    Some(out)
}

/// Whether an answer-derived output would route differently from the
/// fallback the node takes below `enforce` (the shadow `would_change`).
/// `None` when that cannot be told (an emulated fallback).
pub(crate) fn differs_from_fallback(
    derived: &Map<String, Value>,
    fallback: Option<&JudgeFallback>,
    questions: &JudgeQuestions,
) -> Option<bool> {
    match fallback {
        Some(JudgeFallback::Default(values)) => Some(
            questions
                .iter()
                .any(|(id, _)| values.get(id) != derived.get(id)),
        ),
        Some(JudgeFallback::Emulate) => None,
        _ => Some(true),
    }
}

/// The compact JSON a node output is written as.
pub(crate) fn render(out: &Map<String, Value>) -> String {
    serde_json::to_string(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> (JudgeQuestions, OrderedMap<JudgeThreshold>) {
        #[derive(serde::Deserialize)]
        struct S {
            questions: JudgeQuestions,
            thresholds: OrderedMap<JudgeThreshold>,
        }
        let s: S = serde_yaml_ng::from_str(
            r#"
questions:
  verdict: { type: choice, instructions: i, criteria: { clean: a, needs_fix: b, unclear: c } }
  risky: { type: noul, instructions: i }
  effort: { type: score, instructions: i, levels: [a, b, c, d, e] }
thresholds:
  verdict: { min_confidence: 0.6, below: unclear }
  risky: { yes_at: 0.7 }
  effort: { bands: { small: [0, 1.5], medium: [1.5, 2.5], large: [2.5, 4] } }
"#,
        )
        .unwrap();
        (s.questions, s.thresholds)
    }

    fn answers(
        verdict: (&str, f64, f64),
        risky: f64,
        effort: f64,
    ) -> BTreeMap<String, DecisionAnswer> {
        BTreeMap::from([
            (
                "verdict".to_string(),
                DecisionAnswer {
                    value: Some(json!(verdict.0)),
                    p: Some(verdict.1),
                    confidence: Some(verdict.2),
                    invalid: None,
                },
            ),
            (
                "risky".to_string(),
                DecisionAnswer {
                    p: Some(risky),
                    ..Default::default()
                },
            ),
            (
                "effort".to_string(),
                DecisionAnswer {
                    value: Some(json!(effort)),
                    p: Some(0.5),
                    confidence: Some(0.4),
                    invalid: None,
                },
            ),
        ])
    }

    #[test]
    fn thresholds_map_answers_to_the_documented_output() {
        let (q, t) = spec();
        let out = output_from_answers(
            &q,
            &t,
            &answers(("needs_fix", 0.83, 0.71), 0.12, 1.9),
            "typesafe/jev-1.13.0",
        )
        .unwrap();
        assert_eq!(
            render(&out),
            r#"{"decided_by":"typesafe/jev-1.13.0","effort":"medium","effort_score":1.9,"risky":false,"risky_p":0.12,"verdict":"needs_fix","verdict_confidence":0.71,"verdict_p":0.83}"#
        );
        // Below min_confidence the safe option wins; yes_at is inclusive;
        // the last band is closed at its top.
        let out =
            output_from_answers(&q, &t, &answers(("clean", 0.5, 0.59), 0.7, 4.0), "x").unwrap();
        assert_eq!(out["verdict"], json!("unclear"));
        assert_eq!(out["risky"], json!(true));
        assert_eq!(out["effort"], json!("large"));
        let out =
            output_from_answers(&q, &t, &answers(("clean", 0.5, 0.9), 0.1, 1.5), "x").unwrap();
        assert_eq!(out["effort"], json!("medium"));
    }

    #[test]
    fn without_a_threshold_the_raw_answer_stands() {
        let (q, _) = spec();
        let out = output_from_answers(
            &q,
            &OrderedMap::default(),
            &answers(("clean", 0.4, 0.1), 0.5, 2.2),
            "x",
        )
        .unwrap();
        assert_eq!(out["verdict"], json!("clean"));
        assert_eq!(out["risky"], json!(true));
        assert!(out.get("effort").is_none());
        assert_eq!(out["effort_score"], json!(2.2));
    }

    #[test]
    fn an_invalid_item_means_no_output() {
        let (q, t) = spec();
        let mut a = answers(("clean", 0.4, 0.9), 0.5, 2.2);
        a.get_mut("risky").unwrap().invalid = Some("out of range".into());
        assert!(output_from_answers(&q, &t, &a, "x").is_err());
        a.remove("risky");
        assert!(output_from_answers(&q, &t, &a, "x").is_err());
    }

    #[test]
    fn fallback_outputs_name_the_reason() {
        let route = JudgeFallback::Route("human".into());
        assert_eq!(
            render(&fallback_output(Some(&route), "mode").unwrap()),
            r#"{"decided_by":"unavailable","reason":"mode"}"#
        );
        let default =
            JudgeFallback::Default(BTreeMap::from([("verdict".into(), json!("unclear"))]));
        assert_eq!(
            render(&fallback_output(Some(&default), "timeout").unwrap()),
            r#"{"decided_by":"default","reason":"timeout","verdict":"unclear"}"#
        );
        assert!(fallback_output(Some(&JudgeFallback::Fail), "x").is_none());
        assert!(fallback_output(None, "x").is_none());
    }
}
