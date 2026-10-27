//! Run quality on the HTTP API, over the tool layer MCP and the CLI use
//! (`apb_mcp::tools::grade`, `apb_mcp::tools::candidate`), so a grade
//! stored here re-decides the candidate exactly as elsewhere:
//!
//! - `GET /api/runs/{id}/grades`: the run's stored grades, oldest first;
//! - `POST /api/runs/{id}/grades`: grade the ended run by rules (409 while
//!   it is live);
//! - `POST /api/runs/{id}/grades/reopen`: a person lets a host grade the
//!   run once more (409 without a host grade);
//! - `GET /api/playbooks/{id}/candidate`: the candidate and how it compares
//!   with `current` (`candidate: null` without one);
//! - `POST /api/playbooks/{id}/candidate/promote` and
//!   `POST /api/playbooks/{id}/candidate/keep-current` (`{ reason? }`): a
//!   person's decision, recorded as made in the dashboard (404 without a
//!   candidate).
//!
//! Every handler reads or writes files, off the async runtime. A write is
//! refused (409) when the server itself runs as a step of a run.

use apb_core::quality::grade::Grade;
use apb_core::step_env::StepCaller;
use apb_engine::candidate::{CandidateQualityView, PersonDecision};
use apb_mcp::tools::ToolError;
use apb_mcp::tools::candidate::{candidate_decide, candidate_view};
use apb_mcp::tools::grade::{grade_by_rules, grade_reopen, run_grades};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use crate::state::*;

/// Who decided, as the candidate's trial record and a reopened grading
/// keep it.
const DECIDED_BY: &str = "dashboard";

/// `GET /api/runs/{id}/grades`.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct RunGrades {
    pub run_id: String,
    pub grades: Vec<Grade>,
}

/// `GET /api/playbooks/{id}/candidate`.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PlaybookCandidate {
    pub id: String,
    pub candidate: Option<CandidateQualityView>,
}

/// Body of `POST /api/playbooks/{id}/candidate/keep-current`.
#[derive(Deserialize, Default)]
pub(crate) struct KeepCurrentBody {
    #[serde(default)]
    reason: Option<String>,
}

/// The status a tool refusal answers with.
fn refusal(e: ToolError) -> Response {
    match e {
        ToolError::NotFound(what) => (StatusCode::NOT_FOUND, what).into_response(),
        ToolError::Conflict(what) => (StatusCode::CONFLICT, what).into_response(),
        ToolError::Engine(what) => (StatusCode::UNPROCESSABLE_ENTITY, what).into_response(),
    }
}

/// Runs `work` on the project the request names, off the runtime, and
/// answers its value as JSON.
async fn answer<T, F>(state: &AppState, id: &str, workspace: Option<&str>, work: F) -> Response
where
    T: Serialize + Send + 'static,
    F: FnOnce(std::path::PathBuf) -> Result<T, ToolError> + Send + 'static,
{
    if !is_safe_id(id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(state, workspace) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match tokio::task::spawn_blocking(move || work(root)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => refusal(e),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(crate) async fn get_grades_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let run_id = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        run_grades(&root, &run_id).map(|grades| RunGrades { run_id, grades })
    })
    .await
}

pub(crate) async fn post_grades_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let run_id = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        grade_by_rules(&root, &run_id, &StepCaller::from_env())
    })
    .await
}

pub(crate) async fn post_reopen_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let run_id = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        grade_reopen(&root, &run_id, DECIDED_BY, &StepCaller::from_env())
    })
    .await
}

pub(crate) async fn get_candidate_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        candidate_view(&root, &pid).map(|candidate| PlaybookCandidate { id: pid, candidate })
    })
    .await
}

pub(crate) async fn post_promote_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        candidate_decide(
            &root,
            &pid,
            PersonDecision::Promote,
            DECIDED_BY,
            &StepCaller::from_env(),
        )
    })
    .await
}

pub(crate) async fn post_keep_current_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    body: Option<Json<KeepCurrentBody>>,
) -> Response {
    let pid = id.clone();
    let reason = body.and_then(|Json(b)| b.reason);
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        candidate_decide(
            &root,
            &pid,
            PersonDecision::KeepCurrent { reason },
            DECIDED_BY,
            &StepCaller::from_env(),
        )
    })
    .await
}
