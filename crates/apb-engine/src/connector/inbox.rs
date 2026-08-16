//! Native execution of the `inbox` connector function kind (spec
//! 2026-08-16-webhook-ingest-design).
//!
//! Strictly simpler than every other kind: there is no network, no auth, no
//! secret to resolve on this path, and therefore nothing to redact. What it
//! shares with the others is everything that matters for control: the grant
//! gate, the account allowlist, the `max_calls` budget, `args_schema`
//! validation, and one `ConnectorCall` event per reached call.
//!
//! The three envelope builders are pure and public inside the crate so the
//! offline contract-test runner asserts against exactly the shapes a live
//! call returns, without seeding a real store.
//!
//! Nothing here logs a stored body. The event log gets `inbox://<connector>/
//! <account>` and an outcome, never a payload.

use apb_core::connector::def::{InboxOp, InboxSpec};
use apb_core::connector::inbox::{Inbox, InboxEvent};
use serde_json::{Value, json};

use crate::connector::call::{CallError, CallErrorCode, CallOk};

/// The consumer a call uses when it names none. One default consumer per
/// account is the common case (a single playbook draining an inbox); a
/// second reader names itself explicitly.
pub const DEFAULT_CONSUMER: &str = "default";
/// Events returned when a read names no `limit`.
pub const DEFAULT_LIMIT: usize = 50;
/// Hard ceiling on `limit`. A larger request is clamped rather than refused:
/// the caller gets a page and a cursor, which is the contract either way.
pub const MAX_LIMIT: usize = 500;
/// Ceiling on the serialized size of one read envelope's events, the same
/// 1 MiB the HTTP kind caps a response body at (`call::BODY_CAP`).
///
/// `limit` alone does not bound anything useful: 500 events of the 256 KiB
/// the ingest listener allows each is ~128 MiB, and even the default limit of
/// 50 is ~12.8 MiB. Every byte of it is provider-written, which under this
/// feature's threat model means written by arbitrary internet users, so
/// handing an agent an unbounded quantity of it while every other connector
/// kind is capped is the wrong asymmetry.
pub const READ_BYTE_CAP: usize = 1024 * 1024;

/// A gated, argument-checked inbox call ready to run against the local
/// store.
#[derive(Debug)]
pub struct InboxCall {
    connector: String,
    account: String,
    op: InboxOp,
    consumer: String,
    limit: usize,
    up_to_seq: u64,
    /// The effective `response_pick` projection; empty when the function
    /// declares none or `--full` bypasses it.
    response_pick: Vec<String>,
}

/// Either a dry-run description or a call to run, mirroring
/// `smtp::SmtpBuild` and `imap::ImapBuild`.
pub enum InboxBuild {
    DryRun(Value),
    Call(Box<InboxCall>),
}

/// Validates the call arguments against the op and produces the call (or its
/// dry-run description). Reads nothing: a dry run must not touch the store,
/// and the live path defers every read to `send`.
pub fn build(
    spec: &InboxSpec,
    connector: &str,
    account: &str,
    args: &Value,
    response_pick: Vec<String>,
    dry_run: bool,
) -> Result<InboxBuild, CallError> {
    let consumer = match args.get("consumer") {
        None | Some(Value::Null) => DEFAULT_CONSUMER.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(CallError::new(
                CallErrorCode::InvalidArgs,
                format!("`consumer` must be a string, got {other}"),
            ));
        }
    };
    // The consumer becomes a key in the account's cursor file, so it is an
    // identifier and not free text. Checked here rather than at the store so
    // the caller gets `invalid_args` instead of a config error.
    apb_core::connector::validate_snake_name(&consumer)
        .map_err(|e| CallError::new(CallErrorCode::InvalidArgs, format!("`consumer`: {e}")))?;

    let limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(Value::Number(n)) => n
            .as_u64()
            .filter(|v| *v > 0)
            .map(|v| (v as usize).min(MAX_LIMIT))
            .ok_or_else(|| {
                CallError::new(
                    CallErrorCode::InvalidArgs,
                    "`limit` must be a positive integer",
                )
            })?,
        Some(other) => {
            return Err(CallError::new(
                CallErrorCode::InvalidArgs,
                format!("`limit` must be a positive integer, got {other}"),
            ));
        }
    };

    let up_to_seq = match (spec.op, args.get("up_to_seq")) {
        (InboxOp::Ack, Some(Value::Number(n))) => n.as_u64().ok_or_else(|| {
            CallError::new(
                CallErrorCode::InvalidArgs,
                "`up_to_seq` must be a non-negative integer",
            )
        })?,
        (InboxOp::Ack, _) => {
            return Err(CallError::new(
                CallErrorCode::InvalidArgs,
                "op `ack` requires `up_to_seq`, the highest seq the consumer has processed",
            ));
        }
        _ => 0,
    };

    let call = InboxCall {
        connector: connector.to_string(),
        account: account.to_string(),
        op: spec.op,
        consumer,
        limit,
        up_to_seq,
        response_pick,
    };
    if dry_run {
        return Ok(InboxBuild::DryRun(json!({
            "ok": true,
            "dry_run": true,
            "inbox": {
                "op": call.op.as_str(),
                "endpoint": call.endpoint(),
                "consumer": call.consumer,
                "limit": call.limit,
                "up_to_seq": call.up_to_seq,
            },
        })));
    }
    Ok(InboxBuild::Call(Box::new(call)))
}

impl InboxCall {
    /// The value recorded as the call's `url` in the event log. A scheme
    /// plus the store identity, never a network address, so a reader of the
    /// log can tell an inbox call from an HTTP one at a glance.
    pub fn endpoint(&self) -> String {
        format!("inbox://{}/{}", self.connector, self.account)
    }

    /// The smtp-only event extras every other kind reports as absent.
    pub fn event_extra(&self) -> (Option<String>, Option<u32>) {
        (None, None)
    }

    /// Runs the op against the local store.
    pub fn send(self) -> Result<CallOk, CallError> {
        let store = Inbox::open(&self.connector, &self.account).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("inbox unavailable: {e}"))
        })?;
        let body = match self.op {
            InboxOp::Read => {
                let (events, cursor) = store
                    .read(&self.consumer, self.limit)
                    .map_err(|e| CallError::new(CallErrorCode::Config, e.to_string()))?;
                read_envelope(&events, cursor)
            }
            InboxOp::Ack => {
                let moved = store
                    .ack(&self.consumer, self.up_to_seq)
                    .map_err(|e| CallError::new(CallErrorCode::Config, e.to_string()))?;
                ack_envelope(moved)
            }
            InboxOp::PeekDepth => {
                let depth = store
                    .depth(&self.consumer)
                    .map_err(|e| CallError::new(CallErrorCode::Config, e.to_string()))?;
                depth_envelope(depth.pending)
            }
        };
        let picked = !self.response_pick.is_empty();
        let body = if picked {
            crate::connector::call::encode::project(&body, &self.response_pick)
        } else {
            body
        };
        Ok(CallOk::Inbox { body, picked })
    }
}

/// `{ events: [{ seq, received_at, body }], cursor, truncated }`.
///
/// `provider_id` is deliberately not in the envelope: it is a dedupe
/// identity, not information the reader needs, and leaving it out keeps one
/// less provider-controlled string flowing toward an agent.
///
/// Events are dropped from the newest end once [`READ_BYTE_CAP`] is reached,
/// and `truncated` says so. Newest-end, because the oldest pending events are
/// the ones a consumer must see to make progress: it processes what it got,
/// acks that seq, and the next read starts where this one stopped. Dropping
/// the oldest instead would hand back a page the consumer cannot ack past,
/// and the same tail would come back forever.
///
/// The first event is always included, whatever its size. An inbox that
/// answers "no events" while holding one is worse than one that is briefly
/// over its cap, and it would be unackable: the same reasoning as the store's
/// retention floor. In practice this cannot happen, since the ingest listener
/// caps a single delivery at 256 KiB.
///
/// `truncated` is always present, false included, mirroring the imap kind: a
/// reader should not have to know that an absent flag means "all of it".
pub fn read_envelope(events: &[InboxEvent], cursor: u64) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    let mut bytes: usize = 0;
    let mut truncated = false;
    for event in events {
        let row = json!({
            "seq": event.seq,
            "received_at": event.received_at,
            "body": event.body,
        });
        let size = serde_json::to_string(&row).map(|s| s.len()).unwrap_or(0);
        if !rows.is_empty() && bytes.saturating_add(size) > READ_BYTE_CAP {
            truncated = true;
            break;
        }
        bytes = bytes.saturating_add(size);
        rows.push(row);
    }
    json!({ "events": rows, "cursor": cursor, "truncated": truncated })
}

/// `{ acked_up_to }`.
pub fn ack_envelope(acked_up_to: u64) -> Value {
    json!({ "acked_up_to": acked_up_to })
}

/// `{ pending }`.
pub fn depth_envelope(pending: u64) -> Value {
    json!({ "pending": pending })
}
