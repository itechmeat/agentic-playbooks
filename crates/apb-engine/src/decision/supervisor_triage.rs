//! Supervisor wake pre-triage (issue #165 Part 10, Part 14.3).
//!
//! Asked once per park wake (a supervised run parked on a failed or
//! timed-out node), just before the wake is raised. The `action` choice is
//! built from what is legal at that point (no `switch_executor` without an
//! alternative executor, no `continue_from_next` without a successor), plus
//! the `looping` noul.
//!
//! Shadow journals only. Advise puts `triage` on the wake, one sentence in
//! its detail, and one line in the supervisor's brief; nothing executes.
//! Enforce (the playbook opted in with `supervisor: { pre_triage: enforce }`,
//! a threshold is stored, and the use has actions left): a `retry_same` or
//! `retry_with_note` answer at `looping` below
//! `uses.supervisor_triage.thresholds.looping_max` is posted as the same
//! `node_retry` command a supervisor sends, attributed to `triage` by a
//! `supervisor_action` marker; the wake is still raised with
//! `triage.applied: true`. Everything else wakes the supervisor as usual.

use std::collections::BTreeMap;

use apb_core::decisions::DecisionMode;
use apb_decide::{Question, UseSite};
use serde_json::{Value, json};

use super::{
    DecisionCall, DecisionJournal, DecisionOutcome, DecisionRunner, Enforce, FieldClass,
    Judgement, StateField, StateParts,
};
use crate::event::{DecisionAnswer, WakeTriage};

/// `supervisor_action.action` of a retry the engine posted from triage.
pub(crate) const TRIAGE_RETRY_ACTION: &str = "triage_retry";

const STEP_HEAD: usize = 2 * 1024;
const OUTPUT_TAIL: usize = 6 * 1024;
/// How many recent supervisor actions the state carries.
pub(crate) const RECENT_ACTIONS: usize = 5;

/// The brief sentence a supervisor gets when advise is on for its run.
pub(crate) const BRIEF_LINE: &str = " A wake may carry an advisory `triage`; follow it unless the detail contradicts it, and use supervisor_run_inspect only when neither is enough.";

/// Everything the park site knows about the wake.
pub(crate) struct Park<'a> {
    pub(crate) node: &'a str,
    pub(crate) title: Option<&'a str>,
    /// The node's prompt template (the step as authored).
    pub(crate) prompt: &'a str,
    pub(crate) trigger: &'a str,
    pub(crate) failure_kind: Option<&'a str>,
    pub(crate) attempt: u32,
    pub(crate) retries_left: u32,
    pub(crate) output: &'a str,
    /// Another executor or profile exists for the node.
    pub(crate) has_alternative: bool,
    /// The node has a successor to continue from.
    pub(crate) has_successor: bool,
    pub(crate) recent_actions: Vec<Value>,
    /// The seq the wake will get (journaled as a join key).
    pub(crate) wake_seq: u64,
    /// `supervisor: { pre_triage: enforce }`.
    pub(crate) opted_in: bool,
}

/// The legal `action` options, in order.
fn criteria(p: &Park) -> Vec<(&'static str, &'static str)> {
    let mut c = vec![
        (
            "retry_same",
            "Run the step again unchanged; the failure looks incidental.",
        ),
        (
            "retry_with_note",
            "Run the step again with a short correction; the output shows a specific mistake.",
        ),
    ];
    if p.has_alternative {
        c.push((
            "switch_executor",
            "Run the step on another executor; this agent or model looks unable to do it.",
        ));
    }
    if p.has_successor {
        c.push((
            "continue_from_next",
            "The step's goal is already met or not needed; move on.",
        ));
    }
    c.push((
        "pause_for_human",
        "A person must decide or fix something outside the run.",
    ));
    c.push((
        "needs_supervisor",
        "The situation needs a closer look than these options allow.",
    ));
    c
}

pub(crate) fn questions(p: &Park) -> BTreeMap<String, Question> {
    BTreeMap::from([
        (
            "action".to_string(),
            Question::Choice {
                instructions: json!(
                    "What should happen next for `step`, given `trigger`, `failure_kind` and `output_tail`?"
                ),
                criteria: criteria(p)
                    .into_iter()
                    .map(|(k, v)| (k, Some(json!(v))))
                    .collect(),
            },
        ),
        (
            "looping".to_string(),
            Question::Noul {
                instructions: json!(
                    "Does `output_tail` repeat the same failure the recent actions already tried to fix?"
                ),
                criteria: None,
            },
        ),
    ])
}

/// What the park does with the answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Triage {
    /// For the wake (advise or enforce), `None` otherwise.
    pub(crate) triage: Option<WakeTriage>,
    /// Enforced: post a retry, after this note when `Some` (retry_with_note).
    pub(crate) retry: Option<Option<String>>,
}

fn action(answers: &BTreeMap<String, DecisionAnswer>) -> Option<(&str, f64, f64)> {
    let a = answers.get("action")?;
    Some((
        a.value.as_ref()?.as_str()?,
        a.p.unwrap_or(0.0),
        a.confidence.unwrap_or(0.0),
    ))
}

/// The code-template note of an automatic `retry_with_note`: the failure
/// kind and the first line of the output tail that reads like an error.
pub(crate) fn retry_note(failure_kind: Option<&str>, output: &str) -> String {
    let tail: String = {
        let chars = output.chars().count();
        output.chars().skip(chars.saturating_sub(OUTPUT_TAIL)).collect()
    };
    let is_error = |l: &str| {
        let l = l.to_ascii_lowercase();
        ["error", "failed", "failure", "panic", "exception", "cannot", "not found"]
            .iter()
            .any(|w| l.contains(w))
    };
    let line = tail
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && is_error(l))
        .or_else(|| tail.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    let line: String = line.chars().take(300).collect();
    format!(
        "Previous attempt failed with: {}; {line}",
        failure_kind.unwrap_or("unknown")
    )
}

/// Asks once for this wake. Fail-open: anything but an answer is a plain
/// wake.
pub(crate) fn triage(runner: &DecisionRunner, journal: &dyn DecisionJournal, p: Park) -> Triage {
    let mode = runner.mode_for(UseSite::SupervisorTriage);
    if mode == DecisionMode::Off {
        return Triage::default();
    }
    let looping_max = runner.threshold_or(UseSite::SupervisorTriage, "looping_max", 0.3);
    let step = match p.title {
        Some(t) if !t.trim().is_empty() => format!("{}: {t}\n\n{}", p.node, p.prompt),
        _ => format!("{}\n\n{}", p.node, p.prompt),
    };
    let mut meta = serde_json::Map::new();
    meta.insert("trigger".into(), json!(p.trigger));
    meta.insert("failure_kind".into(), json!(p.failure_kind));
    meta.insert("attempt".into(), json!(p.attempt));
    meta.insert("retries_left".into(), json!(p.retries_left));
    meta.insert("alternative_executor".into(), json!(p.has_alternative));
    meta.insert("recent_actions".into(), Value::Array(p.recent_actions.clone()));
    let judge = |answers: &BTreeMap<String, DecisionAnswer>| Judgement {
        applied: false,
        would_change: action(answers).map(|(a, _, _)| a != "needs_supervisor"),
    };
    let acts = move |answers: &BTreeMap<String, DecisionAnswer>, threshold: f64| {
        let looping = answers.get("looping").and_then(|a| a.p).unwrap_or(1.0);
        matches!(action(answers), Some((a, p, _))
            if (a == "retry_same" || a == "retry_with_note") && p >= threshold && looping < looping_max)
    };
    let outcome = runner.decide(
        journal,
        DecisionCall {
            site: UseSite::SupervisorTriage,
            node: Some(p.node),
            attempt: Some(p.attempt),
            state: StateParts {
                fields: vec![
                    StateField {
                        name: "step",
                        class: FieldClass::Prompt,
                        text: step,
                        head: STEP_HEAD,
                        tail: 0,
                    },
                    StateField {
                        name: "output_tail",
                        class: FieldClass::Output,
                        text: p.output.to_string(),
                        head: 0,
                        tail: OUTPUT_TAIL,
                    },
                ],
                meta,
            },
            questions: questions(&p),
            baseline: None,
            judge: &judge,
            join_from: None,
            join: BTreeMap::from([("wake_seq".to_string(), Value::from(p.wake_seq))]),
            enforce: Some(Enforce {
                opted_in: p.opted_in,
                refused: None,
                acts: &acts,
            }),
        },
    );
    let DecisionOutcome::Answered {
        answers,
        mode,
        meta,
        ..
    } = outcome
    else {
        return Triage::default();
    };
    if mode < DecisionMode::Advise {
        return Triage::default();
    }
    let Some((act, p_act, confidence)) = action(&answers) else {
        return Triage::default();
    };
    let wake = WakeTriage {
        action: act.to_string(),
        p: p_act,
        confidence,
        looping_p: answers.get("looping").and_then(|a| a.p),
        provider: meta.provider.clone().unwrap_or_default(),
        model: meta.model.clone().unwrap_or_default(),
        applied: meta.applied,
    };
    let retry = meta
        .applied
        .then(|| (act == "retry_with_note").then(|| retry_note(p.failure_kind, p.output)));
    Triage {
        triage: Some(wake),
        retry,
    }
}

/// The sentence the wake detail gets.
pub(crate) fn detail_line(t: &WakeTriage) -> String {
    if t.applied {
        format!(
            " Triage (applied): {} p={:.2}; the engine posted the retry itself.",
            t.action, t.p
        )
    } else {
        format!(" Triage (advisory): {} p={:.2}.", t.action, t.p)
    }
}
