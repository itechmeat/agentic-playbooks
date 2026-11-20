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

/// One transition of the run: its kind, the seq of the event that opened
/// it, and which reminder of it this is (0 for the notice itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionId {
    pub kind: Transition,
    pub seq: u64,
    pub reminder: u8,
}

impl TransitionId {
    fn new(kind: Transition, seq: u64) -> Self {
        TransitionId {
            kind,
            seq,
            reminder: 0,
        }
    }

    /// `<kind>.<seq>`, the transition's id on the run surfaces; a reminder
    /// appends `.r<n>` (`review.12.r2`).
    pub fn id(&self) -> String {
        if self.reminder > 0 {
            format!("{}.{}.r{}", self.kind.as_str(), self.seq, self.reminder)
        } else {
            format!("{}.{}", self.kind.as_str(), self.seq)
        }
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
            EventPayload::RunPaused { by, reason, .. } => {
                supervisor = None;
                // A park is the engine's own way to wait on a timer, not a
                // pause someone asked for: no `paused` transition, so the
                // sweep sends nothing for it.
                if crate::pause::cause::is_park_event(reason, *by) {
                    paused = None;
                } else {
                    // An older journal's pause carries no `by`; the views
                    // read it as the supervisor's.
                    paused =
                        (by.unwrap_or(PauseBy::Supervisor) != PauseBy::Operator).then_some(e.seq);
                }
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
    let mut out: Vec<TransitionId> = open_transitions(events)
        .into_iter()
        .filter(|t| t.seq >= active.since_seq && covered(t))
        .filter(|t| {
            let held: Vec<&IntentRecord> = tries(&ledger, &t.label())
                .into_iter()
                .filter(|r| active.is_after_set(r.seq, r.ts_ms) || !retired(r, now_ms))
                .collect();
            may_try(&held, now_ms, window, gap_ms)
        })
        .collect();
    // The reminders (design D9): one per open review, question or host
    // task, the latest due threshold's, never before the notice it follows
    // and never after the wait closed.
    for t in open_transitions(events)
        .into_iter()
        .filter(|t| t.seq >= active.since_seq && REMINDED.contains(&t.kind) && covered(t))
    {
        if let Some(n) = reminder_due(events, &ledger, active, &t, now_ms, window, gap_ms) {
            out.push(TransitionId { reminder: n, ..t });
        }
    }
    out
}

/// The reminder of the wait `t` (its notice's id; the reminder number it
/// holds is ignored) due a send at `now_ms` as of `events` under `active`:
/// the latest threshold of the cadence its active waiting time reached,
/// when [`reminder_may_send`] allows it. The ordering boundary
/// (`Notifier::send`) asks it again under the journal lock and admits a
/// reminder only when it answers the number the sweep selected.
pub fn due_reminder(
    events: &[Event],
    active: &crate::notify::Active,
    t: &TransitionId,
    now_ms: u64,
    window: KeyWindow,
    gap_ms: u64,
) -> Option<u8> {
    let ledger = Ledger::fold(events);
    reminder_due(events, &ledger, active, t, now_ms, window, gap_ms)
}

/// [`due_reminder`] over a ledger the caller already folded.
fn reminder_due(
    events: &[Event],
    ledger: &Ledger,
    active: &crate::notify::Active,
    t: &TransitionId,
    now_ms: u64,
    window: KeyWindow,
    gap_ms: u64,
) -> Option<u8> {
    let waited = crate::pause::active_run_ms(
        events,
        events.iter().find(|e| e.seq == t.seq).map_or(0, |e| e.ts),
        u128::from(now_ms),
        u128::from(now_ms),
    );
    let n = active
        .remind_ms
        .iter()
        .filter(|ms| u128::from(**ms) <= waited)
        .count() as u8;
    (n > 0 && reminder_may_send(ledger, active, t, n, now_ms, window, gap_ms)).then_some(n)
}

/// Whether the wait of reminder `t` is still open and remindable as of
/// `events`, with the filters [`due`] applies: journaled at or after the
/// subscription took effect, of a reminded kind the subscription covers.
/// The ordering boundary (`Notifier::send`) asks it under the journal lock.
pub fn reminder_open(events: &[Event], active: &crate::notify::Active, t: &TransitionId) -> bool {
    t.seq >= active.since_seq
        && REMINDED.contains(&t.kind)
        && active.on.contains(&t.kind)
        && open_transitions(events)
            .iter()
            .any(|o| o.kind == t.kind && o.seq == t.seq)
}

/// The kinds of wait a reminder follows: a wait on a person. A park is not
/// one, and a parked run has none of these open (the timer wait is the
/// only frontier node).
const REMINDED: [Transition; 3] = [
    Transition::Review,
    Transition::Question,
    Transition::HostTask,
];

/// Whether reminder `n` of transition `t` may go out: its notice was
/// confirmed sent to the target in force, and the reminder's own label is
/// admitted (`may_try` over its tries, filtered as the notice's are).
/// Nothing else is checked: a first try that failed is tried again after
/// the retry gap, while `n` is the latest due threshold. The sweep and the
/// admission under the journal lock decide by this one rule, through
/// [`due_reminder`].
#[allow(clippy::too_many_arguments)]
fn reminder_may_send(
    ledger: &Ledger,
    active: &crate::notify::Active,
    t: &TransitionId,
    n: u8,
    now_ms: u64,
    window: KeyWindow,
    gap_ms: u64,
) -> bool {
    // The notice went out, confirmed, to the target in force now: the
    // notice's own label, whatever reminder id the caller holds.
    let notice_label = TransitionId { reminder: 0, ..*t }.label();
    let confirmed = tries(ledger, &notice_label).into_iter().any(|r| {
        matches!(r.state, IntentState::Confirmed { .. }) && names_current_target(r, active)
    });
    if !confirmed {
        return false;
    }
    // The reminder's own label is admitted.
    let label = format!("{LABEL_PREFIX}{}.{}.r{}", t.kind.as_str(), t.seq, n);
    let held: Vec<&IntentRecord> = tries(ledger, &label)
        .into_iter()
        .filter(|r| active.is_after_set(r.seq, r.ts_ms) || !retired(r, now_ms))
        .collect();
    may_try(&held, now_ms, window, gap_ms)
}

/// Whether a try was sent to the target in force: its intent records the
/// digest of the whole target in force now (recipient and pins included).
/// A try an older apb recorded carries no digest: it counts only on the
/// run's own subscription, naming the current target's connector, function
/// and account, recorded after that target was set in this journal, so it
/// gets no reminder after any target change.
fn names_current_target(r: &IntentRecord, active: &crate::notify::Active) -> bool {
    let t = &active.target;
    match &r.target_digest {
        Some(digest) => *digest == t.digest(),
        None => {
            active.source != apb_core::notify::NotifySource::Parent.as_str()
                && r.key.connector == t.connector
                && r.key.function == t.function
                && r.key.account == t.account
                && r.seq > active.target_set_seq
        }
    }
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

    /// An event at its own journal time, for the reminder clock.
    fn ev_at(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
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

    /// A park is no `paused` transition: the engine's own way to wait on a
    /// timer sends nothing.
    #[test]
    fn the_notify_sweep_sends_no_paused_notice_for_a_park() {
        let park = ev(
            2,
            EventPayload::RunPaused {
                reason: "wait (pause)".into(),
                by: Some(PauseBy::Engine),
                mode: Some(crate::pause::PauseMode::Drain),
                until: Some(1_000),
                keep_lock: false,
            },
        );
        assert!(
            !open_transitions(std::slice::from_ref(&park))
                .iter()
                .any(|t| t.kind == Transition::Paused),
            "no paused transition for a park"
        );
        // An engine pause that is not a park still opens the transition.
        let quota = ev(
            2,
            EventPayload::RunPaused {
                reason: "quota (a/m)".into(),
                by: Some(PauseBy::Engine),
                mode: Some(crate::pause::PauseMode::Drain),
                until: None,
                keep_lock: false,
            },
        );
        assert!(
            open_transitions(std::slice::from_ref(&quota))
                .iter()
                .any(|t| t.kind == Transition::Paused),
            "a quota pause still opens paused"
        );
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
    // --- reminders for open waits (design D9) --------------------------------

    use apb_core::connector::sites::NOTIFY_SITE;

    /// The target record of target A.
    fn target_a() -> crate::notify::record::NotifyTargetRecord {
        crate::notify::record::NotifyTargetRecord {
            connector: "chat".into(),
            account: "me".into(),
            function: "send".into(),
            to: "42".into(),
            connector_digest: "sha256:c".into(),
            account_digest: "sha256:a".into(),
        }
    }

    /// A notify_set with the reminder cadence and target A in force.
    fn subscribed(seq: u64, ts: u128, remind_ms: &[u64]) -> Event {
        ev_at(
            seq,
            ts,
            EventPayload::NotifySet {
                target: Some(target_a()),
                on: vec!["review".into(), "question".into(), "host_task".into()],
                source: "run".into(),
                by: None,
                problem: None,
                remind_ms: remind_ms.to_vec(),
            },
        )
    }

    fn review_requested(seq: u64, ts: u128) -> Event {
        ev_at(
            seq,
            ts,
            EventPayload::ReviewRequested {
                node: "gate".into(),
                options: vec!["approve".into(), "reject".into()],
                title: None,
                instruction: String::new(),
                prompt: None,
                recommendation: None,
                plan_digest: None,
                binding: None,
                person_only: false,
            },
        )
    }

    /// A confirmed send under `label` to target A, recorded at `ts`: two
    /// events, its intent and its confirmation.
    fn confirmed_notice(seq: u64, ts: u128, label: &str) -> Vec<Event> {
        confirmed_notice_to(seq, ts, label, Some(target_a().digest()))
    }

    /// [`confirmed_notice`] whose intent records `target_digest` (`None`
    /// for one an older apb recorded).
    fn confirmed_notice_to(
        seq: u64,
        ts: u128,
        label: &str,
        target_digest: Option<String>,
    ) -> Vec<Event> {
        vec![
            ev_at(
                seq,
                ts,
                EventPayload::SendIntentRecorded {
                    intent: format!("i-{seq}"),
                    nonce: format!("n-{seq}"),
                    node: NOTIFY_SITE.into(),
                    attempt: None,
                    attempt_seq: None,
                    connector: "chat".into(),
                    function: "send".into(),
                    account: "me".into(),
                    mode: "none".into(),
                    label: label.into(),
                    pid: 1,
                    deadline_ms: 0,
                    replay_of: None,
                    resend_of: None,
                    item: None,
                    target_digest,
                },
            ),
            ev_at(
                seq + 1,
                ts,
                EventPayload::SendIntentSettled {
                    intent: format!("i-{seq}"),
                    outcome: "confirmed".into(),
                    source: "response".into(),
                    detail: None,
                    effect_ref: None,
                    by: None,
                },
            ),
        ]
    }

    /// A send under `label` that was tried and did not go out, settled
    /// `not_sent` from `source`, recorded at `ts`.
    fn not_sent_notice(seq: u64, ts: u128, label: &str, source: &str) -> Vec<Event> {
        let mut events = confirmed_notice(seq, ts, label);
        events[1].payload = EventPayload::SendIntentSettled {
            intent: format!("i-{seq}"),
            outcome: "not_sent".into(),
            source: source.into(),
            detail: None,
            effect_ref: None,
            by: None,
        };
        events
    }

    fn label_of(id: &str) -> String {
        format!("{LABEL_PREFIX}{id}")
    }

    /// The reminders `due` returns, as their ids.
    fn reminders(events: &[Event], now_ms: u64) -> Vec<String> {
        let state = NotifyState::fold(events);
        due(
            events,
            &state,
            now_ms,
            KeyWindow::default(),
            30_000,
            &|| false,
        )
        .into_iter()
        .filter(|t| t.reminder > 0)
        .map(|t| t.id())
        .collect()
    }

    #[test]
    fn a_reminder_has_its_own_label() {
        let mut events = vec![
            subscribed(1, 0, &[3_600_000, 4 * 3_600_000]),
            review_requested(2, 0),
        ];
        events.extend(confirmed_notice(3, 1_000, &label_of("review.2")));
        let due = reminders(&events, 3_600_000);
        assert_eq!(due, vec!["review.2.r1".to_string()], "one reminder, r1");
        // The reminder sends under its own label, and the ledger then shows
        // two intents for the transition: the notice's and the reminder's.
        events.extend(confirmed_notice(5, 3_600_000, &label_of("review.2.r1")));
        let ledger = Ledger::fold(&events);
        assert_eq!(tries(&ledger, &label_of("review.2")).len(), 1);
        assert_eq!(tries(&ledger, &label_of("review.2.r1")).len(), 1);
        // A confirmed .r1 sends no second message while its threshold is
        // the only due one.
        assert!(reminders(&events, 2 * 3_600_000).is_empty());
        // At the next threshold its .r2 is due.
        let due = reminders(&events, 4 * 3_600_000 + 30_000);
        assert_eq!(due, vec!["review.2.r2".to_string()], "the next threshold");
    }

    #[test]
    fn no_reminder_before_the_notice_is_confirmed() {
        let events = vec![subscribed(1, 0, &[3_600_000]), review_requested(2, 0)];
        assert!(reminders(&events, 2 * 3_600_000).is_empty());
    }

    #[test]
    fn only_the_latest_due_reminder_is_sent() {
        let mut events = vec![
            subscribed(1, 0, &[3_600_000, 4 * 3_600_000]),
            review_requested(2, 0),
        ];
        events.extend(confirmed_notice(3, 1_000, &label_of("review.2")));
        // A day down: one reminder, the latest due threshold's.
        let due = reminders(&events, 25 * 3_600_000);
        assert_eq!(due, vec!["review.2.r2".to_string()]);
    }

    #[test]
    fn a_failed_reminder_is_tried_again() {
        let mut events = vec![
            subscribed(1, 0, &[3_600_000, 4 * 3_600_000]),
            review_requested(2, 0),
        ];
        events.extend(confirmed_notice(3, 1_000, &label_of("review.2")));
        // A first try of .r1 that failed, 61 minutes in.
        events.extend(not_sent_notice(
            5,
            61 * 60_000,
            &label_of("review.2.r1"),
            "refused",
        ));
        // After the retry gap .r1 is due again, while it is the latest due
        // threshold.
        let gap = 30_000;
        assert_eq!(
            reminders(&events, 61 * 60_000 + gap),
            vec!["review.2.r1".to_string()]
        );
        // MAX_TRIES tries of .r1 spent: nothing is due.
        for i in 0..2 {
            let seq = 10 + 2 * i;
            events.extend(confirmed_notice(seq, 62 * 60_000, &label_of("review.2.r1")));
        }
        assert!(reminders(&events, 63 * 60_000).is_empty());
        // At 4h the next threshold is due with no tries of its own.
        assert_eq!(
            reminders(&events, 4 * 3_600_000 + gap),
            vec!["review.2.r2".to_string()]
        );
    }

    /// A notify_set that keeps `remind` and changes the target to B.
    fn set_target_b(seq: u64, ts: u128) -> Event {
        ev_at(
            seq,
            ts,
            EventPayload::NotifySet {
                target: Some(crate::notify::record::NotifyTargetRecord {
                    connector: "chat".into(),
                    account: "her".into(),
                    function: "send".into(),
                    to: "7".into(),
                    connector_digest: "sha256:c".into(),
                    account_digest: "sha256:a".into(),
                }),
                on: vec!["review".into()],
                source: "person".into(),
                by: Some(PauseBy::Operator),
                problem: None,
                remind_ms: vec![3_600_000],
            },
        )
    }

    /// A notify_set that narrows `on`, same target as A.
    fn set_narrowed(seq: u64, ts: u128) -> Event {
        ev_at(
            seq,
            ts,
            EventPayload::NotifySet {
                target: Some(target_a()),
                on: vec!["review".into()],
                source: "person".into(),
                by: Some(PauseBy::Operator),
                problem: None,
                remind_ms: vec![3_600_000],
            },
        )
    }

    #[test]
    fn no_reminder_to_a_target_the_notice_missed() {
        // The notice confirmed while A was in force; then the target
        // changed to B.
        let mut events = vec![subscribed(1, 0, &[3_600_000]), review_requested(2, 0)];
        events.extend(confirmed_notice(3, 1_000, &label_of("review.2")));
        events.push(set_target_b(5, 2_000));
        assert!(
            reminders(&events, 2 * 3_600_000).is_empty(),
            "an open request gets no reminder after its target changed"
        );

        // A set that only narrows `on` keeps the reminders going.
        let mut narrowed = vec![subscribed(1, 0, &[3_600_000]), review_requested(2, 0)];
        narrowed.extend(confirmed_notice(3, 1_000, &label_of("review.2")));
        narrowed.push(set_narrowed(5, 2_000));
        assert_eq!(
            reminders(&narrowed, 2 * 3_600_000),
            vec!["review.2.r1".to_string()]
        );
    }

    /// Target A with another recipient only: same connector, function,
    /// account and pins.
    fn target_a_to(to: &str) -> crate::notify::record::NotifyTargetRecord {
        crate::notify::record::NotifyTargetRecord {
            to: to.into(),
            ..target_a()
        }
    }

    /// A notify_set that changes only the recipient of target A.
    fn set_recipient(seq: u64, ts: u128, to: &str) -> Event {
        ev_at(
            seq,
            ts,
            EventPayload::NotifySet {
                target: Some(target_a_to(to)),
                on: vec!["review".into()],
                source: "person".into(),
                by: Some(PauseBy::Operator),
                problem: None,
                remind_ms: vec![3_600_000],
            },
        )
    }

    #[test]
    fn no_reminder_to_a_recipient_the_notice_did_not_reach() {
        // The sweep read target A; the recipient changed to 7 in the same
        // millisecond; the notice's intent, admitted after the change,
        // went to A (its recorded target) and was confirmed.
        let mut events = vec![subscribed(1, 0, &[3_600_000]), review_requested(2, 0)];
        events.push(set_recipient(3, 2_000, "7"));
        events.extend(confirmed_notice(4, 2_000, &label_of("review.2")));
        assert!(
            reminders(&events, 2 * 3_600_000).is_empty(),
            "the new recipient got no notice, so it gets no reminder"
        );

        // A notice recorded to the recipient in force is reminded there.
        let mut events = vec![subscribed(1, 0, &[3_600_000]), review_requested(2, 0)];
        events.push(set_recipient(3, 2_000, "7"));
        events.extend(confirmed_notice_to(
            4,
            2_000,
            &label_of("review.2"),
            Some(target_a_to("7").digest()),
        ));
        assert_eq!(
            reminders(&events, 2 * 3_600_000),
            vec!["review.2.r1".to_string()]
        );
    }

    #[test]
    fn a_notice_without_a_target_digest_is_reminded_only_without_a_target_change() {
        // An older apb's notice, no target change since: reminded.
        let mut events = vec![subscribed(1, 0, &[3_600_000]), review_requested(2, 0)];
        events.extend(confirmed_notice_to(3, 1_000, &label_of("review.2"), None));
        assert_eq!(
            reminders(&events, 2 * 3_600_000),
            vec!["review.2.r1".to_string()]
        );
        // The recipient changed after it: no reminder.
        events.push(set_recipient(5, 2_000, "7"));
        assert!(reminders(&events, 2 * 3_600_000).is_empty());
    }

    #[test]
    fn paused_time_does_not_count_toward_a_reminder() {
        let mut events = vec![subscribed(1, 0, &[3_600_000]), review_requested(2, 0)];
        events.extend(confirmed_notice(3, 1_000, &label_of("review.2")));
        // The run was paused 90 minutes of the two wall hours.
        events.push(ev_at(
            5,
            1_000,
            EventPayload::RunPaused {
                reason: "looking".into(),
                by: Some(PauseBy::Operator),
                mode: Some(crate::pause::PauseMode::Drain),
                until: None,
                keep_lock: false,
            },
        ));
        events.push(ev_at(
            6,
            1_000 + 90 * 60_000,
            EventPayload::resumed("gate", None, false),
        ));
        // Two wall hours are 30 active minutes: no reminder yet.
        assert!(reminders(&events, 2 * 3_600_000).is_empty());
        // One active hour later it is due.
        assert_eq!(
            reminders(&events, 2 * 3_600_000 + 30 * 60_000),
            vec!["review.2.r1".to_string()]
        );
    }

    #[test]
    fn no_reminder_for_a_closed_wait() {
        let mut events = vec![subscribed(1, 0, &[3_600_000]), review_requested(2, 0)];
        events.extend(confirmed_notice(3, 1_000, &label_of("review.2")));
        events.push(ev_at(
            5,
            60_000,
            EventPayload::ReviewDecided {
                node: "gate".into(),
                decision: "approve".into(),
                note: String::new(),
                plan_digest: None,
                decided_by: None,
                binding: None,
                via: None,
            },
        ));
        assert!(reminders(&events, 2 * 3_600_000).is_empty());
    }

    #[test]
    fn only_reviews_questions_and_host_tasks_are_reminded() {
        // A question is reminded.
        let mut question = vec![subscribed(1, 0, &[3_600_000])];
        question.push(ev_at(
            2,
            0,
            EventPayload::QuestionAsked {
                node: "ask".into(),
                question: "go on?".into(),
                options: Vec::new(),
            },
        ));
        question.extend(confirmed_notice(3, 1_000, &label_of("question.2")));
        assert_eq!(
            reminders(&question, 2 * 3_600_000),
            vec!["question.2.r1".to_string()]
        );

        // A host task is reminded.
        let mut host = vec![subscribed(1, 0, &[3_600_000])];
        host.push(ev_at(
            2,
            0,
            EventPayload::HostTaskRequested {
                task_id: "t-1".into(),
                node: "work".into(),
                attempt: 1,
                prompt_ref: String::new(),
                role_prompt_ref: None,
                skills: Vec::new(),
                workdir: String::new(),
                outputs: None,
                deadline_ms: None,
                model_hint: None,
                fallback_of: None,
                hint_note: None,
                hint_source: None,
            },
        ));
        host.extend(confirmed_notice(3, 1_000, &label_of("host_task.2")));
        assert_eq!(
            reminders(&host, 2 * 3_600_000),
            vec!["host_task.2.r1".to_string()]
        );

        // A stalled step and a person's pause are not, though the
        // subscription covers both: the kind rule alone keeps them out.
        let covers_all = || {
            ev_at(
                1,
                0,
                EventPayload::NotifySet {
                    target: Some(target_a()),
                    on: vec!["stalled".into(), "paused".into()],
                    source: "run".into(),
                    by: None,
                    problem: None,
                    remind_ms: vec![3_600_000],
                },
            )
        };
        let mut paused = vec![covers_all()];
        paused.push(ev_at(
            2,
            0,
            EventPayload::RunPaused {
                reason: "looking".into(),
                by: Some(PauseBy::Operator),
                mode: Some(crate::pause::PauseMode::Drain),
                until: None,
                keep_lock: false,
            },
        ));
        paused.extend(confirmed_notice(3, 1_000, &label_of("paused.2")));
        assert!(reminders(&paused, 2 * 3_600_000).is_empty());
        let mut stalled = vec![covers_all()];
        stalled.push(ev_at(
            2,
            0,
            EventPayload::WakeRaised {
                trigger: crate::event::WakeTrigger::NodeTimeout,
                node: "fix".into(),
                detail: String::new(),
                triage: None,
            },
        ));
        stalled.extend(confirmed_notice(3, 1_000, &label_of("stalled.2")));
        assert!(reminders(&stalled, 2 * 3_600_000).is_empty());
    }

    #[test]
    fn a_park_is_never_reminded() {
        // A run parked on a timer wait for two hours under remind [1h]:
        // nothing is due (a park is not a reminded kind, and no reminded
        // wait is open while a run is parked).
        let mut events = vec![subscribed(1, 0, &[3_600_000])];
        events.push(ev_at(
            2,
            0,
            EventPayload::RunPaused {
                reason: "wait (pause)".into(),
                by: Some(PauseBy::Engine),
                mode: Some(crate::pause::PauseMode::Drain),
                until: Some(2 * 3_600_000),
                keep_lock: false,
            },
        ));
        assert!(reminders(&events, 2 * 3_600_000).is_empty());
        assert!(reminders(&events, 3 * 3_600_000).is_empty());
    }
}
