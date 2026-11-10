//! Which transitions are due: news to the subscription in force, still
//! open, and not sent, in flight or given up on yet. A pure function of
//! the journal, so a restarted driver decides exactly what the old one
//! would have, and the send-once ledger keeps it from sending twice.

use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::connector::sites::NOTIFY_SITE;
use apb_core::notify::{FinishOutcome, Transition};

use super::NotifyState;
use crate::connector::intent::{IntentRecord, IntentState, Ledger, effective_state, ledger};
use crate::event::{Event, EventPayload, WakeTrigger};
use crate::pause::kind::PauseBy;

/// How many sends one transition gets in all.
pub const MAX_TRIES: usize = 3;

/// The least time between two tries of one transition, in milliseconds.
pub const RETRY_GAP_MS: u64 = 30_000;

/// Overrides [`RETRY_GAP_MS`] (milliseconds), for tests only: a release
/// build ignores it ([`crate::env_duration::test_override`]).
pub const RETRY_GAP_ENV: &str = "APB_NOTIFY_RETRY_GAP_MS";

/// The retry gap in force: [`RETRY_GAP_MS`], shortened by
/// [`RETRY_GAP_ENV`] in a debug build only.
pub fn retry_gap_ms() -> u64 {
    crate::env_duration::test_override(std::env::var(RETRY_GAP_ENV).ok().as_deref())
        .map_or(RETRY_GAP_MS, |d| {
            u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
        })
}

/// What every notification label starts with. A caller's `--intent` never
/// does (`:` is not allowed in one), so the two never collide.
pub const LABEL_PREFIX: &str = "notify:";

/// One transition of the run: its kind and the seq of the event that
/// opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionId {
    pub kind: Transition,
    pub seq: u64,
}

impl TransitionId {
    fn new(kind: Transition, seq: u64) -> Self {
        TransitionId { kind, seq }
    }

    /// `<kind>.<seq>`, the transition's id on the run surfaces.
    pub fn id(&self) -> String {
        format!("{}.{}", self.kind.as_str(), self.seq)
    }

    /// The send-intent label of its notification.
    pub fn label(&self) -> String {
        format!("{LABEL_PREFIX}{}", self.id())
    }
}

/// The transition id a notification label names, if it is one.
pub fn transition_of(label: &str) -> Option<&str> {
    label.strip_prefix(LABEL_PREFIX)
}

/// The transitions of `events` still open at the end of the journal, in
/// journal order: a review nobody decided, a question nobody answered, a
/// host task nobody submitted, a failed or timed-out node the supervisor
/// has not acted on, a pause the engine or the supervisor made that nothing
/// resumed, and the run's end (a stop a person made is not news to them).
/// The end of the run closes every wait; a resume closes the end.
pub fn open_transitions(events: &[Event]) -> Vec<TransitionId> {
    let mut reviews: Vec<(String, u64)> = Vec::new();
    let mut questions: Vec<(String, Vec<u64>, usize)> = Vec::new();
    let mut host_tasks: Vec<(String, u64)> = Vec::new();
    let mut supervisor: Option<u64> = None;
    let mut paused: Option<u64> = None;
    let mut finished: Option<u64> = None;
    // A read follow-up opens `follow_up` at its seq; a resume or a start
    // closes it like they close the run's end. The run's finish does not
    // (the observation is news about a run that already ended).
    let mut follow_up: Option<u64> = None;
    // A stalled attempt opens `stalled` at its seq; the run's end does not
    // close it (it is news about a step, not a wait), and the send ledger
    // keeps it to one send (spec node-contracts D5).
    let mut stalled: Vec<u64> = Vec::new();
    for e in events {
        match &e.payload {
            EventPayload::HostTaskRequested { task_id, .. } => {
                host_tasks.retain(|(id, _)| id != task_id);
                host_tasks.push((task_id.clone(), e.seq));
            }
            EventPayload::HostTaskSubmitted { task_id, .. } => {
                host_tasks.retain(|(id, _)| id != task_id);
            }
            // The supervisor park of `progress::pending_supervisor_decision`:
            // a failure or a timeout waits until the supervisor retries or
            // continues, or the run pauses, is patched or ends.
            EventPayload::WakeRaised {
                trigger: WakeTrigger::NodeFailed | WakeTrigger::NodeTimeout,
                ..
            } => supervisor = Some(e.seq),
            EventPayload::SupervisorAction { action, .. }
                if crate::progress::supervisor_action_resolves_wake(action) =>
            {
                supervisor = None;
            }
            EventPayload::PatchApplied { .. } => supervisor = None,
            EventPayload::ReviewRequested { node, .. } => {
                reviews.retain(|(n, _)| n != node);
                reviews.push((node.clone(), e.seq));
            }
            EventPayload::ReviewDecided { node, .. }
            | EventPayload::ReviewWithdrawn { node, .. } => {
                reviews.retain(|(n, _)| n != node);
            }
            EventPayload::QuestionAsked { node, .. } => {
                match questions.iter_mut().find(|q| &q.0 == node) {
                    Some(q) => q.1.push(e.seq),
                    None => questions.push((node.clone(), vec![e.seq], 0)),
                }
            }
            EventPayload::QuestionAnswered { node, .. } => {
                if let Some(q) = questions.iter_mut().find(|q| &q.0 == node) {
                    q.2 += 1;
                }
            }
            EventPayload::RunPaused { by, .. } => {
                supervisor = None;
                // An older journal's pause carries no `by`; the views read
                // it as the supervisor's.
                paused = (by.unwrap_or(PauseBy::Supervisor) != PauseBy::Operator).then_some(e.seq);
            }
            EventPayload::FollowUpObserved { .. } => follow_up = Some(e.seq),
            EventPayload::RunResumed { .. } | EventPayload::RunStarted { .. } => {
                paused = None;
                finished = None;
                follow_up = None;
            }
            EventPayload::AttemptFinished {
                item: None,
                stalled: true,
                ..
            } => stalled.push(e.seq),
            EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. } => {
                reviews.clear();
                questions.clear();
                host_tasks.clear();
                supervisor = None;
                paused = None;
                let by_person = matches!(
                    &e.payload,
                    EventPayload::RunAborted {
                        by: Some(PauseBy::Operator),
                        ..
                    }
                );
                finished = (!by_person).then_some(e.seq);
            }
            _ => {}
        }
    }
    let mut out: Vec<TransitionId> = reviews
        .into_iter()
        .map(|(_, seq)| TransitionId::new(Transition::Review, seq))
        .collect();
    for (_, asked, answered) in questions {
        out.extend(
            asked
                .into_iter()
                .skip(answered)
                .map(|seq| TransitionId::new(Transition::Question, seq)),
        );
    }
    out.extend(
        host_tasks
            .into_iter()
            .map(|(_, seq)| TransitionId::new(Transition::HostTask, seq)),
    );
    out.extend(supervisor.map(|seq| TransitionId::new(Transition::Supervisor, seq)));
    out.extend(paused.map(|seq| TransitionId::new(Transition::Paused, seq)));
    out.extend(finished.map(|seq| TransitionId::new(Transition::Finished, seq)));
    out.extend(follow_up.map(|seq| TransitionId::new(Transition::FollowUp, seq)));
    out.extend(
        stalled
            .into_iter()
            .map(|seq| TransitionId::new(Transition::Stalled, seq)),
    );
    out.sort_by_key(|t| t.seq);
    out
}

/// How a `key`-mode target drops a repeat: the service's deduplication
/// window and the longest one try takes. Zero for any other mode, where a
/// try whose outcome nobody knows is never sent again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyWindow {
    pub window_ms: u64,
    pub timeout_ms: u64,
}

impl KeyWindow {
    /// A window that never closes: what may be due at most, before the
    /// target is loaded to tell.
    pub const OPEN: KeyWindow = KeyWindow {
        window_ms: u64::MAX,
        timeout_ms: 0,
    };

    /// The window of a function that declares `idempotency` with
    /// `window_sec` and answers within `timeout_sec`.
    pub fn new(window_sec: u64, timeout_sec: u64) -> Self {
        KeyWindow {
            window_ms: window_sec.saturating_mul(1000),
            timeout_ms: timeout_sec.saturating_mul(1000),
        }
    }

    /// Whether a replay of `rec` sent at `now_ms` is answered while the
    /// service still drops it (the ledger's own rule for a replay).
    fn covers(&self, rec: &IntentRecord, now_ms: u64) -> bool {
        self.window_ms > 0
            && now_ms.saturating_add(self.timeout_ms)
                <= rec.nonce_ts_ms.saturating_add(self.window_ms)
    }
}

/// How the run whose end is journaled at `seq` came out. A success is
/// `needs_look` when `needs_look` says the outcome calls for a look (it is
/// asked only for a success); a stop by anything but a person is
/// `aborted`.
pub fn finish_outcome(events: &[Event], seq: u64, needs_look: &dyn Fn() -> bool) -> FinishOutcome {
    match events.iter().find(|e| e.seq == seq).map(|e| &e.payload) {
        Some(EventPayload::RunFinished { outcome }) if outcome == "succeeded" => {
            if needs_look() {
                FinishOutcome::NeedsLook
            } else {
                FinishOutcome::Succeeded
            }
        }
        Some(EventPayload::RunFinished { outcome }) if outcome == "aborted" => {
            FinishOutcome::Aborted
        }
        Some(EventPayload::RunAborted { .. }) => FinishOutcome::Aborted,
        _ => FinishOutcome::Failed,
    }
}

/// The transitions due a notification as of `now_ms`: open, of a kind the
/// subscription covers (for the run's end, of an outcome it covers),
/// journaled at or after it took effect, and allowed another try. `window`
/// is the target's [`KeyWindow`]; `gap_ms` the least time between two
/// tries ([`retry_gap_ms`], read once by the caller, so this stays a pure
/// function of its inputs); `needs_look` tells a success that calls for a
/// look, asked only when the subscription filters outcomes.
pub fn due(
    events: &[Event],
    state: &NotifyState,
    now_ms: u64,
    window: KeyWindow,
    gap_ms: u64,
    needs_look: &dyn Fn() -> bool,
) -> Vec<TransitionId> {
    let Some(active) = &state.active else {
        return Vec::new();
    };
    let coverage = active.coverage();
    let covered = |t: &TransitionId| match t.kind {
        Transition::Finished if coverage.filters_finish() => {
            coverage.covers_finish(finish_outcome(events, t.seq, needs_look))
        }
        kind => active.on.contains(&kind),
    };
    let ledger = Ledger::fold(events);
    open_transitions(events)
        .into_iter()
        .filter(|t| t.seq >= active.since_seq && covered(t))
        .filter(|t| {
            let held: Vec<&IntentRecord> = tries(&ledger, &t.label())
                .into_iter()
                .filter(|r| active.is_after_set(r.seq, r.ts_ms) || !retired(r, now_ms))
                .collect();
            may_try(&held, now_ms, window, gap_ms)
        })
        .collect()
}

/// The notification intents recorded under `label`, oldest first.
pub fn tries<'a>(ledger: &'a Ledger, label: &str) -> Vec<&'a IntentRecord> {
    ledger
        .records()
        .iter()
        .filter(|r| r.key.node == NOTIFY_SITE && r.key.label == label)
        .collect()
}

/// Whether the trust gate refused this try for good.
fn retired(rec: &IntentRecord, now_ms: u64) -> bool {
    matches!(effective_state(rec, now_ms), IntentState::NotSent { source } if source == GATE)
}

/// Whether a transition with these earlier tries may be sent now. Nothing
/// is sent again once a try went out, is going out, or was refused by the
/// trust gate. A try that failed, or that a passing fault kept from going
/// out, is retried after `gap_ms`, up to [`MAX_TRIES`] in all. A
/// try whose outcome nobody knows is sent again only in `key` mode and only
/// while the service still drops the repeat ([`KeyWindow`]).
fn may_try(tries: &[&IntentRecord], now_ms: u64, window: KeyWindow, gap_ms: u64) -> bool {
    let Some(last) = tries.last() else {
        return true;
    };
    if tries.len() >= MAX_TRIES || now_ms < last.ts_ms.saturating_add(gap_ms) {
        return false;
    }
    match effective_state(last, now_ms) {
        IntentState::Confirmed { .. } | IntentState::Pending { .. } => false,
        IntentState::NotSent { source } => source != GATE,
        IntentState::Unknown { .. } => {
            last.mode == IdempotencyMode::Key && window.covers(last, now_ms)
        }
    }
}

/// The settlement source of a send the trust gate refused: the connector
/// or the account is not approved, or changed since the subscription
/// pinned it. It retires the transition until the subscription is set
/// again.
pub(crate) const GATE: &str = "gate";

/// The settlement source of a send refused before its intent was recorded
/// for any other reason (the target does not load, the journal did not
/// take the intent): a passing fault, tried again.
pub(crate) const REFUSED: &str = "refused";

/// The state names the run surfaces report for a try.
pub(crate) fn state_name(state: &IntentState) -> &'static str {
    match state {
        IntentState::Pending { .. } => "pending",
        IntentState::Confirmed { .. } => ledger::CONFIRMED,
        IntentState::NotSent { .. } => ledger::NOT_SENT,
        IntentState::Unknown { .. } => ledger::UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 1,
            payload,
        }
    }

    fn follow_up_observed(seq: u64) -> Event {
        ev(
            seq,
            EventPayload::FollowUpObserved {
                check: "deal_won".into(),
                status: "observed".into(),
                reason: None,
                observations: Default::default(),
                results: Vec::new(),
                verdict: "met".into(),
                finished_seq: seq - 1,
                due_at_ms: 0,
                observed_at_ms: 0,
                input_digest: String::new(),
            },
        )
    }

    /// The documented default (docs say "about 30 s"): nothing in this
    /// unit-test binary sets the override.
    #[test]
    fn a_notification_is_retried_after_thirty_seconds_by_default() {
        assert_eq!(super::retry_gap_ms(), 30_000);
    }

    /// A read follow-up opens `follow_up` at its seq, a resume or a start
    /// closes it like they close `finished`, and the run's end does not
    /// (the observation is news about a run that already ended).
    #[test]
    fn a_follow_up_observation_opens_an_opt_in_transition_a_resume_closes() {
        assert!(!Transition::DEFAULT.contains(&Transition::FollowUp));
        let events = vec![follow_up_observed(2)];
        let open = open_transitions(&events);
        let follow: Vec<_> = open
            .iter()
            .filter(|t| t.kind == Transition::FollowUp)
            .collect();
        assert_eq!(follow.len(), 1);
        assert_eq!(follow[0].seq, 2);
        assert_eq!(follow[0].label(), "notify:follow_up.2");

        // A later re-finish of the same run does not close it.
        let mut ended = events.clone();
        ended.push(ev(
            3,
            EventPayload::RunFinished {
                outcome: "succeeded".into(),
            },
        ));
        assert!(
            open_transitions(&ended)
                .iter()
                .any(|t| t.kind == Transition::FollowUp)
        );

        // A resume closes it, like it closes the run's end.
        let mut resumed = events.clone();
        resumed.push(ev(3, EventPayload::resumed("done", None, false)));
        assert!(
            !open_transitions(&resumed)
                .iter()
                .any(|t| t.kind == Transition::FollowUp)
        );

        // A start closes it too.
        let mut started = events.clone();
        started.push(ev(
            3,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
                definition: None,
                external_data: false,
            },
        ));
        assert!(
            !open_transitions(&started)
                .iter()
                .any(|t| t.kind == Transition::FollowUp)
        );
    }
}
