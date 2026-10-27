//! The send-once gate around a call to a function that declares
//! `idempotency`: record the intent before the request leaves, answer or
//! refuse a repeat from the ledger, look a `marker` send up again, and
//! settle the intent from the call's outcome.
//!
//! The order is what makes it safe. The intent is appended under the journal
//! lock and synced to disk before anything is sent, and a journal that
//! cannot take it refuses the call (nothing is sent). After the call the
//! `connector_call` is appended first and the settlement second; if the
//! settlement is lost, the intent stays pending and reads as unknown once
//! its sender is gone, the safe direction.

use std::path::Path;

use apb_core::connector::common::scalar_at_path;
use apb_core::connector::def::{ConnectorDoc, FunctionSpec};
use apb_core::connector::idempotency::{Idempotency, IdempotencyMode};
use apb_core::connector::marker::MarkerFields;
use apb_core::schema::OnUnknown;
use serde_json::{Value, json};

use super::attempt::{self, AttemptBinding};
use super::request::{CallContext, CallRequest, validate_args};
use crate::connector::effect::{CallClass, CallFacts, classify};
use crate::connector::intent::ledger::{self, Admission, AdmitQuery, IntentRecord, Ledger};
use crate::connector::intent::reconcile::{self, ReconcileInput, Reconciled};
use crate::connector::intent::stamp::Stamp;
use crate::connector::intent::{self, IntentDraft, IntentKey, NewIntent, Settlement};
use crate::connector::result::{CallError, CallErrorCode, CallOk};
use crate::error::EngineError;
use crate::event::{AppendMark, Event, EventLog};

/// The longest error detail a settlement keeps.
const DETAIL_CAP: usize = 300;

/// The ordinary call path, handed in by its owner so this module never
/// imports it: a request and its context in, the result JSON and the ok flag
/// out. A reconciliation reads through it as apb's own call.
pub(super) type Read<'a> = dyn Fn(CallRequest, CallContext) -> (Value, bool) + 'a;

/// The `max_calls` check of a call that is about to go out, against the
/// journal as the gate reads it under the lock.
pub(super) type Budget<'a> = dyn Fn(&[Event]) -> Result<(), CallError> + 'a;

/// What the gate needs about the call.
pub(super) struct GateInput<'a> {
    pub req: &'a CallRequest<'a>,
    pub ctx: &'a CallContext<'a>,
    pub on_unknown: OnUnknown,
    pub doc: &'a ConnectorDoc,
    pub function: &'a FunctionSpec,
    pub account: &'a str,
    /// How a reconciliation reads (the call path itself).
    pub read: &'a Read<'a>,
    /// The budget, charged only by a call that actually goes out: a repeat
    /// the ledger answers costs nothing, so a spent budget never hides the
    /// earlier result.
    pub budget: &'a Budget<'a>,
}

/// The gate's verdict on a call.
pub(super) enum SendGate {
    /// Not a tracked send (no declaration, a dry-run, apb's own read).
    Untracked,
    /// Send it, stamped as this intent.
    Send(Box<Tracked>),
    /// Do not send: it already went out.
    Answer(CallOk),
}

/// A send under way, as its call path needs it.
#[derive(Debug, Clone)]
pub(super) struct Tracked {
    pub id: String,
    pub label: String,
    pub nonce: String,
    pub mode: IdempotencyMode,
    pub replay: bool,
    pub marker: Option<MarkerFields>,
    pub result_ref: Option<String>,
    pub attempt: Option<u32>,
    pub attempt_seq: Option<u64>,
}

impl Tracked {
    pub(super) fn stamp(&self) -> Stamp<'_> {
        Stamp::Send {
            nonce: &self.nonce,
            marker: self.marker.as_ref(),
        }
    }
}

/// What the decision under the journal lock produced.
struct Decided {
    admission: Admission,
    new: Option<NewIntent>,
    binding: AttemptBinding,
    record: Option<IntentRecord>,
}

/// Decides a call against the ledger, recording its intent when it may go.
pub(super) fn gate(input: &GateInput<'_>) -> Result<SendGate, CallError> {
    let GateInput {
        req, ctx, function, ..
    } = input;
    let Some(idem) = function.idempotency.as_ref() else {
        return Ok(SendGate::Untracked);
    };
    if req.dry_run || !ctx.origin.tracks_sends() {
        return Ok(SendGate::Untracked);
    }
    // Invalid args must not leave a pending intent behind.
    if let Some(schema) = &function.args_schema {
        validate_args(schema, &req.args)?;
    }
    let label = match ctx.intent {
        Some(label) if ctx.origin.labels_itself() => label.to_string(),
        Some(label) => {
            intent::validate_label(label)
                .map_err(|m| CallError::new(CallErrorCode::InvalidArgs, m))?;
            label.to_string()
        }
        None => intent::default_label(ctx.claim.item.as_deref(), &req.args),
    };
    let key = IntentKey {
        node: req.node_id.to_string(),
        connector: req.connector.to_string(),
        function: req.function.to_string(),
        account: input.account.to_string(),
        label,
        item: ctx.claim.item.clone(),
    };
    let decided = decide(input, idem, &key)?;
    match decided.admission {
        Admission::New { .. } | Admission::Replay { .. } => {
            let new = decided
                .new
                .ok_or_else(|| journal_failed(&"no intent was recorded"))?;
            Ok(SendGate::Send(Box::new(tracked(
                input,
                idem,
                &key,
                new,
                &decided.binding,
            ))))
        }
        Admission::AlreadySent {
            intent,
            label,
            attempt,
            effect_ref,
        } => Ok(SendGate::Answer(CallOk::AlreadySent {
            intent,
            label,
            sent_in_attempt: attempt,
            effect_ref,
        })),
        Admission::InFlight { intent } => Err(CallError::new(
            CallErrorCode::InFlight,
            format!(
                "send intent `{}` ({intent}) is being sent by another call right now; wait for that call's result instead of sending it again",
                key.label
            ),
        )),
        Admission::RefuseUnknown { intent, why } => Err(unknown_effect(req, &key, &intent, &why)),
        Admission::AttemptClosed => Err(attempt_closed(req)),
        Admission::Reconcile { intent } => {
            let record = decided
                .record
                .ok_or_else(|| unknown_effect(req, &key, &intent, "its record is missing"))?;
            reconcile_marker(input, idem, &key, &record)
        }
    }
}

/// Runs the admission under the journal lock and records the intent when it
/// may go. A journal that cannot take the record refuses the call, and so
/// does a spent budget (nothing is recorded then).
fn decide(
    input: &GateInput<'_>,
    idem: &Idempotency,
    key: &IntentKey,
) -> Result<Decided, CallError> {
    let req = input.req;
    let mut decided: Option<Decided> = None;
    let mut over_budget: Option<CallError> = None;
    let mut log = EventLog::open_foreign(req.run_dir).map_err(|e| journal_failed(&e))?;
    log.append_decided_synced(|events| {
        let binding = attempt::bind(events, req.node_id, &input.ctx.claim);
        let now_ms = apb_core::clock::now_ms_u64();
        let query = AdmitQuery {
            key,
            mode: idem.mode(),
            window_sec: idem.window_sec(),
            timeout_sec: input.function.timeout_sec,
            on_unknown: input.on_unknown,
            attempt_open: binding.may_send,
            now_ms,
        };
        let admission = ledger::admit(events, &query);
        if matches!(admission, Admission::New { .. } | Admission::Replay { .. })
            && let Err(e) = (input.budget)(events)
        {
            over_budget = Some(e);
            return None;
        }
        let draft = IntentDraft {
            key,
            mode: idem.mode(),
            attempt: binding.attempt,
            attempt_seq: binding.attempt_seq,
            timeout_sec: input.function.timeout_sec,
            now_ms,
        };
        let new = intent::admitted(&draft, &admission);
        let record = match &admission {
            Admission::Reconcile { intent } => Ledger::fold(events).get(intent).cloned(),
            _ => None,
        };
        let payload = new.as_ref().map(|n| n.payload.clone());
        decided = Some(Decided {
            admission,
            new,
            binding,
            record,
        });
        payload
    })
    .map_err(|e| {
        // Nothing was sent. A whole intent line whose sync failed is
        // settled as not sent, or it would read as unknown once this
        // process ends. A line known not to be in the file needs nothing,
        // and a write that broke off may have left a torn line that
        // another append must not extend: that one stays pending.
        if let Some(new) = decided.as_ref().and_then(|d| d.new.as_ref())
            && log.last_append() == AppendMark::Whole
        {
            settle_unsynced(req.run_dir, &new.id, &e);
        }
        journal_failed(&e)
    })?;
    if let Some(e) = over_budget {
        return Err(e);
    }
    decided.ok_or_else(|| journal_failed(&"the journal was not read"))
}

fn tracked(
    input: &GateInput<'_>,
    idem: &Idempotency,
    key: &IntentKey,
    new: NewIntent,
    binding: &AttemptBinding,
) -> Tracked {
    let marker = matches!(idem, Idempotency::Marker { .. }).then(|| MarkerFields {
        run: run_id(input.req),
        node: key.node.clone(),
        attempt: binding.attempt,
        nonce: new.nonce.clone(),
    });
    Tracked {
        id: new.id,
        label: key.label.clone(),
        nonce: new.nonce,
        mode: idem.mode(),
        replay: new.replay,
        marker,
        result_ref: idem.result_ref().map(str::to_string),
        attempt: binding.attempt,
        attempt_seq: binding.attempt_seq,
    }
}

/// Looks an earlier `marker` send up and settles it: found means it went
/// out, so the repeat is answered; not found keeps it unknown and refused.
fn reconcile_marker(
    input: &GateInput<'_>,
    idem: &Idempotency,
    key: &IntentKey,
    record: &IntentRecord,
) -> Result<SendGate, CallError> {
    let req = input.req;
    let (
        Some(identity),
        Idempotency::Marker {
            reconcile: spec,
            format,
            ..
        },
    ) = (input.doc.identity.as_ref(), idem)
    else {
        return Err(unknown_effect(
            req,
            key,
            &record.id,
            "the connector declares no way to read its own sends back",
        ));
    };
    let run = run_id(req);
    let lookup = ReconcileInput {
        identity,
        spec,
        format: *format,
        run_id: &run,
        record,
        send_args: &req.args,
    };
    let mut fetch = |function: &str, args: Value| read_back(input, function, args);
    match reconcile::reconcile(&lookup, &mut fetch) {
        Reconciled::Confirmed { effect_ref } => {
            settle_logged(
                req.run_dir,
                &record.id,
                Settlement {
                    outcome: ledger::CONFIRMED,
                    source: "reconcile",
                    detail: None,
                    effect_ref: effect_ref.clone(),
                    by: None,
                },
            );
            Ok(SendGate::Answer(CallOk::AlreadySent {
                intent: record.id.clone(),
                label: key.label.clone(),
                sent_in_attempt: record.attempt,
                effect_ref,
            }))
        }
        Reconciled::Unknown { detail } => {
            settle_logged(
                req.run_dir,
                &record.id,
                Settlement {
                    outcome: ledger::UNKNOWN,
                    source: "reconcile",
                    detail: Some(detail.clone()),
                    effect_ref: None,
                    by: None,
                },
            );
            Err(unknown_effect(req, key, &record.id, &detail))
        }
    }
}

/// One read of the reconciliation, through the ordinary call path as apb's
/// own call (journaled with `origin: reconcile`, never charged to the
/// budget), on the account the send used, with the raw body. A
/// notification's read goes to the same live target, checked against the
/// same pins.
fn read_back(input: &GateInput<'_>, function: &str, args: Value) -> Result<(Value, bool), String> {
    let req = input.req;
    let (value, ok) = (input.read)(
        CallRequest {
            run_dir: req.run_dir,
            root: req.root,
            node_id: req.node_id,
            connector: req.connector,
            function,
            account: Some(input.account),
            args,
            dry_run: false,
            full: true,
        },
        CallContext {
            intent: None,
            claim: input.ctx.claim.clone(),
            origin: input.ctx.origin.read_back(),
            pins: input.ctx.pins.clone(),
        },
    );
    if !ok {
        let code = value["error"]["code"].as_str().unwrap_or("error");
        return Err(format!("the `{function}` read failed ({code})"));
    }
    let next_page = value
        .get("link")
        .and_then(Value::as_str)
        .is_some_and(|link| link.contains("rel=\"next\""));
    Ok((value.get("body").cloned().unwrap_or(Value::Null), next_page))
}

/// Settles an intent whose request never left because the call failed
/// after the intent was recorded (a render or secret error). The source
/// tells it apart from a refusal of the trust gate: nothing went out, and a
/// later try may well go.
pub(super) fn settle_unsent(run_dir: &Path, tracked: &Tracked, err: &CallError) {
    settle_logged(
        run_dir,
        &tracked.id,
        Settlement {
            outcome: ledger::NOT_SENT,
            source: "unsent",
            detail: Some(error_detail(err)),
            effect_ref: None,
            by: None,
        },
    );
}

/// Settles a reached send from its outcome (`facts`, as journaled) and marks
/// the result with its intent (and `effect: unknown` when nobody can tell
/// whether it acted).
pub(super) fn finish(
    run_dir: &Path,
    tracked: &Tracked,
    facts: CallFacts<'_>,
    result: Result<&CallOk, &CallError>,
    value: &mut Value,
) {
    // A service that dedupes by key and answers a conflict says the key was
    // seen, not what became of the first request.
    let key_conflict = tracked.mode == IdempotencyMode::Key && facts.http_status == Some(409);
    let detail = result.err().map(error_detail);
    let (outcome, detail) = match (classify(facts, false), key_conflict) {
        (_, true) => (ledger::UNKNOWN, detail),
        (CallClass::WriteUnknown, false) => (ledger::UNKNOWN, detail),
        (CallClass::Failed, false) => (ledger::NOT_SENT, detail),
        (
            CallClass::WriteOk
            | CallClass::WriteIncomplete
            | CallClass::ReadOk
            | CallClass::ReadUnknown,
            false,
        ) => (ledger::CONFIRMED, None),
    };
    let effect_ref = match (result, &tracked.result_ref) {
        (Ok(ok), Some(path)) if outcome == ledger::CONFIRMED => scalar_at_path(ok.body(), path),
        _ => None,
    };
    let source = if outcome == ledger::CONFIRMED && tracked.replay {
        "key_replay"
    } else {
        "response"
    };
    settle_logged(
        run_dir,
        &tracked.id,
        Settlement {
            outcome,
            source,
            detail,
            effect_ref,
            by: None,
        },
    );
    value["intent"] = json!({ "id": tracked.id, "label": tracked.label });
    if outcome == ledger::UNKNOWN {
        value["effect"] = json!("unknown");
    }
}

fn error_detail(err: &CallError) -> String {
    let text = format!("{}: {}", err.code.as_str(), err.message);
    text.chars().take(DETAIL_CAP).collect()
}

fn run_id(req: &CallRequest) -> String {
    req.run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Journals `settlement` for `intent`. A failure leaves the intent as it
/// was (pending reads as unknown once its sender is gone, the safe
/// direction) and is reported on stderr, never dropped.
fn settle_logged(run_dir: &Path, intent: &str, settlement: Settlement) {
    let outcome = settlement.outcome;
    if intent::settle(run_dir, intent, settlement).is_err() {
        eprintln!(
            "apb: warning: could not record a send intent settlement ({outcome}); it reads as unknown until a person resolves it"
        );
    }
}

/// Settles as not sent an intent the gate recorded whose append then failed
/// (its sync), so the journal says why the send did not go out.
fn settle_unsynced(run_dir: &Path, intent: &str, why: &EngineError) {
    settle_logged(
        run_dir,
        intent,
        Settlement {
            outcome: ledger::NOT_SENT,
            source: "journal",
            detail: Some(format!("the journal failed: {why}")),
            effect_ref: None,
            by: None,
        },
    );
}

fn journal_failed(why: &dyn std::fmt::Display) -> CallError {
    CallError::new(
        CallErrorCode::Config,
        format!("the run journal could not record the send intent; nothing was sent: {why}"),
    )
}

fn unknown_effect(req: &CallRequest, key: &IntentKey, intent: &str, why: &str) -> CallError {
    CallError::new(
        CallErrorCode::UnknownEffect,
        format!(
            "whether `{}.{}` intent `{}` ({intent}) went out earlier is not known: {why}. apb refuses to send it again. Do not send it under another name; report it as a finding; a person resolves it",
            req.connector, req.function, key.label,
        ),
    )
}

/// The refusal of a call whose attempt has ended, or that does not prove
/// it belongs to the node's current attempt.
pub(super) fn attempt_closed(req: &CallRequest) -> CallError {
    CallError::new(
        CallErrorCode::AttemptClosed,
        format!(
            "this call does not belong to the node's current attempt (that attempt has ended, or the call carries another attempt's APB_ATTEMPT_TOKEN, or none), so it may not call `{}.{}`; only the node's current attempt may",
            req.connector, req.function
        ),
    )
}
