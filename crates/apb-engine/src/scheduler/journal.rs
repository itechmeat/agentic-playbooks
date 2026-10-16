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

    /// Runs `f` with the log itself, under the lock: for the drive-thread
    /// writers that take `&mut EventLog` (frontier advance, write-offs) while
    /// a concurrent batch's workers share this journal (issue #195).
    pub(crate) fn with_log<R>(&self, f: impl FnOnce(&mut EventLog) -> R) -> R {
        let mut guard = self.log.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    /// Appends one event and returns its seq (the decision runner names its
    /// debug-state file after it).
    pub(crate) fn append_seq(&self, payload: EventPayload) -> Result<u64, EngineError> {
        Ok(self
            .log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .append(payload)?
            .seq)
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
            triage: None,
        })?;
        let _ = crate::event::propagate_wake_to_parent(run_dir, trigger, node, &detail, None);
        Ok(())
    }
}

/// Decisions are journaled mid-node through the attempt journal, the same
/// handle as `attempt_finished` (issue #165 Part 3), never the post-node batch.
impl crate::decision::DecisionJournal for Journal<'_> {
    fn append_decision(&self, payload: EventPayload) -> Result<Option<u64>, EngineError> {
        self.append_seq(payload).map(Some)
    }
}

// The two review counters live in `event` (issue #103.1): the decision
// channel validates against exactly the same predicate before it accepts a
// decision, and it must not reach up into the scheduler to do so. Re-exported
// here so the drive loop keeps naming them alongside the question counters.
pub(crate) use crate::event::{review_decided_count, review_open_count};

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

/// How long the current wait of `node` has been running at `now`, paused
/// time left out (issue #200): a run paused mid-wait resumes its timer and
/// its `timeout_seconds` where they stopped. `None` before the wait started.
pub(crate) fn wait_active_ms(events: &[Event], node: &str, now: u128) -> Option<u128> {
    last_wait_started_ts(events, node)
        .map(|start| crate::pause::active_elapsed_ms(events, start, now, now))
}

/// How long the pending question of `node` has been open at `now`, paused
/// time left out (issue #200), for `question_timeout_seconds`. `None` when
/// the node never asked.
pub(crate) fn question_active_ms(events: &[Event], node: &str, now: u128) -> Option<u128> {
    last_question_asked_ts(events, node)
        .map(|asked| crate::pause::active_elapsed_ms(events, asked, now, now))
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

/// What the attempt a `--now` pause cut short left for the resume (issue
/// #200): the binding it ran on, its agent session and the directory it ran
/// in, so the first attempt after the resume continues that session where
/// the session lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PausedSession {
    pub agent: String,
    pub model: String,
    pub session: String,
    /// The attempt's working directory, when it is recorded and still there.
    pub workdir: Option<std::path::PathBuf>,
}

/// The session `node`'s most recent finished attempt leaves for the resume,
/// when that attempt is the one a `--now` pause cut short: journaled by
/// [`journal_interrupted`] (its marker, not merely an `interrupted` status:
/// a missing verdict and a reaped attempt are `interrupted` too) with a
/// session id, and followed by the `run_paused` of that pause. `None` for
/// any other latest attempt, so a crash resume or a cycle revisit never
/// continues an old session.
///
/// The session is handed to the first resume after that pause only: a
/// `--from-node` resume (which re-runs the nodes it starts from) voids it,
/// and so does any later pause and resume of the run before the node ran
/// again.
pub(crate) fn paused_attempt_session(
    run_dir: &Path,
    events: &[Event],
    node: &str,
) -> Option<PausedSession> {
    let (at, attempt, session) = events.iter().enumerate().rev().find_map(|(i, e)| {
        match &e.payload {
            EventPayload::AttemptFinished {
                node: n,
                attempt,
                status,
                session,
                partial_output,
                ..
            } if n == node => Some((i, *attempt, status, session, partial_output)),
            _ => None,
        }
        .map(|(i, attempt, status, session, partial)| {
            let by_pause = status.as_str() == NodeStatus::Interrupted.as_str()
                && partial.as_deref() == Some(PAUSE_INTERRUPT_NOTE);
            (i, attempt, by_pause.then(|| session.clone()).flatten())
        })
    })?;
    let id = session?;
    if !first_resume_after_pause(&events[at + 1..]) {
        return None;
    }
    events[..at].iter().rev().find_map(|e| match &e.payload {
        EventPayload::AttemptStarted {
            node: n,
            attempt: a,
            agent,
            model: Some(model),
            workdir,
            ..
        } if n == node && *a == attempt => Some(PausedSession {
            agent: agent.clone(),
            model: model.clone(),
            session: id.clone(),
            workdir: workdir
                .as_deref()
                .and_then(|d| attempt_dir(run_dir, Path::new(d))),
        }),
        _ => None,
    })
}

/// `dir` when it is an isolated attempt's own directory of this run: an
/// existing directory that resolves (symlinks followed) to a path below
/// `<run_dir>/work`. Only such a directory is reused, and cleaned, for a
/// continued session; the journal is a file in the workspace, so a path it
/// names is not trusted on its own. Any other directory (a shared node
/// directory, which the node computes again anyway) means a fresh one.
/// Returns the resolved path the check validated, never the journaled one.
fn attempt_dir(run_dir: &Path, dir: &Path) -> Option<std::path::PathBuf> {
    let work = run_dir.join("work").canonicalize().ok()?;
    let dir = dir.canonicalize().ok()?;
    (dir != work && dir.starts_with(&work) && dir.is_dir()).then_some(dir)
}

/// Whether the run's lifecycle since a pause interrupt (`after`) is that
/// pause and at most the first resume after it, which did not name its
/// start node: the one shape in which the interrupted attempt's session
/// carries over.
fn first_resume_after_pause(after: &[Event]) -> bool {
    let (mut paused, mut resumed) = (false, false);
    for e in after {
        match &e.payload {
            EventPayload::RunPaused { .. } if !resumed => paused = true,
            EventPayload::RunPaused { .. } => return false,
            EventPayload::RunResumed { explicit, .. } if paused && !resumed && !explicit => {
                resumed = true;
            }
            EventPayload::RunResumed { .. } => return false,
            _ => {}
        }
    }
    paused
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

// --- issue #200: the `--now` pause interrupt ---

/// Whether a node that ended `cancelled` was cut short by a `--now` pause
/// rather than by a stop: the pause fired and no Abort did.
pub(crate) fn interrupted_by_pause(
    pause_now: &std::sync::atomic::AtomicBool,
    run_cancel: &std::sync::atomic::AtomicBool,
) -> bool {
    pause_now.load(std::sync::atomic::Ordering::SeqCst)
        && !run_cancel.load(std::sync::atomic::Ordering::SeqCst)
}

/// The `partial_output` of an attempt a `--now` pause cut short: the marker
/// [`paused_attempt_session`] keys on.
pub(crate) const PAUSE_INTERRUPT_NOTE: &str = "interrupted by a pause (--now)";

/// The agent session of attempt `attempt` of `node`, which a `--now` pause
/// cut short before it could journal its own finish: the id the attempt
/// recorded in its transcript directory when it was killed (an assigned,
/// continued or printed one), else one its raw output carries. A pause
/// during a retry backoff finds that attempt already finished: its own
/// journaled session then. `None` when the attempt never started or
/// reported no session.
fn interrupted_attempt_session(
    run_dir: &std::path::Path,
    events: &[Event],
    node: &str,
    attempt: u32,
) -> Option<String> {
    // Attempt numbers restart with every execution of the node, so only the
    // latest start of this attempt (and a finish after it) is its own.
    let start = events.iter().rposition(|e| {
        matches!(&e.payload, EventPayload::AttemptStarted { node: n, attempt: a, .. }
            if n == node && *a == attempt)
    })?;
    transcript_session(run_dir, &events[start]).or_else(|| {
        events[start..].iter().find_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node: n,
                attempt: a,
                session,
                ..
            } if n == node && *a == attempt => session.clone(),
            _ => None,
        })
    })
}

/// The session the attempt started by `start` recorded in its transcript
/// directory (see [`interrupted_attempt_session`]). A journaled transcript
/// path that does not resolve under the run directory is ignored.
fn transcript_session(run_dir: &std::path::Path, start: &Event) -> Option<String> {
    let EventPayload::AttemptStarted {
        agent,
        transcript: Some(transcript),
        ..
    } = &start.payload
    else {
        return None;
    };
    let dir = run_dir.join(transcript).canonicalize().ok()?;
    if !dir.starts_with(run_dir.canonicalize().ok()?) {
        return None;
    }
    if let Ok(id) = std::fs::read_to_string(dir.join(super::transcript::SESSION_ID_FILE)) {
        let id = id.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
    crate::adapter::capture_session(agent, &read("stdout.log"))
        .or_else(|| crate::adapter::capture_session_header(agent, &read("stderr.log")))
}

/// Journals the end of an attempt a `--now` pause cut short:
/// `attempt_finished { status: "interrupted" }` and no `node_finished`, so
/// the node stays unfinished and the resume runs it again. No retry and no
/// infrastructure retry is spent, and the attempt's own agent session id
/// ([`interrupted_attempt_session`]) is kept so a resume can continue the
/// session. The `run_paused` checkpoint follows right after it.
pub(crate) fn journal_interrupted(
    journal: &Journal,
    run_dir: &std::path::Path,
    node: &str,
) -> Result<(), crate::error::EngineError> {
    let events = crate::event::read_all(run_dir)?;
    let attempt = crate::state::RunState::fold(&events)
        .attempts
        .get(node)
        .copied()
        .unwrap_or(1);
    let session = interrupted_attempt_session(run_dir, &events, node, attempt);
    journal.append(EventPayload::AttemptFinished {
        node: node.to_string(),
        attempt,
        status: "interrupted".into(),
        duration_ms: None,
        session,
        summary: None,
        rejected_output: None,
        partial_output: Some(PAUSE_INTERRUPT_NOTE.into()),
        failure_kind: None,
        usage: None,
    })
}

// --- end of the `--now` pause interrupt ---

/// Queues the pause a quota failure asks for (issue #200, design 6) on the
/// run's own control channel, so the drive applies it at the next boundary
/// exactly like an operator's drain pause: the `run_paused` checkpoint, the
/// driver's exit and the waker for `until`.
pub(crate) fn post_quota_pause(
    run_dir: &std::path::Path,
    pause: crate::pause::PauseRequest,
) -> Result<(), crate::error::EngineError> {
    crate::control::post_control(run_dir, crate::control::Control::Pause(pause)).map(|_| ())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_stop_after_a_pause_is_not_a_pause_interrupt() {
        let (pause, cancel) = (
            std::sync::atomic::AtomicBool::new(true),
            std::sync::atomic::AtomicBool::new(false),
        );
        assert!(interrupted_by_pause(&pause, &cancel));
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(!interrupted_by_pause(&pause, &cancel));
        assert!(!interrupted_by_pause(
            &std::sync::atomic::AtomicBool::new(false),
            &cancel
        ));
    }

    use super::*;

    fn at(ts: u128, payload: EventPayload) -> Event {
        Event {
            seq: ts as u64,
            ts,
            payload,
        }
    }

    /// Issue #200: a wait timer and a question timeout do not count the time
    /// the run sat paused. Wait opened at 1 s, paused 3 s -> 2 h + 3 s, now
    /// 2 h + 6 s: 5 s of active wait, not 2 h.
    #[test]
    fn wait_and_question_clocks_leave_out_paused_time() {
        let hour: u128 = 3_600_000;
        let resumed = || EventPayload::RunResumed {
            from_node: "w".into(),
            by: None,
            explicit: false,
        };
        let waits = vec![
            at(
                1_000,
                EventPayload::WaitStarted {
                    node: "w".into(),
                    kind: "timer".into(),
                },
            ),
            at(3_000, crate::pause::engine_paused("x")),
            at(3_000 + 2 * hour, resumed()),
        ];
        assert_eq!(wait_active_ms(&waits, "w", 6_000 + 2 * hour), Some(5_000));
        assert_eq!(wait_active_ms(&waits, "other", 6_000 + 2 * hour), None);
        // Still paused: the clock stands still at 2 s.
        assert_eq!(wait_active_ms(&waits[..2], "w", 3_000 + hour), Some(2_000));

        let asked: EventPayload = serde_json::from_value(serde_json::json!({
            "type": "question_asked", "node": "q", "question": "?"
        }))
        .unwrap();
        let questions = vec![
            at(1_000, asked),
            at(3_000, crate::pause::engine_paused("x")),
            at(3_000 + 2 * hour, resumed()),
        ];
        assert_eq!(
            question_active_ms(&questions, "q", 6_000 + 2 * hour),
            Some(5_000)
        );
    }

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
                usage: None,
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

    /// Only the attempt a `--now` pause cut short, followed by that pause,
    /// hands its session to the resume: an attempt closed `interrupted` for
    /// a missing verdict or by the reaper never does, and neither does a
    /// pause interrupt whose `run_paused` never landed (the driver died).
    #[test]
    fn only_a_pause_interrupt_followed_by_its_pause_hands_over_a_session() {
        let started = |ts| {
            at(
                ts,
                EventPayload::AttemptStarted {
                    node: "work".into(),
                    attempt: 1,
                    agent: "claude".into(),
                    soul_delivery: None,
                    skills_mode: None,
                    pid: None,
                    spawn_ms: None,
                    model: Some("haiku".into()),
                    workdir: None,
                    transcript: None,
                },
            )
        };
        let finished = |ts, partial: Option<&str>| {
            at(
                ts,
                EventPayload::AttemptFinished {
                    node: "work".into(),
                    attempt: 1,
                    status: "interrupted".into(),
                    duration_ms: None,
                    session: Some("sid".into()),
                    summary: None,
                    rejected_output: None,
                    partial_output: partial.map(str::to_string),
                    failure_kind: None,
                    usage: None,
                },
            )
        };
        let paused = |ts| at(ts, crate::pause::engine_paused("x"));

        let by_pause = vec![
            started(1),
            finished(2, Some(PAUSE_INTERRUPT_NOTE)),
            paused(3),
        ];
        let got = paused_attempt_session(Path::new("/nonexistent"), &by_pause, "work")
            .expect("a paused session");
        assert_eq!(
            (got.agent.as_str(), got.model.as_str(), got.session.as_str()),
            ("claude", "haiku", "sid")
        );
        // A missing verdict, then a pause: the attempt was not cut short.
        let no_verdict = vec![started(1), finished(2, Some("partial work")), paused(3)];
        assert_eq!(
            paused_attempt_session(Path::new("/nonexistent"), &no_verdict, "work"),
            None
        );
        // The pause interrupt without its pause (a driver that died).
        let no_pause = vec![started(1), finished(2, Some(PAUSE_INTERRUPT_NOTE))];
        assert_eq!(
            paused_attempt_session(Path::new("/nonexistent"), &no_pause, "work"),
            None
        );
    }

    /// A kept session's directory is reused only when it is an attempt
    /// directory of the run (below `<run_dir>/work`, symlinks resolved);
    /// any other journaled path starts the attempt in a fresh one.
    #[cfg(unix)]
    #[test]
    fn a_kept_session_reuses_only_an_attempt_directory_of_the_run() {
        let run = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = run.path().join("work/work/1");
        std::fs::create_dir_all(&inside).unwrap();
        let link = run.path().join("work/work/2");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let session = |dir: &Path| {
            let events = vec![
                at(
                    1,
                    EventPayload::AttemptStarted {
                        node: "work".into(),
                        attempt: 1,
                        agent: "claude".into(),
                        soul_delivery: None,
                        skills_mode: None,
                        pid: None,
                        spawn_ms: None,
                        model: Some("haiku".into()),
                        workdir: Some(dir.to_string_lossy().into_owned()),
                        transcript: None,
                    },
                ),
                at(
                    2,
                    EventPayload::AttemptFinished {
                        node: "work".into(),
                        attempt: 1,
                        status: "interrupted".into(),
                        duration_ms: None,
                        session: Some("sid".into()),
                        summary: None,
                        rejected_output: None,
                        partial_output: Some(PAUSE_INTERRUPT_NOTE.into()),
                        failure_kind: None,
                        usage: None,
                    },
                ),
                at(3, crate::pause::engine_paused("x")),
            ];
            paused_attempt_session(run.path(), &events, "work")
                .expect("a paused session")
                .workdir
        };
        let canonical = inside.canonicalize().unwrap();
        assert_eq!(session(&inside), Some(canonical.clone()));
        assert_eq!(
            session(&inside.join("../1")),
            Some(canonical),
            "the path the check resolved, not the journaled one"
        );
        assert_eq!(session(outside.path()), None, "a path outside the run");
        assert_eq!(session(&link), None, "a symlink out of the run");
        assert_eq!(session(&run.path().join("work")), None, "the work root");
    }

    /// A `--now` pause during a retry backoff cuts no attempt short: the
    /// attempt that just failed already journaled its session, and that
    /// session is the one the pause keeps.
    #[test]
    fn a_pause_in_a_retry_backoff_keeps_the_failed_attempts_session() {
        let dir = tempfile::tempdir().unwrap();
        let events = vec![
            at(
                1,
                EventPayload::AttemptStarted {
                    node: "work".into(),
                    attempt: 1,
                    agent: "claude".into(),
                    soul_delivery: None,
                    skills_mode: None,
                    pid: None,
                    spawn_ms: None,
                    model: Some("haiku".into()),
                    workdir: None,
                    transcript: None,
                },
            ),
            at(
                2,
                EventPayload::AttemptFinished {
                    node: "work".into(),
                    attempt: 1,
                    status: "failed".into(),
                    duration_ms: None,
                    session: Some("sid".into()),
                    summary: None,
                    rejected_output: None,
                    partial_output: None,
                    failure_kind: None,
                    usage: None,
                },
            ),
        ];
        assert_eq!(
            interrupted_attempt_session(dir.path(), &events, "work", 1).as_deref(),
            Some("sid")
        );
        assert_eq!(
            interrupted_attempt_session(dir.path(), &events, "work", 2),
            None
        );
        // A later execution of the node restarts at attempt 1: the older
        // execution's session is not this attempt's.
        let mut rerun = events.clone();
        rerun.push(at(3, events[0].payload.clone()));
        assert_eq!(
            interrupted_attempt_session(dir.path(), &rerun, "work", 1),
            None
        );
    }

    fn started(seq: u128, transcript: Option<&str>) -> Event {
        at(
            seq,
            EventPayload::AttemptStarted {
                node: "work".into(),
                attempt: 1,
                agent: "claude".into(),
                soul_delivery: None,
                skills_mode: None,
                pid: None,
                spawn_ms: None,
                model: None,
                workdir: None,
                transcript: transcript.map(Into::into),
            },
        )
    }

    /// Only the latest start of the attempt counts: an older execution's
    /// transcript is not read when the latest start has none.
    #[test]
    fn the_session_comes_from_the_latest_start_only() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("t-old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join(super::super::transcript::SESSION_ID_FILE), "old").unwrap();
        let events = vec![started(1, Some("t-old")), started(2, None)];
        assert_eq!(
            interrupted_attempt_session(dir.path(), &events, "work", 1),
            None
        );
    }

    /// A journaled transcript path that leaves the run directory is ignored.
    #[test]
    fn a_transcript_path_outside_the_run_dir_is_ignored() {
        let base = tempfile::tempdir().unwrap();
        let run = base.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let outside = base.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join(super::super::transcript::SESSION_ID_FILE), "x").unwrap();
        let events = vec![started(1, Some("../elsewhere"))];
        assert_eq!(interrupted_attempt_session(&run, &events, "work", 1), None);
        let abs = outside.to_str().unwrap();
        let events = vec![started(1, Some(abs))];
        assert_eq!(interrupted_attempt_session(&run, &events, "work", 1), None);
        std::fs::create_dir_all(run.join("t")).unwrap();
        std::fs::write(
            run.join("t")
                .join(super::super::transcript::SESSION_ID_FILE),
            "ok",
        )
        .unwrap();
        let events = vec![started(1, Some("t"))];
        assert_eq!(
            interrupted_attempt_session(&run, &events, "work", 1).as_deref(),
            Some("ok")
        );
    }

    /// The kept session belongs to the first resume after the pause only: a
    /// `--from-node` resume and a second pause and resume cycle void it.
    #[test]
    fn a_pause_session_is_handed_to_the_first_resume_only() {
        let events = |tail: &[EventPayload]| {
            let mut v = vec![
                at(
                    1,
                    EventPayload::AttemptStarted {
                        node: "work".into(),
                        attempt: 1,
                        agent: "claude".into(),
                        soul_delivery: None,
                        skills_mode: None,
                        pid: None,
                        spawn_ms: None,
                        model: Some("haiku".into()),
                        workdir: None,
                        transcript: None,
                    },
                ),
                at(
                    2,
                    EventPayload::AttemptFinished {
                        node: "work".into(),
                        attempt: 1,
                        status: "interrupted".into(),
                        duration_ms: None,
                        session: Some("sid".into()),
                        summary: None,
                        rejected_output: None,
                        partial_output: Some(PAUSE_INTERRUPT_NOTE.into()),
                        failure_kind: None,
                        usage: None,
                    },
                ),
            ];
            v.extend(
                tail.iter()
                    .cloned()
                    .enumerate()
                    .map(|(i, p)| at(3 + i as u128, p)),
            );
            v
        };
        let paused = crate::pause::engine_paused("x");
        let resumed = |explicit| EventPayload::RunResumed {
            from_node: "work".into(),
            by: None,
            explicit,
        };
        let first = events(&[paused.clone(), resumed(false)]);
        assert!(paused_attempt_session(Path::new("/nonexistent"), &first, "work").is_some());
        let from_node = events(&[paused.clone(), resumed(true)]);
        assert_eq!(
            paused_attempt_session(Path::new("/nonexistent"), &from_node, "work"),
            None
        );
        let second_cycle = events(&[
            paused.clone(),
            resumed(false),
            paused.clone(),
            resumed(false),
        ]);
        assert_eq!(
            paused_attempt_session(Path::new("/nonexistent"), &second_cycle, "work"),
            None
        );
    }
}
