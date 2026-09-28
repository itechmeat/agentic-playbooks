//! Client-side limit checks before sending, and strict validation of every
//! reply item after.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{Answer, DecideError, DecisionRequest, Limits, Question};

/// How far a probability distribution may stray from summing to 1.
const SUM_TOLERANCE: f64 = 1e-3;

/// Bytes per token assumed when estimating a request's size: deliberately
/// low, so the estimate errs towards "too big" (English prose runs at about
/// four bytes per token, code and JSON lower).
const BYTES_PER_TOKEN: usize = 3;

/// Refuses a request that breaks a limit, before anything is sent: no
/// questions, a `choice` without options or with more than the provider
/// allows, a `score` outside 2..=10 levels (or the provider's lower limit), a
/// state plus longest question beyond the token budget.
pub fn check_limits(req: &DecisionRequest, limits: &Limits) -> Result<(), DecideError> {
    if req.questions.is_empty() {
        return Err(DecideError::Invalid("no questions".into()));
    }
    let max_options = limits.max_options.min(255);
    let max_levels = limits.max_levels.min(10);
    let mut longest = 0usize;
    for (id, q) in &req.questions {
        if id.is_empty() {
            return Err(DecideError::Invalid("empty question id".into()));
        }
        match q {
            Question::Choice { criteria, .. } => {
                if criteria.is_empty() || criteria.len() > max_options {
                    return Err(DecideError::Invalid(format!(
                        "question `{id}`: a choice needs 1 to {max_options} options, got {}",
                        criteria.len()
                    )));
                }
            }
            Question::Score { levels, .. } => {
                if levels.len() < 2 || levels.len() > max_levels {
                    return Err(DecideError::Invalid(format!(
                        "question `{id}`: a score needs 2 to {max_levels} levels, got {}",
                        levels.len()
                    )));
                }
            }
            Question::Noul { .. } => {}
        }
        let size = serde_json::to_string(q).map(|s| s.len()).unwrap_or(0);
        longest = longest.max(size);
    }
    let state_size = serde_json::to_string(&req.state)
        .map(|s| s.len())
        .unwrap_or(0);
    let estimate = (state_size + longest).div_ceil(BYTES_PER_TOKEN);
    if estimate > limits.max_state_tokens {
        return Err(DecideError::Invalid(format!(
            "state plus longest question is about {estimate} tokens, over the limit of {}",
            limits.max_state_tokens
        )));
    }
    Ok(())
}

/// The confidence of a distribution when the provider reported none:
/// `(n * p_max - 1) / (n - 1)`, clipped to [0, 1] (0 for a uniform
/// distribution, 1 for a certain one).
pub fn recompute_confidence(probabilities: &[f64]) -> f64 {
    let n = probabilities.len();
    if n < 2 {
        return 1.0;
    }
    let p_max = probabilities.iter().copied().fold(0.0, f64::max);
    ((n as f64 * p_max - 1.0) / (n as f64 - 1.0)).clamp(0.0, 1.0)
}

fn probability(v: &Value) -> Result<f64, String> {
    let p = v
        .as_f64()
        .ok_or_else(|| "probability is not a number".to_string())?;
    if !(0.0..=1.0).contains(&p) || p.is_nan() {
        return Err(format!("probability {p} outside [0, 1]"));
    }
    Ok(p)
}

fn distribution(raw: Option<&Value>, keys: &[String]) -> Result<BTreeMap<String, f64>, String> {
    let obj = raw
        .and_then(Value::as_object)
        .filter(|o| !o.is_empty())
        .ok_or_else(|| "no probabilities".to_string())?;
    if let Some(unknown) = obj.keys().find(|k| !keys.contains(k)) {
        return Err(format!("probabilities name unknown option `{unknown}`"));
    }
    let mut out = BTreeMap::new();
    for k in keys {
        let p = match obj.get(k) {
            Some(v) => probability(v)?,
            None => 0.0,
        };
        out.insert(k.clone(), p);
    }
    let sum: f64 = out.values().sum();
    if (sum - 1.0).abs() > SUM_TOLERANCE {
        return Err(format!("probabilities sum to {sum:.4}"));
    }
    Ok(out)
}

fn confidence(raw: Option<&Value>, probabilities: &BTreeMap<String, f64>) -> Result<f64, String> {
    match raw {
        Some(Value::Null) | None => Ok(recompute_confidence(
            &probabilities.values().copied().collect::<Vec<_>>(),
        )),
        Some(v) => probability(v).map_err(|e| format!("confidence: {e}")),
    }
}

/// The longest `Answer::Invalid` reason kept: a reason can quote the reply,
/// which is provider text.
const MAX_REASON_CHARS: usize = 200;

/// The longest model name a reply may report.
const MAX_MODEL_CHARS: usize = 128;

/// The model a reply names, when it looks like a model id (at most 128
/// characters of letters, digits and `._:/@-`); otherwise `configured`. The
/// name is provider text that reaches the journal, the dashboard, a
/// supervising agent and the threshold lookup.
pub fn reply_model(reported: Option<&str>, configured: &str) -> String {
    match reported {
        Some(m)
            if !m.is_empty()
                && m.chars().count() <= MAX_MODEL_CHARS
                && m.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c)) =>
        {
            m.to_string()
        }
        _ => configured.to_string(),
    }
}

/// A reported cost, when it is a finite, non-negative number: a negative
/// one would lower the run's spend.
pub fn reply_cost(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
        .filter(|c| c.is_finite() && *c >= 0.0)
}

/// Validates one reply item against its question.
pub fn validate_item(question: &Question, item: &Value) -> Answer {
    match validate_item_inner(question, item) {
        Ok(a) => a,
        Err(reason) => Answer::Invalid {
            reason: reason.chars().take(MAX_REASON_CHARS).collect(),
        },
    }
}

fn validate_item_inner(question: &Question, item: &Value) -> Result<Answer, String> {
    let obj = item
        .as_object()
        .ok_or_else(|| "answer is not an object".to_string())?;
    if let Some(t) = obj.get("type").and_then(Value::as_str)
        && t != question.type_str()
    {
        return Err(format!(
            "type mismatch: asked {}, got {t}",
            question.type_str()
        ));
    }
    match question {
        Question::Noul { .. } => {
            let p = obj
                .get("noul")
                .ok_or_else(|| "noul answer without a value".to_string())?;
            Ok(Answer::Noul { p: probability(p)? })
        }
        Question::Choice { criteria, .. } => {
            let keys: Vec<String> = criteria.names().map(str::to_string).collect();
            let probabilities = distribution(obj.get("probabilities"), &keys)?;
            let value = obj
                .get("choice")
                .and_then(Value::as_str)
                .ok_or_else(|| "choice answer without a choice".to_string())?;
            let chosen = *probabilities
                .get(value)
                .ok_or_else(|| format!("choice `{value}` is not an option"))?;
            let best = probabilities.values().copied().fold(0.0, f64::max);
            if chosen + 1e-9 < best {
                return Err(format!("choice `{value}` is not the most probable option"));
            }
            let confidence = confidence(obj.get("confidence"), &probabilities)?;
            Ok(Answer::Choice {
                value: value.to_string(),
                probabilities,
                confidence,
            })
        }
        Question::Score { levels, .. } => {
            let keys: Vec<String> = (0..levels.len()).map(|i| i.to_string()).collect();
            let probabilities = distribution(obj.get("probabilities"), &keys)?;
            let expected: f64 = probabilities
                .iter()
                .map(|(k, p)| k.parse::<f64>().unwrap_or(0.0) * p)
                .sum();
            let value = match obj.get("score") {
                Some(v) => v
                    .as_f64()
                    .ok_or_else(|| "score is not a number".to_string())?,
                None => expected,
            };
            let top = (levels.len() - 1) as f64;
            if !(0.0..=top).contains(&value) {
                return Err(format!("score {value} outside 0..={top}"));
            }
            let confidence = confidence(obj.get("confidence"), &probabilities)?;
            Ok(Answer::Score {
                value,
                probabilities,
                confidence,
            })
        }
    }
}

/// Validates a reply's `answers` object against the asked questions: every
/// asked id gets an answer (`Invalid` when missing or malformed); items for
/// ids that were not asked are ignored and counted.
pub fn validate_answers(
    questions: &BTreeMap<String, Question>,
    answers: &serde_json::Map<String, Value>,
) -> (BTreeMap<String, Answer>, usize) {
    let mut out = BTreeMap::new();
    for (id, q) in questions {
        let answer = match answers.get(id) {
            Some(item) => validate_item(q, item),
            None => Answer::Invalid {
                reason: "missing from the reply".into(),
            },
        };
        out.insert(id.clone(), answer);
    }
    let ignored = answers
        .keys()
        .filter(|k| !questions.contains_key(*k))
        .count();
    (out, ignored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn choice(options: &[&str]) -> Question {
        Question::Choice {
            instructions: json!("pick"),
            criteria: options.iter().map(|o| (o.to_string(), None)).collect(),
        }
    }

    fn noul() -> Question {
        Question::Noul {
            instructions: json!("yes?"),
            criteria: None,
        }
    }

    fn score(n: usize) -> Question {
        Question::Score {
            instructions: json!("how much"),
            levels: (0..n).map(|i| json!(format!("level {i}"))).collect(),
        }
    }

    fn request(questions: Vec<(&str, Question)>, state: Value) -> DecisionRequest {
        DecisionRequest {
            use_site: crate::UseSite::CompletionCheck,
            state_order: Vec::new(),
            state,
            questions: questions
                .into_iter()
                .map(|(k, q)| (k.to_string(), q))
                .collect(),
        }
    }

    #[test]
    fn limits_refuse_empty_oversized_and_out_of_range_requests() {
        let l = Limits::default();
        assert!(check_limits(&request(vec![], json!("s")), &l).is_err());
        assert!(check_limits(&request(vec![("c", choice(&[]))], json!("s")), &l).is_err());
        let many: Vec<String> = (0..256).map(|i| format!("o{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(check_limits(&request(vec![("c", choice(&many))], json!("s")), &l).is_err());
        assert!(check_limits(&request(vec![("s", score(1))], json!("s")), &l).is_err());
        assert!(check_limits(&request(vec![("s", score(11))], json!("s")), &l).is_err());
        let narrow = Limits {
            max_options: 2,
            ..Limits::default()
        };
        assert!(
            check_limits(
                &request(vec![("c", choice(&["a", "b", "c"]))], json!("s")),
                &narrow
            )
            .is_err()
        );
        let big = json!("x".repeat(100_000));
        assert!(check_limits(&request(vec![("n", noul())], big), &l).is_err());
        assert!(
            check_limits(
                &request(
                    vec![("n", noul()), ("s", score(10)), ("c", choice(&["a"]))],
                    json!("s")
                ),
                &l
            )
            .is_ok()
        );
    }

    #[test]
    fn confidence_is_recomputed_from_the_peak() {
        assert_eq!(recompute_confidence(&[0.5, 0.5]), 0.0);
        assert_eq!(recompute_confidence(&[1.0, 0.0, 0.0]), 1.0);
        assert!((recompute_confidence(&[0.7, 0.2, 0.1]) - 0.55).abs() < 1e-9);
        let a = validate_item(
            &choice(&["a", "b"]),
            &json!({"type": "choice", "choice": "a", "probabilities": {"a": 0.75, "b": 0.25}}),
        );
        assert_eq!(
            a,
            Answer::Choice {
                value: "a".into(),
                probabilities: [("a".to_string(), 0.75), ("b".to_string(), 0.25)].into(),
                confidence: 0.5
            }
        );
    }

    #[test]
    fn malformed_items_become_invalid_one_by_one() {
        let bad = [
            (
                choice(&["a", "b"]),
                json!({"choice": "b", "probabilities": {"a": 0.8, "b": 0.2}}),
            ),
            (
                choice(&["a", "b"]),
                json!({"choice": "a", "probabilities": {"a": 0.8, "b": 0.3}}),
            ),
            (
                choice(&["a", "b"]),
                json!({"choice": "a", "probabilities": {"a": 1.0, "z": 0.0}}),
            ),
            (
                choice(&["a"]),
                json!({"choice": "a", "probabilities": {"a": 1.0}, "confidence": 2}),
            ),
            (noul(), json!({"type": "noul", "noul": 1.5})),
            (noul(), json!({"type": "choice", "noul": 0.5})),
            (noul(), json!({"type": "noul"})),
            (
                score(3),
                json!({"score": 5, "probabilities": {"0": 0.0, "1": 0.0, "2": 1.0}}),
            ),
            (score(3), json!({"probabilities": {"0": 0.5, "7": 0.5}})),
            (noul(), json!("yes")),
        ];
        for (q, item) in bad {
            assert!(
                matches!(validate_item(&q, &item), Answer::Invalid { .. }),
                "{item} must be invalid for {}",
                q.type_str()
            );
        }
        let s = validate_item(
            &score(3),
            &json!({"probabilities": {"0": 0.0, "1": 0.5, "2": 0.5}}),
        );
        assert!(matches!(s, Answer::Score { value, .. } if (value - 1.5).abs() < 1e-9));
    }

    #[test]
    fn a_partial_batch_keeps_the_valid_items_and_counts_the_unknown() {
        let questions: BTreeMap<String, Question> =
            [("a".to_string(), noul()), ("b".to_string(), noul())].into();
        let reply = json!({"a": {"type": "noul", "noul": 0.9}, "extra": {"noul": 0.1}});
        let (answers, ignored) = validate_answers(&questions, reply.as_object().unwrap());
        assert_eq!(answers["a"], Answer::Noul { p: 0.9 });
        assert!(matches!(answers["b"], Answer::Invalid { .. }));
        assert_eq!(ignored, 1);
    }

    /// Any distribution the provider could send over two options: valid
    /// replies validate, and every accepted answer satisfies the contract.
    #[test]
    fn every_accepted_choice_satisfies_the_contract() {
        let q = choice(&["a", "b"]);
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..2_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let pa = (seed % 1_001) as f64 / 1_000.0;
            let pb = ((seed >> 20) % 1_001) as f64 / 1_000.0;
            let pick = if seed & 1 == 0 { "a" } else { "b" };
            let item = json!({"choice": pick, "probabilities": {"a": pa, "b": pb}});
            if let Answer::Choice {
                value,
                probabilities,
                confidence,
            } = validate_item(&q, &item)
            {
                let sum: f64 = probabilities.values().sum();
                assert!((sum - 1.0).abs() <= SUM_TOLERANCE);
                let best = probabilities.values().copied().fold(0.0, f64::max);
                assert!(probabilities[&value] + 1e-9 >= best);
                assert!((0.0..=1.0).contains(&confidence));
            } else {
                assert!(
                    (pa + pb - 1.0).abs() > SUM_TOLERANCE
                        || (pick == "a" && pa < pb)
                        || (pick == "b" && pb < pa),
                    "a well-formed reply was refused: {item}"
                );
            }
        }
    }
}
