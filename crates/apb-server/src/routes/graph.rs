//! The playbook page's graph layers: the editor's draft validation (reads
//! only) and a run laid over a version (`overlay`).

use crate::state::*;

use apb_core::registry::{Registry, RegistryError};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub(crate) struct DraftQuery {
    workspace: Option<String>,
    /// The playbook and version the draft edits, for the reference checks
    /// against that version's directory.
    id: Option<String>,
    version: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DraftBody {
    yaml: String,
}

/// What `POST /api/validate` answers.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct DraftValidation {
    /// The YAML does not parse as a playbook: the parser's message, and no
    /// issues (the editor keeps the last valid ones).
    pub parse_error: Option<String>,
    pub issues: Vec<apb_core::validate::IssueView>,
}

/// `POST /api/validate?workspace=&id=&version=` with `{ yaml }`:
/// the checks the playbook detail runs, over a draft. Writes nothing.
pub(crate) async fn post_validate_draft(
    State(state): State<AppState>,
    Query(q): Query<DraftQuery>,
    Json(body): Json<DraftBody>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let playbook = match apb_core::schema::Playbook::from_yaml(&body.yaml) {
        Ok(p) => p,
        Err(e) => {
            return Json(DraftValidation {
                parse_error: Some(e.to_string()),
                issues: Vec::new(),
            })
            .into_response();
        }
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let id =
        q.id.filter(|i| is_safe_id(i))
            .unwrap_or_else(|| playbook.id.clone());
    let version_dir = match q.version.filter(|v| is_safe_id(v)) {
        Some(v) if is_safe_id(&id) => reg.version_dir(&id, &v),
        // No version to read references from: an empty, never created
        // directory under the run root reports them as missing warnings.
        _ => root.join(".apb").join("draft-validation-none"),
    };
    let issues = super::playbooks::validation_issues(&reg, &root, &version_dir, &playbook);
    Json(DraftValidation {
        parse_error: None,
        issues,
    })
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct OverlayQuery {
    workspace: Option<String>,
    /// The shown version; the current one when absent.
    version: Option<String>,
    run: Option<String>,
    before: Option<u128>,
}

/// `GET /api/playbooks/{id}/overlay?workspace=&version=&run=&before=`: a
/// page of the playbook's runs with their alignment to the shown version,
/// the default (newest exact) run, and the chosen run's graph with each
/// segment's mapping. The graph goes through the chosen run's redactor.
pub(crate) async fn get_overlay(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<OverlayQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id)
        || q.version.as_deref().is_some_and(|v| !is_safe_id(v))
        || q.run.as_deref().is_some_and(|r| !is_safe_id(r))
    {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let loaded = match reg.load(&id, q.version.as_deref()) {
        Ok(l) => l,
        Err(RegistryError::NotFound(what)) => return (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let overlay = apb_engine::run_outcome::overlay::overlay(
        &apb_engine::run_outcome::overlay::OverlayRequest {
            root: &root,
            playbook: &id,
            version: &loaded.version,
            yaml: &loaded.yaml,
            run: q.run.as_deref(),
            before: q.before,
        },
    );
    let graph = overlay
        .graph
        .as_ref()
        .zip(overlay.chosen.as_ref())
        .map(|(g, run)| {
            let redactor = apb_engine::run_redact::RunRedactor::for_run(
                &root,
                &root.join(".apb").join("runs").join(run),
            );
            g.present(&redactor)
        });
    match serde_json::to_value(&overlay) {
        Ok(mut v) => {
            v["graph"] = graph.unwrap_or(serde_json::Value::Null);
            Json(v).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
