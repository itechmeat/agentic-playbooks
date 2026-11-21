//! The start rules on the HTTP API (design D1 "HTTP"), over the tool layer
//! (`apb_mcp::tools::trigger`) and the core store (`apb_core::trigger`):
//!
//! - `GET /api/triggers`: every rule as the list shows it;
//! - `GET /api/triggers/{name}`: one rule's view, its document and its
//!   journal tail;
//! - `GET /api/triggers/{name}/approval`: the `ApprovalNeed` a person
//!   approves;
//! - `PUT /api/triggers/{name}` with `{ "rule": <Rule> }`,
//!   `POST /api/triggers/{name}/enable`, `POST /api/triggers/{name}/disable`,
//!   `POST /api/triggers/{name}/approve` with `{ "shown": <ApprovalNeed> }`
//!   and `DELETE /api/triggers/{name}` with `{}`: a person's writes, only a
//!   signed-in session (403 `trigger_requires_session` otherwise), refused
//!   409 from a run step, each holding the rule's change and sweep locks
//!   across its write inside `spawn_blocking`;
//! - `POST /api/triggers/{name}/test` with `{ "event_seq"? }`: the dry
//!   report without values, on every channel, refused from a run step.
//!
//! No route takes a passkey token: a rule's approval is a person's command
//! under main's rule (design D0 "A person's command", D1 "Approval" item 3).
//! Every body is a type of its own with `#[serde(deny_unknown_fields)]`, so
//! a body naming a field it does not carry is 422.

use apb_core::scope::Scope;
use apb_core::trigger::{Rule, RuleEntry, RulesFile, TriggerError};
use apb_core::validate::IssueView;
use apb_mcp::tools::trigger::{self, ApprovalNeed};
use axum::Router;
use axum::extract::{Path as AxPath, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post, put};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::RequestChannel;
use crate::state::{AppState, WorkspaceQuery, resolve_root, tool_refusal};

/// Body of `PUT /api/triggers/{name}`: the rule document as the dashboard
/// edits it (JSON on the wire, the map form the store serves).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetBody {
    rule: Value,
}

/// Body of `POST /api/triggers/{name}/approve`: the need as it was shown.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApproveBody {
    shown: ApprovalNeed,
}

/// Body of `POST /api/triggers/{name}/test`: the event to test, newest when
/// absent. `{}` tests the newest stored event.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct TestBody {
    #[serde(default)]
    event_seq: Option<u64>,
}

/// Body of `DELETE /api/triggers/{name}` and of `enable` and `disable`:
/// empty by type, so an unknown field is 422.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyBody {}

/// One rule's detail: the list view with the rule's document and its
/// journal tail (the newest twenty lines, newest last).
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(rename = "TriggerDetail"))]
pub(crate) struct RuleDetail {
    #[serde(flatten)]
    view: apb_engine::trigger::view::RuleView,
    rule: Rule,
    journal: Vec<apb_core::trigger::LogEntry>,
}

/// The trigger routes: the writes behind [`session_gate`], the reads and
/// the dry test on every channel.
pub(crate) fn router() -> Router<AppState> {
    let writes = Router::new()
        .route(
            "/api/triggers/{name}",
            put(set_handler).delete(remove_handler),
        )
        .route("/api/triggers/{name}/enable", post(enable_handler))
        .route("/api/triggers/{name}/disable", post(disable_handler))
        .route("/api/triggers/{name}/approve", post(approve_handler))
        .route_layer(axum::middleware::from_fn(session_gate));
    let reads = Router::new()
        .route("/api/triggers", get(list_handler))
        .route("/api/triggers/{name}", get(detail_handler))
        .route("/api/triggers/{name}/approval", get(approval_handler))
        .route("/api/triggers/{name}/test", post(test_handler));
    writes.merge(reads)
}

/// The write routes' gate: a signed-in session only (a keyless dashboard or
/// a bearer key is 403 `trigger_requires_session`), then a run step's 409
/// (as `routes/discover.rs`'s gate does).
async fn session_gate(req: Request, next: Next) -> Response {
    let session = req
        .extensions()
        .get::<RequestChannel>()
        .is_some_and(|c| matches!(c, RequestChannel::Session));
    if !session {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "trigger_requires_session",
                "detail": "changing a start rule takes a signed-in dashboard session (apb trigger at a terminal is the other way)",
            })),
        )
            .into_response();
    }
    if let Some(why) = apb_core::step_env::StepCaller::from_env().refusal("changing a trigger rule")
    {
        return (StatusCode::CONFLICT, why).into_response();
    }
    next.run(req).await
}

/// Runs `work` off the runtime; a panic in it answers 500.
async fn off_runtime<F>(work: F) -> Response
where
    F: FnOnce() -> Response + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
}

/// A `TriggerError` as a response: 404 for what is not there, 409 for a
/// rule whose locks stayed held, 422 for anything the store or the rule
/// refused. A rule with issues carries every issue (errors and warnings
/// alike, as the CLI prints them).
#[allow(clippy::result_large_err)]
fn rule_refusal(e: TriggerError) -> Response {
    let code = match &e {
        TriggerError::NotFound(_) => "trigger_rule_not_found",
        TriggerError::Busy(_) => "trigger_rule_busy",
        TriggerError::EnabledNotSettable => "trigger_enabled_not_settable",
        TriggerError::Unregistered => "trigger_project_unregistered",
        TriggerError::StoreFull(_) => "rules_store_full",
        TriggerError::Invalid(_) => "trigger_rule_invalid",
        TriggerError::Store(_) | TriggerError::Inbox(_) => "trigger_rules_unreadable",
    };
    let status = match &e {
        TriggerError::NotFound(_) => StatusCode::NOT_FOUND,
        TriggerError::Busy(_) => StatusCode::CONFLICT,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    let mut body = json!({ "error": code, "detail": e.to_string() });
    if let TriggerError::Invalid(issues) = &e {
        body["issues"] = json!(issues.iter().map(IssueView::from).collect::<Vec<_>>());
    }
    (status, Json(body)).into_response()
}

/// A refusal value of the tools layer (`approval_need`, `approve`) as a
/// response, its `policy` as the code.
fn value_refusal(v: &Value) -> Response {
    let code = v["policy"].as_str().unwrap_or("trigger_refused");
    let detail = v["detail"].as_str().unwrap_or_default();
    let status = match code {
        "trigger_rule_not_found" => StatusCode::NOT_FOUND,
        "trigger_rule_busy" | "trigger_approval_changed" => StatusCode::CONFLICT,
        "trigger_launch_denied" => StatusCode::FORBIDDEN,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

/// A `ToolError` of the tools layer as a response (the shared mapping).
fn tool_refused(e: apb_mcp::tools::ToolError) -> Response {
    tool_refusal(e)
}

/// The stored entry of `name`, only as this installation signed it for this
/// project; the refusal a route answers when the file does not verify or
/// the rule is not there.
#[allow(clippy::result_large_err)]
fn stored_entry(root: &std::path::Path, name: &str) -> Result<RuleEntry, Response> {
    let loaded =
        apb_core::signed_store::read::<RulesFile>(root, Scope::Project, "rules").map_err(|e| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": "trigger_rules_unreadable",
                    "detail": e.to_string(),
                })),
            )
                .into_response()
        })?;
    if !loaded.signed {
        let path = apb_core::signed_store::file::<RulesFile>(root, Scope::Project, "rules");
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": apb_core::trigger::RULES_UNVERIFIED,
                "detail": apb_core::trigger::rules_unverified_detail(&path),
            })),
        )
            .into_response());
    }
    loaded
        .file
        .rules
        .into_iter()
        .find(|e| e.rule.name == name)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no rule `{name}`")).into_response())
}

/// The list view of one rule, after a write (the writes answer the view the
/// list shows, approved and needs_approval filled).
#[allow(clippy::result_large_err)]
fn view_of(
    root: &std::path::Path,
    name: &str,
) -> Result<apb_engine::trigger::view::RuleView, Response> {
    let views = trigger::list(root).map_err(tool_refused)?;
    views
        .into_iter()
        .find(|v| v.name == name)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no rule `{name}`")).into_response())
}

pub(crate) async fn list_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    off_runtime(move || match trigger::list(&root) {
        Ok(rules) => Json(json!({ "rules": rules })).into_response(),
        Err(e) => tool_refused(e),
    })
    .await
}

pub(crate) async fn detail_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    AxPath(name): AxPath<String>,
) -> Response {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    off_runtime(move || {
        if !crate::state::is_safe_id(&name) {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        let entry = match stored_entry(&root, &name) {
            Ok(e) => e,
            Err(refused) => return refused,
        };
        let view = match view_of(&root, &name) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        let journal = apb_core::trigger::read(&root, &entry.incarnation, 20);
        Json(RuleDetail {
            view,
            rule: entry.rule,
            journal,
        })
        .into_response()
    })
    .await
}

pub(crate) async fn approval_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    AxPath(name): AxPath<String>,
) -> Response {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    off_runtime(move || {
        if !crate::state::is_safe_id(&name) {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        let entry = match stored_entry(&root, &name) {
            Ok(e) => e,
            Err(refused) => return refused,
        };
        match trigger::approval_need(&root, &entry) {
            Ok(need) => Json(need).into_response(),
            Err(v) => value_refusal(&v),
        }
    })
    .await
}

/// Creates or replaces the rule from the body's document. The document's
/// `name` must equal the name in the path (as `apb trigger set` holds the
/// file's name to the argument); the document naming `enabled` is refused
/// by the store (`EnabledNotSettable`), never by this route.
pub(crate) async fn set_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    AxPath(name): AxPath<String>,
    Json(body): Json<SetBody>,
) -> Response {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    off_runtime(move || {
        if !crate::state::is_safe_id(&name) {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        if body.rule.get("name").and_then(|n| n.as_str()) != Some(name.as_str()) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "the rule document's `name` must equal the name in the path",
            )
                .into_response();
        }
        let doc = match serde_json::to_string(&body.rule) {
            Ok(d) => d,
            Err(e) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("the rule document does not parse: {e}"),
                )
                    .into_response();
            }
        };
        match apb_core::trigger::set_rule(
            &root,
            &doc,
            "dashboard_session",
            apb_core::clock::Millis::now(),
        ) {
            Ok(_) => match view_of(&root, &name) {
                Ok(v) => Json(v).into_response(),
                Err(refused) => refused,
            },
            Err(e) => rule_refusal(e),
        }
    })
    .await
}

pub(crate) async fn enable_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    AxPath(name): AxPath<String>,
    Json(body): Json<EmptyBody>,
) -> Response {
    enabled_handler(state, q, name, body, true).await
}

pub(crate) async fn disable_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    AxPath(name): AxPath<String>,
    Json(body): Json<EmptyBody>,
) -> Response {
    enabled_handler(state, q, name, body, false).await
}

/// `enable` and `disable`: the only way `enabled` changes; enabling deletes
/// the rule's state, so the next pass positions it at the head it reads
/// (the store does that; the route never reads the inbox).
async fn enabled_handler(
    state: AppState,
    q: WorkspaceQuery,
    name: String,
    body: EmptyBody,
    enabled: bool,
) -> Response {
    let _ = body;
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    off_runtime(move || {
        if !crate::state::is_safe_id(&name) {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        match apb_core::trigger::set_enabled(
            &root,
            &name,
            enabled,
            "dashboard_session",
            apb_core::clock::Millis::now(),
        ) {
            Ok(()) => match view_of(&root, &name) {
                Ok(v) => Json(v).into_response(),
                Err(refused) => refused,
            },
            Err(e) => rule_refusal(e),
        }
    })
    .await
}

/// Stores the person's approval of the need exactly as shown: the tool
/// layer compares a fresh need of the stored entry with `shown` field by
/// field under the rule's locks; any difference is 409
/// `trigger_approval_changed` and stores nothing.
pub(crate) async fn approve_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    AxPath(name): AxPath<String>,
    Json(body): Json<ApproveBody>,
) -> Response {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    off_runtime(move || {
        if !crate::state::is_safe_id(&name) {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        match trigger::approve(&root, &name, &body.shown, "dashboard_session") {
            Ok(_approval) => match view_of(&root, &name) {
                Ok(v) => Json(v).into_response(),
                Err(refused) => refused,
            },
            Err(v) => value_refusal(&v),
        }
    })
    .await
}

/// Removes the entry, its state and its journal file (the store does, under
/// the rule's locks).
pub(crate) async fn remove_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    AxPath(name): AxPath<String>,
    Json(body): Json<EmptyBody>,
) -> Response {
    let _ = body;
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    off_runtime(move || {
        if !crate::state::is_safe_id(&name) {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        match apb_core::trigger::remove_rule(&root, &name) {
            Ok(outcome) => Json(json!({
                "removed": outcome.removed,
                // A state entry or a journal the removal could not delete
                // beneath the anchor: the rule is gone either way, and
                // the answer names what stays behind (W5).
                "state_left": outcome.state_left,
                "journal_left": outcome.journal_left,
            }))
            .into_response(),
            Err(e) => rule_refusal(e),
        }
    })
    .await
}

/// The dry test of one stored event, the report without values (the CLI at
/// a terminal is where values show). Allowed on every channel; the tool
/// layer refuses a run step (`trigger_test_refused_in_run_step`), which is
/// 409 through the shared mapping.
pub(crate) async fn test_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    AxPath(name): AxPath<String>,
    Json(body): Json<TestBody>,
) -> Response {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let seq = body.event_seq;
    off_runtime(move || {
        if !crate::state::is_safe_id(&name) {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        match trigger::test_rule(&root, &name, seq, false) {
            Ok(report) => Json(report).into_response(),
            Err(e) => tool_refused(e),
        }
    })
    .await
}
