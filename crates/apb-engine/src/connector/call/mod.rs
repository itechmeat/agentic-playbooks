//! Connector call executor (spec 2026-07-18-connectors-design section 6 step
//! 4, plus 6.1 HTTP hardening and 6.2 interim redaction). The `apb connector
//! call` CLI process is a thin wrapper over [`execute_with`]: it reads the
//! run context from env, builds a [`CallRequest`] and a [`CallContext`],
//! prints the returned JSON, and exits 0 when the second element is `true`.
//!
//! A send through a function that declares `idempotency` additionally goes
//! through the send-intent ledger (`send_once`, over `connector::intent`):
//! its intent is recorded before the request leaves and settled after it,
//! so a repeat never sends it twice. `attempt` binds every call to the
//! attempt it belongs to.
//!
//! Everything after run start reads the immutable snapshot, never live files:
//! the run manifest (`manifest.yaml`) for grants and non-secret account
//! fields, and the copied `runs/<id>/connectors/<name>.yaml` for the function
//! definition. Only secret VALUES resolve live at call time, from the process
//! env / project dotenv / global dotenv chain (`secrets::resolve_var`), and
//! they never leave this process except inside the outgoing `auth` block.

mod account;
mod attempt;
mod auth;
pub(crate) mod encode;
mod http;
mod request;
mod response;
mod send_once;
mod target;

pub use account::select_live_account;
use account::{account_selection_error, select_account};
pub use attempt::AttemptClaim;
use auth::{non_secret_fields, resolve_secrets};
use encode::{assemble_url, encode_args_for_url, encode_component};
use http::HttpCall;
use request::validate_args;
pub use request::{CallContext, CallRequest};
pub(crate) use request::{CallOrigin, TargetPins};
use response::map_status;
pub(crate) use target::LiveTarget;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use apb_core::connector::config;
use apb_core::connector::def::{ConnectorDoc, FunctionSpec};
use apb_core::connector::secrets;
use apb_core::connector::store;
use apb_core::connector::template::{
    RenderCtx, render_body, render_encoded, render_raw, resolve_optional_arg,
    single_args_placeholder,
};
use apb_core::secret::{Redaction, Secret};
use apb_core::trust::{TrustStore, account_trust_id};
use serde_json::{Value, json};

use crate::connector::intent::stamp::{self, Stamp};
use crate::event::{EventLog, EventPayload};
use crate::manifest::{self, ManifestAccount, ManifestConnector, ManifestConnectorGrant};

/// The maximum response body read into memory (spec 6.1). A longer body is
/// cut at the cap: a read-only call fails as `incomplete_response`, a call
/// that may have changed the world stays a success but carries
/// `response_complete: false` on its result.
const BODY_CAP: usize = 1024 * 1024;

/// The default User-Agent (spec 4.4): ureq sends its own, and GitHub rejects
/// requests without one it recognizes. Overridden by a function `headers`
/// entry naming `User-Agent`.
const USER_AGENT: &str = concat!("apb/", env!("CARGO_PKG_VERSION"));

/// Where a reached call belongs, for its journal record: the attempt it
/// ran in, who made it, and the send intent it carried out.
#[derive(Debug, Clone, Default)]
struct CallScope {
    attempt: Option<u32>,
    attempt_seq: Option<u64>,
    origin: CallOrigin,
    send: Option<send_once::Tracked>,
    /// The list item the call belongs to (the caller's claim).
    item: Option<String>,
}

// The result taxonomy (`CallError`, `CallErrorCode`, `CallOk`) and the interim
// message redaction live in `connector_result`, the shared sink both the HTTP
// and SMTP call paths point at (keeps the file graph acyclic). Re-exported here
// so the public path `apb_engine::connector::call::{CallError, ...}` is stable.
pub use crate::connector::result::{
    BodyIncomplete, CallError, CallErrorCode, CallOk, IncompleteReason,
};

/// The metadata a reached call (mock or HTTP, ok or error) records in the
/// event log. A dry-run or a gate rejection records nothing, so those never
/// consume a `max_calls` budget.
struct EventMeta {
    account: String,
    /// Pre-auth rendered URL, `""` for a mock, `smtp://host:port` for smtp.
    url: String,
    outcome: String,
    http_status: Option<u16>,
    duration_ms: u64,
    /// SMTP-only: message subject and total recipient count, both `None` for
    /// HTTP/mock and for smtp `verify`.
    smtp_subject: Option<String>,
    smtp_recipients: Option<u32>,
    /// Whether the response body was read whole (`Some(true)`), cut or failed
    /// partway (`Some(false)`), or completeness does not apply (`None`).
    response_complete: Option<bool>,
    scope: CallScope,
}

/// Runs the whole call pipeline for a node with no send label or attempt
/// claim; see [`execute_with`].
pub fn execute(req: CallRequest) -> (Value, bool) {
    execute_with(req, CallContext::default())
}

/// Runs the whole call pipeline and returns the JSON document to print plus an
/// ok hint (`true` -> exit 0, `false` -> non-zero). Appends a `ConnectorCall`
/// event itself for any call that actually executed; a dry-run and every gate
/// rejection append nothing. A send through a function that declares
/// `idempotency` also records and settles its intent.
pub fn execute_with(req: CallRequest, ctx: CallContext) -> (Value, bool) {
    execute_bounded(req, &ctx, None)
}

/// [`execute`] bounded by `max_secs` as well as the function's own
/// `timeout_sec`: the smaller one applies (a baseline read honours its
/// check's `timeout_seconds`).
pub fn execute_within(req: CallRequest, max_secs: u64) -> (Value, bool) {
    execute_bounded(req, &CallContext::default(), Some(max_secs))
}

/// [`execute_with`] bounded like [`execute_within`].
pub fn execute_with_within(req: CallRequest, ctx: CallContext, max_secs: u64) -> (Value, bool) {
    execute_bounded(req, &ctx, Some(max_secs))
}

fn execute_bounded(req: CallRequest, ctx: &CallContext, max_secs: Option<u64>) -> (Value, bool) {
    match run(&req, ctx, max_secs) {
        Outcome::DryRun(value) => (value, true),
        Outcome::Answer(ok) => (ok.to_success_json(), true),
        Outcome::Ok(ok, meta) => {
            append_event(&req, &meta);
            if let CallOk::Inbox { gap: Some(gap), .. } = &ok {
                crate::connector::inbox::journal_gap(
                    req.run_dir,
                    req.connector,
                    &meta.account,
                    req.node_id,
                    gap,
                );
            }
            let mut value = ok.to_success_json();
            settle_send(&req, &meta, Ok(&ok), &mut value);
            (value, true)
        }
        Outcome::Executed(err, meta) => {
            append_event(&req, &meta);
            let mut value = err.to_json();
            settle_send(&req, &meta, Err(&err), &mut value);
            (value, false)
        }
        Outcome::Gate(err) => (err.to_json(), false),
    }
}

/// Settles the send intent a reached call carried out, if any.
fn settle_send(
    req: &CallRequest,
    meta: &EventMeta,
    result: Result<&CallOk, &CallError>,
    value: &mut Value,
) {
    if let Some(tracked) = &meta.scope.send {
        let facts = crate::connector::effect::CallFacts {
            outcome: &meta.outcome,
            http_status: meta.http_status,
            response_complete: meta.response_complete,
        };
        send_once::finish(req.run_dir, tracked, facts, result, value);
    }
}

/// The internal result of the pipeline: a dry-run render, an answer given
/// without a call (a send already confirmed), a reached-call success or
/// error (both carry event metadata), or a pre-execution gate rejection (no
/// event).
enum Outcome {
    DryRun(Value),
    Answer(CallOk),
    Ok(CallOk, EventMeta),
    Executed(CallError, EventMeta),
    Gate(CallError),
}

fn run(req: &CallRequest, ctx: &CallContext, max_secs: Option<u64>) -> Outcome {
    // 1-8: gate + render. Terminates as a dry-run, an answer, or a gate
    // rejection, none of which carries event metadata.
    let (mut prepared, scope) = match prepare(req, ctx) {
        Ok(Staged::Ready(Prepared::DryRun(v), _)) => return Outcome::DryRun(v),
        Ok(Staged::Ready(Prepared::Call(p), scope)) => (*p, scope),
        Ok(Staged::Answer(ok)) => return Outcome::Answer(ok),
        Err(e) => return Outcome::Gate(e),
    };
    if let Some(secs) = max_secs {
        prepared.cap_timeout(secs);
    }

    // 6 (mock) / 9-11 (HTTP): from here the call executes and carries event
    // metadata regardless of outcome.
    let account = prepared.account().to_string();
    let url = prepared.pre_auth_url();
    let (smtp_subject, smtp_recipients) = prepared.event_extra();
    // Whether the journal event can speak about response completeness is a
    // property of the call kind (only a real HTTP response read a body), so
    // it is read off the prepared call before dispatch consumes it.
    let http_like = matches!(prepared, PreparedCall::Http(_));
    let started = Instant::now();
    let (result, http_status) = prepared.dispatch();
    let duration_ms = started.elapsed().as_millis() as u64;
    let response_complete = match (&result, http_like) {
        (Ok(ok), true) => ok.response_complete(),
        (Err(err), true) if err.code == CallErrorCode::IncompleteResponse => Some(false),
        _ => None,
    };
    let (outcome, http_status) = match &result {
        Ok(_) => ("ok".to_string(), http_status),
        Err(err) => (
            err.code.as_str().to_string(),
            err.http_status.or(http_status),
        ),
    };
    let meta = EventMeta {
        account,
        url,
        outcome,
        http_status,
        duration_ms,
        smtp_subject,
        smtp_recipients,
        response_complete,
        scope,
    };
    match result {
        Ok(ok) => Outcome::Ok(ok, meta),
        Err(err) => Outcome::Executed(err, meta),
    }
}

/// What the gate + render stage hands on: a prepared call (or dry-run) with
/// the scope its journal record carries, or an answer that needs no call.
enum Staged {
    Ready(Prepared, CallScope),
    Answer(CallOk),
}

/// The result of the gate + render stage: a dry-run terminal, or a fully
/// prepared call ready to dispatch.
enum Prepared {
    DryRun(Value),
    // Boxed: `PreparedCall::Http` is much larger than `DryRun(Value)`.
    Call(Box<PreparedCall>),
}

/// A gated, rendered, secret-resolved call ready to dispatch. A mock carries a
/// pre-resolved status + body (no network, no secrets); an HTTP call carries
/// everything the request needs.
enum PreparedCall {
    Mock {
        account: String,
        status: u16,
        body: Value,
    },
    // Boxed: `HttpCall` is much larger than `Mock`, so the variant is boxed to
    // keep the enum small (clippy `large_enum_variant`).
    Http(Box<HttpCall>),
    // Boxed for the same reason: `SmtpCall` carries the built message bytes.
    Smtp {
        account: String,
        call: Box<crate::connector::smtp::SmtpCall>,
    },
    // Boxed for the same reason: `ImapCall` carries the resolved connection and
    // typed op plan. An imap call has no HTTP status, like smtp.
    Imap {
        account: String,
        call: Box<crate::connector::imap::ImapCall>,
    },
    // Boxed for the same reason: an inbox call carries its resolved consumer
    // and paging state. Like smtp and imap it has no HTTP status, and unlike
    // all three others it resolves no secret at all.
    Inbox {
        account: String,
        call: Box<crate::connector::inbox::InboxCall>,
    },
}

impl PreparedCall {
    /// Lowers the call's timeout to at most `secs` (never raises it). A
    /// mock answers at once, and an inbox call reads local state.
    fn cap_timeout(&mut self, secs: u64) {
        match self {
            PreparedCall::Http(h) => h.timeout_sec = h.timeout_sec.min(secs),
            PreparedCall::Smtp { call, .. } => call.cap_timeout(secs),
            PreparedCall::Imap { call, .. } => call.cap_timeout(secs),
            PreparedCall::Mock { .. } | PreparedCall::Inbox { .. } => {}
        }
    }

    fn account(&self) -> &str {
        match self {
            PreparedCall::Mock { account, .. } => account,
            PreparedCall::Http(h) => &h.account,
            PreparedCall::Smtp { account, .. } => account,
            PreparedCall::Imap { account, .. } => account,
            PreparedCall::Inbox { account, .. } => account,
        }
    }

    /// The pre-auth URL / endpoint for the event log; `""` for a mock, the
    /// pre-auth URL for HTTP, `smtp://host:port` for smtp, and
    /// `inbox://<connector>/<account>` for inbox.
    fn pre_auth_url(&self) -> String {
        match self {
            PreparedCall::Mock { .. } => String::new(),
            PreparedCall::Http(h) => h.pre_auth_url.clone(),
            PreparedCall::Smtp { call, .. } => call.endpoint(),
            PreparedCall::Imap { call, .. } => call.endpoint(),
            PreparedCall::Inbox { call, .. } => call.endpoint(),
        }
    }

    /// SMTP-only event metadata (subject, recipient count). `(None, None)` for
    /// every other kind, which record neither.
    fn event_extra(&self) -> (Option<String>, Option<u32>) {
        match self {
            PreparedCall::Mock { .. } | PreparedCall::Http(_) => (None, None),
            PreparedCall::Smtp { call, .. } => call.event_extra(),
            PreparedCall::Imap { call, .. } => call.event_extra(),
            PreparedCall::Inbox { call, .. } => call.event_extra(),
        }
    }

    /// Executes the call, returning the mapped result and the HTTP status when
    /// a response (or a mock status) was obtained.
    fn dispatch(self) -> (Result<CallOk, CallError>, Option<u16>) {
        match self {
            PreparedCall::Mock { status, body, .. } => {
                // A mock body is complete by construction: no read happened,
                // so no incompleteness can be reported, whatever the
                // function's read_only flag says.
                (map_status(status, body, None, None, false), Some(status))
            }
            PreparedCall::Http(h) => h.send(),
            // An smtp call has no HTTP status; the event log records the
            // endpoint plus subject/recipient count, never a status code.
            PreparedCall::Smtp { call, .. } => (call.send(), None),
            // An imap call likewise has no HTTP status; the event log records
            // only the endpoint (spec 3.4: no subjects).
            PreparedCall::Imap { call, .. } => (call.send(), None),
            // An inbox call never leaves the machine, so there is no status
            // and nothing to redact.
            PreparedCall::Inbox { call, .. } => (call.send(), None),
        }
    }
}

/// Gate + resolve + render (steps 1-8). Returns a dry-run terminal, a
/// `PreparedCall` with its journal scope, or an answer that needs no call;
/// or a gate error for any pre-execution failure.
fn prepare(req: &CallRequest, ctx: &CallContext) -> Result<Staged, CallError> {
    // 1, 2 and 4: where the call goes (the run's grant and snapshot, or a
    // live trust-checked target).
    let target = target::resolve(req, ctx)?;

    // 3. The attempt the call belongs to. A node's call from an attempt
    // that has ended, or that claims another attempt, is refused whatever
    // it calls: it would act, and be charged, in the current attempt's name.
    let events = attempt::journal(req.run_dir);
    let binding = attempt::bind(&events, req.node_id, &ctx.claim);
    if ctx.origin.charges_budget() && !binding.open {
        return Err(send_once::attempt_closed(req));
    }
    // Its max_calls budget: prior ConnectorCall events for this (node,
    // connector) grant, from the current attempt only. The budget is per
    // grant, so a second connector granted to the same node must not
    // consume this connector's budget. apb's own calls are never charged.
    let budget = |events: &[crate::event::Event]| -> Result<(), CallError> {
        let Some(grant) = target
            .grant
            .as_ref()
            .filter(|_| ctx.origin.charges_budget())
        else {
            return Ok(());
        };
        let binding = attempt::bind(events, req.node_id, &ctx.claim);
        check_budget(
            req,
            grant,
            &attempt::prior_calls(
                events,
                crate::exec_key::ExecKey::new(req.node_id, ctx.claim.item.as_deref()),
                req.connector,
                &binding,
            ),
        )
    };
    if ctx.origin.read_only_only() && !target.function.read_only {
        return Err(CallError::new(
            CallErrorCode::Permission,
            format!(
                "apb reads back an earlier send, and checks an item, only through read-only functions; `{}` is not one",
                req.function
            ),
        ));
    }

    // 4b. The send-once gate: a send through a function that declares
    // `idempotency` is answered from the ledger, or charged to the budget
    // and records its intent first, or is refused. Every other call is
    // charged here.
    let mut scope = CallScope {
        attempt: binding.attempt,
        attempt_seq: binding.attempt_seq,
        origin: ctx.origin,
        send: None,
        item: ctx.claim.item.clone(),
    };
    let gate_input = send_once::GateInput {
        req,
        ctx,
        on_unknown: target
            .grant
            .as_ref()
            .and_then(|g| g.on_unknown)
            .unwrap_or_default(),
        doc: &target.doc,
        function: &target.function,
        account: &target.account,
        read: &execute_with,
        budget: &budget,
    };
    match send_once::gate(&gate_input)? {
        send_once::SendGate::Untracked => budget(&events)?,
        send_once::SendGate::Answer(ok) => return Ok(Staged::Answer(ok)),
        send_once::SendGate::Send(tracked) => {
            scope.attempt = tracked.attempt;
            scope.attempt_seq = tracked.attempt_seq;
            scope.send = Some(*tracked);
        }
    }

    let stamp = scope.send.as_ref().map(send_once::Tracked::stamp);
    let built = build_prepared(
        &target.function,
        &target.doc,
        target.account.clone(),
        &target.maccount,
        &req.args,
        req.root,
        CallMode {
            dry_run: req.dry_run,
            full: req.full,
            stamp: stamp.as_ref(),
        },
    );
    match built {
        Ok(prepared) => Ok(Staged::Ready(prepared, scope)),
        Err(e) => {
            // The intent is recorded but nothing left: say so, or it would
            // read as unknown and block every repeat.
            if let Some(tracked) = &scope.send {
                send_once::settle_unsent(req.run_dir, tracked, &e);
            }
            Err(e)
        }
    }
}

/// Refuses a call once the grant's `max_calls` budget is spent in the
/// current attempt (or visit).
fn check_budget(
    req: &CallRequest,
    grant: &ManifestConnectorGrant,
    prior: &attempt::PriorCalls,
) -> Result<(), CallError> {
    let Some(limit) = grant.max_calls else {
        return Ok(());
    };
    if prior.current < u64::from(limit) {
        return Ok(());
    }
    let earlier = match prior.earlier {
        0 => String::new(),
        n => format!(
            "; {n} call(s) made by {} of this node are not counted against it",
            prior.unit.earlier_phrase()
        ),
    };
    Err(CallError::new(
        CallErrorCode::Permission,
        format!(
            "node `{}` reached its max_calls budget of {limit} for connector `{}`{}{earlier}",
            req.node_id,
            req.connector,
            prior.unit.current_phrase()
        ),
    ))
}

/// The call-mode settings `build_prepared` needs: `dry_run` renders without
/// executing or resolving secrets, `full` bypasses the `response_pick`
/// projection (spec 4.5), and `stamp` marks a send of a recorded intent.
/// Bundled so the shared render entry point stays within a sane argument
/// count.
#[derive(Debug, Clone, Copy)]
struct CallMode<'a> {
    dry_run: bool,
    full: bool,
    stamp: Option<&'a Stamp<'a>>,
}

/// The rendered HTTP shape of a function - method, the pre-auth URL (function
/// query included, auth NOT), the rendered body, and the effective request
/// headers (function `headers` plus the default `User-Agent` unless a function
/// header overrides it) - produced without touching the network. Shared by the
/// dry-run terminal, the live send path, and the offline `tests.yaml` runner
/// (`connector_test`), so a contract test renders exactly what a `--dry-run`
/// call renders.
pub struct RenderedRequest {
    pub method: String,
    pub pre_auth_url: String,
    pub rendered_body: Option<Value>,
    /// The rendered `body_form` pairs (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.1), DECODED and in
    /// manifest (BTreeMap) order; percent-encoding happens at send time so the
    /// offline runner and the dry-run terminal compare readable values.
    /// `None` when the function declares no `body_form` (mutually exclusive
    /// with `rendered_body` by validation).
    pub rendered_form: Option<Vec<(String, String)>>,
    /// The headers that would be sent: the rendered per-function `headers` with
    /// the default `User-Agent` folded in unless a function header already names
    /// it (case-insensitively). Auth headers are NOT included here (auth is
    /// injected on the wire, after this render).
    pub headers: BTreeMap<String, String>,
}

/// Renders a function's method, pre-auth URL, body, and headers against the
/// given non-secret account fields, args, and secrets. `secrets` is threaded
/// through so the render context stays uniform with the live send path (the
/// secret-placement policy forbids `{{secret.*}}` outside `auth`, so it does
/// not affect the URL/body here). Args substituted into the URL are
/// percent-encoded; account prefixes stay raw (spec 6.1), matching the previous
/// inline logic exactly. The default `User-Agent` (spec 4.4) is folded into the
/// header map unless a function header overrides it.
pub(crate) fn render_http(
    function: &FunctionSpec,
    account_fields: &BTreeMap<String, String>,
    args: &Value,
    secrets: &BTreeMap<String, Secret>,
    stamp: Option<&Stamp>,
) -> Result<RenderedRequest, CallError> {
    let ctx = RenderCtx {
        account: account_fields,
        args,
        secrets,
    };
    let method = function.method.clone().unwrap_or_else(|| "GET".to_string());
    // The URL base renders with raw substitution EXCEPT that `{{args.*}}` values
    // are pre-encoded: an account field used as a URL prefix (`base_url`) is
    // trusted config that must keep its `://` (spec 6.1) and so cannot go through
    // whole-value percent-encoding, while argument values in a path segment must
    // still be encoded so they cannot inject traversal or extra query structure.
    let url_args = encode_args_for_url(args);
    let url_ctx = RenderCtx {
        account: account_fields,
        args: &url_args,
        secrets,
    };
    let base = render_raw(function.url.as_deref().unwrap_or(""), &url_ctx)
        .map_err(|e| CallError::new(CallErrorCode::Config, format!("URL render failed: {e}")))?;
    let query = render_query(function, &ctx)?;
    let pre_auth_url = assemble_url(&base, &query);
    validate_url(&pre_auth_url)?;
    // Text modes prepare the named args before they reach the body, so no
    // body template can place the raw value; a write also has forged send
    // markers neutralized and carries its own stamp.
    let body_args = stamp::prepare_body_args(function, args, stamp);
    let body_ctx = RenderCtx {
        args: &body_args,
        ..ctx
    };
    let mut rendered_body = match &function.body {
        Some(b) => Some(render_body(b, &body_ctx).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("body render failed: {e}"))
        })?),
        None => None,
    };
    let rendered_form = if function.body_form.is_empty() {
        None
    } else {
        Some(render_form(function, &body_ctx)?)
    };
    let mut headers = BTreeMap::new();
    for (name, template) in &function.headers {
        let value = render_raw(template, &ctx).map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("header `{name}` render failed: {e}"),
            )
        })?;
        headers.insert(name.clone(), value);
    }
    // Default User-Agent (spec 4.4) unless a function header already names it.
    let has_ua = headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent"));
    if !has_ua {
        headers.insert("User-Agent".to_string(), USER_AGENT.to_string());
    }
    stamp::apply_key(function, stamp, &mut rendered_body, &mut headers)
        .map_err(|m| CallError::new(CallErrorCode::Config, m))?;
    Ok(RenderedRequest {
        method,
        pre_auth_url,
        rendered_body,
        rendered_form,
        headers,
    })
}

/// Renders the function's `body_form` pairs (spec
/// 2026-07-22-official-connectors-wave-3, section 4.1) as DECODED key-value
/// pairs, mirroring `render_query`'s semantics: a value that is exactly one
/// `{{args.field}}` placeholder drops the whole pair when the arg is absent
/// or explicitly null, renders the typed arg in canonical string form when
/// present (string verbatim, number/bool via their JSON text), and turns a
/// non-scalar into a Config error; mixed-content templates keep string
/// interpolation where an absent arg is an error.
fn render_form(
    function: &FunctionSpec,
    ctx: &RenderCtx,
) -> Result<Vec<(String, String)>, CallError> {
    let mut pairs = Vec::new();
    for (key, template) in &function.body_form {
        if let Some(field) = single_args_placeholder(template)
            && resolve_optional_arg(ctx, field).is_none()
        {
            continue;
        }
        let value = render_raw(template, ctx).map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("body_form render failed: {e}"),
            )
        })?;
        pairs.push((key.clone(), value));
    }
    Ok(pairs)
}

/// Shared gate + render logic (pipeline steps 5-8): validates args against the
/// function's schema, returns a mock immediately (no network, no secrets), or
/// resolves secrets and renders the URL/query/body with the 6.1 hardening.
/// Reused by both the run-scoped `CallRequest` pipeline (`prepare`, above,
/// which resolves `function`/`maccount` from the manifest + grant) and the
/// live healthcheck probe (`healthcheck`, below, which resolves them from the
/// live connector + account config) - the render/dispatch code itself must
/// never be duplicated between the two callers.
fn build_prepared(
    function: &FunctionSpec,
    doc: &ConnectorDoc,
    account_name: String,
    maccount: &ManifestAccount,
    args: &Value,
    root: &Path,
    mode: CallMode,
) -> Result<Prepared, CallError> {
    let CallMode {
        dry_run,
        full,
        stamp,
    } = mode;
    // A dry-run of a send shows where its stamp goes, never a real nonce.
    let stamp = match (dry_run, &function.idempotency) {
        (true, Some(_)) => Some(&Stamp::Preview),
        (true, None) => None,
        (false, _) => stamp,
    };
    // The two connector-level bindings an HTTP call snapshots from the
    // (snapshot or live) doc: the auth block and the error_when predicate
    // (spec 2026-07-22-official-connectors-wave-3, section 4.2).
    let auth = doc.auth.as_ref();
    let error_when = doc.error_when.as_ref();
    // 5. Validate args against the function schema.
    if let Some(schema) = &function.args_schema {
        validate_args(schema, args)?;
    }

    // 6. Mock: return the canned response (mapped through the status table by
    // dispatch). A mock makes no network call and needs no secrets.
    if let Some(mock) = &function.mock {
        return Ok(Prepared::Call(Box::new(PreparedCall::Mock {
            account: account_name,
            status: mock.status,
            body: mock.body.clone(),
        })));
    }

    // 6b. Inbox: a local store read or cursor move. Terminates before secret
    // resolution on purpose - the read path needs no credential, so it must
    // not cause one to be resolved (or a `{{cmd:...}}` helper to be run) as a
    // side effect of being called.
    if let Some(spec) = &function.inbox {
        return match crate::connector::inbox::build(
            spec,
            &doc.name,
            &account_name,
            args,
            // `--full` bypasses the projection (spec 4.5), like HTTP.
            if full {
                Vec::new()
            } else {
                function.response_pick.clone()
            },
            dry_run,
        )? {
            crate::connector::inbox::InboxBuild::DryRun(v) => Ok(Prepared::DryRun(v)),
            crate::connector::inbox::InboxBuild::Call(call) => {
                Ok(Prepared::Call(Box::new(PreparedCall::Inbox {
                    account: account_name,
                    call,
                })))
            }
        };
    }

    // 7. Secrets: resolve every env-ref field. Skipped entirely for a dry-run.
    let account_fields = non_secret_fields(maccount);
    let (secrets, redactions) = if dry_run {
        (BTreeMap::new(), Vec::new())
    } else {
        resolve_secrets(root, maccount)?
    };

    // 7b. SMTP: render the message/connection off the same resolved secrets and
    // account fields, or produce a dry-run envelope without connecting. An smtp
    // function has no URL/query/body/header rendering (those are HTTP-only), so
    // this branch is terminal.
    if let Some(smtp) = &function.smtp {
        return match crate::connector::smtp::build(
            smtp,
            &account_fields,
            args,
            &secrets,
            redactions,
            dry_run,
            function.timeout_sec,
        )? {
            crate::connector::smtp::SmtpBuild::DryRun(v) => Ok(Prepared::DryRun(v)),
            crate::connector::smtp::SmtpBuild::Call(call) => {
                Ok(Prepared::Call(Box::new(PreparedCall::Smtp {
                    account: account_name,
                    call,
                })))
            }
        };
    }

    // 7c. IMAP: render the connection/op off the same resolved secrets and
    // account fields, or produce a dry-run render without connecting. Like
    // smtp, an imap function has no URL/query/body/header rendering, so this
    // branch is terminal.
    if let Some(imap) = &function.imap {
        return match crate::connector::imap::build(
            imap,
            &account_fields,
            args,
            &secrets,
            redactions,
            dry_run,
            function.timeout_sec,
        )? {
            crate::connector::imap::ImapBuild::DryRun(v) => Ok(Prepared::DryRun(v)),
            crate::connector::imap::ImapBuild::Call(call) => {
                Ok(Prepared::Call(Box::new(PreparedCall::Imap {
                    account: account_name,
                    call,
                })))
            }
        };
    }

    // 8. Render URL, query, body, and headers (shared with the offline test
    // runner via `render_http`); enforce 6.1; build the pre-auth URL. The
    // default User-Agent and any function headers are folded into
    // `rendered.headers` there, so the same bytes reach the wire and a contract
    // test alike.
    let rendered = render_http(function, &account_fields, args, &secrets, stamp)?;
    // A Basic credential leaves as base64 of `user:password`, a form no
    // single secret's redaction matches: it gets its own.
    let mut redactions = redactions;
    redactions.extend(http::basic_redaction(auth, &account_fields, &secrets));

    if dry_run {
        let mut out = json!({
            "ok": true,
            "dry_run": true,
            "method": rendered.method,
            "url": rendered.pre_auth_url,
            "body": rendered.rendered_body.unwrap_or(Value::Null),
        });
        // A form body surfaces its DECODED pairs under `body_form` (with
        // `body: null`, since the two are mutually exclusive by validation).
        if let Some(pairs) = &rendered.rendered_form {
            out["body_form"] = Value::Object(
                pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            );
        }
        return Ok(Prepared::DryRun(out));
    }

    Ok(Prepared::Call(Box::new(PreparedCall::Http(Box::new(
        HttpCall {
            account: account_name,
            method: rendered.method,
            read_only: function.read_only,
            pre_auth_url: rendered.pre_auth_url,
            rendered_body: rendered.rendered_body,
            rendered_form: rendered.rendered_form,
            headers: rendered.headers,
            // `--full` bypasses the projection (spec 4.5), returning the raw body.
            response_pick: if full {
                Vec::new()
            } else {
                function.response_pick.clone()
            },
            auth: auth.cloned(),
            error_when: error_when.cloned(),
            secrets,
            account_fields,
            timeout_sec: function.timeout_sec,
            redactions,
        },
    )))))
}

/// Live healthcheck probe (spec 2026-07-18-connectors-design section 9): runs
/// ONLY the connector's declared `healthcheck` function against the LIVE
/// connector definition and LIVE merged account config - no run context, no
/// manifest, no event log, no grant/budget checks (those are run-scoped
/// concepts that do not apply to a standalone probe). Reuses the exact same
/// gate + render + dispatch pipeline a real call goes through
/// (`build_prepared` and `PreparedCall::dispatch`), so a mock healthcheck
/// returns its canned response and an HTTP healthcheck actually reaches the
/// network with the same URL hardening, auth injection, and interim secret
/// redaction as a normal call - that live reachability probe is the point of
/// the dashboard's probe button. Returns the same `{ "ok": bool, ... }` shape
/// `execute` does; the bool mirrors `execute`'s ok hint.
pub fn healthcheck(root: &Path, name: &str, account: &str) -> (Value, bool) {
    answer_live(prepare_healthcheck(root, name, account))
}

/// Dispatches a prepared live call outside a run (healthcheck, playground)
/// and shapes its answer: no journal, the same result JSON `execute` gives.
fn answer_live(prepared: Result<Prepared, CallError>) -> (Value, bool) {
    match prepared {
        Ok(Prepared::DryRun(v)) => (v, true),
        Ok(Prepared::Call(prepared)) => match prepared.dispatch().0 {
            Ok(ok) => (ok.to_success_json(), true),
            Err(err) => (err.to_json(), false),
        },
        Err(e) => (e.to_json(), false),
    }
}

/// Resolves the live connector + account for `healthcheck` and gates +
/// renders its declared healthcheck function through the shared
/// `prepare_play_call`, with no arguments (a healthcheck is a reachability
/// probe, not a data call) and always live (never dry-run).
fn prepare_healthcheck(root: &Path, name: &str, account: &str) -> Result<Prepared, CallError> {
    let loaded = store::load(name)
        .map_err(|e| CallError::new(CallErrorCode::Config, format!("connector `{name}`: {e}")))?;
    let hc_name = loaded.doc.healthcheck.clone().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{name}` declares no healthcheck"),
        )
    })?;
    // A healthcheck is always live (never dry-run) and takes no arguments,
    // per the healthcheck contract - it is a reachability probe, not a data
    // call. Delegates account resolution, trust gating, and rendering to
    // the shared playground preparation path so the two live-call callers
    // never duplicate that logic. `full: true` keeps the probe's existing
    // behavior: a reachability check shows the raw body, never a
    // `response_pick` projection.
    prepare_play_call(root, name, Some(account), &hc_name, &json!({}), false, true)
}

/// Live playground call (dashboard slice 6, spec
/// 2026-07-19-official-connectors-design section 7): the connector detail
/// page's manual "call a function" panel. Runs an arbitrary function
/// against the LIVE connector definition and LIVE merged account config
/// through the exact same gate + render + dispatch pipeline `healthcheck`
/// uses (`prepare_play_call` and `build_prepared`), so a dry-run renders
/// the request without touching secrets and a real call gets the same
/// trust gating, URL hardening, auth injection, and interim redaction as
/// any other call. `account: None` selects the single or default
/// configured account, exactly like the CLI's `select_account`. `full`
/// mirrors the CLI's `--full` escape (spec 4.5): `false` (the playground's
/// default) applies the function's `response_pick` projection like a normal
/// agent call, `true` bypasses it and returns the raw body. Returns the
/// same `{ "ok": bool, ... }` shape `execute`/`healthcheck` do.
pub fn play_call(
    root: &Path,
    name: &str,
    account: Option<&str>,
    function_name: &str,
    args: &Value,
    dry_run: bool,
    full: bool,
) -> (Value, bool) {
    answer_live(prepare_play_call(
        root,
        name,
        account,
        function_name,
        args,
        dry_run,
        full,
    ))
}

/// Shared live-call preparation for both `healthcheck` (fixed function, no
/// args, always live) and `play_call` (dashboard playground, spec section
/// 7: arbitrary function, args, optional dry-run): the [`LiveTarget`], with
/// no run context, no manifest, no event log and no grant or budget checks
/// (those are run-scoped concepts a standalone live call does not have).
///
/// Trust gating applies ONLY when `dry_run` is false: a dry-run never
/// resolves secrets (`build_prepared` skips secret resolution entirely for
/// `dry_run: true`), so gating it would refuse a safe, secret-free render
/// for no security benefit - the gate exists to guard secret egress, and a
/// dry-run has none to guard.
///
/// `full` threads straight into `CallMode` exactly like a real agent call's
/// `--full`: it is NOT hardcoded here, so a playground call without it
/// applies the function's `response_pick` projection (and can mark
/// `picked: true`) the same as any other call; `prepare_healthcheck` passes
/// `true` to keep the reachability probe showing the raw body.
fn prepare_play_call(
    root: &Path,
    name: &str,
    account: Option<&str>,
    function_name: &str,
    args: &Value,
    dry_run: bool,
    full: bool,
) -> Result<Prepared, CallError> {
    let live = LiveTarget::load(root, name, account, function_name)?;
    if !dry_run {
        live.require_trust()?;
    }
    let target = live.into_target()?;
    build_prepared(
        &target.function,
        &target.doc,
        target.account,
        &target.maccount,
        args,
        root,
        CallMode {
            dry_run,
            full,
            stamp: None,
        },
    )
}

/// Loads the snapshotted `ConnectorDoc` from `run_dir/connectors/<name>.yaml`.
pub(crate) fn load_snapshot(run_dir: &Path, connector: &str) -> Result<ConnectorDoc, CallError> {
    let path = run_dir.join("connectors").join(format!("{connector}.yaml"));
    let yaml = std::fs::read_to_string(&path).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{connector}` snapshot is missing or unreadable (drift): {e}"),
        )
    })?;
    ConnectorDoc::from_yaml(&yaml, connector).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{connector}` snapshot is invalid (drift): {e}"),
        )
    })
}

/// Renders the function's query pairs (keys literal, values percent-encoded)
/// into an already-encoded `k=v&k2=v2` string.
fn render_query(function: &FunctionSpec, ctx: &RenderCtx) -> Result<String, CallError> {
    let mut parts = Vec::new();
    for (key, template) in &function.query {
        // spec 5.1: a query value that is exactly `{{args.field}}` drops the
        // whole pair when the arg is absent, instead of erroring. An
        // explicit null arg is treated as absent and dropped too, so a
        // literal "null" never renders into the query string. A present,
        // non-null arg (scalar or not) still goes through `render_encoded`
        // below, which already renders scalars typed (string verbatim,
        // number/bool via their JSON text) and turns a non-scalar into a
        // Config error.
        if let Some(field) = single_args_placeholder(template)
            && resolve_optional_arg(ctx, field).is_none()
        {
            continue;
        }
        let value = render_encoded(template, ctx).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("query render failed: {e}"))
        })?;
        parts.push(format!("{}={value}", encode_component(key)));
    }
    Ok(parts.join("&"))
}

/// Enforces spec 6.1 URL hardening without a URL-parsing dependency: the URL
/// must begin (case-insensitively) with `http://` or `https://`, and the
/// authority (between `//` and the next `/`, `?`, or `#`) must not contain
/// userinfo (`user:pass@host`).
fn validate_url(url: &str) -> Result<(), CallError> {
    let lower = url.to_ascii_lowercase();
    let after = if let Some(rest) = lower.strip_prefix("https://") {
        &url[url.len() - rest.len()..]
    } else if let Some(rest) = lower.strip_prefix("http://") {
        &url[url.len() - rest.len()..]
    } else {
        return Err(CallError::new(
            CallErrorCode::Config,
            format!("rendered URL is not an absolute http(s) URL: `{url}`"),
        ));
    };
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    if after[..authority_end].contains('@') {
        return Err(CallError::new(
            CallErrorCode::Config,
            "rendered URL must not contain userinfo (user:pass@host)".to_string(),
        ));
    }
    Ok(())
}

/// Appends the `ConnectorCall` event for a reached call. Best-effort: a log
/// write failure must not change the call's already-computed result, so the
/// error is dropped (the result was printed regardless). The call runs
/// outside the drive, so the journal is opened as a foreign writer.
fn append_event(req: &CallRequest, meta: &EventMeta) {
    if let Ok(mut log) = EventLog::open_foreign(req.run_dir) {
        let _ = log.append(EventPayload::ConnectorCall {
            item: meta.scope.item.clone(),
            node_id: req.node_id.to_string(),
            connector: req.connector.to_string(),
            function: req.function.to_string(),
            account: meta.account.clone(),
            url: meta.url.clone(),
            outcome: meta.outcome.clone(),
            http_status: meta.http_status,
            duration_ms: meta.duration_ms,
            smtp_subject: meta.smtp_subject.clone(),
            smtp_recipients: meta.smtp_recipients,
            response_complete: meta.response_complete,
            attempt: meta.scope.attempt,
            attempt_seq: meta.scope.attempt_seq,
            intent: meta.scope.send.as_ref().map(|t| t.id.clone()),
            origin: meta.scope.origin.journal_tag(),
        });
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod select_account_tests {
    use super::*;
    use crate::manifest::{ManifestAccount, ManifestConnector, ManifestConnectorGrant};
    use std::collections::BTreeMap;
    use std::path::Path;

    fn maccount(name: &str, default: bool) -> ManifestAccount {
        ManifestAccount {
            name: name.to_string(),
            default,
            fields: BTreeMap::new(),
            env: BTreeMap::new(),
            cmd: BTreeMap::new(),
            digest: format!("sha256:{name}"),
        }
    }

    fn req<'a>() -> CallRequest<'a> {
        CallRequest {
            run_dir: Path::new("/tmp"),
            root: Path::new("/tmp"),
            node_id: "n",
            connector: "github",
            function: "create_issue",
            account: None,
            args: serde_json::json!({}),
            dry_run: true,
            full: false,
        }
    }

    #[test]
    fn ambiguous_grant_lists_account_names_in_error() {
        let grant = ManifestConnectorGrant {
            connector: "github".into(),
            accounts: vec!["work".into(), "personal".into()],
            functions: vec!["create_issue".into()],
            max_calls: None,
            on_unknown: None,
        };
        let mconn = ManifestConnector {
            name: "github".into(),
            digest: "sha256:c".into(),
            accounts: vec![maccount("work", false), maccount("personal", false)],
        };
        let err = select_account(&req(), &grant, &mconn).unwrap_err();
        assert!(
            err.message.contains("work") && err.message.contains("personal"),
            "choices must name granted accounts: {}",
            err.message
        );
        assert!(
            err.message.contains("choices: work, personal"),
            "choices list must be non-empty and ordered: {}",
            err.message
        );
    }

    #[test]
    fn empty_grant_accepts_no_account_but_error_still_names_snapshot_accounts() {
        // Finding 12 (narrowed): grant.accounts empty while the connector
        // snapshot still carries several accounts. Nothing is auto-selected,
        // but the error may still name the snapshot accounts as informational
        // context, not as valid choices.
        let grant = ManifestConnectorGrant {
            connector: "github".into(),
            accounts: vec![],
            functions: vec!["create_issue".into()],
            max_calls: None,
            on_unknown: None,
        };
        let mconn = ManifestConnector {
            name: "github".into(),
            digest: "sha256:c".into(),
            accounts: vec![maccount("work", false), maccount("personal", false)],
        };
        let err = select_account(&req(), &grant, &mconn).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Config);
        assert!(
            err.message.contains("grants no accounts"),
            "empty-grant error must say the binding grants no accounts: {}",
            err.message
        );
        assert!(
            err.message.contains("work") && err.message.contains("personal"),
            "error may still name snapshot accounts as context: {}",
            err.message
        );
    }

    #[test]
    fn empty_grant_rejects_explicit_account_even_if_snapshot_has_it() {
        // A deny-all `accounts: []` grant must not let an explicit --account
        // through just because the connector snapshot configures that
        // account. This is the authorization gate the narrowing restores.
        let grant = ManifestConnectorGrant {
            connector: "github".into(),
            accounts: vec![],
            functions: vec!["create_issue".into()],
            max_calls: None,
            on_unknown: None,
        };
        let mconn = ManifestConnector {
            name: "github".into(),
            digest: "sha256:c".into(),
            accounts: vec![maccount("work", true), maccount("personal", false)],
        };
        let mut request = req();
        request.account = Some("work");
        let err = select_account(&request, &grant, &mconn).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Permission);
        assert!(
            err.message.contains("not granted account `work`"),
            "explicit --account must be rejected for an empty grant: {}",
            err.message
        );
    }

    #[test]
    fn empty_grant_auto_selects_nothing_even_with_single_snapshot_account() {
        // Before commit 6639db5's regression, a single snapshot account with
        // an empty grant must still fail closed rather than auto-select.
        let grant = ManifestConnectorGrant {
            connector: "github".into(),
            accounts: vec![],
            functions: vec!["create_issue".into()],
            max_calls: None,
            on_unknown: None,
        };
        let mconn = ManifestConnector {
            name: "github".into(),
            digest: "sha256:c".into(),
            accounts: vec![maccount("work", true)],
        };
        let err = select_account(&req(), &grant, &mconn).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Config);
        assert!(err.message.contains("grants no accounts"));
    }
}
