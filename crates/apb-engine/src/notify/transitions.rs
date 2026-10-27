//! Which transitions are due: news to the subscription in force, still
//! open, and not sent, in flight or given up on yet. A pure function of
//! the journal, so a restarted driver decides exactly what the old one
//! would have, and the send-once ledger keeps it from sending twice.

use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::connector::sites::NOTIFY_SITE;
use apb_core::notify::Transition;

use super::NotifyState;
use crate::connector::intent::{IntentRecord, IntentState, Ledger, effective_state, ledger};
use crate::event::{Event, EventPayload};
use crate::pause::kind::PauseBy;

/// How many sends one transition gets in all.
pub const MAX_TRIES: usize = 3;

/// The least time between two tries of one transition, in milliseconds.
pub const RETRY_GAP_MS: u64 = 30_000;

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
/// pause the engine or the supervisor made that nothing resumed, and the
/// run's end (a stop a person made is not news to them). The end of the
/// run closes every wait; a resume closes the end.
pub fn open_transitions(events: &[Event]) -> Vec<TransitionId> {
    let mut reviews: Vec<(String, u64)> = Vec::new();
    let mut questions: Vec<(String, Vec<u64>, usize)> = Vec::new();
    let mut paused: Option<u64> = None;
    let mut finished: Option<u64> = None;
    for e in events {
        match &e.payload {
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
                // An older journal's pause carries no `by`; the views read
                // it as the supervisor's.
                paused = (by.unwrap_or(PauseBy::Supervisor) != PauseBy::Operator).then_some(e.seq);
            }
            EventPayload::RunResumed { .. } | EventPayload::RunStarted { .. } => {
                paused = None;
                finished = None;
            }
            EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. } => {
                reviews.clear();
                questions.clear();
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
    out.extend(paused.map(|seq| TransitionId::new(Transition::Paused, seq)));
    out.extend(finished.map(|seq| TransitionId::new(Transition::Finished, seq)));
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

/// The transitions due a notification as of `now_ms`: open, of a kind the
/// subscription covers, journaled at or after it took effect, and allowed
/// another try. `window` is the target's [`KeyWindow`].
pub fn due(
    events: &[Event],
    state: &NotifyState,
    now_ms: u64,
    window: KeyWindow,
) -> Vec<TransitionId> {
    let Some(active) = &state.active else {
        return Vec::new();
    };
    let ledger = Ledger::fold(events);
    open_transitions(events)
        .into_iter()
        .filter(|t| t.seq >= active.since_seq && active.on.contains(&t.kind))
        .filter(|t| {
            let held: Vec<&IntentRecord> = tries(&ledger, &t.label())
                .into_iter()
                .filter(|r| active.is_after_set(r.seq, r.ts_ms) || !retired(r, now_ms))
                .collect();
            may_try(&held, now_ms, window)
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
/// out, is retried after [`RETRY_GAP_MS`], up to [`MAX_TRIES`] in all. A
/// try whose outcome nobody knows is sent again only in `key` mode and only
/// while the service still drops the repeat ([`KeyWindow`]).
fn may_try(tries: &[&IntentRecord], now_ms: u64, window: KeyWindow) -> bool {
    let Some(last) = tries.last() else {
        return true;
    };
    if tries.len() >= MAX_TRIES || now_ms < last.ts_ms.saturating_add(RETRY_GAP_MS) {
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
