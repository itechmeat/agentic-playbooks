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
use apb_core::connector::event_filter::EventFilter;
use apb_core::connector::inbox::{Inbox, InboxEvent, InboxGap};
use serde_json::{Value, json};

use crate::connector::call::{CallError, CallErrorCode, CallOk};
use crate::event::{EventLog, EventPayload};

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
    /// Which events the read wants. Empty on `ack` and `peek_depth`, where a
    /// filter is refused, and reads as today when empty.
    filter: EventFilter,
    /// The effective `response_pick` projection; empty when the function
    /// declares none or `--full` bypasses it.
    response_pick: Vec<String>,
    /// Whether a filtered read may ack the events it skipped: only a call
    /// whose caller may also move the cursor (see [`build`]).
    may_ack: bool,
    /// The run journal a gap goes into before a filtered read moves the
    /// cursor past it ([`InboxCall::journal_gaps_to`]); `None` for a
    /// person's live call, which has no run.
    gap_journal: Option<GapJournal>,
}

/// Where a call's gap is journaled: the run and the node that read.
#[derive(Debug)]
struct GapJournal {
    run_dir: std::path::PathBuf,
    node_id: String,
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
///
/// `may_ack` says whether a filtered read may move the consumer's cursor
/// past the events it skipped. The caller decides it from who calls: a
/// node whose grant also covers the connector's `ack` function, or a
/// person's live call. A read under a grant of read-only functions only,
/// and every engine site (a check, a reconcile), passes `false`: such a
/// read skips and reports `skipped` and the gap, and moves no cursor.
pub fn build(
    spec: &InboxSpec,
    connector: &str,
    account: &str,
    args: &Value,
    response_pick: Vec<String>,
    dry_run: bool,
    may_ack: bool,
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

    // A filter travels with a read only: on `ack` or `peek_depth` either key
    // is refused, whatever its shape.
    if spec.op != InboxOp::Read && (args.get("types").is_some() || args.get("where").is_some()) {
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            "only op `read` takes types or where",
        ));
    }
    let mut filter_value = serde_json::Map::new();
    for key in ["types", "where"] {
        if let Some(v) = args.get(key) {
            filter_value.insert(key.to_string(), v.clone());
        }
    }
    let filter: EventFilter = if filter_value.is_empty() {
        EventFilter::default()
    } else {
        match serde_json::from_value(Value::Object(filter_value)) {
            Ok(f) => f,
            Err(e) => {
                return Err(CallError::new(
                    CallErrorCode::InvalidArgs,
                    format!("`types` or `where`: {e}"),
                ));
            }
        }
    };
    if let Err(problem) = filter.check() {
        return Err(CallError::new(CallErrorCode::InvalidArgs, problem));
    }

    let call = InboxCall {
        connector: connector.to_string(),
        account: account.to_string(),
        op: spec.op,
        consumer,
        limit,
        up_to_seq,
        filter,
        response_pick,
        may_ack,
        gap_journal: None,
    };
    if dry_run {
        let mut inbox = json!({
            "op": call.op.as_str(),
            "endpoint": call.endpoint(),
            "consumer": call.consumer,
            "limit": call.limit,
            "up_to_seq": call.up_to_seq,
        });
        if !call.filter.types.is_empty() {
            inbox["types"] = json!(call.filter.types);
        }
        if !call.filter.conditions.is_empty() {
            inbox["where"] = json!(call.filter.conditions);
        }
        return Ok(InboxBuild::DryRun(json!({
            "ok": true,
            "dry_run": true,
            "inbox": inbox,
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

    /// Names the run journal a gap this call finds goes into, and the node
    /// that read. A filtered read that acks writes the gap there, synced,
    /// before it moves the cursor past it, and moves nothing when the write
    /// fails. Without it (a person's live call, which has no run) the gap is
    /// only in the returned envelope.
    pub fn journal_gaps_to(&mut self, run_dir: &std::path::Path, node_id: &str) {
        self.gap_journal = Some(GapJournal {
            run_dir: run_dir.to_path_buf(),
            node_id: node_id.to_string(),
        });
    }

    /// The smtp-only event extras every other kind reports as absent.
    pub fn event_extra(&self) -> (Option<String>, Option<u32>) {
        (None, None)
    }

    /// Runs the op against the local store. A read that found stored-then-
    /// dropped events this consumer never acknowledged carries their range
    /// back in the result's `gap`, outside the projected body.
    pub fn send(self) -> Result<CallOk, CallError> {
        let store = Inbox::open(&self.connector, &self.account).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("inbox unavailable: {e}"))
        })?;
        let mut gap = None;
        let body = match self.op {
            InboxOp::Read => {
                // An empty filter reads as today; a filtered read skips the
                // events that do not match and, when its caller may move the
                // cursor and outside a read-only process, acks the leading
                // ones for this consumer only.
                let ack_skipped = !self.filter.is_empty()
                    && self.may_ack
                    && !apb_core::process_mode::is_read_only();
                if self.filter.is_empty() {
                    let (events, cursor, lost) = store
                        .read(&self.consumer, self.limit)
                        .map_err(|e| CallError::new(CallErrorCode::Config, e.to_string()))?;
                    gap = lost;
                    read_envelope(&events, cursor)
                } else {
                    let filter = &self.filter;
                    // The gap is durable in the run journal before the
                    // cursor moves past it, or the cursor stays.
                    let record_gap = |gap: &InboxGap| match &self.gap_journal {
                        Some(j) => {
                            journal_gap(&j.run_dir, &self.connector, &self.account, &j.node_id, gap)
                        }
                        None => true,
                    };
                    let r = store
                        .read_filtered(
                            &self.consumer,
                            self.limit,
                            &|event| filter.matches(event),
                            ack_skipped,
                            record_gap,
                        )
                        .map_err(|e| CallError::new(CallErrorCode::Config, e.to_string()))?;
                    gap = r.gap;
                    filtered_envelope(&r.events, r.cursor, r.skipped)
                }
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
        Ok(CallOk::Inbox { body, picked, gap })
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
        let row = event_row(event);
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

/// [`read_envelope`] plus `"skipped"`: the pending events the read did not
/// return because they did not match. The cursor is the one after the read,
/// moved past a leading run of skipped events when it acked them.
pub fn filtered_envelope(events: &[InboxEvent], cursor: u64, skipped: u64) -> Value {
    let mut out = read_envelope(events, cursor);
    out["skipped"] = json!(skipped);
    out
}

/// One envelope event row: `seq`, `received_at`, `body`, and `type` when the
/// event has one. `provider_id` stays out (a dedupe identity, not
/// information), and so does the body of a type the reader did not ask for.
/// The dashboard's events view lists the same rows.
pub fn event_row(event: &InboxEvent) -> Value {
    match &event.event_type {
        Some(ty) => json!({
            "seq": event.seq,
            "received_at": event.received_at,
            "type": ty,
            "body": event.body,
        }),
        None => json!({
            "seq": event.seq,
            "received_at": event.received_at,
            "body": event.body,
        }),
    }
}

/// `{ acked_up_to }`.
pub fn ack_envelope(acked_up_to: u64) -> Value {
    json!({ "acked_up_to": acked_up_to })
}

/// `{ pending }`.
pub fn depth_envelope(pending: u64) -> Value {
    json!({ "pending": pending })
}

/// Which earlier `inbox_gap` line already records a gap, so the once-per-gap
/// check knows the gap is in the journal. [`GapScope::Account`] matches on
/// connector, account and range, as every connector read journals it
/// (unchanged from main). [`GapScope::Node`] additionally requires the
/// line's `node_id` to equal the scope's node, so a range another node
/// already recorded (a connector read of the same account, another wait) is
/// still journaled for the wait before its position advances (inbox-waits
/// D4 "The visit" step 4).
pub(crate) enum GapScope {
    /// Any line of the connector and account over the same range.
    Account,
    /// A line of this node alone over the same range.
    Node,
}

/// Records in the run journal that an inbox read found `gap` (unread events
/// retention dropped), once per distinct gap under `scope`: later reads that
/// cross the same gap stay silent, a gap that grew is a new one. Which node
/// read first is recorded in the line's `node_id`, but under
/// [`GapScope::Account`] it does not make a gap distinct. The line is synced
/// before the append lock is released. The caller owns `log` (the drive's
/// open log, or a foreign one over the run directory).
///
/// Returns whether the gap is in the journal (written now or already there).
/// One rule for every read: a cursor moves past a gap only once this said
/// `true`. An unfiltered read moves no cursor (its caller acks later, after
/// this ran), so for it the answer only matters as best effort, like the
/// `connector_call` event: a journal failure never changes its result. A
/// filtered read that would ack asks first and keeps the cursor on `false`.
pub(crate) fn journal_gap_in(
    log: &mut EventLog,
    connector: &str,
    account: &str,
    node_id: &str,
    gap: &InboxGap,
    scope: &GapScope,
) -> bool {
    let payload = EventPayload::InboxGap {
        node_id: node_id.to_string(),
        connector: connector.to_string(),
        account: account.to_string(),
        from: gap.from,
        through: gap.through,
    };
    log.append_decided_synced(|events| {
        let known = events.iter().any(|e| {
            matches!(
                &e.payload,
                EventPayload::InboxGap { connector: c, account: a, from, through, node_id: n, .. }
                if c == connector
                    && a == account
                    && *from == gap.from
                    && *through == gap.through
                    && (matches!(scope, GapScope::Account) || n == node_id)
            )
        });
        (!known).then_some(payload)
    })
    .is_ok()
}

/// Records in the run journal that an inbox read found `gap` (unread events
/// retention dropped), once per distinct gap of an inbox and account per run:
/// later reads that cross the same gap stay silent, a gap that grew is a new
/// one. Which node read first is recorded, but does not make a gap distinct.
/// The line is synced before the append lock is released.
///
/// Returns whether the gap is in the journal (written now or already there).
/// One rule for every read: a cursor moves past a gap only once this said
/// `true`. An unfiltered read moves no cursor (its caller acks later, after
/// this ran), so for it the answer only matters as best effort, like the
/// `connector_call` event: a journal failure never changes its result. A
/// filtered read that would ack asks first and keeps the cursor on `false`.
pub(crate) fn journal_gap(
    run_dir: &std::path::Path,
    connector: &str,
    account: &str,
    node_id: &str,
    gap: &InboxGap,
) -> bool {
    // `apb connector call` runs outside the drive, so the journal is opened
    // as a foreign writer (a torn last line from a concurrent append is
    // forgiven), and the "once per gap" check runs on a fresh read under the
    // append lock: two nodes reading the same gap at once journal it once.
    let Ok(mut log) = EventLog::open_foreign(run_dir) else {
        return false;
    };
    journal_gap_in(
        &mut log,
        connector,
        account,
        node_id,
        gap,
        &GapScope::Account,
    )
}
