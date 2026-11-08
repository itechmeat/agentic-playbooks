//! A playbook's corrections ledger on the HTTP API, over the tool layer
//! MCP, the CLI and the dashboard share (`apb_mcp::tools::corrections`):
//!
//! - `GET /api/playbooks/{id}/corrections`: the entries with their drafts
//!   and rates (`CorrectionsView`); a run step is refused (409);
//! - `POST /api/playbooks/{id}/corrections/{entry}/{action}` with `action`
//!   `fixed` (`{ version, reason? }`), `dismiss` or `reopen`
//!   (`{ reason? }`): a person's decision, `by: "dashboard"`. A refusal is
//!   409, an unknown action or body field 422.
//!
//! Every handler reads or writes files, off the async runtime. These
//! routes serve project playbooks; MCP and the CLI also take a global one.

use apb_core::step_env::StepCaller;
use apb_mcp::tools::corrections::{Decision, Target, correction_decide, corrections_view};
use axum::Router;
use axum::extract::{Path as AxPath, Query, State};
use axum::response::{Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use crate::state::*;

/// The corrections routes.
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/playbooks/{id}/corrections",
            get(get_corrections_handler),
        )
        .route(
            "/api/playbooks/{id}/corrections/{entry}/{action}",
            post(post_correction_action_handler),
        )
}

#[derive(Deserialize, Default)]
pub(crate) struct CorrectionsQuery {
    #[serde(default)]
    workspace: Option<String>,
    /// `project` (default) or `global`.
    #[serde(default)]
    scope: Option<String>,
}

/// The target of the routes: the playbook `id` of the query's scope (the
/// scope the playbook lives under when the query names none, like the
/// problems routes). Resolved under `root`.
fn target_of(
    root: &std::path::Path,
    id: &str,
    scope: Option<&str>,
) -> Result<Target, apb_mcp::tools::ToolError> {
    Target::resolve(root, id, scope)
}

pub(crate) async fn get_corrections_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<CorrectionsQuery>,
) -> Response {
    let pid = id.clone();
    let scope = q.scope.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let target = target_of(&root, &pid, scope.as_deref())?;
        corrections_view(&root, &target, &StepCaller::from_env())
    })
    .await
}

/// The body of `POST .../corrections/{entry}/{action}`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct CorrectionAction {
    /// `fixed` only: the version that carries the fix.
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

pub(crate) async fn post_correction_action_handler(
    State(state): State<AppState>,
    AxPath((id, entry, action)): AxPath<(String, String, String)>,
    Query(q): Query<CorrectionsQuery>,
    Json(body): Json<CorrectionAction>,
) -> Response {
    let decision = match action.as_str() {
        "fixed" => match body.version.clone() {
            Some(version) => Decision::MarkFixed { version },
            None => {
                return tool_refusal(apb_mcp::tools::ToolError::Engine(
                    "the fixed action names the version that carries the fix".into(),
                ));
            }
        },
        "dismiss" => Decision::Dismiss,
        "reopen" => Decision::Reopen,
        other => {
            return tool_refusal(apb_mcp::tools::ToolError::Engine(format!(
                "unknown action `{other}`: fixed, dismiss or reopen"
            )));
        }
    };
    let pid = id.clone();
    let scope = q.scope.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let target = target_of(&root, &pid, scope.as_deref())?;
        correction_decide(
            &root,
            &target,
            &entry,
            decision,
            "dashboard",
            body.reason.as_deref(),
            &StepCaller::from_env(),
        )
    })
    .await
}
