//! The completion check (issue #165 Part 8, Part 14.1): after an agent
//! attempt reported success and passed its `success_check`, ask whether its
//! reply is a finished result rather than a progress note, a plan or a
//! question back.
//!
//! Shadow means journal only: the `DecisionMade` records the answers, whether
//! a flag would have been raised (`would_change`) and a code-only regex
//! verdict on the same reply for comparison. The attempt's status, output
//! and events are otherwise exactly what they would be without the check.
//!
//! Advise: a flagged attempt (`final_result` below the configured cut) also
//! raises one `Anomaly` wake naming the node, the attempt, both answers and
//! the `completion` choice; the attempt's status never changes.
//!
//! Enforce (the node opted in with `completion_check: enforce`, and the
//! runner found a stored threshold): `final_result` below that threshold
//! fails the attempt with the reply kept as `rejected_output`, consuming a
//! normal retry. A `blocked_on_input` answer never fails the attempt; it only
//! raises the advise anomaly.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use apb_core::decisions::{COMPLETION_FINAL_RESULT_CUT, DecisionMode};

use super::{DecisionOutcome, Enforce};
use apb_decide::{Question, UseSite};
use regex::Regex;
use serde_json::{Value, json};

use super::{
    DecisionCall, DecisionJournal, DecisionRunner, FieldClass, Judgement, StateField, StateParts,
};
use crate::event::{DecisionAnswer, DecisionBaseline};

/// `task`: the head of the rendered prompt.
const TASK_HEAD: usize = 4 * 1024;
/// `result`: the head and the tail of the raw reply.
const RESULT_HEAD: usize = 1024;
const RESULT_TAIL: usize = 8 * 1024;
/// The regex baseline reads the last 600 characters of the reply, as it did
/// when it was measured.
const BASELINE_TAIL_CHARS: usize = 600;
/// A reply shorter than this is flagged by the baseline.
const BASELINE_SHORT_CHARS: usize = 80;

/// The two questions, in one call. `final_result` is Part 8's noul as
/// written; `completion` carries the reworded criteria from the Phase 0
/// evaluation (the `mixed` prompt). Only `final_result` decides a flag.
pub(crate) fn questions() -> BTreeMap<String, Question> {
    let criteria = [
        (
            "complete",
            "The work `task` asks for is done and `result` reports it. Follow-ups, suggestions, and steps `task` excludes or leaves to a later phase do not count against it.",
        ),
        (
            "partial",
            "Some of the work `task` asks for is done, but a step it requires is still running, failed or was skipped, or a field listed in `meta.missing_fields` is absent.",
        ),
        (
            "not_started",
            "`result` describes intentions, a plan or preparation, but none of the requested work done.",
        ),
        (
            "blocked_on_input",
            "`result` stops to ask for information, a decision or a permission.",
        ),
        (
            "unclear",
            "`result` is too short, cut off, or unrelated to `task` to tell.",
        ),
    ];
    BTreeMap::from([
        (
            "final_result".to_string(),
            Question::Noul {
                instructions: json!(
                    "Is `result` a finished result for `task`, rather than a progress note, a plan, or a question back to the user?"
                ),
                criteria: None,
            },
        ),
        (
            "completion".to_string(),
            Question::Choice {
                instructions: json!(
                    "How far does `result` show the work in `task` was carried out?"
                ),
                criteria: criteria.iter().map(|(k, v)| (*k, Some(json!(v)))).collect(),
            },
        ),
    ])
}

/// The generic endings the baseline looks for: no project markers.
static BASELINE_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        r"still running",
        r"i'?ll pause",
        r"pausing here",
        r"will report",
        r"next step",
        r"next,? i will",
        r"waiting for",
        r"please confirm",
        r"should i\b",
        r"\blet me\b",
        r"\bawaiting\b",
        r"i'?ll (continue|finish|wait|pick up|report|push|rerun|start)",
    ]
    .into_iter()
    .map(|p| (p, Regex::new(&format!("(?m){p}")).expect("valid pattern")))
    .collect()
});

/// The code-only verdict recorded next to the model's answer: flags a
/// declared field left out, a reply ending in a question mark, a reply under
/// 80 characters, or a generic "not done yet" phrase in its last 600
/// characters. `pattern` names what flagged.
pub(crate) fn regex_baseline(output: &str, missing_fields: &[String]) -> DecisionBaseline {
    let flag = |p: &str| DecisionBaseline {
        regex_flag: true,
        pattern: Some(p.to_string()),
    };
    if !missing_fields.is_empty() {
        return flag("missing_fields");
    }
    let chars = output.chars().count();
    let tail: String = output
        .chars()
        .skip(chars.saturating_sub(BASELINE_TAIL_CHARS))
        .collect::<String>()
        .to_lowercase();
    if tail.trim_end().ends_with('?') {
        return flag("trailing_question");
    }
    if output.trim().chars().count() < BASELINE_SHORT_CHARS {
        return flag("short_reply");
    }
    for (name, rx) in BASELINE_PATTERNS.iter() {
        if rx.is_match(&tail) {
            return flag(name);
        }
    }
    DecisionBaseline::default()
}

/// Whether a flag would be raised: `final_result` below the cut. The
/// `completion` choice never flags on its own (it rates finished reports
/// that list follow-ups as partial).
fn judge(cut: f64) -> impl Fn(&BTreeMap<String, DecisionAnswer>) -> Judgement {
    move |answers| Judgement {
        applied: false,
        would_change: answers
            .get("final_result")
            .and_then(|a| a.p)
            .map(|p| p < cut),
    }
}

/// What the attempt site hands the check.
pub(crate) struct Attempt<'a> {
    pub(crate) node: &'a str,
    pub(crate) attempt: u32,
    /// The rendered prompt the attempt ran with.
    pub(crate) prompt: &'a str,
    /// The attempt's raw output (the node output).
    pub(crate) output: &'a str,
    pub(crate) missing_fields: Vec<String>,
    /// The node opted in to the enforce path (`completion_check: enforce`).
    pub(crate) enforce: bool,
    /// A same-executor retry is left for a rejection to consume. Without
    /// one the enforce path does not act (`enforce_refused: no_retry`): a
    /// rejection would fail the node, not retry it.
    pub(crate) retry_left: bool,
}

/// What the attempt site does with the check's answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Verdict {
    /// How long the check took (counted into the attempt's duration).
    pub(crate) elapsed: Duration,
    /// Advise: the detail of the one `Anomaly` wake to raise.
    pub(crate) anomaly: Option<String>,
    /// Enforce: the attempt fails with this reason, its reply kept as
    /// `rejected_output`.
    pub(crate) reject: Option<String>,
}

/// `partial (p=0.81)` for the `completion` answer, `unknown` without one.
fn choice_text(answers: &BTreeMap<String, DecisionAnswer>) -> String {
    match answers.get("completion") {
        Some(a) => format!(
            "{} (p={:.2})",
            a.value
                .as_ref()
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            a.p.unwrap_or(0.0)
        ),
        None => "unknown".to_string(),
    }
}

fn completion_value(answers: &BTreeMap<String, DecisionAnswer>) -> Option<&str> {
    answers.get("completion")?.value.as_ref()?.as_str()
}

/// Runs the check for one successful attempt. Fail-open: every failure is
/// journaled by the runner and changes nothing else.
pub(crate) fn check(
    runner: &DecisionRunner,
    journal: &dyn DecisionJournal,
    attempt: Attempt,
) -> Verdict {
    if runner.mode_for(UseSite::CompletionCheck) == DecisionMode::Off
        || attempt.output.trim().is_empty()
    {
        return Verdict::default();
    }
    let started = Instant::now();
    let cut = runner
        .settings()
        .threshold(UseSite::CompletionCheck.as_str(), "final_result")
        .unwrap_or(COMPLETION_FINAL_RESULT_CUT);
    let baseline = regex_baseline(attempt.output, &attempt.missing_fields);
    let mut meta = serde_json::Map::new();
    meta.insert(
        "missing_fields".into(),
        Value::Array(
            attempt
                .missing_fields
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    let judge = judge(cut);
    let acts = |answers: &BTreeMap<String, DecisionAnswer>, threshold: f64| {
        completion_value(answers) != Some("blocked_on_input")
            && answers
                .get("final_result")
                .and_then(|a| a.p)
                .is_some_and(|p| p < threshold)
    };
    let outcome = runner.decide(
        journal,
        DecisionCall {
            site: UseSite::CompletionCheck,
            node: Some(attempt.node),
            attempt: Some(attempt.attempt),
            state: StateParts {
                fields: vec![
                    StateField {
                        name: "task",
                        class: FieldClass::Prompt,
                        text: attempt.prompt.to_string(),
                        head: TASK_HEAD,
                        tail: 0,
                    },
                    StateField {
                        name: "result",
                        class: FieldClass::Output,
                        text: attempt.output.to_string(),
                        head: RESULT_HEAD,
                        tail: RESULT_TAIL,
                    },
                ],
                meta,
            },
            questions: questions(),
            baseline: Some(baseline),
            judge: &judge,
            join_from: None,
            join: BTreeMap::new(),
            enforce: Some(Enforce {
                opted_in: attempt.enforce,
                refused: (!attempt.retry_left).then_some("no_retry"),
                acts: &acts,
            }),
        },
    );
    let mut verdict = Verdict::default();
    if let DecisionOutcome::Answered {
        answers,
        mode,
        meta,
        ..
    } = &outcome
    {
        let final_p = answers.get("final_result").and_then(|a| a.p);
        if meta.applied {
            verdict.reject = Some(format!("completion check: {}", choice_text(answers)));
        } else if *mode >= DecisionMode::Advise
            && let Some(p) = final_p
            && p < cut
        {
            verdict.anomaly = Some(format!(
                "agent_task node `{}` attempt {} reported success, but the completion check rates it {} and final_result p={p:.2}",
                attempt.node,
                attempt.attempt,
                choice_text(answers),
            ));
        }
    }
    verdict.elapsed = started.elapsed();
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_baseline_flags_generic_not_done_endings_only() {
        let long = "Implemented the parser, added tests, all green. ".repeat(4);
        assert!(!regex_baseline(&long, &[]).regex_flag);
        for (text, pattern) in [
            (
                format!("{long} The suite is still running, I will check back."),
                "still running",
            ),
            (
                format!("{long} Should I also update the docs?"),
                "trailing_question",
            ),
            ("Done.".to_string(), "short_reply"),
            (
                format!("{long}\nI'll continue with the second half next."),
                "i'?ll (continue|finish|wait|pick up|report|push|rerun|start)",
            ),
            (format!("{long} Let me know"), r"\blet me\b"),
        ] {
            let b = regex_baseline(&text, &[]);
            assert!(b.regex_flag, "{text}");
            assert_eq!(b.pattern.as_deref(), Some(pattern));
        }
        let b = regex_baseline(&long, &["pr_url".to_string()]);
        assert_eq!(b.pattern.as_deref(), Some("missing_fields"));
        // Only the tail counts: a phrase early in a long reply does not flag.
        let early = format!(
            "Waiting for the build. {}",
            "Then everything passed. ".repeat(40)
        );
        assert!(!regex_baseline(&early, &[]).regex_flag);
    }

    #[test]
    fn only_final_result_below_the_cut_would_change() {
        let j = judge(0.15);
        let answers = |p: f64, value: &str| {
            BTreeMap::from([
                (
                    "final_result".to_string(),
                    DecisionAnswer {
                        p: Some(p),
                        ..Default::default()
                    },
                ),
                (
                    "completion".to_string(),
                    DecisionAnswer {
                        value: Some(json!(value)),
                        p: Some(0.95),
                        confidence: Some(0.9),
                        invalid: None,
                    },
                ),
            ])
        };
        assert_eq!(j(&answers(0.10, "complete")).would_change, Some(true));
        assert_eq!(j(&answers(0.40, "partial")).would_change, Some(false));
        assert_eq!(j(&BTreeMap::new()).would_change, None);
        assert!(!j(&answers(0.01, "not_started")).applied);
    }

    #[test]
    fn the_questions_are_the_measured_prompt() {
        let q = questions();
        let Question::Choice { criteria, .. } = &q["completion"] else {
            panic!()
        };
        assert_eq!(
            criteria.names().collect::<Vec<_>>(),
            [
                "complete",
                "partial",
                "not_started",
                "blocked_on_input",
                "unclear"
            ]
        );
        assert!(matches!(
            q["final_result"],
            Question::Noul { criteria: None, .. }
        ));
    }
}
