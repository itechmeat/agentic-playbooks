//! The playbook trash: deleted playbooks per project and their restore. Thin
//! over `apb_core::versioning::{list_trash, restore_from_trash}`, the one
//! path `apb trash` and the MCP trash tools use too.

use crate::state::*;

use apb_core::versioning::{TrashEntry, list_trash, restore_from_trash};
use axum::extract::{Path as AxPath, Query, State};
use axum::response::{IntoResponse, Json};
use serde::Serialize;

/// One row of `GET /api/trash`: a trash entry stamped with its project.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct TrashListEntry {
    #[serde(flatten)]
    pub entry: TrashEntry,
    /// Owning project (global dashboard). Empty on the pinned-root test server.
    pub workspace_id: String,
    pub project: String,
}

/// GET /api/trash: the deleted playbooks of every reachable project, or of
/// the one `?workspace=<id>` names, newest deletion first within a project.
pub(crate) async fn list_trash_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    let workspaces = match selected_workspaces(&state, q.workspace.as_deref()) {
        Ok(w) => w,
        Err(e) => return e,
    };
    let mut out: Vec<TrashListEntry> = Vec::new();
    for (workspace_id, project, root) in workspaces {
        let Ok(entries) = list_trash(&root) else {
            continue;
        };
        out.extend(entries.into_iter().map(|entry| TrashListEntry {
            entry,
            workspace_id: workspace_id.clone(),
            project: project.clone(),
        }));
    }
    Json(out).into_response()
}

/// POST /api/trash/{name}/restore?workspace=<id>: restores a trash entry (or
/// a playbook id's latest deletion). 409 with a readable message when a
/// playbook with that id exists again, 404 when nothing matches.
pub(crate) async fn restore_trash_handler(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match restore_from_trash(&root, &name) {
        Ok(restored) => Json(restored).into_response(),
        Err(e) => versioning_error(e),
    }
}
