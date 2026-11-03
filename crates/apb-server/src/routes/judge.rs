//! The judge sandbox and labelled examples on the HTTP API, over the tool
//! layer MCP and the CLI use (`apb_mcp::tools::judge`), so every surface
//! answers the same evaluation:
//!
//! - `POST /api/playbooks/{id}/judge/{node}/evaluate` (a `SandboxRequest`):
//!   the sandbox evaluation; without `answers` it asks this machine's
//!   providers, which a dashboard without keys refuses (409);
//! - `GET /api/playbooks/{id}/judge/{node}/examples`: the node's examples;
//! - `POST /api/playbooks/{id}/judge/{node}/examples/check` (`{ live? }`):
//!   the examples and their check (a live one refused without keys);
//! - `POST /api/runs/{run}/judge/{node}/examples` (an `ExampleSaveRequest`):
//!   saves that judge execution as an example, refused without keys;
//! - `GET /api/runs/{run}/judge/{node}/values?execution=N`: "fill from a
//!   run", the execution's inputs and the run's other values, redacted.
//!
//! Every handler reads or writes files, off the async runtime. A spend or
//! a save is refused (409) when the server itself runs as a step of a run.

use apb_core::step_env::StepCaller;
use apb_engine::judge_eval::example::ExampleSaveRequest;
use apb_engine::judge_eval::sandbox::SandboxRequest;
use apb_mcp::tools::judge::{
    JudgeCaller, judge_evaluate, judge_example_save, judge_examples, judge_run_values,
};
use axum::Router;
use axum::extract::{Path as AxPath, Query, State};
use axum::response::{Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use crate::state::*;

/// Who labelled an example saved here.
const LABELLED_BY: &str = "dashboard";

/// The judge routes.
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/playbooks/{id}/judge/{node}/evaluate",
            post(post_evaluate_handler),
        )
        .route(
            "/api/playbooks/{id}/judge/{node}/examples",
            get(get_examples_handler),
        )
        .route(
            "/api/playbooks/{id}/judge/{node}/examples/check",
            post(post_check_handler),
        )
        .route(
            "/api/runs/{id}/judge/{node}/examples",
            post(post_save_handler),
        )
        .route(
            "/api/runs/{id}/judge/{node}/values",
            get(get_values_handler),
        )
}

pub(crate) async fn post_evaluate_handler(
    State(state): State<AppState>,
    AxPath((id, node)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
    Json(req): Json<SandboxRequest>,
) -> Response {
    let keyless = !state.auth.enabled();
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let step = StepCaller::from_env();
        let caller = JudgeCaller {
            step: &step,
            keyless_dashboard: keyless,
        };
        judge_evaluate(&root, &pid, &node, &req, caller)
    })
    .await
}

pub(crate) async fn get_examples_handler(
    State(state): State<AppState>,
    AxPath((id, node)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let step = StepCaller::from_env();
        judge_examples(
            &root,
            &pid,
            Some(&node),
            false,
            false,
            JudgeCaller::of(&step),
        )
    })
    .await
}

/// Body of `POST .../judge/{node}/examples/check`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckBody {
    #[serde(default)]
    live: bool,
}

pub(crate) async fn post_check_handler(
    State(state): State<AppState>,
    AxPath((id, node)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
    body: Option<Json<CheckBody>>,
) -> Response {
    let live = body.is_some_and(|Json(b)| b.live);
    let keyless = !state.auth.enabled();
    let pid = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let step = StepCaller::from_env();
        let caller = JudgeCaller {
            step: &step,
            keyless_dashboard: keyless,
        };
        judge_examples(&root, &pid, Some(&node), true, live, caller)
    })
    .await
}

pub(crate) async fn post_save_handler(
    State(state): State<AppState>,
    AxPath((id, node)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
    Json(req): Json<ExampleSaveRequest>,
) -> Response {
    let keyless = !state.auth.enabled();
    let run_id = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let step = StepCaller::from_env();
        let caller = JudgeCaller {
            step: &step,
            keyless_dashboard: keyless,
        };
        judge_example_save(&root, &run_id, &node, &req, LABELLED_BY, caller)
    })
    .await
}

/// Query of `GET .../judge/{node}/values`.
#[derive(Deserialize, Default)]
pub(crate) struct ValuesQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    execution: Option<u32>,
}

pub(crate) async fn get_values_handler(
    State(state): State<AppState>,
    AxPath((id, node)): AxPath<(String, String)>,
    Query(q): Query<ValuesQuery>,
) -> Response {
    let run_id = id.clone();
    let execution = q.execution;
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        judge_run_values(&root, &run_id, &node, execution)
    })
    .await
}
