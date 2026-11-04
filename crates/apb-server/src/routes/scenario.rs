//! Scenario runs on the HTTP API, over the tool layer MCP and the CLI use
//! (`apb_mcp::tools::scenario`):
//!
//! - `GET /api/playbooks/{id}/scenarios?version=`: the scenarios of the
//!   version with their newest results (`PlaybookScenarios`);
//! - `POST /api/playbooks/{id}/scenario-runs` (a `ScenarioRunRequest`):
//!   plays one scenario through `apb __scenario-run` and answers its
//!   report and graph (`ScenarioRunView`); the request waits for it (quick
//!   60 s, smoke 300 s). A smoke run spends, so a dashboard without keys
//!   refuses it (409);
//! - `GET /api/playbooks/{id}/scenario-runs/{run}`: a stored scenario run's
//!   report and graph.
//!
//! Every handler reads or writes files, off the async runtime. These routes
//! serve project playbooks only; MCP and `apb eval` also take a global one.

use apb_core::step_env::StepCaller;
use apb_mcp::tools::ToolError;
use apb_mcp::tools::scenario::{
    ScenarioCaller, ScenarioRunRequest, Scope, run_scenario, scenario_run_view, scenarios,
};
use axum::Router;
use axum::extract::{Path as AxPath, Query, State};
use axum::response::{Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use crate::state::*;

/// The scenario routes.
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/playbooks/{id}/scenarios", get(get_scenarios_handler))
        .route(
            "/api/playbooks/{id}/scenario-runs",
            post(post_scenario_run_handler),
        )
        .route(
            "/api/playbooks/{id}/scenario-runs/{run}",
            get(get_scenario_run_handler),
        )
}

/// Query of `GET .../scenarios`.
#[derive(Deserialize, Default)]
pub(crate) struct ScenariosQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

pub(crate) async fn get_scenarios_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<ScenariosQuery>,
) -> Response {
    let pid = id.clone();
    let version = q.version;
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        scenarios(&root, Scope::Project, &pid, version.as_deref())
    })
    .await
}

pub(crate) async fn post_scenario_run_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    Json(req): Json<ScenarioRunRequest>,
) -> Response {
    let keyless = !state.auth.enabled();
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let exe = apb_core::fsutil::reexec_exe()
            .map_err(|e| ToolError::Engine(format!("the apb executable: {e}")))?;
        let step = StepCaller::from_env();
        let caller = ScenarioCaller {
            step: &step,
            keyless_dashboard: keyless,
        };
        run_scenario(&root, Scope::Project, &pid, &req, caller, &exe)
    })
    .await
}

pub(crate) async fn get_scenario_run_handler(
    State(state): State<AppState>,
    AxPath((id, run)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        scenario_run_view(&root, &pid, &run)
    })
    .await
}
