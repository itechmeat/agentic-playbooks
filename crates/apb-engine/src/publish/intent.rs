//! The push in the send ledger: its key, its admission under the journal
//! lock before it leaves, its settlement, and the reconcile of an unknown
//! result, which reads the remote ref back and never pushes.
//!
//! The key is `{ node, "@git", "push", <url>, "ref:<branch>" }` in mode
//! `ref`: an unknown result means reconcile, never a repeat.

use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::publish::{GIT_CONNECTOR, PUSH_FUNCTION, ledger_label};
use apb_core::schema::OnUnknown;

use crate::connector::intent::{
    self, Admission, AdmitQuery, IntentDraft, IntentKey, IntentRecord, IntentState, Ledger,
    NewIntent, Settlement, ledger,
};
use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload};

/// The ledger key of a push of `node` to `branch` at `url`.
pub fn key(node: &str, url: &str, branch: &str) -> IntentKey {
    IntentKey {
        node: node.to_string(),
        connector: GIT_CONNECTOR.to_string(),
        function: PUSH_FUNCTION.to_string(),
        account: url.to_string(),
        label: ledger_label(branch),
        item: None,
    }
}

/// The latest push of `node` within its effect scope (since its last
/// success), and what its chain says.
pub fn latest(events: &[Event], node: &str) -> Option<(IntentRecord, IntentState)> {
    let floor = intent::effect_scope_floor(events, crate::exec_key::ExecKey::node(node));
    latest_since(events, node, floor, apb_core::clock::now_ms_u64())
}

/// The latest push of `node` recorded at or after `floor`, and what its
/// chain says at `now_ms`.
pub fn latest_since(
    events: &[Event],
    node: &str,
    floor: u64,
    now_ms: u64,
) -> Option<(IntentRecord, IntentState)> {
    let ledger = Ledger::fold(events);
    let rec = ledger
        .records()
        .iter()
        .rev()
        .find(|r| r.seq >= floor && r.key.node == node && r.key.connector == GIT_CONNECTOR)?;
    let verdict = intent::chain_verdict(&ledger, rec, now_ms);
    Some((verdict.member.clone(), verdict.state))
}

/// The facts of the plan a push carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedPlan {
    pub after: String,
    pub branch: String,
    pub url: String,
    pub remote_before: Option<String>,
    pub plan_digest: String,
}

/// The plan an intent pushed: the latest `publish_planned` of the node
/// before it was recorded (a push only follows a consent to the plan the
/// drive just re-made).
pub fn pushed_plan(events: &[Event], rec: &IntentRecord) -> Option<PushedPlan> {
    events
        .iter()
        .rev()
        .filter(|e| e.seq < rec.seq)
        .find_map(|e| match &e.payload {
            EventPayload::PublishPlanned {
                node,
                after,
                branch,
                url,
                remote_before,
                plan_digest,
                ..
            } if *node == rec.key.node => Some(PushedPlan {
                after: after.clone(),
                branch: branch.clone(),
                url: url.clone(),
                remote_before: remote_before.clone(),
                plan_digest: plan_digest.clone(),
            }),
            _ => None,
        })
}

/// Records the push under the journal lock, synced, when the ledger admits
/// a new send; any other answer records nothing and comes back.
pub fn admit(
    log: &mut EventLog,
    key: &IntentKey,
    timeout_sec: u64,
) -> Result<Result<NewIntent, Admission>, EngineError> {
    let mut outcome: Option<Result<NewIntent, Admission>> = None;
    log.append_decided_synced(|events| {
        let now_ms = apb_core::clock::now_ms_u64();
        let admission = ledger::admit(
            events,
            &AdmitQuery {
                key,
                mode: IdempotencyMode::Ref,
                window_sec: 0,
                timeout_sec,
                on_unknown: OnUnknown::Refuse,
                attempt_open: true,
                now_ms,
            },
        );
        let draft = IntentDraft {
            key,
            mode: IdempotencyMode::Ref,
            attempt: None,
            attempt_seq: None,
            timeout_sec,
            now_ms,
        };
        match intent::admitted(&draft, &admission) {
            Some(new) => {
                let payload = new.payload.clone();
                outcome = Some(Ok(new));
                Some(payload)
            }
            None => {
                outcome = Some(Err(admission));
                None
            }
        }
    })?;
    outcome.ok_or_else(|| EngineError::Invalid("the journal was not read".into()))
}

/// Journals the settlement of `intent` on the drive's own log.
pub fn settle(log: &mut EventLog, intent: &str, s: Settlement) -> Result<(), EngineError> {
    log.append(s.payload(intent))?;
    Ok(())
}

/// A settlement from the push itself or a reconcile.
pub fn settlement(
    outcome: &'static str,
    source: &'static str,
    detail: Option<String>,
    effect_ref: Option<String>,
) -> Settlement {
    Settlement {
        outcome,
        source,
        detail,
        effect_ref,
        by: None,
    }
}

/// The source of a settlement read back from the remote.
pub const RECONCILE: &str = "reconcile";
/// The source of a settlement the push reported.
pub const RESPONSE: &str = "response";
