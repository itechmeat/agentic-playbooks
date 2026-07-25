//! Live observation of an in-flight attempt's interaction channels: the
//! question and review files an agent writes while a node is still running.
//! Shares the parent module's imports via `use super::*`.

use super::*;

/// Observes the question/answer channels for a LIVE interactive attempt and
/// journals `QuestionAsked` (+ a `WakeRaised`) and `QuestionAnswered` for each
/// new channel entry beyond what the log already carries (spec 2026-07-20, Task
/// 11). Count-based, exactly like the parked branch, so it is idempotent across
/// repeated ticks.
///
/// Single-writer invariant: this runs ONLY on the drive thread - from the
/// adapter's drive-owned poll loop through the live `on_tick` hook, and once
/// more as a final sweep after the attempt returns. The live agent blocks
/// inside its `ask_user` tool (via the sidecar) while drive keeps journaling
/// here, so drive remains the sole writer of the event log throughout.
pub(crate) fn observe_live_channels(
    run_dir: &Path,
    node: &str,
    journal: &Journal,
) -> Result<(), EngineError> {
    let events = read_all(run_dir)?;
    let asked = question_asked_count(&events, node);
    let answered = question_answered_count(&events, node);
    let questions: Vec<_> = read_questions_after(run_dir, None)?
        .into_iter()
        .filter(|q| q.node == node)
        .collect();
    for q in questions.iter().skip(asked) {
        journal.append(EventPayload::QuestionAsked {
            node: node.to_string(),
            question: q.question.clone(),
            options: q.options.clone(),
        })?;
        journal.raise_wake(run_dir, WakeTrigger::Anomaly, node, "interactive question")?;
    }
    let answers: Vec<_> = read_answers_after(run_dir, None)?
        .into_iter()
        .filter(|a| a.node == node)
        .collect();
    for a in answers.iter().skip(answered) {
        journal.append(EventPayload::QuestionAnswered {
            node: node.to_string(),
            answer: a.answer.clone(),
            answered_by: a.answered_by.clone(),
        })?;
    }
    Ok(())
}

/// One drive-thread tick of a live attempt (spec 2026-07-20, Task 11 fix):
/// observe the channels (journal the round) AND enforce the node's
/// `question_timeout_seconds` for the currently OPEN question, all on the drive
/// thread from the adapter's `on_tick`.
///
/// When the open question has been unanswered for `>= timeout_secs`:
/// - WITH `default_answer`, post it via `post_answer_if_unanswered`
///   (`answered_by: "timeout"`, idempotent under the answers lock so it races a
///   real answer cleanly); the blocked sidecar returns it to the agent and the
///   next observation journals the `QuestionAnswered`.
/// - WITHOUT `default_answer`, return `Ok(Some(msg))` with the Task 5 wording;
///   the caller sets the abort flag so the adapter tears the agent down and the
///   attempt fails naming the node and the timeout.
///
/// Returns `Ok(None)` when nothing was enforced (no open question, not yet
/// expired, or a default was posted). Engine-owned: the injected sidecar
/// timeout is only a larger backstop, never the enforcement mechanism.
pub(crate) fn tick_live_observation(
    run_dir: &Path,
    node: &str,
    journal: &Journal,
    timeout_secs: Option<u64>,
    default_answer: Option<&str>,
) -> Result<Option<String>, EngineError> {
    observe_live_channels(run_dir, node, journal)?;
    let Some(secs) = timeout_secs else {
        return Ok(None);
    };
    let questions = read_questions_after(run_dir, None)?
        .into_iter()
        .filter(|q| q.node == node)
        .count();
    let answers = read_answers_after(run_dir, None)?
        .into_iter()
        .filter(|a| a.node == node)
        .count();
    // No open (asked-but-unanswered) question for the node: nothing to enforce.
    if questions <= answers {
        return Ok(None);
    }
    let events = read_all(run_dir)?;
    let Some(asked_ts) = last_question_asked_ts(&events, node) else {
        return Ok(None);
    };
    if apb_core::clock::now_ms().saturating_sub(asked_ts) < u128::from(secs) * 1000 {
        return Ok(None);
    }
    match default_answer {
        Some(ans) => {
            // `answers` is the channel answer count observed above;
            // `post_answer_if_unanswered` re-checks it under the answers lock,
            // so a real answer landing in the gap makes this a no-op.
            post_answer_if_unanswered(run_dir, node, ans, "timeout", answers)?;
            Ok(None)
        }
        None => Ok(Some(format!(
            "interactive node `{node}` timed out after {secs}s waiting for an answer and has no default_answer"
        ))),
    }
}
