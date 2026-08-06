//! Read-side folds over a run's append-only journal.
//!
//! The drive loop asks the same questions of the event log over and over -
//! how many times has this node started, was its question answered, when did
//! the current visit begin - and every answer is a pure fold over the events.
//! They live here so the loop reads as decisions rather than counting, and so
//! the counting can be tested without a running scheduler.
//! Shares the parent module's imports via `use super::*`.

use super::*;

/// A shared append handle over the run's single event log, used only for the
/// two attempt-lifecycle events that are journaled off the return batch:
/// `attempt_started` at spawn time (so a crash mid-attempt leaves an open
/// attempt on disk) and `attempt_finished` at return time. Every OTHER event
/// keeps drive's direct, return-batch write path.
///
/// The log is wrapped in a `Mutex` so the same handle serves both drive paths:
/// the sequential path holds the sole reference, while the parallel batch path
/// shares `&Journal` across scoped worker threads. Each append is a single
/// atomic line write, so lock contention is irrelevant.
pub(crate) struct Journal<'a> {
    log: std::sync::Mutex<&'a mut EventLog>,
}

impl<'a> Journal<'a> {
    pub(crate) fn new(log: &'a mut EventLog) -> Self {
        Self {
            log: std::sync::Mutex::new(log),
        }
    }

    /// Appends one event under the lock. Poison-tolerant: a worker thread that
    /// panicked mid-append would poison the mutex, but the log file itself is
    /// append-only and a partial line is impossible (a single `writeln!`), so
    /// recovering the inner guard and continuing is safe.
    pub(crate) fn append(&self, payload: EventPayload) -> Result<(), EngineError> {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .append(payload)?;
        Ok(())
    }

    /// Journals a wake and mirrors it to a parent run when nested (issue #45
    /// finding 8). Same contract as [`crate::event::raise_wake`].
    pub(crate) fn raise_wake(
        &self,
        run_dir: &Path,
        trigger: crate::event::WakeTrigger,
        node: &str,
        detail: impl Into<String>,
    ) -> Result<(), EngineError> {
        let detail = detail.into();
        self.append(EventPayload::WakeRaised {
            trigger,
            node: node.to_string(),
            detail: detail.clone(),
        })?;
        let _ = crate::event::propagate_wake_to_parent(run_dir, trigger, node, &detail);
        Ok(())
    }
}

pub(crate) fn review_decided_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ReviewDecided { node: n, .. } if n == node))
        .count()
}

pub(crate) fn review_requested_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node: n, .. } if n == node),
        )
        .count()
}

/// How many `QuestionAsked` events a node already carries. Mirrors
/// `review_requested_count`: drive declares a question once per suspension and
/// this count keeps the declaration loop-resilient (spec 2026-07-20).
pub(crate) fn question_asked_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::QuestionAsked { node: n, .. } if n == node))
        .count()
}

/// How many `QuestionAnswered` events a node already carries. Mirrors
/// `review_decided_count`: the N-th answer for a node is consumed once there
/// are already N `QuestionAnswered` events for it (count-based consumption).
pub(crate) fn question_answered_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::QuestionAnswered { node: n, .. } if n == node),
        )
        .count()
}

/// How many `AttemptStarted` events a node already carries - the number of the
/// current attempt while one is open, used to tag a posted question.
pub(crate) fn attempt_started_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node))
        .count()
}

/// How many `NodeStarted` / `NodeFinished` events a node already carries. A
/// node visit is "open" when started exceeds finished; the interactive branch
/// uses this to journal `NodeStarted` exactly once per visit (like the wait
/// branch's `wait_started_count`/`wait_ended_count` guard), so re-invocations
/// and park spins within the same visit do not re-declare it.
pub(crate) fn node_started_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .count()
}

pub(crate) fn node_finished_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
        .count()
}

/// Whether `questions.jsonl` already carries an unanswered question for this
/// node: more question entries than answer entries for it (count-based
/// consumption). Guards against a duplicate channel append when drive crashed
/// after `post_question` but before journaling `QuestionAsked` - on resume the
/// journal-count declare-once guard would re-post, but the channel entry is
/// already there. Keyed on the unanswered tail rather than the attempt number:
/// the attempt drifts across a crash-resume (the pre-crash `AttemptStarted` is
/// durable, so the resumed post computes a higher attempt), which is exactly
/// the crash window this guard must cover.
pub(crate) fn node_has_unanswered_channel_question(
    run_dir: &Path,
    node: &str,
) -> Result<bool, EngineError> {
    let questions = read_questions_after(run_dir, None)?
        .into_iter()
        .filter(|q| q.node == node)
        .count();
    let answers = read_answers_after(run_dir, None)?
        .into_iter()
        .filter(|a| a.node == node)
        .count();
    Ok(questions > answers)
}

pub(crate) fn wait_started_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::WaitStarted { node: n, .. } if n == node))
        .count()
}

pub(crate) fn wait_ended_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::WaitSignalled { node: n } if n == node)
                || matches!(&e.payload, EventPayload::WaitTimeout { node: n } if n == node)
        })
        .count()
}

/// How many times the wait node's waiting resolved specifically via a SIGNAL
/// (not a timeout) - i.e. how many webhook signals this node has already
/// consumed across past visits.
pub(crate) fn wait_signalled_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::WaitSignalled { node: n } if n == node))
        .count()
}

pub(crate) fn last_wait_started_ts(events: &[Event], node: &str) -> Option<u128> {
    events
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::WaitStarted { node: n, .. } if n == node))
        .map(|e| e.ts)
}

/// The `ts` of the most recent `QuestionAsked` event for `node` - the moment
/// the currently pending (unanswered) question was raised, since by
/// construction only one question is ever open per node at a time (spec
/// 2026-07-20, Task 5). Used by the interactive park loop to evaluate
/// `question_timeout_seconds` expiry.
pub(crate) fn last_question_asked_ts(events: &[Event], node: &str) -> Option<u128> {
    events
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::QuestionAsked { node: n, .. } if n == node))
        .map(|e| e.ts)
}

/// The session id captured from the node's MOST RECENT finished attempt (spec
/// 2026-07-20, Task 7). That attempt is the one that asked the pending
/// question, so its `AttemptFinished.session` is the session to resume into.
/// `None` when the latest attempt captured none (the drive loop then downgrades
/// `resume` to `reprompt`). Deliberately keyed on the latest attempt, not the
/// latest attempt WITH a session: a fresh attempt that captured nothing must
/// not silently resume a stale session from an earlier round.
pub(crate) fn last_attempt_session(events: &[Event], node: &str) -> Option<String> {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node: n, session, ..
            } if n == node => Some(session.clone()),
            _ => None,
        })
        .flatten()
}

/// Whether `node`'s MOST RECENT finished attempt was journaled `interrupted`
/// (spec 2026-08-05 section 2.2 composed with 2.4), i.e. ended without recording
/// a required verdict.
///
/// Exists because the in-attempt flag that delivers the interruption note lives
/// in ONE `execute_node` call: an attempt closed as interrupted by a DIFFERENT
/// execution - the drive-entry reaper after a driver death, a supervisor retry, a
/// `--from-node` re-run - is invisible to it. The journal is the only thing those
/// paths share, so the note is seeded from this fold instead. Keyed on the LATEST
/// attempt, exactly like [`last_attempt_session`]: a later attempt that finished
/// some other way has already recovered, and a note about it would be stale.
pub(crate) fn last_attempt_interrupted(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node: n, status, ..
            } if n == node => Some(status.as_str() == NodeStatus::Interrupted.as_str()),
            _ => None,
        })
        .unwrap_or(false)
}

/// The primary executor's `(agent_id, interaction)` for a node, read from the
/// run's immutable manifest snapshot (spec 2026-07-20, Task 7). `None` when the
/// run has no manifest or the node has no bound profile chain - the caller then
/// treats the node as `Reprompt` (the safe floor). The interaction ceiling is
/// snapshotted at run start, so editing the live config mid-run cannot change
/// the transport an in-flight node uses.
pub(crate) fn node_primary_invocation(
    run_dir: &Path,
    node: &str,
) -> Result<Option<(String, Interaction)>, EngineError> {
    let Some(manifest) = crate::manifest::read(run_dir)? else {
        return Ok(None);
    };
    // The EFFECTIVE binding: a rebound node's interaction ceiling comes from its
    // new profile (issue #45 finding 5).
    Ok(effective_for_node(run_dir, &manifest, node)?
        .and_then(|p| p.chain.first().cloned())
        .map(|ri| (ri.agent_id.clone(), ri.spec.interaction)))
}

/// The `seq` of the node's most recent `NodeStarted` - the start of the CURRENT
/// visit (spec 2026-07-20, Task 6 carry-over fix). Used to scope a reprompt
/// transcript to this visit's Q&A so a looped re-entry does not replay prior
/// visits' rounds. `0` when the node has never started (no prior questions
/// to exclude).
pub(crate) fn current_visit_start_seq(events: &[Event], node: &str) -> u64 {
    events
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .map(|e| e.seq)
        .unwrap_or(0)
}

/// How many `QuestionAsked` events for `node` were journaled BEFORE `seq` -
/// i.e. in prior visits, given `seq` is the current visit's `NodeStarted`
/// (spec 2026-07-20, Task 6). The i-th channel question for a node corresponds
/// to its i-th `QuestionAsked`, so skipping this many channel questions yields
/// the current visit's window.
pub(crate) fn questions_asked_before_seq(events: &[Event], node: &str, seq: u64) -> usize {
    events
        .iter()
        .filter(|e| {
            e.seq < seq
                && matches!(&e.payload, EventPayload::QuestionAsked { node: n, .. } if n == node)
        })
        .count()
}

/// How many `QuestionAnswered` events for `node` were journaled BEFORE `seq`
/// (the current visit's `NodeStarted`). The reprompt transcript skips this many
/// channel answers so it pairs only the current visit's Q&A (spec 2026-07-20,
/// Task 6).
pub(crate) fn questions_answered_before_seq(events: &[Event], node: &str, seq: u64) -> usize {
    events
        .iter()
        .filter(|e| {
            e.seq < seq
                && matches!(&e.payload, EventPayload::QuestionAnswered { node: n, .. } if n == node)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(seq: u64, node: &str, status: &str) -> Event {
        Event {
            seq,
            ts: 1_000 + seq as u128,
            payload: EventPayload::AttemptFinished {
                node: node.into(),
                attempt: 1,
                status: status.into(),
                duration_ms: None,
                session: None,
                summary: None,
                rejected_output: None,
                partial_output: None,
                failure_kind: None,
            },
        }
    }

    #[test]
    fn a_node_with_no_finished_attempt_is_not_interrupted() {
        assert!(!last_attempt_interrupted(&[], "work"));
        assert!(!last_attempt_interrupted(
            &[finished(0, "other", "interrupted")],
            "work"
        ));
    }

    #[test]
    fn the_latest_interrupted_attempt_is_reported() {
        let events = vec![
            finished(0, "work", "failed"),
            finished(1, "work", "interrupted"),
        ];
        assert!(last_attempt_interrupted(&events, "work"));
    }

    /// Keyed on the LATEST attempt, not on any interrupted one in history: a node
    /// that went on to finish some other way has already recovered, and a note
    /// about the older interruption would be stale.
    #[test]
    fn an_interruption_followed_by_another_outcome_is_not_reported() {
        let events = vec![
            finished(0, "work", "interrupted"),
            finished(1, "work", "succeeded"),
        ];
        assert!(!last_attempt_interrupted(&events, "work"));
    }
}
