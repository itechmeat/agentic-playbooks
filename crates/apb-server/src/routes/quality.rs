//! Run quality on the HTTP API, over the tool layer MCP and the CLI use
//! (`apb_mcp::tools::grade`, `apb_mcp::tools::candidate`), so a grade
//! stored here re-decides the candidate exactly as elsewhere:
//!
//! - `GET /api/runs/{id}/grades`: the run's stored grades, oldest first;
//! - `POST /api/runs/{id}/grades` (`{ grader? }`): grade the ended run by
//!   `rules` (the default) or by `model` (409 while it is live; a model
//!   grade that stored nothing answers `skipped` with the reason, 422 for
//!   an unknown grader);
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
use apb_mcp::tools::candidate::{candidate_decide, candidate_view};
use apb_mcp::tools::grade::{grade_by, grade_reopen, grader_of, run_grades};
use axum::extract::{Path as AxPath, Query, State};
use axum::response::{Json, Response};
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

/// Body of `POST /api/runs/{id}/grades`.
#[derive(Deserialize, Default)]
pub(crate) struct GradeBody {
    /// `rules` (default) or `model`.
    #[serde(default)]
    grader: Option<String>,
}

pub(crate) async fn post_grades_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    body: Option<Json<GradeBody>>,
) -> Response {
    let run_id = id.clone();
    let grader = body.and_then(|Json(b)| b.grader);
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let grader = grader_of(grader.as_deref())?;
        grade_by(&root, &run_id, grader, &StepCaller::from_env())
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
