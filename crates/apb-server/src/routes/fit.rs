//! "Will it fit here?" on the HTTP API: `GET
//! /api/playbooks/{id}/fit?workspace&scope`, the four-section report of
//! [`apb_engine::fit`] for a run of the playbook (definition `scope`:
//! `project`, the default, or `global`) in the workspace's project. Read
//! only; the report reads the playbook tree and the machine, off the async
//! runtime.

use apb_core::registry::RegistryError;
use apb_core::scope::{Origin, PlaybookRef, Scope};
use apb_engine::fit::{FitError, assess};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::*;

#[derive(Deserialize)]
pub(crate) struct FitQuery {
    #[serde(default)]
    workspace: Option<String>,
    /// `project` (default) or `global`.
    #[serde(default)]
    scope: Option<String>,
}

/// GET: the `FitReport` of the playbook's current version.
pub(crate) async fn get_fit_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<FitQuery>,
) -> Response {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let origin = match Scope::parse(q.scope.as_deref().unwrap_or("project")) {
        Some(Scope::Project) => Origin::Project { workspace_id: None },
        Some(Scope::Global) => Origin::Global,
        None => {
            return (StatusCode::BAD_REQUEST, "scope must be project or global").into_response();
        }
    };
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let wref = PlaybookRef {
        origin,
        id,
        version: None,
    };
    match tokio::task::spawn_blocking(move || assess(&root, &wref)).await {
        Ok(Ok(report)) => Json(report).into_response(),
        Ok(Err(FitError::Registry(RegistryError::NotFound(what)))) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
