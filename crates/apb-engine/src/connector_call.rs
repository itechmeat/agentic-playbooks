//! Connector call executor (spec 2026-07-18-connectors-design section 6 step
//! 4, plus 6.1 HTTP hardening and 6.2 interim redaction). The `apb connector
//! call` CLI process is a thin wrapper over [`execute`]: it reads the run
//! context from env, builds a [`CallRequest`], calls `execute`, prints the
//! returned JSON, and exits 0 when the second element is `true`.
//!
//! Everything after run start reads the immutable snapshot, never live files:
//! the run manifest (`manifest.yaml`) for grants and non-secret account
//! fields, and the copied `runs/<id>/connectors/<name>.yaml` for the function
//! definition. Only secret VALUES resolve live at call time, from the process
//! env / project dotenv / global dotenv chain (`secrets::resolve_var`), and
//! they never leave this process except inside the outgoing `auth` block.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::connector::config;
use apb_core::connector::def::{AuthSpec, ConnectorDoc, ErrorWhen, FunctionSpec};
use apb_core::connector::secrets;
use apb_core::connector::store;
use apb_core::connector::template::{
    RenderCtx, render_body, render_encoded, render_raw, resolve_optional_arg,
    single_args_placeholder,
};
use apb_core::trust::{TrustStore, account_trust_id};
use serde_json::{Value, json};

use crate::event::{EventLog, EventPayload};
use crate::manifest::{self, ManifestAccount, ManifestConnector, ManifestConnectorGrant};

/// The maximum response body read into memory (spec 6.1). A longer body is
/// truncated and the result marked `"truncated": true`.
const BODY_CAP: usize = 1024 * 1024;

/// The default User-Agent (spec 4.4): ureq sends its own, and GitHub rejects
/// requests without one it recognizes. Overridden by a function `headers`
/// entry naming `User-Agent`.
const USER_AGENT: &str = concat!("apb/", env!("CARGO_PKG_VERSION"));

/// One connector call request, assembled by the CLI from its arguments and the
/// run context env variables.
pub struct CallRequest<'a> {
    /// `runs/<id>` - the manifest, connector snapshots, and event log live here.
    pub run_dir: &'a Path,
    /// Project root, for secret resolution (`.apb/secrets.env`).
    pub root: &'a Path,
    pub node_id: &'a str,
    pub connector: &'a str,
    pub function: &'a str,
    /// Explicit `--account`; `None` selects the single or default granted account.
    pub account: Option<&'a str>,
    /// The call arguments (validated against `args_schema` when present).
    pub args: Value,
    /// `--dry-run`: render and print without executing, resolving no secrets.
    pub dry_run: bool,
    /// `--full`: return the complete response body, skipping the function's
    /// `response_pick` projection (spec 4.5 debugging escape).
    pub full: bool,
}

// The result taxonomy (`CallError`, `CallErrorCode`, `CallOk`) and the interim
// message redaction live in `connector_result`, the shared sink both the HTTP
// and SMTP call paths point at (keeps the file graph acyclic). Re-exported here
// so the public path `apb_engine::connector_call::{CallError, ...}` is stable.
use crate::connector_result::redact_message;
pub use crate::connector_result::{CallError, CallErrorCode, CallOk};

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
}

/// Runs the whole call pipeline and returns the JSON document to print plus an
/// ok hint (`true` -> exit 0, `false` -> non-zero). Appends a `ConnectorCall`
/// event itself for any call that actually executed; a dry-run and every gate
/// rejection append nothing.
pub fn execute(req: CallRequest) -> (Value, bool) {
    match run(&req) {
        Outcome::DryRun(value) => (value, true),
        Outcome::Ok(ok, meta) => {
            append_event(&req, &meta);
            (ok.to_success_json(), true)
        }
        Outcome::Executed(err, meta) => {
            append_event(&req, &meta);
            (err.to_json(), false)
        }
        Outcome::Gate(err) => (err.to_json(), false),
    }
}

/// The internal result of the pipeline: a dry-run render, a reached-call
/// success or error (both carry event metadata), or a pre-execution gate
/// rejection (no event).
enum Outcome {
    DryRun(Value),
    Ok(CallOk, EventMeta),
    Executed(CallError, EventMeta),
    Gate(CallError),
}

fn run(req: &CallRequest) -> Outcome {
    // 1-8: gate + render. Terminates as a dry-run or a gate rejection, neither
    // of which carries event metadata.
    let prepared = match prepare(req) {
        Ok(Prepared::DryRun(v)) => return Outcome::DryRun(v),
        Ok(Prepared::Call(p)) => *p,
        Err(e) => return Outcome::Gate(e),
    };

    // 6 (mock) / 9-11 (HTTP): from here the call executes and carries event
    // metadata regardless of outcome.
    let account = prepared.account().to_string();
    let url = prepared.pre_auth_url();
    let (smtp_subject, smtp_recipients) = prepared.event_extra();
    let started = Instant::now();
    let (result, http_status) = prepared.dispatch();
    let duration_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(ok) => Outcome::Ok(
            ok,
            EventMeta {
                account,
                url,
                outcome: "ok".to_string(),
                http_status,
                duration_ms,
                smtp_subject,
                smtp_recipients,
            },
        ),
        Err(err) => {
            let outcome = err.code.as_str().to_string();
            let http_status = err.http_status.or(http_status);
            Outcome::Executed(
                err,
                EventMeta {
                    account,
                    url,
                    outcome,
                    http_status,
                    duration_ms,
                    smtp_subject,
                    smtp_recipients,
                },
            )
        }
    }
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
        call: Box<crate::connector_smtp::SmtpCall>,
    },
    // Boxed for the same reason: `ImapCall` carries the resolved connection and
    // typed op plan. An imap call has no HTTP status, like smtp.
    Imap {
        account: String,
        call: Box<crate::connector_imap::ImapCall>,
    },
}

/// An HTTP call ready to send.
struct HttpCall {
    account: String,
    method: String,
    /// URL rendered BEFORE auth injection (function query included, auth query
    /// NOT). This is the URL recorded in the event log.
    pre_auth_url: String,
    rendered_body: Option<Value>,
    /// Rendered `body_form` pairs, DECODED (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.1); percent-encoded
    /// into the wire body by `send_raw`. Mutually exclusive with
    /// `rendered_body` by validation.
    rendered_form: Option<Vec<(String, String)>>,
    /// Rendered per-function headers (spec 4.4). Values use `account.*`/`args.*`
    /// only; `secret.*` is forbidden here by `validate_templates`.
    headers: BTreeMap<String, String>,
    /// The effective `response_pick` projection (spec 4.5); empty when the
    /// function declares none or `--full` bypasses it.
    response_pick: Vec<String>,
    auth: Option<AuthSpec>,
    /// The connector-level false-success predicate (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.2), applied by
    /// `send_raw` after a 2xx maps Ok and after body redaction, BEFORE the
    /// `response_pick` projection.
    error_when: Option<ErrorWhen>,
    secrets: BTreeMap<String, String>,
    account_fields: BTreeMap<String, String>,
    timeout_sec: u64,
    /// (resolved secret value, ENV var name) pairs for interim redaction.
    redactions: Vec<(String, String)>,
}

impl PreparedCall {
    fn account(&self) -> &str {
        match self {
            PreparedCall::Mock { account, .. } => account,
            PreparedCall::Http(h) => &h.account,
            PreparedCall::Smtp { account, .. } => account,
            PreparedCall::Imap { account, .. } => account,
        }
    }

    /// The pre-auth URL / endpoint for the event log; `""` for a mock, the
    /// pre-auth URL for HTTP, `smtp://host:port` for smtp.
    fn pre_auth_url(&self) -> String {
        match self {
            PreparedCall::Mock { .. } => String::new(),
            PreparedCall::Http(h) => h.pre_auth_url.clone(),
            PreparedCall::Smtp { call, .. } => call.endpoint(),
            PreparedCall::Imap { call, .. } => call.endpoint(),
        }
    }

    /// SMTP-only event metadata (subject, recipient count). `(None, None)` for
    /// HTTP and mock, which record neither.
    fn event_extra(&self) -> (Option<String>, Option<u32>) {
        match self {
            PreparedCall::Mock { .. } | PreparedCall::Http(_) => (None, None),
            PreparedCall::Smtp { call, .. } => call.event_extra(),
            PreparedCall::Imap { call, .. } => call.event_extra(),
        }
    }

    /// Executes the call, returning the mapped result and the HTTP status when
    /// a response (or a mock status) was obtained.
    fn dispatch(self) -> (Result<CallOk, CallError>, Option<u16>) {
        match self {
            PreparedCall::Mock { status, body, .. } => {
                (map_status(status, body, false, None), Some(status))
            }
            PreparedCall::Http(h) => h.send(),
            // An smtp call has no HTTP status; the event log records the
            // endpoint plus subject/recipient count, never a status code.
            PreparedCall::Smtp { call, .. } => (call.send(), None),
            // An imap call likewise has no HTTP status; the event log records
            // only the endpoint (spec 3.4: no subjects).
            PreparedCall::Imap { call, .. } => (call.send(), None),
        }
    }
}

/// Gate + resolve + render (steps 1-8). Returns a dry-run terminal or a
/// `PreparedCall`, or a gate error for any pre-execution failure.
fn prepare(req: &CallRequest) -> Result<Prepared, CallError> {
    // 1. Manifest.
    let manifest = manifest::read(req.run_dir)
        .map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("run manifest unreadable: {e}"),
            )
        })?
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                "this run has no manifest; connectors are unavailable",
            )
        })?;

    // 2. Grant lookup + account selection.
    let grants = manifest.grants_for(req.node_id);
    let grant = grants
        .iter()
        .find(|g| g.connector == req.connector)
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Permission,
                format!(
                    "node `{}` has no grant for connector `{}`",
                    req.node_id, req.connector
                ),
            )
        })?;
    if !grant.functions.iter().any(|f| f == req.function) {
        return Err(CallError::new(
            CallErrorCode::Permission,
            format!(
                "node `{}` may not call function `{}` on connector `{}`",
                req.node_id, req.function, req.connector
            ),
        ));
    }

    let mconn = manifest.connector(req.connector).ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{}` is not in the run manifest", req.connector),
        )
    })?;
    let account_name = select_account(req, grant, mconn)?;
    let maccount = mconn
        .accounts
        .iter()
        .find(|a| a.name == account_name)
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                format!("granted account `{account_name}` is not in the connector snapshot"),
            )
        })?;

    // 3. max_calls budget: count prior ConnectorCall events for this
    // (node, connector) grant. The budget is per grant, so a second connector
    // granted to the same node must not consume this connector's budget.
    if let Some(limit) = grant.max_calls {
        let prior = prior_call_count(req.run_dir, req.node_id, req.connector);
        if prior >= limit as u64 {
            return Err(CallError::new(
                CallErrorCode::Permission,
                format!(
                    "node `{}` reached its max_calls budget of {limit} for connector `{}`",
                    req.node_id, req.connector
                ),
            ));
        }
    }

    // 4. Load the snapshotted connector definition. The write-once run dir is
    // the integrity boundary: the manifest tree digest covers the whole live
    // connector folder and cannot be recomputed from this one copied file, so
    // drift protection is the snapshot itself. A missing or unparsable
    // snapshot, or a name that no longer matches, is Config drift.
    let doc = load_snapshot(req.run_dir, req.connector)?;
    let function = doc.function(req.function).cloned().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!(
                "function `{}` is missing from the `{}` snapshot (drift)",
                req.function, req.connector
            ),
        )
    })?;

    build_prepared(
        &function,
        &doc,
        account_name,
        maccount,
        &req.args,
        req.root,
        CallMode {
            dry_run: req.dry_run,
            full: req.full,
        },
    )
}

/// The two call-mode flags `build_prepared` needs: `dry_run` renders without
/// executing or resolving secrets, `full` bypasses the `response_pick`
/// projection (spec 4.5). Bundled so the shared render entry point stays within
/// a sane argument count.
#[derive(Debug, Clone, Copy)]
struct CallMode {
    dry_run: bool,
    full: bool,
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
    secrets: &BTreeMap<String, String>,
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
    let rendered_body = match &function.body {
        Some(b) => Some(render_body(b, &ctx).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("body render failed: {e}"))
        })?),
        None => None,
    };
    let rendered_form = if function.body_form.is_empty() {
        None
    } else {
        Some(render_form(function, &ctx)?)
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
    let CallMode { dry_run, full } = mode;
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
        return match crate::connector_smtp::build(
            smtp,
            &account_fields,
            args,
            &secrets,
            redactions,
            dry_run,
            function.timeout_sec,
        )? {
            crate::connector_smtp::SmtpBuild::DryRun(v) => Ok(Prepared::DryRun(v)),
            crate::connector_smtp::SmtpBuild::Call(call) => {
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
        return match crate::connector_imap::build(
            imap,
            &account_fields,
            args,
            &secrets,
            redactions,
            dry_run,
            function.timeout_sec,
        )? {
            crate::connector_imap::ImapBuild::DryRun(v) => Ok(Prepared::DryRun(v)),
            crate::connector_imap::ImapBuild::Call(call) => {
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
    let rendered = render_http(function, &account_fields, args, &secrets)?;

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
    match prepare_healthcheck(root, name, account) {
        Ok(Prepared::DryRun(v)) => (v, true),
        Ok(Prepared::Call(prepared)) => {
            let (result, _status) = prepared.dispatch();
            match result {
                Ok(ok) => (ok.to_success_json(), true),
                Err(err) => (err.to_json(), false),
            }
        }
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
    match prepare_play_call(root, name, account, function_name, args, dry_run, full) {
        Ok(Prepared::DryRun(v)) => (v, true),
        Ok(Prepared::Call(prepared)) => {
            let (result, _status) = prepared.dispatch();
            match result {
                Ok(ok) => (ok.to_success_json(), true),
                Err(err) => (err.to_json(), false),
            }
        }
        Err(e) => (e.to_json(), false),
    }
}

/// Shared live-call preparation for both `healthcheck` (fixed function, no
/// args, always live) and `play_call` (dashboard playground, spec section
/// 7: arbitrary function, args, optional dry-run). Loads the LIVE
/// connector definition and LIVE merged account config - no run context,
/// no manifest, no event log, no grant/budget checks (those are run-scoped
/// concepts a standalone live call does not have).
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
    let loaded = store::load(name)
        .map_err(|e| CallError::new(CallErrorCode::Config, format!("connector `{name}`: {e}")))?;
    let function = loaded.doc.function(function_name).cloned().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{name}` declares no function `{function_name}`"),
        )
    })?;

    let accounts = config::load_merged(root, name).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{name}` account config: {e}"),
        )
    })?;
    let acct = select_live_account(&accounts, account)
        .ok_or_else(|| account_selection_error(name, account, &accounts))?
        .clone();

    if !dry_run {
        // Trust gate (spec 2026-07-18-connectors-design section 9): a live
        // call resolves LIVE secrets and sends them to the LIVE config's
        // base_url, so an unapproved or changed connector/account must
        // never be callable - the same guard `apb_mcp::policy::check_connectors`
        // applies before a real run, checked here before anything below
        // touches a secret. Connector digest first (a changed folder is a
        // bigger deal than one account), then the target account's own
        // digest.
        let trust = TrustStore::load();
        if !trust.is_approved(&loaded.digest) {
            return Err(CallError::new(
                CallErrorCode::Permission,
                format!(
                    "connector `{name}` is not approved; approve it before calling (see the connector approve flow)"
                ),
            ));
        }
        let account_digest = config::account_digest(&acct);
        if !trust.is_approved(&account_digest) {
            return Err(CallError::new(
                CallErrorCode::Permission,
                format!(
                    "account `{}` is not approved; approve it before calling (see the connector approve flow)",
                    account_trust_id(name, &acct.name)
                ),
            ));
        }
    }

    let errors = config::validate_accounts(&loaded.doc, std::slice::from_ref(&acct));
    if !errors.is_empty() {
        return Err(CallError::new(
            CallErrorCode::Config,
            format!(
                "connector `{name}` account `{}` is invalid: {}",
                acct.name,
                errors.join("; ")
            ),
        ));
    }

    let env = config::env_refs(&loaded.doc, &acct);
    let cmd = config::cmd_refs(&loaded.doc, &acct);
    let secret_keys: std::collections::HashSet<&str> =
        env.keys().chain(cmd.keys()).map(String::as_str).collect();
    let fields: BTreeMap<String, String> = acct
        .fields
        .iter()
        .filter(|(k, _)| !secret_keys.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let digest = config::account_digest(&acct);
    let maccount = ManifestAccount {
        name: acct.name.clone(),
        default: acct.default,
        fields,
        env,
        cmd,
        digest,
    };

    build_prepared(
        &function,
        &loaded.doc,
        acct.name.clone(),
        &maccount,
        args,
        root,
        // `full` is the caller's real flag, not hardcoded: `prepare_healthcheck`
        // passes `true` (a reachability probe shows the raw body), the
        // playground's `play_call` passes whatever the caller asked for
        // (default `false`, applying `response_pick` like a normal call).
        CallMode { dry_run, full },
    )
}

/// Picks the account for a live probe/playground call: an explicit name
/// must match one of the LIVE configured accounts; with none given, the
/// single configured account is used, else the one flagged `default`, else
/// no selection (ambiguous, reported by the caller via
/// `account_selection_error`). Mirrors the CLI pipeline's `select_account`
/// defaulting rule, minus the grant list (a live call has no grants).
fn select_live_account<'a>(
    accounts: &'a [config::Account],
    account: Option<&str>,
) -> Option<&'a config::Account> {
    if let Some(explicit) = account {
        return accounts.iter().find(|a| a.name == explicit);
    }
    if let [only] = accounts {
        return Some(only);
    }
    let defaults: Vec<&config::Account> = accounts.iter().filter(|a| a.default).collect();
    if let [only] = defaults.as_slice() {
        return Some(only);
    }
    None
}

fn account_selection_error(
    name: &str,
    account: Option<&str>,
    accounts: &[config::Account],
) -> CallError {
    if let Some(explicit) = account {
        return CallError::new(
            CallErrorCode::Config,
            format!("connector `{name}` has no account `{explicit}`"),
        );
    }
    let choices: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
    CallError::new(
        CallErrorCode::Config,
        format!(
            "connector `{name}` has several accounts and no single default; specify an account (choices: {})",
            choices.join(", ")
        ),
    )
}

/// Account selection (spec 6 step 4): an explicit `--account` must be granted;
/// with none given, the single granted account is used, else the granted
/// account flagged `default` in the connector snapshot, else a Config error
/// listing the choices.
fn select_account(
    req: &CallRequest,
    grant: &ManifestConnectorGrant,
    mconn: &ManifestConnector,
) -> Result<String, CallError> {
    if let Some(explicit) = req.account {
        if grant.accounts.iter().any(|a| a == explicit) {
            return Ok(explicit.to_string());
        }
        return Err(CallError::new(
            CallErrorCode::Permission,
            format!(
                "node `{}` is not granted account `{explicit}` on connector `{}`",
                req.node_id, req.connector
            ),
        ));
    }

    if grant.accounts.len() == 1 {
        return Ok(grant.accounts[0].clone());
    }

    let defaults: Vec<&String> = grant
        .accounts
        .iter()
        .filter(|name| mconn.accounts.iter().any(|a| &a.name == *name && a.default))
        .collect();
    if let [only] = defaults.as_slice() {
        return Ok((*only).clone());
    }

    Err(CallError::new(
        CallErrorCode::Config,
        format!(
            "connector `{}` has several granted accounts and no single default; pass --account (choices: {})",
            req.connector,
            grant.accounts.join(", ")
        ),
    ))
}

/// Counts prior `ConnectorCall` events for this `(node_id, connector)` grant,
/// of any outcome (spec 6 step 4 max_calls). Filtering by connector too keeps
/// each grant's budget independent when one node is granted several connectors.
/// A read failure yields 0 (fail open on the budget count is safe: the event
/// log only grows, and a genuinely-hit budget still trips on the next call once
/// the log reads again).
fn prior_call_count(run_dir: &Path, node_id: &str, connector: &str) -> u64 {
    crate::event::read_all(run_dir)
        .map(|events| {
            events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.payload,
                        EventPayload::ConnectorCall { node_id: n, connector: c, .. }
                            if n == node_id && c == connector
                    )
                })
                .count() as u64
        })
        .unwrap_or(0)
}

/// Loads the snapshotted `ConnectorDoc` from `run_dir/connectors/<name>.yaml`.
fn load_snapshot(run_dir: &Path, connector: &str) -> Result<ConnectorDoc, CallError> {
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

/// Validates `args` against the function's `args_schema`, failing as
/// `InvalidArgs` naming the first offending instance path.
fn validate_args(schema: &Value, args: &Value) -> Result<(), CallError> {
    let validator = jsonschema::validator_for(schema).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("function args_schema is not a valid JSON schema: {e}"),
        )
    })?;
    if let Some(err) = validator.iter_errors(args).next() {
        let path = err.instance_path().to_string();
        let where_ = if path.is_empty() {
            "(root)".to_string()
        } else {
            path
        };
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            format!("args failed schema validation at `{where_}`: {err}"),
        ));
    }
    Ok(())
}

/// The non-secret account fields: every field whose key is NOT a secret field
/// (env-backed or command-backed). Secret fields hold a raw `{{env.VAR}}` /
/// `{{cmd:...}}` reference in the manifest and must never reach the render
/// context's `account` map.
fn non_secret_fields(account: &ManifestAccount) -> BTreeMap<String, String> {
    account
        .fields
        .iter()
        .filter(|(k, _)| {
            !account.env.contains_key(k.as_str()) && !account.cmd.contains_key(k.as_str())
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The resolved secrets map (field name -> value) plus the redaction pairs
/// (resolved value, redaction label) the response body is scrubbed against.
/// The label is the ENV var name for env-sourced secrets and `cmd:<field>`
/// for command-sourced ones.
type ResolvedSecrets = (BTreeMap<String, String>, Vec<(String, String)>);

/// Resolves every secret field to its value: env-ref fields via the secrets
/// resolution chain, cmd-ref fields by executing the command (spec 4.1).
/// Returns the secrets map keyed by FIELD name (for the render context) and
/// the redaction pairs (resolved value, redaction label). A var that resolves
/// nowhere, or a command that fails, is a Config error naming the field.
fn resolve_secrets(root: &Path, account: &ManifestAccount) -> Result<ResolvedSecrets, CallError> {
    let mut secrets = BTreeMap::new();
    let mut redactions = Vec::new();
    for (field, var) in &account.env {
        let value = secrets::resolve_var(root, var).ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                format!("secret env var `{var}` (account field `{field}`) is not set"),
            )
        })?;
        // Empty secrets would redact every empty run in the body; skip them.
        if !value.is_empty() {
            redactions.push((value.clone(), var.clone()));
        }
        secrets.insert(field.clone(), value);
    }
    for (field, cmdline) in &account.cmd {
        let value = secrets::resolve_cmd(cmdline, secrets::CMD_SECRET_TIMEOUT)
            .map_err(|e| cmd_secret_error(&account.name, field, e))?;
        // resolve_cmd rejects empty output, so the value is always non-empty
        // and safe to register for redaction. The label carries no secret.
        redactions.push((value.clone(), format!("cmd:{field}")));
        secrets.insert(field.clone(), value);
    }
    Ok((secrets, redactions))
}

/// Maps a `CmdSecretError` to a `config` call error naming the account and
/// field and, where the helper produced one, a trimmed stderr excerpt. The
/// resolved secret is never part of any variant, so nothing sensitive can
/// reach this message.
fn cmd_secret_error(account: &str, field: &str, err: secrets::CmdSecretError) -> CallError {
    use secrets::CmdSecretError as E;
    let detail = match err {
        E::Parse(m) => format!("command reference is not valid: {m}"),
        E::Spawn(m) => format!("command could not start: {m}"),
        E::Timeout => "command timed out after 10s".to_string(),
        E::NonZero { code, stderr } => {
            let code = code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string());
            if stderr.is_empty() {
                format!("command exited with status {code}")
            } else {
                format!("command exited with status {code}: {stderr}")
            }
        }
        E::Empty { stderr } => {
            if stderr.is_empty() {
                "command produced no output".to_string()
            } else {
                format!("command produced no output: {stderr}")
            }
        }
    };
    CallError::new(
        CallErrorCode::Config,
        format!("secret for account `{account}` field `{field}`: {detail}"),
    )
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

/// Appends an already-encoded query string to a base URL, choosing `?` or `&`
/// depending on whether the base already carries a query.
fn assemble_url(base: &str, query: &str) -> String {
    if query.is_empty() {
        return base.to_string();
    }
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}{query}")
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

impl HttpCall {
    /// Sends the request and returns the mapped result, redacting any error
    /// MESSAGE through the same interim literal scrub as the body. Kept as
    /// defense in depth regardless of what a given transport error's
    /// `Display` happens to include (ureq 2's did put the request URL in it,
    /// which for `query`-kind auth carries the resolved secret): every error
    /// produced after secrets are resolved is scrubbed before it can be
    /// printed or logged (spec 6.2).
    fn send(&self) -> (Result<CallOk, CallError>, Option<u16>) {
        let (result, status) = self.send_raw();
        (result.map_err(|e| self.redact_error(e)), status)
    }

    /// Sends the request with redirects disabled and the per-function timeout,
    /// injecting auth after the pre-auth URL is fixed, then maps the response
    /// (or transport error) into the result taxonomy. Returns the mapped
    /// result and the observed HTTP status when one exists.
    ///
    /// ureq 3 notes: `http_status_as_error(false)` keeps every status (2xx
    /// through 5xx) coming back as `Ok(response)` exactly like ureq 2's
    /// `Error::Status(_, resp)` did, so the taxonomy below is still driven
    /// entirely by `map_status` reading the real response, never by ureq's own
    /// status-to-error translation. `allow_non_standard_methods(true)` keeps
    /// an unusual/lowercased connector `method` string sendable, matching
    /// ureq 2's untyped `&str` method (ureq 3's methods are typed
    /// `http::Method`, which rejects nothing byte-valid but does gate
    /// non-standard tokens behind this flag).
    fn send_raw(&self) -> (Result<CallOk, CallError>, Option<u16>) {
        let method = match ureq::http::Method::from_bytes(self.method.as_bytes()) {
            Ok(m) => m,
            Err(e) => {
                return (
                    Err(CallError::new(
                        CallErrorCode::Config,
                        format!("invalid HTTP method `{}`: {e}", self.method),
                    )),
                    None,
                );
            }
        };
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(self.timeout_sec)))
            .allow_non_standard_methods(true)
            .http_status_as_error(false)
            .build();
        let agent = ureq::Agent::new_with_config(config);

        // Build the request URL: the pre-auth URL plus a `query`-kind auth
        // param, if any. Only this final URL is sent; the pre-auth URL is what
        // the event log records.
        let ctx = RenderCtx {
            account: &self.account_fields,
            args: &Value::Null,
            secrets: &self.secrets,
        };
        let request_url = match &self.auth {
            Some(AuthSpec::Path { value_template }) => match render_raw(value_template, &ctx) {
                Ok(seg) => self
                    .pre_auth_url
                    .replacen("{{auth}}", &encode_path_segment(&seg), 1),
                Err(e) => {
                    return (
                        Err(CallError::new(
                            CallErrorCode::Config,
                            format!("auth render failed: {e}"),
                        )),
                        None,
                    );
                }
            },
            _ => match self.auth_query(&ctx) {
                Ok(Some((param, value))) => {
                    assemble_url(&self.pre_auth_url, &format!("{param}={value}"))
                }
                Ok(None) => self.pre_auth_url.clone(),
                Err(e) => return (Err(e), None),
            },
        };

        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(&request_url);
        // Headers were fully assembled by `render_http` (function headers plus
        // the default User-Agent unless overridden, spec 4.4), so send them
        // verbatim.
        for (name, value) in &self.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        // Header / Basic auth injection.
        match self.auth_header(&ctx) {
            Ok(Some((name, value))) => builder = builder.header(name, value),
            Ok(None) => {}
            Err(e) => return (Err(e), None),
        }

        // The outgoing body payload: a JSON body serializes with the JSON
        // content type, a form body (spec 2026-07-22-official-connectors-wave-3,
        // section 4.1) percent-encodes its pairs with the urlencoded content
        // type. The two are mutually exclusive by validation.
        let payload = match &self.rendered_body {
            Some(body) => match serde_json::to_vec(body) {
                Ok(bytes) => Some((bytes, "application/json")),
                Err(e) => {
                    return (
                        Err(CallError::new(
                            CallErrorCode::Config,
                            format!("body serialize failed: {e}"),
                        )),
                        None,
                    );
                }
            },
            None => self.rendered_form.as_ref().map(|pairs| {
                (
                    encode_form_body(pairs).into_bytes(),
                    "application/x-www-form-urlencoded",
                )
            }),
        };
        let response = match payload {
            Some((bytes, content_type)) => {
                // Mirrors ureq 2's `send_json`: only add the default content
                // type when the function's own headers did not already set one.
                let has_content_type = self
                    .headers
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case("content-type"));
                if !has_content_type {
                    builder = builder.header("content-type", content_type);
                }
                match builder.body(bytes) {
                    Ok(request) => agent.run(request),
                    Err(e) => {
                        return (
                            Err(CallError::new(
                                CallErrorCode::Config,
                                format!("request could not be built: {e}"),
                            )),
                            None,
                        );
                    }
                }
            }
            None => match builder.body(()) {
                Ok(request) => agent.run(request),
                Err(e) => {
                    return (
                        Err(CallError::new(
                            CallErrorCode::Config,
                            format!("request could not be built: {e}"),
                        )),
                        None,
                    );
                }
            },
        };

        let response = match response {
            Ok(resp) => resp,
            Err(e) => {
                // ureq 3 gives timeouts their own variant (no more sniffing
                // "timed out" out of a generic transport error's Display).
                let code = if matches!(e, ureq::Error::Timeout(_)) {
                    CallErrorCode::Timeout
                } else {
                    CallErrorCode::Network
                };
                return (Err(CallError::new(code, e.to_string())), None);
            }
        };

        let status = response.status().as_u16();
        // Parse `Retry-After` before the response is consumed by the reader;
        // only used to populate `retry_after_sec` on a 429 (spec section 8).
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());
        // The `Link` header must be read before the response is consumed by the
        // body reader; surfaced verbatim in the result (spec 4.4).
        let link = response
            .headers()
            .get("Link")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let (body, truncated) = read_body(response);
        // 12. Interim literal secret redaction (see the TODO below).
        let body = self.redact(body);
        let mut mapped = map_status(status, body, truncated, retry_after);
        // error_when reclassification (spec
        // 2026-07-22-official-connectors-wave-3, section 4.2): a 2xx body
        // matching the connector's predicate maps to a `service` error
        // instead of a false success. Runs on the already-redacted body, so
        // an extracted message is scrubbed like any other body text, and
        // BEFORE the `response_pick` projection below (a reclassified
        // response is never projected; the error envelope carries the
        // extracted message instead). Non-2xx statuses arrive here as Err
        // and keep the status-table mapping untouched.
        if let Ok(CallOk::Http { body, .. }) = &mapped
            && let Some(rule) = &self.error_when
            && let Some(err) = reclassify_error_when(rule, body, status)
        {
            mapped = Err(err);
        }
        if let Ok(CallOk::Http {
            link: link_slot,
            body: body_slot,
            picked,
            ..
        }) = &mut mapped
        {
            *link_slot = link;
            if !self.response_pick.is_empty() {
                *body_slot = project(body_slot, &self.response_pick);
                *picked = true;
            }
        }
        (mapped, Some(status))
    }

    /// The `query`-kind auth param, already percent-encoded, if auth is a
    /// `Query` variant.
    fn auth_query(&self, ctx: &RenderCtx) -> Result<Option<(String, String)>, CallError> {
        match &self.auth {
            Some(AuthSpec::Query {
                param,
                value_template,
            }) => {
                let value = render_encoded(value_template, ctx).map_err(|e| {
                    CallError::new(CallErrorCode::Config, format!("auth render failed: {e}"))
                })?;
                Ok(Some((encode_component(param), value)))
            }
            _ => Ok(None),
        }
    }

    /// The auth header name/value, if auth is a `Header` or `Basic` variant.
    fn auth_header(&self, ctx: &RenderCtx) -> Result<Option<(String, String)>, CallError> {
        match &self.auth {
            Some(AuthSpec::Header {
                header,
                value_template,
            }) => {
                let value = render_raw(value_template, ctx).map_err(|e| {
                    CallError::new(CallErrorCode::Config, format!("auth render failed: {e}"))
                })?;
                Ok(Some((header.clone(), value)))
            }
            Some(AuthSpec::Basic {
                username_template,
                password_template,
            }) => {
                let user = render_raw(username_template, ctx).map_err(|e| {
                    CallError::new(CallErrorCode::Config, format!("auth render failed: {e}"))
                })?;
                let pass = render_raw(password_template, ctx).map_err(|e| {
                    CallError::new(CallErrorCode::Config, format!("auth render failed: {e}"))
                })?;
                let token = base64_encode(format!("{user}:{pass}").as_bytes());
                Ok(Some((
                    "Authorization".to_string(),
                    format!("Basic {token}"),
                )))
            }
            _ => Ok(None),
        }
    }

    // TODO(redaction-layer): interim literal redaction, replaced by the dedicated LLM-output redaction story.
    /// Replaces every literal occurrence of each resolved secret value in the
    /// body's string leaves with `[redacted:<ENV_NAME>]`. Only exact literal
    /// matches are caught (spec 6.2, accepted interim limit).
    fn redact(&self, body: Value) -> Value {
        if self.redactions.is_empty() {
            return body;
        }
        redact_value(body, &self.redactions)
    }

    /// Scrubs an error message with the same interim literal redaction as the
    /// body. A transport error's URL (and thus a `query`-kind auth secret) can
    /// otherwise leak into `error.message`.
    fn redact_error(&self, mut err: CallError) -> CallError {
        err.message = redact_message(err.message, &self.redactions);
        err
    }
}

/// Recursively redacts secret values in a JSON value's string leaves.
fn redact_value(value: Value, redactions: &[(String, String)]) -> Value {
    match value {
        Value::String(mut s) => {
            for (secret, var) in redactions {
                if s.contains(secret.as_str()) {
                    s = s.replace(secret.as_str(), &format!("[redacted:{var}]"));
                }
            }
            Value::String(s)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| redact_value(v, redactions))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, redact_value(v, redactions)))
                .collect(),
        ),
        other => other,
    }
}

/// Reads a response body up to `BODY_CAP` (+1 to detect overflow), returning
/// the parsed-or-string body and whether it was truncated. A JSON content type
/// yields a parsed value; anything else yields a lossy-UTF8 string.
fn read_body(mut response: ureq::http::Response<ureq::Body>) -> (Value, bool) {
    let is_json = response.body().mime_type() == Some("application/json");
    let mut buf = Vec::new();
    // A read error yields whatever was collected so far rather than failing the
    // whole call after a response was already obtained. `as_reader()` is not
    // capped by ureq itself, so the `.take(BODY_CAP + 1)` below is what bounds
    // memory (same as before).
    let _ = response
        .body_mut()
        .as_reader()
        .take(BODY_CAP as u64 + 1)
        .read_to_end(&mut buf);
    let truncated = buf.len() > BODY_CAP;
    if truncated {
        buf.truncate(BODY_CAP);
    }
    let body = if is_json {
        serde_json::from_slice(&buf)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&buf).into_owned()))
    } else {
        Value::String(String::from_utf8_lossy(&buf).into_owned())
    };
    (body, truncated)
}

/// The HTTP-status -> result mapping (spec section 8). 2xx is success; 3xx is a
/// `service` error (redirects are not followed); 401/403 -> `auth`; 404 ->
/// `not_found`; 429 -> `rate_limited` (with `retry_after` when the service
/// gave a `Retry-After`); every other non-2xx -> `service`.
fn map_status(
    status: u16,
    body: Value,
    truncated: bool,
    retry_after: Option<u64>,
) -> Result<CallOk, CallError> {
    if (200..300).contains(&status) {
        return Ok(CallOk::Http {
            status,
            body,
            truncated,
            link: None,
            picked: false,
        });
    }
    let mut err = match status {
        300..=399 => CallError::new(
            CallErrorCode::Service,
            format!("the service returned a redirect (HTTP {status}); redirects are not followed"),
        ),
        401 | 403 => CallError::new(
            CallErrorCode::Auth,
            format!("the service rejected the credentials (HTTP {status})"),
        ),
        404 => CallError::new(
            CallErrorCode::NotFound,
            "the service returned 404 not found".to_string(),
        ),
        429 => {
            let mut e = CallError::new(
                CallErrorCode::RateLimited,
                "the service rate-limited the request (HTTP 429)".to_string(),
            );
            e.retry_after_sec = retry_after;
            e
        }
        _ => CallError::new(
            CallErrorCode::Service,
            format!("the service returned HTTP {status}"),
        ),
    };
    err.http_status = Some(status);
    Err(err)
}

/// Evaluates the connector's `error_when` predicate (spec
/// 2026-07-22-official-connectors-wave-3, section 4.2) against a 2xx response
/// body: when the value at `path` equals `equals` (JSON equality), returns
/// the `service` error the call reclassifies to, carrying the real HTTP
/// status (e.g. 200) and the message extracted at `message_path` - or a
/// fixed fallback when `message_path` is absent, resolves to nothing, or the
/// value there is not a string. A `path` that resolves to nothing means no
/// match (success passes through).
fn reclassify_error_when(rule: &ErrorWhen, body: &Value, status: u16) -> Option<CallError> {
    if lookup_path(body, &rule.path) != Some(&rule.equals) {
        return None;
    }
    let message = rule
        .message_path
        .as_deref()
        .and_then(|p| lookup_path(body, p))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "the service reported an error in the response body".to_string());
    let mut err = CallError::new(CallErrorCode::Service, message);
    err.http_status = Some(status);
    Some(err)
}

/// Resolves a dot-separated field chain over JSON objects (like
/// `response_pick` paths, spec 4.5, without array semantics). `None` when any
/// segment is missing or a value midway through the chain is not an object.
fn lookup_path<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(body, |value, segment| {
        value.as_object().and_then(|map| map.get(segment))
    })
}

/// Percent-encodes the top-level string values of an args object so they can
/// be substituted RAW into a URL template without letting an argument inject
/// path traversal or extra query structure. Non-string scalars pass through
/// (their `to_string` form carries no reserved bytes); nested values are not
/// reachable by a URL `{{args.name}}` placeholder (only top-level scalar args
/// are), so they are left untouched.
fn encode_args_for_url(args: &Value) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => Value::String(encode_component(s)),
                        other => other.clone(),
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Projects `body` down to the dot-separated field chains in `paths`
/// (spec 4.5). Objects keep the named field; arrays (top-level or midway
/// through a chain) map the projection over their elements; a path absent
/// from the body is silently dropped.
fn project(body: &Value, paths: &[String]) -> Value {
    let split: Vec<Vec<&str>> = paths.iter().map(|p| p.split('.').collect()).collect();
    let refs: Vec<&[&str]> = split.iter().map(Vec::as_slice).collect();
    project_value(body, &refs)
}

fn project_value(source: &Value, paths: &[&[&str]]) -> Value {
    match source {
        Value::Array(items) => {
            Value::Array(items.iter().map(|it| project_value(it, paths)).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, val) in map {
                let mut sub: Vec<&[&str]> = Vec::new();
                let mut terminal = false;
                for p in paths {
                    if p.first() == Some(&key.as_str()) {
                        if p.len() == 1 {
                            terminal = true;
                        } else {
                            sub.push(&p[1..]);
                        }
                    }
                }
                if terminal {
                    out.insert(key.clone(), val.clone());
                } else if !sub.is_empty() {
                    out.insert(key.clone(), project_value(val, &sub));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Percent-encodes a rendered auth value as one URL path segment per RFC 3986
/// pchar rules, keeping ':' literal (Telegram tokens embed a colon, spec 4.3).
/// pchar = unreserved / sub-delims / ':' / '@'.
fn encode_path_segment(s: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    const PCHAR: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~')
        .remove(b'!')
        .remove(b'$')
        .remove(b'&')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')')
        .remove(b'*')
        .remove(b'+')
        .remove(b',')
        .remove(b';')
        .remove(b'=')
        .remove(b':')
        .remove(b'@');
    utf8_percent_encode(s, PCHAR).to_string()
}

/// Percent-encodes a whole string as a single URL component (same set the
/// renderer uses for substituted values). Applied to literal query keys and
/// auth param names so a stray reserved byte cannot restructure the URL.
fn encode_component(s: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    const SET: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    utf8_percent_encode(s, SET).to_string()
}

/// Encodes DECODED form pairs into the `application/x-www-form-urlencoded`
/// wire body, percent-encoding keys and values with the same component set
/// query rendering uses so encoding stays uniform (spec
/// 2026-07-22-official-connectors-wave-3, section 4.1).
fn encode_form_body(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Standard RFC 4648 base64 (with padding). Local to keep the engine lean: the
/// only use is HTTP Basic auth, and pulling a base64 crate for `user:pass`
/// encoding is not worth the dependency.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(n >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Appends the `ConnectorCall` event for a reached call. Best-effort: a log
/// write failure must not change the call's already-computed result, so the
/// error is dropped (the result was printed regardless).
fn append_event(req: &CallRequest, meta: &EventMeta) {
    if let Ok(mut log) = EventLog::open(req.run_dir) {
        let _ = log.append(EventPayload::ConnectorCall {
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
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- error_when reclassification (spec
    // 2026-07-22-official-connectors-wave-3, section 4.2) --

    fn rule(
        path: &str,
        equals: Value,
        message_path: Option<&str>,
    ) -> apb_core::connector::def::ErrorWhen {
        apb_core::connector::def::ErrorWhen {
            path: path.to_string(),
            equals,
            message_path: message_path.map(str::to_string),
        }
    }

    #[test]
    fn error_when_match_reclassifies_to_service_with_extracted_message() {
        let r = rule("ok", json!(false), Some("error"));
        let body = json!({ "ok": false, "error": "missing_scope" });
        let err = reclassify_error_when(&r, &body, 200).unwrap();
        assert_eq!(err.code, CallErrorCode::Service);
        assert_eq!(err.message, "missing_scope");
        // The error carries the real HTTP status of the reclassified response.
        assert_eq!(err.http_status, Some(200));
    }

    #[test]
    fn error_when_no_match_passes_through() {
        let r = rule("ok", json!(false), Some("error"));
        assert!(reclassify_error_when(&r, &json!({ "ok": true }), 200).is_none());
    }

    #[test]
    fn error_when_path_resolving_to_nothing_means_no_match() {
        let r = rule("ok", json!(false), Some("error"));
        assert!(reclassify_error_when(&r, &json!({ "status": "fine" }), 200).is_none());
        // A non-object midway through the chain resolves to nothing too.
        let nested = rule("a.b", json!(false), None);
        assert!(reclassify_error_when(&nested, &json!({ "a": 1 }), 200).is_none());
    }

    #[test]
    fn error_when_nested_paths_resolve_over_objects() {
        let r = rule("result.ok", json!(false), Some("result.detail.message"));
        let body = json!({ "result": { "ok": false, "detail": { "message": "bad" } } });
        let err = reclassify_error_when(&r, &body, 201).unwrap();
        assert_eq!(err.message, "bad");
        assert_eq!(err.http_status, Some(201));
    }

    #[test]
    fn error_when_fallback_message_when_message_path_absent_or_not_string() {
        // No message_path declared at all.
        let none = rule("ok", json!(false), None);
        let fallback = reclassify_error_when(&none, &json!({ "ok": false }), 200)
            .unwrap()
            .message;
        assert!(!fallback.is_empty());
        // message_path resolves to nothing.
        let r = rule("ok", json!(false), Some("error"));
        let missing = reclassify_error_when(&r, &json!({ "ok": false }), 200).unwrap();
        assert_eq!(missing.message, fallback);
        // message_path resolves to a non-string.
        let non_string =
            reclassify_error_when(&r, &json!({ "ok": false, "error": 42 }), 200).unwrap();
        assert_eq!(non_string.message, fallback);
    }

    #[test]
    fn error_when_equals_compares_by_json_equality() {
        let s = rule("status", json!("error"), None);
        assert!(reclassify_error_when(&s, &json!({ "status": "error" }), 200).is_some());
        assert!(reclassify_error_when(&s, &json!({ "status": "fine" }), 200).is_none());
        // A number does not equal its string form.
        let n = rule("code", json!(1), None);
        assert!(reclassify_error_when(&n, &json!({ "code": 1 }), 200).is_some());
        assert!(reclassify_error_when(&n, &json!({ "code": "1" }), 200).is_none());
    }

    #[test]
    fn render_http_builds_method_url_body_and_headers_offline() {
        use apb_core::connector::def::ConnectorDoc;
        let yaml = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: api_base\n    required: true\nfunctions:\n  - name: create\n    description: d\n    method: POST\n    url: \"{{account.api_base}}/items/{{args.id}}\"\n    body: \"{{args}}\"\n    headers: { X-Trace: \"{{args.id}}\" }\n    args_schema: { type: object }\n";
        let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
        let f = doc.function("create").unwrap();
        let mut account = BTreeMap::new();
        account.insert(
            "api_base".to_string(),
            "https://api.example.com".to_string(),
        );
        let args = json!({ "id": "4 2", "title": "Hi" });
        let secrets = BTreeMap::new();
        let r = render_http(f, &account, &args, &secrets).unwrap();
        assert_eq!(r.method, "POST");
        // args are percent-encoded into the URL path (space -> %20).
        assert_eq!(r.pre_auth_url, "https://api.example.com/items/4%202");
        assert_eq!(r.rendered_body, Some(json!({ "id": "4 2", "title": "Hi" })));
        // The function header renders, and the default User-Agent is applied.
        assert_eq!(r.headers.get("X-Trace").map(String::as_str), Some("4 2"));
        assert_eq!(
            r.headers.get("User-Agent").map(String::as_str),
            Some(USER_AGENT)
        );
    }

    #[test]
    fn render_http_lets_a_function_header_override_the_default_user_agent() {
        use apb_core::connector::def::ConnectorDoc;
        let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: g\n    description: d\n    method: GET\n    url: \"https://api.example.com/x\"\n    headers: { User-Agent: custom-ua }\n    args_schema: { type: object }\n";
        let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
        let f = doc.function("g").unwrap();
        let r = render_http(f, &BTreeMap::new(), &json!({}), &BTreeMap::new()).unwrap();
        assert_eq!(
            r.headers.get("User-Agent").map(String::as_str),
            Some("custom-ua")
        );
    }

    #[test]
    fn render_query_drops_absent_single_placeholder_pair() {
        use apb_core::connector::def::ConnectorDoc;
        let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list\n    description: d\n    method: GET\n    url: \"https://api.example.com/items\"\n    query: { offset: \"{{args.offset}}\", limit: \"{{args.limit}}\" }\n    args_schema: { type: object }\n";
        let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
        let f = doc.function("list").unwrap();
        let args = json!({ "limit": 50 });
        let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new()).unwrap();
        assert_eq!(r.pre_auth_url, "https://api.example.com/items?limit=50");
    }

    #[test]
    fn render_query_typed_scalars() {
        use apb_core::connector::def::ConnectorDoc;
        let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list\n    description: d\n    method: GET\n    url: \"https://api.example.com/items\"\n    query: { active: \"{{args.active}}\", limit: \"{{args.limit}}\" }\n    args_schema: { type: object }\n";
        let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
        let f = doc.function("list").unwrap();
        let args = json!({ "limit": 50, "active": true });
        let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new()).unwrap();
        assert_eq!(
            r.pre_auth_url,
            "https://api.example.com/items?active=true&limit=50"
        );
    }

    #[test]
    fn render_query_non_scalar_single_placeholder_is_config_error() {
        use apb_core::connector::def::ConnectorDoc;
        let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list\n    description: d\n    method: GET\n    url: \"https://api.example.com/items\"\n    query: { limit: \"{{args.limit}}\" }\n    args_schema: { type: object }\n";
        let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
        let f = doc.function("list").unwrap();
        let args = json!({ "limit": [1] });
        match render_http(f, &BTreeMap::new(), &args, &BTreeMap::new()) {
            Ok(_) => panic!("expected a config error for a non-scalar query arg"),
            Err(err) => assert_eq!(err.code, CallErrorCode::Config),
        }
    }

    #[test]
    fn render_query_null_arg_drops_pair() {
        use apb_core::connector::def::ConnectorDoc;
        let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list\n    description: d\n    method: GET\n    url: \"https://api.example.com/items\"\n    query: { offset: \"{{args.offset}}\", limit: \"{{args.limit}}\" }\n    args_schema: { type: object }\n";
        let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
        let f = doc.function("list").unwrap();
        // `offset` is explicitly null (not merely absent): it must drop the
        // pair exactly like an absent arg, not render a literal `null`.
        let args = json!({ "offset": null, "limit": 50 });
        let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new()).unwrap();
        assert_eq!(r.pre_auth_url, "https://api.example.com/items?limit=50");
    }

    // -- body_form rendering (spec 2026-07-22-official-connectors-wave-3, 4.1) --

    /// A POST function whose `body_form` mixes a literal, single placeholders,
    /// and a mixed-content template.
    fn form_function(body_form_yaml: &str) -> apb_core::connector::def::ConnectorDoc {
        use apb_core::connector::def::ConnectorDoc;
        let yaml = format!(
            "name: x\nversion: 0.1.0\nfunctions:\n  - name: send\n    description: d\n    method: POST\n    url: \"https://api.example.com/messages\"\n    body_form:\n{body_form_yaml}    args_schema: {{ type: object }}\n"
        );
        ConnectorDoc::from_yaml(&yaml, "x").unwrap()
    }

    #[test]
    fn render_http_without_body_form_renders_no_form() {
        use apb_core::connector::def::ConnectorDoc;
        let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: g\n    description: d\n    method: GET\n    url: \"https://api.example.com/x\"\n";
        let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
        let f = doc.function("g").unwrap();
        let r = render_http(f, &BTreeMap::new(), &json!({}), &BTreeMap::new()).unwrap();
        assert!(r.rendered_form.is_none());
    }

    #[test]
    fn render_form_typed_single_placeholders_render_canonical_strings() {
        let doc = form_function(
            "      active: \"{{args.active}}\"\n      limit: \"{{args.limit}}\"\n      name: \"{{args.name}}\"\n",
        );
        let f = doc.function("send").unwrap();
        let args = json!({ "active": true, "limit": 50, "name": "hi" });
        let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new()).unwrap();
        assert_eq!(
            r.rendered_form,
            Some(vec![
                ("active".to_string(), "true".to_string()),
                ("limit".to_string(), "50".to_string()),
                ("name".to_string(), "hi".to_string()),
            ])
        );
    }

    #[test]
    fn render_form_drops_absent_and_null_single_placeholder_pairs() {
        let doc = form_function(
            "      content: \"{{args.content}}\"\n      queue_id: \"{{args.queue_id}}\"\n      topic: \"{{args.topic}}\"\n",
        );
        let f = doc.function("send").unwrap();
        // `queue_id` is absent, `topic` is explicitly null: both pairs drop.
        let args = json!({ "content": "hello", "topic": null });
        let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new()).unwrap();
        assert_eq!(
            r.rendered_form,
            Some(vec![("content".to_string(), "hello".to_string())])
        );
    }

    #[test]
    fn render_form_non_scalar_single_placeholder_is_config_error() {
        let doc = form_function("      content: \"{{args.content}}\"\n");
        let f = doc.function("send").unwrap();
        let args = json!({ "content": [1, 2] });
        match render_http(f, &BTreeMap::new(), &args, &BTreeMap::new()) {
            Ok(_) => panic!("expected a config error for a non-scalar form arg"),
            Err(err) => assert_eq!(err.code, CallErrorCode::Config),
        }
    }

    #[test]
    fn render_form_mixed_template_absent_arg_is_config_error() {
        // Mixed content keeps string interpolation with absent-arg-is-error;
        // only an exact single placeholder gets the drop rule.
        let doc = form_function("      content: \"v={{args.gone}}\"\n");
        let f = doc.function("send").unwrap();
        match render_http(f, &BTreeMap::new(), &json!({}), &BTreeMap::new()) {
            Ok(_) => panic!("expected a config error for an absent arg in mixed content"),
            Err(err) => assert_eq!(err.code, CallErrorCode::Config),
        }
    }

    #[test]
    fn render_form_pairs_are_decoded_and_literals_kept() {
        // The rendered pairs stay DECODED (encoding happens at send time), and
        // a literal (placeholder-free) value renders verbatim.
        let doc = form_function("      content: \"{{args.content}}\"\n      type: stream\n");
        let f = doc.function("send").unwrap();
        let args = json!({ "content": "hello world & more" });
        let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new()).unwrap();
        assert_eq!(
            r.rendered_form,
            Some(vec![
                ("content".to_string(), "hello world & more".to_string()),
                ("type".to_string(), "stream".to_string()),
            ])
        );
    }

    #[test]
    fn encode_form_body_percent_encodes_keys_and_values() {
        let pairs = vec![
            ("a key".to_string(), "b&c=d".to_string()),
            ("content".to_string(), "hello world/100%".to_string()),
        ];
        assert_eq!(
            encode_form_body(&pairs),
            "a%20key=b%26c%3Dd&content=hello%20world%2F100%25"
        );
    }

    #[test]
    fn project_keeps_named_object_fields_and_nested_paths() {
        let body = json!({
            "number": 7, "title": "t", "extra": "drop",
            "user": { "login": "octo", "id": 1 }
        });
        let picked = project(
            &body,
            &["number".into(), "title".into(), "user.login".into()],
        );
        assert_eq!(
            picked,
            json!({ "number": 7, "title": "t", "user": { "login": "octo" } })
        );
    }

    #[test]
    fn project_maps_over_arrays_at_top_level_and_midway() {
        let body = json!([
            { "number": 1, "labels": [ { "name": "bug", "color": "red" }, { "name": "p1" } ], "x": 9 },
            { "number": 2, "labels": [] }
        ]);
        let picked = project(&body, &["number".into(), "labels.name".into()]);
        assert_eq!(
            picked,
            json!([
                { "number": 1, "labels": [ { "name": "bug" }, { "name": "p1" } ] },
                { "number": 2, "labels": [] }
            ])
        );
    }

    #[test]
    fn project_drops_missing_paths_silently() {
        let body = json!({ "a": 1 });
        let picked = project(&body, &["a".into(), "b.c".into()]);
        assert_eq!(picked, json!({ "a": 1 }));
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn validate_url_accepts_http_and_https() {
        assert!(validate_url("http://example.com/x").is_ok());
        assert!(validate_url("https://EXAMPLE.com/x?a=b").is_ok());
    }

    #[test]
    fn validate_url_rejects_non_http_scheme_and_userinfo() {
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("https://user:pass@example.com/x").is_err());
        assert!(validate_url("relative/path").is_err());
        // An `@` only in the path (after the authority) is allowed.
        assert!(validate_url("https://example.com/a@b").is_ok());
    }

    #[test]
    fn map_status_classifies_the_taxonomy() {
        assert!(map_status(200, json!({}), false, None).is_ok());
        assert_eq!(
            map_status(302, json!({}), false, None).unwrap_err().code,
            CallErrorCode::Service
        );
        assert_eq!(
            map_status(401, json!({}), false, None).unwrap_err().code,
            CallErrorCode::Auth
        );
        assert_eq!(
            map_status(404, json!({}), false, None).unwrap_err().code,
            CallErrorCode::NotFound
        );
        let rl = map_status(429, json!({}), false, Some(30)).unwrap_err();
        assert_eq!(rl.code, CallErrorCode::RateLimited);
        assert_eq!(rl.retry_after_sec, Some(30));
        assert_eq!(
            map_status(503, json!({}), false, None).unwrap_err().code,
            CallErrorCode::Service
        );
    }

    #[test]
    fn redact_replaces_secret_in_string_leaves() {
        let redactions = vec![("shh-1".to_string(), "MOCK_TOKEN".to_string())];
        let body = json!({ "echo": "prefix shh-1 suffix", "n": 3, "list": ["shh-1"] });
        let out = redact_value(body, &redactions);
        assert_eq!(
            out,
            json!({ "echo": "prefix [redacted:MOCK_TOKEN] suffix", "n": 3, "list": ["[redacted:MOCK_TOKEN]"] })
        );
    }
}
