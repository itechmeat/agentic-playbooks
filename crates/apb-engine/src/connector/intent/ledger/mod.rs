//! The send ledger: one fold over a run's journal that says, for every send
//! intent, whether it went out, did not, or nobody knows; and the decision a
//! repeat of a send gets from it.
//!
//! The ledger lives in the journal itself (`send_intent_recorded`,
//! `send_intent_settled`), so there is no second store to keep in step. The
//! call path, the retry briefing and the run outcome all read it through
//! [`Ledger::fold`].

use std::collections::HashMap;

use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::schema::OnUnknown;

use super::identity::IntentKey;
use crate::event::{Event, EventPayload};

/// What became of one intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentState {
    /// Recorded, not settled yet: the sending process `pid` is on it until
    /// `deadline_ms`.
    Pending { pid: u32, deadline_ms: u64 },
    /// It went out.
    Confirmed {
        source: String,
        effect_ref: Option<String>,
    },
    /// It did not go out: refused or failed before it could act.
    NotSent { source: String },
    /// It may or may not have gone out.
    Unknown {
        source: String,
        detail: Option<String>,
    },
}

/// The settlement outcomes as the journal writes them.
pub const CONFIRMED: &str = "confirmed";
pub const NOT_SENT: &str = "not_sent";
pub const UNKNOWN: &str = "unknown";

/// The source of a pending intent read as unknown because its sender is gone
/// or its deadline passed.
pub const ORPHAN: &str = "orphan";

/// The source of a person's settlement.
pub const PERSON: &str = "person";

/// One recorded intent and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRecord {
    pub id: String,
    /// The seq of its `send_intent_recorded`.
    pub seq: u64,
    pub ts_ms: u64,
    pub key: IntentKey,
    pub nonce: String,
    pub mode: IdempotencyMode,
    pub attempt: Option<u32>,
    pub attempt_seq: Option<u64>,
    pub replay_of: Option<String>,
    pub resend_of: Option<String>,
    /// When this nonce first went out: the record's own time, or for a
    /// replay the time of the record that started the chain. A key window is
    /// counted from here.
    pub nonce_ts_ms: u64,
    pub state: IntentState,
    /// Who settled it, for a person's decision.
    pub settled_by: Option<String>,
    /// A later intent that replays or resends this one.
    pub superseded_by: Option<String>,
}

/// Every intent of a run, in journal order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    records: Vec<IntentRecord>,
    /// Intent id to its index in `records`.
    index: HashMap<String, usize>,
}

impl Ledger {
    /// Folds the journal. A later settlement wins, except that `confirmed`
    /// is final: once an effect is known to exist, no later answer of a
    /// process unmakes it. Only a person's settlement changes it again.
    pub fn fold(events: &[Event]) -> Self {
        let mut ledger = Ledger::default();
        for event in events {
            match &event.payload {
                EventPayload::SendIntentRecorded { .. } => ledger.record(event),
                EventPayload::SendIntentSettled {
                    intent,
                    outcome,
                    source,
                    detail,
                    effect_ref,
                    by,
                } => {
                    if let Some(&at) = ledger.index.get(intent) {
                        settle(
                            &mut ledger.records[at],
                            outcome,
                            source,
                            detail,
                            effect_ref,
                            by,
                        );
                    }
                }
                _ => {}
            }
        }
        ledger
    }

    fn record(&mut self, event: &Event) {
        let EventPayload::SendIntentRecorded {
            intent,
            nonce,
            node,
            attempt,
            attempt_seq,
            connector,
            function,
            account,
            mode,
            label,
            pid,
            deadline_ms,
            replay_of,
            resend_of,
        } = &event.payload
        else {
            return;
        };
        let ts_ms = event.ts as u64;
        // A link counts only when it names an earlier, different intent: a
        // self link or a forward one (a corrupt or hand-edited journal)
        // would make a chain a loop.
        let earlier = |of: &Option<String>| {
            of.as_ref()
                .filter(|of| *of != intent && self.index.contains_key(of.as_str()))
                .cloned()
        };
        let (replay_of, resend_of) = (&earlier(replay_of), &earlier(resend_of));
        // A replay carries the nonce of an earlier member of its chain: its
        // window counts from the first record that sent that nonce.
        let nonce_ts_ms = replay_of
            .as_ref()
            .and_then(|of| self.get(of))
            .and_then(|prev| {
                self.chain_of(prev)
                    .into_iter()
                    .find(|r| &r.nonce == nonce)
                    .map(|r| r.nonce_ts_ms)
            })
            .unwrap_or(ts_ms);
        for earlier in [replay_of, resend_of].into_iter().flatten() {
            if let Some(&at) = self.index.get(earlier) {
                self.records[at].superseded_by = Some(intent.clone());
            }
        }
        self.index.insert(intent.clone(), self.records.len());
        self.records.push(IntentRecord {
            id: intent.clone(),
            seq: event.seq,
            ts_ms,
            key: IntentKey {
                node: node.clone(),
                connector: connector.clone(),
                function: function.clone(),
                account: account.clone(),
                label: label.clone(),
            },
            nonce: nonce.clone(),
            mode: IdempotencyMode::parse(mode),
            attempt: *attempt,
            attempt_seq: *attempt_seq,
            replay_of: replay_of.clone(),
            resend_of: resend_of.clone(),
            nonce_ts_ms,
            state: IntentState::Pending {
                pid: *pid,
                deadline_ms: *deadline_ms,
            },
            settled_by: None,
            superseded_by: None,
        });
    }

    /// Every intent, in journal order.
    pub fn records(&self) -> &[IntentRecord] {
        &self.records
    }

    pub fn get(&self, id: &str) -> Option<&IntentRecord> {
        self.index.get(id).map(|&at| &self.records[at])
    }

    /// The chain `rec` belongs to, in journal order: the send it started
    /// from and every later intent that replays or resends it. Each repeat
    /// links to the latest member, so a chain is a line. Both walks only
    /// move strictly back or forward in the journal, so a link loop in a
    /// corrupt journal (a duplicated intent id) ends the walk instead of
    /// spinning.
    pub fn chain_of<'a>(&'a self, rec: &'a IntentRecord) -> Vec<&'a IntentRecord> {
        let mut root = rec;
        while let Some(prev) = root
            .replay_of
            .as_deref()
            .or(root.resend_of.as_deref())
            .and_then(|id| self.get(id))
            .filter(|prev| prev.seq < root.seq)
        {
            root = prev;
        }
        let mut chain = vec![root];
        let mut at = root;
        while let Some(next) = at
            .superseded_by
            .as_deref()
            .and_then(|id| self.get(id))
            .filter(|next| next.seq > at.seq)
        {
            chain.push(next);
            at = next;
        }
        chain
    }

    /// The one send `rec` belongs to, in journal order: the members of its
    /// chain that carry its nonce. A replay repeats a send with the same
    /// nonce, so the service acts on it at most once; a resend is a
    /// separate send with a fresh nonce.
    pub fn send_of<'a>(&'a self, rec: &'a IntentRecord) -> Vec<&'a IntentRecord> {
        self.chain_of(rec)
            .into_iter()
            .filter(|m| m.nonce == rec.nonce)
            .collect()
    }

    /// The latest intent of `key` recorded at or after `floor` (a seq).
    pub fn latest(&self, key: &IntentKey, floor: u64) -> Option<&IntentRecord> {
        self.records
            .iter()
            .rev()
            .find(|r| r.seq >= floor && &r.key == key)
    }
}

fn settle(
    rec: &mut IntentRecord,
    outcome: &str,
    source: &str,
    detail: &Option<String>,
    effect_ref: &Option<String>,
    by: &Option<String>,
) {
    if matches!(rec.state, IntentState::Confirmed { .. }) && source != PERSON {
        return;
    }
    let next = match outcome {
        CONFIRMED => IntentState::Confirmed {
            source: source.to_string(),
            effect_ref: effect_ref.clone(),
        },
        NOT_SENT => IntentState::NotSent {
            source: source.to_string(),
        },
        _ => IntentState::Unknown {
            source: source.to_string(),
            detail: detail.clone(),
        },
    };
    rec.state = next;
    rec.settled_by = by.clone();
}

/// Where the effect scope of `node` starts: the first seq after its last
/// `node_finished` with status `succeeded`, or 0 when it never succeeded.
/// Intents recorded from there on count; a loop visit after a success starts
/// afresh, so a send that belongs to each iteration is allowed again.
pub fn effect_scope_floor(events: &[Event], node: &str) -> u64 {
    events
        .iter()
        .rev()
        .find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeFinished { node: n, status, .. }
                    if n == node && status == "succeeded"
            )
        })
        .map_or(0, |e| e.seq + 1)
}

/// The state a record reads as now: a pending intent whose deadline passed
/// or whose sender is gone is an orphan, read as unknown.
pub fn effective_state(rec: &IntentRecord, now_ms: u64) -> IntentState {
    effective_state_with(rec, now_ms, &crate::liveness::pid_alive)
}

/// What a chain of intents says together, and the member that says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainVerdict<'a> {
    pub state: IntentState,
    pub member: &'a IntentRecord,
}

/// The verdict of the chain `rec` belongs to, read as of `now_ms`: what a
/// repeat of the send gets. Any member confirmed: the send went out. Else a
/// member still in flight. Else a member whose outcome is unknown: a replay
/// or a resend that did not go out says only that it did not act, so the
/// earlier unknown send stays unknown until it is found, replayed to an
/// answer, or a person settles it. Else nothing went out. The call path and
/// the retry briefing read a repeat through this verdict; whether one member
/// is open for a person is [`send_verdict`], per send.
pub fn chain_verdict<'a>(
    ledger: &'a Ledger,
    rec: &'a IntentRecord,
    now_ms: u64,
) -> ChainVerdict<'a> {
    chain_verdict_with(ledger, rec, now_ms, &crate::liveness::pid_alive)
}

/// The verdict of the one send `rec` belongs to ([`Ledger::send_of`]), by
/// the same rule as [`chain_verdict`]. Replays of a send share its one
/// effect, so a replay that went out answers for them all; a resend is a
/// separate send, so an earlier unknown member stays unknown (and open for a
/// person) even when a later resend went out, since both may have acted.
/// The run outcome and a person's decision read a member through this.
pub fn send_verdict<'a>(
    ledger: &'a Ledger,
    rec: &'a IntentRecord,
    now_ms: u64,
) -> ChainVerdict<'a> {
    members_verdict(
        ledger.send_of(rec),
        rec,
        now_ms,
        &crate::liveness::pid_alive,
    )
}

/// [`chain_verdict`] with the process check given.
fn chain_verdict_with<'a>(
    ledger: &'a Ledger,
    rec: &'a IntentRecord,
    now_ms: u64,
    alive: &dyn Fn(u32) -> bool,
) -> ChainVerdict<'a> {
    members_verdict(ledger.chain_of(rec), rec, now_ms, alive)
}

/// The verdict of `members` (in journal order, `rec` among them).
fn members_verdict<'a>(
    chain: Vec<&'a IntentRecord>,
    rec: &'a IntentRecord,
    now_ms: u64,
    alive: &dyn Fn(u32) -> bool,
) -> ChainVerdict<'a> {
    let states: Vec<(IntentState, &IntentRecord)> = chain
        .iter()
        .map(|m| (effective_state_with(m, now_ms, alive), *m))
        .collect();
    let latest = |want: fn(&IntentState) -> bool| {
        states
            .iter()
            .rev()
            .find(|(s, _)| want(s))
            .map(|(state, member)| ChainVerdict {
                state: state.clone(),
                member,
            })
    };
    latest(|s| matches!(s, IntentState::Confirmed { .. }))
        .or_else(|| latest(|s| matches!(s, IntentState::Pending { .. })))
        .or_else(|| latest(|s| matches!(s, IntentState::Unknown { .. })))
        .unwrap_or_else(|| {
            let (state, member) = states.last().cloned().unwrap_or((rec.state.clone(), rec));
            ChainVerdict { state, member }
        })
}

/// [`effective_state`] with the process check given.
fn effective_state_with(
    rec: &IntentRecord,
    now_ms: u64,
    alive: &dyn Fn(u32) -> bool,
) -> IntentState {
    match &rec.state {
        IntentState::Pending { pid, deadline_ms } => {
            let detail = if now_ms > *deadline_ms {
                Some("the call's deadline passed with no answer recorded".to_string())
            } else if !alive(*pid) {
                Some("the sending process ended with no answer recorded".to_string())
            } else {
                None
            };
            match detail {
                Some(detail) => IntentState::Unknown {
                    source: ORPHAN.to_string(),
                    detail: Some(detail),
                },
                None => rec.state.clone(),
            }
        }
        other => other.clone(),
    }
}

/// What a send is asking the ledger.
#[derive(Debug, Clone)]
pub struct AdmitQuery<'a> {
    pub key: &'a IntentKey,
    pub mode: IdempotencyMode,
    /// The key window, for `key` mode.
    pub window_sec: u64,
    /// The call's own timeout: a replay must be answered inside the window.
    pub timeout_sec: u64,
    pub on_unknown: OnUnknown,
    /// Whether the call's attempt is still open.
    pub attempt_open: bool,
    pub now_ms: u64,
}

/// The ledger's answer to a send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Send it as a new intent (`resend_of` names the one it sends again).
    New { resend_of: Option<String> },
    /// Send it again with the same nonce, inside the key window.
    Replay { of: String, nonce: String },
    /// It already went out: answer with the earlier result.
    AlreadySent {
        intent: String,
        label: String,
        attempt: Option<u32>,
        effect_ref: Option<String>,
    },
    /// Another call is sending it right now.
    InFlight { intent: String },
    /// Look the earlier send up before deciding.
    Reconcile { intent: String },
    /// Whether it went out is not known, and nothing may resend it.
    RefuseUnknown { intent: String, why: String },
    /// The call's attempt has ended.
    AttemptClosed,
}

/// Decides a send against the journal, with the real process check.
pub fn admit(events: &[Event], q: &AdmitQuery<'_>) -> Admission {
    admit_with(events, q, &crate::liveness::pid_alive)
}

/// [`admit`] with the process check given. A pure decision table over the
/// whole chain of the key's latest intent ([`chain_verdict`]).
fn admit_with(events: &[Event], q: &AdmitQuery<'_>, alive: &dyn Fn(u32) -> bool) -> Admission {
    if !q.attempt_open {
        return Admission::AttemptClosed;
    }
    let ledger = Ledger::fold(events);
    let floor = effect_scope_floor(events, &q.key.node);
    let Some(tip) = ledger.latest(q.key, floor) else {
        return Admission::New { resend_of: None };
    };
    let verdict = chain_verdict_with(&ledger, tip, q.now_ms, alive);
    let member = verdict.member;
    match verdict.state {
        IntentState::Confirmed { effect_ref, .. } => Admission::AlreadySent {
            intent: member.id.clone(),
            label: member.key.label.clone(),
            attempt: member.attempt,
            effect_ref,
        },
        IntentState::NotSent { .. } => Admission::New {
            resend_of: Some(tip.id.clone()),
        },
        IntentState::Pending { .. } => Admission::InFlight {
            intent: member.id.clone(),
        },
        IntentState::Unknown { .. } => decide_unknown(member, tip, q),
    }
}

/// The answer for a chain whose `unknown` member decides it; a repeat links
/// to the chain's `tip`.
fn decide_unknown(rec: &IntentRecord, tip: &IntentRecord, q: &AdmitQuery<'_>) -> Admission {
    let resend = || Admission::New {
        resend_of: Some(tip.id.clone()),
    };
    match q.mode {
        IdempotencyMode::Key => {
            let window_end = rec
                .nonce_ts_ms
                .saturating_add(q.window_sec.saturating_mul(1000));
            // The replay itself must be answered while the service still
            // deduplicates: its whole timeout fits inside the window.
            let answered_by = q.now_ms.saturating_add(q.timeout_sec.saturating_mul(1000));
            if answered_by <= window_end {
                Admission::Replay {
                    of: tip.id.clone(),
                    nonce: rec.nonce.clone(),
                }
            } else if q.on_unknown == OnUnknown::Resend {
                resend()
            } else {
                Admission::RefuseUnknown {
                    intent: rec.id.clone(),
                    why: format!(
                        "the service's {} s deduplication window has passed, so a repeat could act twice",
                        q.window_sec
                    ),
                }
            }
        }
        IdempotencyMode::Marker => Admission::Reconcile {
            intent: rec.id.clone(),
        },
        IdempotencyMode::None if q.on_unknown == OnUnknown::Resend => resend(),
        IdempotencyMode::None => Admission::RefuseUnknown {
            intent: rec.id.clone(),
            why: "the service offers no way to tell whether it went through".to_string(),
        },
    }
}

#[cfg(test)]
mod tests;
