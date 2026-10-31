//! The run estimate and budget on the HTTP API: `GET
//! /api/playbooks/{id}/estimate` (what a run of a version takes and spends,
//! before a start), the budget a run start and a resume take, and their
//! refusals. The refusals are MCP's (`apb_mcp::tools::budget`), so both
//! surfaces answer alike: 409 `budget_needs_decision` for a start the person
//! must decide first, 409 `budget_paused` for a plain resume of a run its
//! budget paused, 400 `invalid_budget` for a bad amount or policy.

use std::path::Path;

use apb_core::budget::BudgetRequest;
use apb_engine::budget::BudgetResume;
use apb_mcp::tools::budget as mcp_budget;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::*;

/// The budget of a run start (`RunBody.budget`).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct BudgetBody {
    #[serde(default)]
    tokens: Option<u64>,
    #[serde(default)]
    usd: Option<f64>,
    #[serde(default)]
    on_no_estimate: Option<String>,
}

/// The budget change of a resume (`ResumeBody.budget`).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ResumeBudgetBody {
    #[serde(default)]
    raise_tokens: Option<u64>,
    #[serde(default)]
    raise_usd: Option<f64>,
    #[serde(default)]
    tokens: Option<u64>,
    #[serde(default)]
    usd: Option<f64>,
    #[serde(default)]
    on_no_estimate: Option<String>,
    #[serde(default)]
    once: bool,
}

/// A budget refusal: its status and JSON body.
pub(crate) struct Refusal(StatusCode, serde_json::Value);

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

fn bad_request(v: serde_json::Value) -> Refusal {
    Refusal(StatusCode::BAD_REQUEST, v)
}

/// What a start takes from its budget check: the budget argument, the
/// warnings for the answer, and the estimate the check read (the run's
/// snapshot).
pub(crate) struct StartBudget {
    pub(crate) budget: Option<BudgetRequest>,
    pub(crate) warnings: Vec<String>,
    pub(crate) estimate: Option<apb_engine::run_estimate::RunEstimate>,
}

/// The budget check of a start of playbook `id` (`version`) in the project
/// at `root`, or the refusal to return instead of starting. Reads the
/// project's run history: call it off the async runtime.
pub(crate) fn start_budget(
    root: &Path,
    id: &str,
    version: Option<&str>,
    body: Option<&BudgetBody>,
) -> Result<StartBudget, Refusal> {
    let req = match body {
        None => None,
        Some(b) => mcp_budget::start_request(b.tokens, b.usd, b.on_no_estimate.as_deref())
            .map_err(bad_request)?,
    };
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: id.to_string(),
        version: version.map(str::to_string),
    };
    let Ok(resolved) = apb_core::store::resolve(root, &wref) else {
        return Ok(StartBudget {
            budget: req,
            warnings: Vec::new(),
            estimate: None,
        });
    };
    let mode = execution_mode(root);
    match mcp_budget::preflight(&resolved, mode.as_str(), req.as_ref()) {
        Ok(p) => Ok(StartBudget {
            budget: req,
            warnings: p.warnings,
            estimate: p.estimate,
        }),
        Err(v) if v["error"] == mcp_budget::BUDGET_NEEDS_DECISION => {
            Err(Refusal(StatusCode::CONFLICT, v))
        }
        Err(v) => Err(bad_request(v)),
    }
}

/// The project's configured execution mode (`cli` unless set to `host`).
fn execution_mode(root: &Path) -> apb_core::execution::ExecutionMode {
    apb_core::execution::resolve_for(root, &Default::default())
        .map(|r| r.mode)
        .unwrap_or_default()
}

/// The engine request of a resume's budget change, or the 400 refusal.
pub(crate) fn resume_budget(
    body: Option<&ResumeBudgetBody>,
) -> Result<Option<BudgetResume>, Refusal> {
    let Some(b) = body else {
        return Ok(None);
    };
    mcp_budget::resume_request(mcp_budget::ResumeArgs {
        raise_tokens: b.raise_tokens,
        raise_usd: b.raise_usd,
        tokens: b.tokens,
        usd: b.usd,
        on_no_estimate: b.on_no_estimate.as_deref(),
        once: b.once,
    })
    .map_err(bad_request)
}

/// The 409 `budget_paused` answer of a resume the budget refused, with the
/// run's budget; `None` for any other conflict.
pub(crate) fn resume_refusal(root: &Path, run_id: &str, detail: &str) -> Option<Response> {
    let dir = root.join(".apb/runs").join(run_id);
    mcp_budget::resume_refusal(&dir, run_id, detail)
        .map(|v| (StatusCode::CONFLICT, Json(v)).into_response())
}

#[derive(Deserialize)]
pub(crate) struct EstimateQuery {
    version: Option<String>,
    workspace: Option<String>,
}

/// GET /api/playbooks/{id}/estimate: what a run of the playbook's version
/// (`?version=`, else current) takes and spends, from the comparable runs of
/// its project, per node and in total, with a text `summary`. Read-only;
/// the journals are read off the async runtime.
pub(crate) async fn get_estimate_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<EstimateQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) || q.version.as_deref().is_some_and(|v| !is_safe_id(v)) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let version = q.version;
    let res = tokio::task::spawn_blocking(move || {
        mcp_budget::playbook_estimate(&root, &id, version.as_deref())
    })
    .await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(apb_mcp::tools::ToolError::NotFound(what))) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
