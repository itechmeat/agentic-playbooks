//! Remembered run input on the HTTP API: `GET|PUT|DELETE
//! /api/playbooks/{id}/memory?workspace&scope&version`. The values a start
//! that passes none uses, per project; `scope` names the definition
//! (`project`, the default, or `global`), and the memory always lives in
//! the project. The same tool layer as MCP `playbook_memory`
//! (`apb_mcp::tools::memory`), so both surfaces store and refuse alike:
//! secret params and credential-shaped values are never stored.

use std::collections::BTreeMap;

use apb_core::run_memory::RememberInput;
use apb_mcp::tools::ToolError;
use apb_mcp::tools::memory::{MemoryAction, MemoryTarget, parse_what, playbook_memory};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::*;

#[derive(Deserialize)]
pub(crate) struct MemoryQuery {
    #[serde(default)]
    workspace: Option<String>,
    /// `project` (default) or `global`.
    #[serde(default)]
    scope: Option<String>,
    /// The version whose params a get checks the memory against (the
    /// current one when absent). Refused on put and delete: a save is
    /// always checked against the current version.
    #[serde(default)]
    version: Option<String>,
    /// delete: `all` (default), `params` or `instruction`.
    #[serde(default)]
    what: Option<String>,
}

/// PUT body: what to remember. Absent fields keep what is stored; given
/// params are merged in; an empty instruction clears it on purpose.
#[derive(Deserialize)]
pub(crate) struct MemoryBody {
    #[serde(default)]
    params: Option<BTreeMap<String, String>>,
    #[serde(default)]
    instruction: Option<String>,
}

async fn respond(id: String, q: MemoryQuery, state: &AppState, action: MemoryAction) -> Response {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    if q.version.is_some() && !matches!(action, MemoryAction::Get) {
        return (StatusCode::BAD_REQUEST, "version is for get only").into_response();
    }
    let root = match resolve_root(state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let scope = q.scope.unwrap_or_else(|| "project".to_string());
    if apb_core::scope::Scope::parse(&scope).is_none() {
        return (StatusCode::BAD_REQUEST, "scope must be project or global").into_response();
    }
    // The registry, the definition and the memory store are files (and a
    // write takes the store's lock): off the async runtime.
    let done = tokio::task::spawn_blocking(move || {
        let target = MemoryTarget {
            id: &id,
            scope: &scope,
            version: q.version.as_deref(),
        };
        playbook_memory(&root, &target, action)
    })
    .await;
    match done {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(ToolError::NotFound(what))) => (StatusCode::NOT_FOUND, what).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET: the memory checked against the playbook's params now (`Recall`
/// plus `id`, `scope` and `version`).
pub(crate) async fn get_memory_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<MemoryQuery>,
) -> Response {
    respond(id, q, &state, MemoryAction::Get).await
}

/// PUT: remembers the body (`Remembered`: `stored` and `refused` with a
/// reason each, plus `id` and `scope`). A refused value is answered, never
/// stored; the call itself succeeds.
pub(crate) async fn put_memory_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<MemoryQuery>,
    Json(body): Json<MemoryBody>,
) -> Response {
    let input = RememberInput {
        params: body.params,
        instruction: body.instruction,
    };
    respond(id, q, &state, MemoryAction::Set(input)).await
}

/// DELETE: forgets `what` (all, params or instruction).
pub(crate) async fn delete_memory_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<MemoryQuery>,
) -> Response {
    let Some(what) = parse_what(q.what.as_deref()) else {
        return (
            StatusCode::BAD_REQUEST,
            "what must be all, params or instruction",
        )
            .into_response();
    };
    respond(id, q, &state, MemoryAction::Clear(what)).await
}
