//! The decision provider of a quick scenario run: the answer book stands in
//! for the providers.
//!
//! A judge site stages the scripted answer of its key ([`stage`]) on its
//! own thread before it asks the run's decision runner; the runner's chain
//! holds one [`ScenarioProvider`], which hands that answer back as a reply.
//! The reply goes through the same validation as a provider's
//! (`apb_decide::validate_answers`), so an option the question does not
//! have is `invalid` exactly as from a provider, and the runner journals
//! `decision_made` with `provider: scenario`, `calibrated: false`.
//! `{ unavailable: <kind> }` fails the ask with that error kind.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use apb_core::scenario::{JudgeAnswer, QuestionAnswer, UnavailableKind};
use apb_decide::{
    DecideError, DecisionProvider, DecisionRequest, DecisionResponse, Limits, Question,
};
use serde_json::{Map, Value, json};

use super::session::{node_key, of, on_missing};
use super::{Answer, BoundaryKey, BoundaryKind, Resolution, Session};
use crate::error::EngineError;

/// The provider id and model a scripted answer is journaled under.
pub const PROVIDER: &str = "scenario";

/// What the next ask on this thread is answered with.
#[derive(Debug, Clone)]
enum Staged {
    Answers(BTreeMap<String, QuestionAnswer>),
    Unavailable(DecideErrorKind),
}

#[derive(Debug, Clone, Copy)]
enum DecideErrorKind {
    Unavailable,
    RateLimited,
    Invalid,
    Auth,
    Budget,
    Timeout,
}

impl DecideErrorKind {
    fn error(self) -> DecideError {
        match self {
            DecideErrorKind::Unavailable => {
                DecideError::Unavailable("the scenario scripts this question unavailable".into())
            }
            DecideErrorKind::RateLimited => DecideError::RateLimited { retry_after: None },
            DecideErrorKind::Invalid => {
                DecideError::Invalid("the scenario scripts this question invalid".into())
            }
            DecideErrorKind::Auth => DecideError::Auth,
            DecideErrorKind::Budget => DecideError::Budget,
            DecideErrorKind::Timeout => DecideError::Timeout,
        }
    }
}

impl From<UnavailableKind> for DecideErrorKind {
    fn from(k: UnavailableKind) -> Self {
        match k {
            UnavailableKind::Unavailable => DecideErrorKind::Unavailable,
            UnavailableKind::RateLimited => DecideErrorKind::RateLimited,
            UnavailableKind::Invalid => DecideErrorKind::Invalid,
            UnavailableKind::Auth => DecideErrorKind::Auth,
            UnavailableKind::Budget => DecideErrorKind::Budget,
            UnavailableKind::Timeout => DecideErrorKind::Timeout,
        }
    }
}

thread_local! {
    static STAGED: RefCell<Option<Staged>> = const { RefCell::new(None) };
}

/// Clears the staged answer when the ask is over.
#[derive(Debug)]
pub struct StagedGuard(());

impl Drop for StagedGuard {
    fn drop(&mut self) {
        STAGED.with(|s| *s.borrow_mut() = None);
    }
}

fn put(staged: Staged) -> StagedGuard {
    STAGED.with(|s| *s.borrow_mut() = Some(staged));
    StagedGuard(())
}

/// Stages the scripted answer of the judge boundary `kind` of `node` (and
/// list item `item`) for the next ask on this thread. `None` for a real run
/// and at smoke, where the providers answer. A missing answer stops the run
/// under `on_missing: error` (`Err`); under `fail` the ask is unavailable.
pub fn stage(
    run_dir: &Path,
    kind: BoundaryKind,
    node: &str,
    item: Option<&str>,
) -> Result<Option<StagedGuard>, EngineError> {
    let Some(session) = of(run_dir) else {
        return Ok(None);
    };
    let key = BoundaryKey {
        item: item.map(str::to_string),
        ..node_key(node)
    };
    let staged = match session.resolve(kind, &key) {
        Resolution::Real | Resolution::Skip => return Ok(None),
        Resolution::Answer(Answer::Judge(JudgeAnswer::Answers(a))) => Staged::Answers(a),
        Resolution::Answer(Answer::Judge(JudgeAnswer::Unavailable { unavailable })) => {
            Staged::Unavailable(unavailable.into())
        }
        Resolution::Answer(Answer::JudgeEdges(a)) => Staged::Answers(a),
        Resolution::Answer(_) => Staged::Unavailable(DecideErrorKind::Invalid),
        Resolution::Missing => {
            on_missing(run_dir, kind, &key)?;
            Staged::Unavailable(DecideErrorKind::Unavailable)
        }
    };
    Ok(Some(put(staged)))
}

/// The provider a quick scenario run's decision runner asks.
#[derive(Debug, Default)]
pub struct ScenarioProvider;

impl DecisionProvider for ScenarioProvider {
    fn id(&self) -> &str {
        PROVIDER
    }

    fn model(&self) -> &str {
        PROVIDER
    }

    fn limits(&self) -> Limits {
        Limits::default()
    }

    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
        let staged = STAGED.with(|s| s.borrow().clone());
        let answers = match staged {
            None => {
                return Err(DecideError::Unavailable(
                    "the scenario has no answer for this question".into(),
                ));
            }
            Some(Staged::Unavailable(kind)) => return Err(kind.error()),
            Some(Staged::Answers(a)) => a,
        };
        let items: Map<String, Value> = answers
            .iter()
            .filter_map(|(id, a)| req.questions.get(id).map(|q| (id.clone(), wire_item(q, a))))
            .collect();
        let (answers, ignored_items) =
            apb_decide::validate::validate_answers(&req.questions, &items);
        Ok(DecisionResponse {
            provider: PROVIDER.into(),
            model: PROVIDER.into(),
            calibrated: false,
            answers,
            usage: Default::default(),
            latency_ms: 0,
            cached: false,
            ignored_items,
        })
    }
}

/// A scripted answer as a provider's reply item for `question`: a choice's
/// `value` is the option chosen, `p` its probability (the rest share what
/// is left); a score's `value` is a level's name (V148), sent as that
/// level's index; a yes or no question reads `p`, else its `value`.
fn wire_item(question: &Question, a: &QuestionAnswer) -> Value {
    let p = a.p.unwrap_or(1.0);
    // A value the question does not offer is sent as given, so the reply
    // validation refuses it like a provider's.
    let spread = |mut keys: Vec<String>, chosen: &str| -> Map<String, Value> {
        if !keys.iter().any(|k| k == chosen) {
            keys.push(chosen.to_string());
        }
        let others = keys.len().saturating_sub(1).max(1) as f64;
        keys.into_iter()
            .map(|k| {
                let v = if k == chosen { p } else { (1.0 - p) / others };
                (k, json!(v))
            })
            .collect()
    };
    let mut item = match question {
        // A yes or no `value` (V148) is that answer for sure, unless `p`
        // says how sure.
        Question::Noul { .. } => {
            let p = match (a.p, a.value.as_ref().and_then(Value::as_bool)) {
                (Some(p), _) => p,
                (None, Some(yes)) => f64::from(u8::from(yes)),
                (None, None) => 1.0,
            };
            json!({ "type": "noul", "noul": p })
        }
        Question::Choice { criteria, .. } => {
            let chosen = a.value.as_ref().and_then(Value::as_str).unwrap_or_default();
            let keys: Vec<String> = criteria.names().map(str::to_string).collect();
            json!({
                "type": "choice",
                "choice": chosen,
                "probabilities": spread(keys, chosen),
            })
        }
        Question::Score { levels, .. } => {
            let keys: Vec<String> = (0..levels.len()).map(|i| i.to_string()).collect();
            // A value that names no level is sent as given (or absent), so
            // the reply validation refuses it like a provider's; it never
            // reads as the lowest level.
            let index = a
                .value
                .as_ref()
                .and_then(|v| levels.iter().position(|l| l == v));
            match index {
                Some(i) => json!({
                    "type": "score",
                    "score": i,
                    "probabilities": spread(keys, &i.to_string()),
                }),
                None => json!({
                    "type": "score",
                    "score": a.value.clone().unwrap_or(Value::Null),
                    "probabilities": spread(keys, "0"),
                }),
            }
        }
    };
    if let (Some(c), Some(obj)) = (a.confidence, item.as_object_mut()) {
        obj.insert("confidence".into(), json!(c));
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice() -> Question {
        serde_json::from_value(json!({
            "type": "choice",
            "instructions": "q",
            "criteria": { "clear": null, "missing_detail": null }
        }))
        .unwrap()
    }

    fn request(questions: BTreeMap<String, Question>) -> DecisionRequest {
        DecisionRequest {
            use_site: apb_decide::UseSite::JudgeNode,
            state: json!({}),
            state_order: Vec::new(),
            questions,
        }
    }

    /// A staged answer is handed back validated like a provider's reply: a
    /// declared option with its probability, a yes or no question by its
    /// `p`, and an option the question does not have is `invalid`.
    #[test]
    fn a_staged_answer_is_validated_like_a_provider_reply() {
        let questions = BTreeMap::from([
            ("sufficient".to_string(), choice()),
            (
                "risky".to_string(),
                serde_json::from_value(json!({"type": "noul", "instructions": "r"})).unwrap(),
            ),
            ("other".to_string(), choice()),
        ]);
        let _g = put(Staged::Answers(BTreeMap::from([
            (
                "sufficient".to_string(),
                QuestionAnswer {
                    value: Some(json!("clear")),
                    p: Some(0.9),
                    confidence: Some(0.8),
                },
            ),
            (
                "risky".to_string(),
                QuestionAnswer {
                    p: Some(0.1),
                    ..Default::default()
                },
            ),
            (
                "other".to_string(),
                QuestionAnswer {
                    value: Some(json!("nope")),
                    ..Default::default()
                },
            ),
        ])));
        let r = ScenarioProvider.decide(&request(questions)).unwrap();
        assert_eq!((r.provider.as_str(), r.calibrated), ("scenario", false));
        assert!(
            matches!(&r.answers["sufficient"], apb_decide::Answer::Choice { value, confidence, .. } if value == "clear" && (*confidence - 0.8).abs() < 1e-9),
            "{:?}",
            r.answers["sufficient"]
        );
        assert!(
            matches!(r.answers["risky"], apb_decide::Answer::Noul { p } if (p - 0.1).abs() < 1e-9)
        );
        assert!(
            matches!(&r.answers["other"], apb_decide::Answer::Invalid { reason } if reason.contains("`nope`")),
            "{:?}",
            r.answers["other"]
        );
    }

    /// `{ unavailable: <kind> }` fails the ask with that error kind, and
    /// nothing staged is no answer at all.
    #[test]
    fn unavailable_fails_with_its_kind_and_nothing_staged_is_unavailable() {
        let q = BTreeMap::from([("q".to_string(), choice())]);
        {
            let _g = put(Staged::Unavailable(UnavailableKind::Timeout.into()));
            let e = ScenarioProvider.decide(&request(q.clone())).unwrap_err();
            assert_eq!(e.kind(), "timeout");
        }
        let e = ScenarioProvider.decide(&request(q)).unwrap_err();
        assert_eq!(e.kind(), "unavailable");
    }
}
