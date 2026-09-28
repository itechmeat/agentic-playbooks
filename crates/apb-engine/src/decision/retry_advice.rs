//! Retry advice for agent failures (issue #165 Part 9, Part 14.2).
//!
//! Asked only for an attempt that failed with [`FailureKind::Agent`] while a
//! same-executor retry is still pending; never for transient, auth or budget
//! failures, continuation timeouts, interrupts or cancellations (the call
//! site checks). One `choice`, `next`, plus the `repeat` noul when an
//! earlier agent failure of the same execution exists.
//!
//! Shadow and advise change nothing: the retry runs as today and the
//! decision records `would_change` (`next` is not `retry_same_likely_helps`
//! at `uses.retry_advice.thresholds.min_confidence` or more). Enforce (the
//! playbook opted in with `defaults.retry_advice: enforce` and a threshold is
//! stored): `switch_executor` skips the remaining same-executor retries to
//! the next fallback (no-op without one), `stop_and_route_failure` fails the
//! node now. The call site journals each skip as a `supervisor_action`
//! marker, like the infrastructure-retry marker.
//!
//! [`FailureKind::Agent`]: crate::failure_class::FailureKind::Agent

use std::collections::BTreeMap;

use apb_core::decisions::DecisionMode;
use apb_decide::{Question, UseSite};
use serde_json::{Value, json};

use super::{
    DecisionCall, DecisionJournal, DecisionOutcome, DecisionRunner, Enforce, FieldClass,
    Judgement, StateField, StateParts,
};
use crate::event::DecisionAnswer;

/// `supervisor_action.action` of an enforced retry-advice skip.
pub(crate) const RETRY_ADVICE_ACTION: &str = "retry_advice";

const STEP_HEAD: usize = 2 * 1024;
const FAILURE_TAIL: usize = 6 * 1024;

pub(crate) const RETRY_SAME: &str = "retry_same_likely_helps";
pub(crate) const SWITCH: &str = "switch_executor";
pub(crate) const STOP: &str = "stop_and_route_failure";

/// The questions; `repeat` only when there is a previous failure.
pub(crate) fn questions(with_previous: bool) -> BTreeMap<String, Question> {
    let criteria = [
        (
            RETRY_SAME,
            "The failure looks incidental (a flaky command, a wrong guess the agent can correct on a second try).",
        ),
        (
            SWITCH,
            "The failure looks tied to this agent or model (it cannot use a tool, misreads the task, loops, or hits a capability limit).",
        ),
        (
            STOP,
            "The failure comes from the environment or the task itself (missing file, broken build, impossible request); another attempt will fail the same way.",
        ),
        ("unclear", "Not enough information in `failure` to tell."),
    ];
    let mut q = BTreeMap::from([(
        "next".to_string(),
        Question::Choice {
            instructions: json!(
                "Given `failure` (and `previous_failure` if present), what is the most useful next step for `step`?"
            ),
            criteria: criteria.iter().map(|(k, v)| (*k, Some(json!(v)))).collect(),
        },
    )]);
    if with_previous {
        q.insert(
            "repeat".to_string(),
            Question::Noul {
                instructions: json!(
                    "Does `failure` describe the same failure as `previous_failure`?"
                ),
                criteria: None,
            },
        );
    }
    q
}

/// The failed attempt, as the call site knows it.
pub(crate) struct Failure<'a> {
    pub(crate) node: &'a str,
    pub(crate) attempt: u32,
    pub(crate) title: Option<&'a str>,
    pub(crate) prompt: &'a str,
    pub(crate) failure: &'a str,
    pub(crate) previous_failure: Option<&'a str>,
    pub(crate) retries_left: u32,
    pub(crate) fallbacks_left: u32,
    /// `defaults.retry_advice: enforce`.
    pub(crate) opted_in: bool,
}

/// What the retry loop does next.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Advice {
    /// Today's behaviour: retry the same executor.
    Retry,
    /// Enforced: skip the remaining same-executor retries. The string is the
    /// marker detail.
    SwitchExecutor(String),
    /// Enforced: fail the node now.
    Stop(String),
}

fn next_answer(answers: &BTreeMap<String, DecisionAnswer>) -> Option<(&str, f64)> {
    let a = answers.get("next")?;
    Some((a.value.as_ref()?.as_str()?, a.confidence.unwrap_or(0.0)))
}

/// Asks once for this failure. Fail-open: anything but an enforced answer is
/// [`Advice::Retry`].
pub(crate) fn advise(
    runner: &DecisionRunner,
    journal: &dyn DecisionJournal,
    f: Failure,
) -> Advice {
    if runner.mode_for(UseSite::RetryAdvice) == DecisionMode::Off {
        return Advice::Retry;
    }
    let min_confidence = runner.threshold_or(UseSite::RetryAdvice, "min_confidence", 0.6);
    let step = match f.title {
        Some(t) if !t.trim().is_empty() => format!("{t}\n\n{}", f.prompt),
        _ => f.prompt.to_string(),
    };
    let mut fields = vec![
        StateField {
            name: "step",
            class: FieldClass::Prompt,
            text: step,
            head: STEP_HEAD,
            tail: 0,
        },
        StateField {
            name: "failure",
            class: FieldClass::Output,
            text: f.failure.to_string(),
            head: 0,
            tail: FAILURE_TAIL,
        },
    ];
    if let Some(prev) = f.previous_failure {
        fields.push(StateField {
            name: "previous_failure",
            class: FieldClass::Output,
            text: prev.to_string(),
            head: 0,
            tail: FAILURE_TAIL,
        });
    }
    let mut meta = serde_json::Map::new();
    meta.insert("attempt".into(), json!(f.attempt));
    meta.insert("retries_left".into(), json!(f.retries_left));
    meta.insert("fallbacks_left".into(), json!(f.fallbacks_left));
    let judge = move |answers: &BTreeMap<String, DecisionAnswer>| Judgement {
        applied: false,
        would_change: next_answer(answers).map(|(v, c)| v != RETRY_SAME && c >= min_confidence),
    };
    let fallbacks_left = f.fallbacks_left;
    let acts = move |answers: &BTreeMap<String, DecisionAnswer>, threshold: f64| {
        matches!(next_answer(answers), Some((v, c))
            if c >= threshold && (v == STOP || (v == SWITCH && fallbacks_left > 0)))
    };
    let outcome = runner.decide(
        journal,
        DecisionCall {
            site: UseSite::RetryAdvice,
            node: Some(f.node),
            attempt: Some(f.attempt),
            state: StateParts { fields, meta },
            questions: questions(f.previous_failure.is_some()),
            baseline: None,
            judge: &judge,
            join_from: None,
            join: BTreeMap::from([
                ("retries_left".to_string(), Value::from(f.retries_left)),
                ("fallbacks_left".to_string(), Value::from(f.fallbacks_left)),
            ]),
            enforce: Some(Enforce {
                opted_in: f.opted_in,
                refused: None,
                acts: &acts,
            }),
        },
    );
    let DecisionOutcome::Answered { answers, meta, .. } = outcome else {
        return Advice::Retry;
    };
    if !meta.applied {
        return Advice::Retry;
    }
    let Some((value, confidence)) = next_answer(&answers) else {
        return Advice::Retry;
    };
    let who = format!(
        "{}/{}",
        meta.provider.as_deref().unwrap_or("-"),
        meta.model.as_deref().unwrap_or("-")
    );
    match value {
        SWITCH => Advice::SwitchExecutor(format!(
            "agent_task node `{}` attempt {}: retry advice ({who}) rates the failure `{SWITCH}` (confidence {confidence:.2}); skipping the {} remaining same-executor retries to the next fallback",
            f.node, f.attempt, f.retries_left
        )),
        STOP => Advice::Stop(format!(
            "agent_task node `{}` attempt {}: retry advice ({who}) rates the failure `{STOP}` (confidence {confidence:.2}); failing the node without the {} remaining retries",
            f.node, f.attempt, f.retries_left
        )),
        _ => Advice::Retry,
    }
}
