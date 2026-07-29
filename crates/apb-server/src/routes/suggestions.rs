//! The suggestion-decision surface of the dashboard (spec
//! 2026-07-29-suggestion-decisions-design section "Dashboard"): read the
//! records that currently silence an offer in a workspace, and remove one.
//! Both handlers are thin wrappers over `apb_core::dismiss`, the same
//! functions `apb suggestions` uses.

use apb_core::dismiss::{self, DecisionScope};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::{AppState, WorkspaceQuery, is_safe_id, resolve_root, resolve_root_for_scope};

/// `GET /api/suggestions?workspace=<id>`: the merged active records for the
/// workspace, project and global scope, each labelled with its scope.
///
/// Without `workspace` on the global server this is the GLOBAL-ONLY view
/// rather than a 400: on a machine with zero registered projects there is no
/// workspace id to send, and the machine-wide records still have to be
/// listable (and removable) from the dashboard. A pinned root (the test
/// harness) keeps answering for that root, as before.
pub(crate) async fn list_suggestions_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let view = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(root) => dismiss::active(&root),
        // `resolve_root` fails with an absent workspace only when there is no
        // pinned root, which is exactly the global-dashboard case.
        Err(_) if q.workspace.is_none() => dismiss::active_global(),
        Err(res) => return res,
    };
    let rows: Vec<serde_json::Value> = view
        .records
        .iter()
        .map(|s| {
            serde_json::json!({
                "pattern": s.record.pattern,
                "synopsis": s.record.synopsis,
                "kind": s.record.kind.as_str(),
                "scope": s.scope.as_str(),
                "declines": s.record.declines,
                "snoozed_until": dismiss::iso_utc(s.record.snoozed_until_ms),
            })
        })
        .collect();
    Json(serde_json::json!({
        "suggestions": rows,
        "diagnostics": view.diagnostics,
    }))
    .into_response()
}

/// Query for the delete route: the target workspace plus which store the
/// record lives in. An absent `scope` means project, matching the tool default.
#[derive(Deserialize, Default)]
pub(crate) struct SuggestionScopeQuery {
    pub(crate) workspace: Option<String>,
    pub(crate) scope: Option<String>,
}

/// `DELETE /api/suggestions/{pattern}?workspace=<id>&scope=project|global`:
/// same effect as `apb suggestions allow`.
///
/// The response carries `still_suppressed` (and the scope that does it): the
/// same pattern can live in both stores, and removing one of them does not
/// necessarily bring the offer back. A global delete resolves its root through
/// `resolve_root_for_scope`, like every other global-scope operation, so it
/// also works on a machine with zero registered projects.
pub(crate) async fn delete_suggestion_handler(
    State(state): State<AppState>,
    Path(pattern): Path<String>,
    Query(q): Query<SuggestionScopeQuery>,
) -> Response {
    if !is_safe_id(&pattern) {
        return (StatusCode::BAD_REQUEST, "invalid pattern").into_response();
    }
    let scope = match q.scope.as_deref() {
        None | Some("") => DecisionScope::Project,
        Some(raw) => match DecisionScope::parse(raw) {
            Some(s) => s,
            None => return (StatusCode::BAD_REQUEST, "invalid scope").into_response(),
        },
    };
    let root = match resolve_root_for_scope(&state, q.workspace.as_deref(), scope.as_str()) {
        Ok(r) => r,
        Err(res) => return res,
    };
    match dismiss::remove_record(&root, &pattern, scope) {
        Ok(outcome) if outcome.removed => Json(serde_json::json!({
            "removed": true,
            "pattern": pattern,
            "still_suppressed": outcome.still_suppressed_by.is_some(),
            "still_suppressed_by": outcome.still_suppressed_by.map(|s| s.as_str()),
        }))
        .into_response(),
        Ok(_) => (StatusCode::NOT_FOUND, "no such suggestion record").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
