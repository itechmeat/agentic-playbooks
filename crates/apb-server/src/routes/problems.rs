//! Known problems on the HTTP API, over the tool layer MCP and the CLI use
//! (`apb_mcp::tools::problems`), so the dashboard reads and decides exactly
//! as elsewhere:
//!
//! - `GET /api/playbooks/{id}/known-problems`: the playbook's problems;
//! - `GET /api/playbooks/{id}/known-problems/{pid}`: one problem and the
//!   passages of its source that are new since it was seen;
//! - `POST /api/playbooks/{id}/known-problems/{pid}/{action}` with
//!   `action` `confirm`, `dismiss`, `reopen` (`{ reason? }`), `close`
//!   (`{ quote, reason? }`) or `recheck` (`{ verdict?, quote?, reason? }`:
//!   with `verdict` (`holds`, `closed` or `undetermined`) the person's own
//!   recheck, without it the project's decision model).
//!
//! Every query takes `workspace` and `scope` (`project` or `global`,
//! defaulting to where the playbook lives). Every handler reads or writes
//! files, off the async runtime. A write is refused (409) when the server
//! itself runs as a step of a run; an unknown action or a close without a
//! quote is 422, and so is a body with a field the route does not know.
//!
//! The API is the dashboard's: it is not guarded against other local
//! callers beyond the run-step variables of the server's own process (an
//! agent that can reach the port acts as the person at the keyboard).

use apb_core::step_env::StepCaller;
use apb_engine::known_problems::RecheckBy;
use apb_mcp::tools::problems::{
    Target, action_of, problem_decide, problem_detail, problem_recheck, problems_view, proposal_of,
};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::*;

/// Who decided or rechecked, as the store keeps it.
const DECIDED_BY: &str = "dashboard";

/// The query every known-problem route takes.
#[derive(Deserialize, Default)]
pub(crate) struct ProblemsQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// Body of `POST /api/playbooks/{id}/known-problems/{pid}/{action}`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProblemBody {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    quote: Option<String>,
    /// A person's recheck verdict; absent: the decision model rechecks.
    #[serde(default)]
    verdict: Option<String>,
}

pub(crate) async fn get_problems_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<ProblemsQuery>,
) -> Response {
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let t = Target::resolve(&root, &pid, q.scope.as_deref())?;
        problems_view(&root, &t)
    })
    .await
}

pub(crate) async fn get_problem_handler(
    State(state): State<AppState>,
    AxPath((id, problem)): AxPath<(String, String)>,
    Query(q): Query<ProblemsQuery>,
) -> Response {
    if !is_safe_id(&problem) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let t = Target::resolve(&root, &pid, q.scope.as_deref())?;
        problem_detail(&root, &t, &problem)
    })
    .await
}

pub(crate) async fn post_problem_handler(
    State(state): State<AppState>,
    AxPath((id, problem, action)): AxPath<(String, String, String)>,
    Query(q): Query<ProblemsQuery>,
    body: Option<Json<ProblemBody>>,
) -> Response {
    if !is_safe_id(&problem) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let t = Target::resolve(&root, &pid, q.scope.as_deref())?;
        let step = StepCaller::from_env();
        if action == "recheck" {
            let by = match body.verdict {
                Some(v) => RecheckBy::Person(proposal_of(&v, body.quote, body.reason)?),
                None => RecheckBy::Model,
            };
            return problem_recheck(&root, &t, &problem, by, DECIDED_BY, &step)
                .map(|r| serde_json::json!(r));
        }
        let act = action_of(&action, body.quote)?;
        problem_decide(
            &root,
            &t,
            &problem,
            act,
            body.reason.as_deref(),
            DECIDED_BY,
            &step,
        )
        .map(|k| serde_json::json!(k))
    })
    .await
}
