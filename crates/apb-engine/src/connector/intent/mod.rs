//! The send-intent ledger: how apb keeps a send through a connector function
//! that declares `idempotency` from acting twice when an attempt is retried,
//! resumed or repeated.
//!
//! Before such a send leaves, its intent is recorded in the run journal
//! under the journal lock and synced to disk; after it, the outcome is
//! settled (`confirmed`, `not_sent` or `unknown`). A repeat of the same
//! intent ([`identity::IntentKey`]) then gets the earlier result, a replay of
//! the same key, a lookup of the earlier item, or a refusal, never a blind
//! second send. [`ledger`] folds the journal and decides; [`stamp`] marks
//! the request; [`reconcile`] finds a `marker` send again.

pub mod identity;
pub mod ledger;
pub mod reconcile;
pub mod stamp;

use std::path::Path;

use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::connector::marker;

pub use identity::{ARGS_LABEL_PREFIX, IntentKey, args_label, validate_label};
pub use ledger::{
    Admission, AdmitQuery, ChainVerdict, IntentRecord, IntentState, Ledger, admit, chain_verdict,
    effect_scope_floor, effective_state, send_verdict,
};

use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload};

/// How long after a call's own timeout a pending intent still counts as in
/// flight.
const DEADLINE_GRACE_MS: u64 = 30_000;

/// The longest effect reference the journal keeps.
const EFFECT_REF_CAP: usize = 256;

/// What a send records about itself when it is admitted.
#[derive(Debug, Clone)]
pub struct IntentDraft<'a> {
    pub key: &'a IntentKey,
    pub mode: IdempotencyMode,
    pub attempt: Option<u32>,
    pub attempt_seq: Option<u64>,
    pub timeout_sec: u64,
    pub now_ms: u64,
}

/// An intent admitted to go out: its id, the nonce it carries, and the
/// record to journal.
#[derive(Debug, Clone)]
pub struct NewIntent {
    pub id: String,
    pub nonce: String,
    pub replay: bool,
    pub payload: EventPayload,
}

/// The record for an admitted send; `None` for every answer that sends
/// nothing.
pub fn admitted(draft: &IntentDraft<'_>, admission: &Admission) -> Option<NewIntent> {
    let (nonce, replay_of, resend_of) = match admission {
        Admission::New { resend_of } => (marker::new_nonce(), None, resend_of.clone()),
        Admission::Replay { of, nonce } => (nonce.clone(), Some(of.clone()), None),
        _ => return None,
    };
    let id = format!("si-{}", &marker::new_nonce()[..12]);
    let replay = replay_of.is_some();
    let payload = EventPayload::SendIntentRecorded {
        intent: id.clone(),
        nonce: nonce.clone(),
        node: draft.key.node.clone(),
        attempt: draft.attempt,
        attempt_seq: draft.attempt_seq,
        connector: draft.key.connector.clone(),
        function: draft.key.function.clone(),
        account: draft.key.account.clone(),
        mode: draft.mode.as_str().to_string(),
        label: draft.key.label.clone(),
        pid: std::process::id(),
        deadline_ms: draft
            .now_ms
            .saturating_add(draft.timeout_sec.saturating_mul(1000))
            .saturating_add(DEADLINE_GRACE_MS),
        replay_of,
        resend_of,
    };
    Some(NewIntent {
        id,
        nonce,
        replay,
        payload,
    })
}

/// How an intent is settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// [`ledger::CONFIRMED`], [`ledger::NOT_SENT`] or [`ledger::UNKNOWN`].
    pub outcome: &'static str,
    /// `response`, `reconcile`, `key_replay`, `gate` or [`ledger::PERSON`].
    pub source: &'static str,
    pub detail: Option<String>,
    pub effect_ref: Option<String>,
    pub by: Option<String>,
}

impl Settlement {
    fn payload(self, intent: &str) -> EventPayload {
        EventPayload::SendIntentSettled {
            intent: intent.to_string(),
            outcome: self.outcome.to_string(),
            source: self.source.to_string(),
            detail: self.detail,
            effect_ref: self
                .effect_ref
                .map(|r| r.chars().take(EFFECT_REF_CAP).collect()),
            by: self.by,
        }
    }
}

/// Journals `settlement` for `intent`. A failure leaves the intent pending,
/// which reads as unknown once its sender is gone: the safe direction.
pub fn settle(run_dir: &Path, intent: &str, settlement: Settlement) -> Result<(), EngineError> {
    EventLog::open_foreign(run_dir)?.append(settlement.payload(intent))?;
    Ok(())
}

/// Why a person's decision was not recorded.
#[derive(Debug)]
pub enum ResolveError {
    /// The run has no such intent.
    NotFound(String),
    /// The intent's send is not unknown now: it is known, or in flight.
    Refused(String),
    /// The journal could not be read or written.
    Journal(EngineError),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NotFound(m) | ResolveError::Refused(m) => f.write_str(m),
            ResolveError::Journal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ResolveError::Journal(e) => Some(e),
            ResolveError::NotFound(_) | ResolveError::Refused(_) => None,
        }
    }
}

/// A person's decision on an intent whose outcome is unknown: it went out
/// (`sent`) or it did not. The intent's own send decides
/// ([`ledger::send_verdict`]): only the member whose unknown outcome holds
/// that send open is resolved, so a send answered by a later replay, or
/// still in flight, is refused. A resend is a separate send: an earlier
/// unknown member stays open to a person after a later resend went out. `by` names who decided; `note` is kept as
/// the settlement's detail.
pub fn resolve_by_person(
    run_dir: &Path,
    intent: &str,
    sent: bool,
    note: Option<String>,
    by: &str,
) -> Result<(), ResolveError> {
    let settlement = Settlement {
        outcome: if sent {
            ledger::CONFIRMED
        } else {
            ledger::NOT_SENT
        },
        source: ledger::PERSON,
        detail: note,
        effect_ref: None,
        by: Some(by.to_string()),
    };
    let mut refusal: Option<ResolveError> = None;
    let mut log = EventLog::open_foreign(run_dir).map_err(ResolveError::Journal)?;
    let written = log
        .append_checked(settlement.payload(intent), |events| {
            refusal = refuse_resolve(events, intent, apb_core::clock::now_ms_u64());
            refusal.is_none()
        })
        .map_err(ResolveError::Journal)?;
    match (written, refusal) {
        (Some(_), _) => Ok(()),
        (None, Some(why)) => Err(why),
        (None, None) => Err(ResolveError::Refused(format!(
            "send intent `{intent}` could not be resolved"
        ))),
    }
}

/// Why `intent` may not be resolved by a person now, or `None` when it may.
fn refuse_resolve(events: &[Event], intent: &str, now_ms: u64) -> Option<ResolveError> {
    let ledger = Ledger::fold(events);
    let Some(rec) = ledger.get(intent) else {
        return Some(ResolveError::NotFound(format!(
            "this run has no send intent `{intent}`"
        )));
    };
    let verdict = ledger::send_verdict(&ledger, rec, now_ms);
    let refused = |m: String| Some(ResolveError::Refused(m));
    match verdict.state {
        IntentState::Unknown { .. } if verdict.member.id == intent => None,
        IntentState::Unknown { .. } => refused(format!(
            "whether send intent `{intent}` went out is decided by `{}` of the same send; resolve that one",
            verdict.member.id
        )),
        IntentState::Pending { .. } => refused(format!(
            "send intent `{intent}` is still in flight; wait for it to settle"
        )),
        IntentState::Confirmed { .. } => refused(format!(
            "send intent `{intent}` is already confirmed as sent"
        )),
        IntentState::NotSent { .. } => refused(format!(
            "send intent `{intent}` is already settled as not sent"
        )),
    }
}
