//! "Notify me" on the HTTP API: `GET /api/notify` (the machine's notify
//! target, so the Run dialog can offer the checkbox), `POST
//! /api/runs/{id}/notify` (a person's change on a live run) and the status
//! of a notify refusal. The argument and the refusals are MCP's
//! (`apb_mcp::tools::notify`), so both surfaces answer alike: 400
//! `invalid_notify`, 409 `notify_unavailable` and `run_ended`.

use apb_core::notify::{NotifyRequest, NotifyTarget, Transition};
use apb_mcp::tools::notify::{NotifyArg, NotifyError};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use crate::state::*;

/// `GET /api/notify`: where this machine sends notifications. The target
/// names a connector account and a recipient; it holds no secret, and the
/// account's own configuration is never part of it.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct MachineNotify {
    /// The machine's target; absent when none is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub target: Option<NotifyTarget>,
    /// The transitions a run notifies about by default.
    pub on: Vec<Transition>,
    /// Whether a run of this workspace notifies without being asked (the
    /// machine default, unless the project turned it off).
    pub default_on: bool,
    /// Why the machine or the project notify settings do not read.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub problem: Option<String>,
}

impl MachineNotify {
    fn read(root: Option<&std::path::Path>) -> Self {
        let machine = match apb_core::config::GlobalConfig::load() {
            Ok(cfg) => cfg.notify,
            Err(problem) => {
                return MachineNotify {
                    target: None,
                    on: Vec::new(),
                    default_on: false,
                    problem: Some(problem),
                };
            }
        };
        let project = match root.map(apb_core::config::project_notify).transpose() {
            Ok(p) => p.unwrap_or_default(),
            Err(problem) => {
                return MachineNotify {
                    target: machine.target,
                    on: Transition::normalized(&machine.on),
                    default_on: false,
                    problem: Some(problem),
                };
            }
        };
        let default_on = apb_core::notify::effective(&machine, &project, None).is_some();
        MachineNotify {
            target: machine.target,
            on: Transition::normalized(&machine.on),
            default_on,
            problem: None,
        }
    }
}

/// The HTTP answer of a notify refusal.
pub(crate) fn refusal(e: NotifyError) -> Response {
    let status = match e {
        NotifyError::Invalid(_) => StatusCode::BAD_REQUEST,
        NotifyError::Unavailable(_) | NotifyError::Ended(_) => StatusCode::CONFLICT,
        NotifyError::NotFound(_) => StatusCode::NOT_FOUND,
        NotifyError::Failed(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(e.body())).into_response()
}

/// GET /api/notify[?workspace=<id>]: the machine's target and whether runs
/// of the workspace notify by default.
pub(crate) async fn get_notify_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    // A workspace asked for must resolve; without one, the pinned project
    // (if the server has one) decides the default.
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => Some(r),
        Err(e) if q.workspace.is_some() => return e,
        Err(_) => None,
    };
    match tokio::task::spawn_blocking(move || MachineNotify::read(root.as_deref())).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Body of `POST /api/runs/{id}/notify`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotifyBody {
    notify: serde_json::Value,
}

/// The engine request of a `notify` value as the client sent it; a value
/// that does not parse is the same `invalid_notify` MCP answers.
pub(crate) fn parse_request(value: &serde_json::Value) -> Result<NotifyRequest, NotifyError> {
    use serde::Deserialize as _;
    NotifyArg::deserialize(value)
        .map_err(|e| {
            NotifyError::Invalid(format!(
                "notify must be true, false or {{ on, target }}: {e}"
            ))
        })
        .and_then(|arg| arg.request())
}

/// POST /api/runs/{id}/notify: a person turns a live run's notifications
/// on or off, or changes what they are about. The target is checked now and
/// its digests pinned (409 `notify_unavailable` when it cannot be used);
/// a run that has ended answers 409 `run_ended`. Answers with the run's
/// `notify` block.
pub(crate) async fn post_run_notify_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    body: Result<Json<NotifyBody>, JsonRejection>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    // A body of the wrong shape is a notify refusal; a transport problem
    // (no JSON content type, a body too large) keeps its own answer.
    let request = match body {
        Ok(Json(b)) => parse_request(&b.notify),
        Err(e @ (JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_))) => {
            Err(NotifyError::Invalid(format!(
                "the body must be {{ \"notify\": true | false | {{ on, target }} }}: {}",
                e.body_text()
            )))
        }
        Err(other) => return other.into_response(),
    };
    let request = match request {
        Ok(r) => r,
        Err(e) => return refusal(e),
    };
    let res = tokio::task::spawn_blocking(move || {
        apb_mcp::tools::notify::set(&root, &id, &request, apb_engine::notify::ChangedBy::Person)
    })
    .await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => refusal(e),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
