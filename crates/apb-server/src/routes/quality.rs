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
//! - `GET /api/playbooks/{id}/candidate/summary`: what a person reads before
//!   promoting (`summary: null` without a candidate);
//! - `POST /api/playbooks/{id}/candidate/promote` (`{ expected_version,
//!   expected_digest, acknowledged? }`: the candidate as the person saw it;
//!   a call without that body is refused `expected_required`; a refusal of
//!   the re-check answers its `PromoteRefusal` as JSON: 409 `stale` or
//!   `needs_acknowledgement` with the summary id, 422 `blocked`) and
//!   `POST /api/playbooks/{id}/candidate/keep-current` (`{ reason? }`): a
//!   person's decision, recorded as made in the dashboard (404 without a
//!   candidate).
//!
//! Every handler reads or writes files, off the async runtime. A write is
//! refused (409) when the server itself runs as a step of a run.

use apb_core::quality::grade::Grade;
use apb_core::step_env::StepCaller;
use apb_engine::candidate::summary::{CandidateSummary, PromoteRefusal, VersionDigest};
use apb_engine::candidate::{CandidateQualityView, PersonDecision};
use apb_mcp::tools::candidate::{
    DecideError, candidate_decide, candidate_decide_typed, candidate_summary, candidate_view,
};
use apb_mcp::tools::grade::{grade_by, grade_reopen, grader_of, run_grades};
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

/// `GET /api/playbooks/{id}/candidate/summary`.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PlaybookCandidateSummary {
    pub id: String,
    pub summary: Option<CandidateSummary>,
}

/// Body of `POST /api/playbooks/{id}/candidate/promote`: the candidate's
/// version and digest as the person saw them, and the summary id the
/// person acknowledged when it had attention items.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PromoteBody {
    pub expected_version: String,
    pub expected_digest: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional))]
    pub acknowledged: Option<String>,
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

/// The candidate summary of playbook `id`: what a person reads before
/// promoting it.
pub(crate) async fn get_candidate_summary_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        candidate_summary(&root, &pid).map(|summary| PlaybookCandidateSummary { id: pid, summary })
    })
    .await
}

/// A promotion's refusal as data: 409 when the person should look again
/// (`stale`, `needs_acknowledgement` with the summary id to name), 422 when
/// it is refused as asked (`blocked`, `expected_required`).
fn refusal_response(r: PromoteRefusal) -> Response {
    let status = match r {
        PromoteRefusal::Stale { .. } | PromoteRefusal::NeedsAcknowledgement { .. } => {
            StatusCode::CONFLICT
        }
        PromoteRefusal::Blocked { .. } | PromoteRefusal::ExpectedRequired => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
    };
    (status, Json(r)).into_response()
}

pub(crate) async fn post_promote_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    body: Option<Json<PromoteBody>>,
) -> Response {
    // An older dashboard promotes without saying what it showed: that is
    // refused before anything is looked up.
    let Some(Json(body)) = body else {
        return refusal_response(PromoteRefusal::ExpectedRequired);
    };
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let decision = PersonDecision::Promote {
        expected: VersionDigest {
            version: body.expected_version,
            digest: body.expected_digest,
        },
        acknowledged: body.acknowledged,
    };
    let work =
        move || candidate_decide_typed(&root, &id, decision, DECIDED_BY, &StepCaller::from_env());
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(rec)) => Json(rec).into_response(),
        Ok(Err(DecideError::Refused(r))) => refusal_response(r),
        Ok(Err(DecideError::Tool(e))) => tool_refusal(e),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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
