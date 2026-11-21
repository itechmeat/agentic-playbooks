//! The template routes: the gallery listing, one template, and the install
//! into either scope through the tool layer's one entry the CLI uses. The
//! routes sit behind the same auth layer as every other route of their
//! method; the dashboard's own server is never a run step, but an install
//! through one is refused like over the CLI.

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::*;

/// `GET /api/templates`: every template's summary.
pub(crate) async fn get_templates() -> Response {
    Json(apb_mcp::tools::templates::list()).into_response()
}

/// `GET /api/templates/{id}`: one template with its yaml; 404 for an
/// unknown or unsafe id.
pub(crate) async fn get_template(AxPath(id): AxPath<String>) -> Response {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match apb_mcp::tools::templates::show(&id) {
        Ok(v) => Json(v).into_response(),
        Err(e) => tool_refusal(e),
    }
}

#[derive(Deserialize)]
pub(crate) struct InstallBody {
    /// `project` (the default) or `global`.
    #[serde(default)]
    scope: Option<String>,
    /// The playbook id to install as.
    #[serde(default)]
    r#as: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
}

/// `POST /api/templates/{id}/install`: the draft in the project or the
/// global store, with the fit report of the new draft when a project was
/// named. Runs off the runtime, like every install.
pub(crate) async fn post_install(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<InstallBody>,
) -> Response {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let scope = body.scope.as_deref().unwrap_or("project").to_string();
    // The root and with_fit: a project named (or pinned) gives both; a
    // global install needs no project, and then no fit report is made.
    let root = resolve_root(&state, body.workspace.as_deref());
    let (root, with_fit) = match root {
        Ok(r) => (r, true),
        Err(e) if scope == "global" && body.workspace.is_none() => {
            match resolve_root_for_scope(&state, None, "global") {
                Ok(r) => (r, false),
                Err(_) => return e,
            }
        }
        Err(e) => return e,
    };
    let answer = tokio::task::spawn_blocking(move || {
        apb_mcp::tools::templates::install(
            &root,
            apb_mcp::tools::templates::InstallRequest {
                template: &id,
                scope: &scope,
                as_id: body.r#as.as_deref(),
                with_fit,
            },
        )
    })
    .await;
    match answer {
        Ok(Ok(v)) => {
            // A rejected install (a duplicate id) is 409 with the body.
            if v.get("rejected").is_some() {
                (StatusCode::CONFLICT, Json(v)).into_response()
            } else {
                Json(v).into_response()
            }
        }
        Ok(Err(e)) => tool_refusal(e),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
