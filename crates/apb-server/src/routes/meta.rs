use apb_core::projects::{self};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};

pub(crate) async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// GET /api/projects: the reachable projects the global dashboard aggregates.
pub(crate) async fn list_projects_handler() -> impl IntoResponse {
    let projects: Vec<serde_json::Value> = projects::list_reachable()
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "workspace_id": e.workspace_id,
                "name": e.name,
                "path": e.path,
                "playbook_count": e.playbook_count,
            })
        })
        .collect();
    Json(projects).into_response()
}

/// GET /api/agents: agents detected on this machine (free detection, cached).
/// Machine-wide, so it needs no project root. Powers the profile form's agent
/// combobox.
pub(crate) async fn list_agents_handler() -> impl IntoResponse {
    Json(serde_json::json!({ "agents": apb_core::detect::detect(false) })).into_response()
}

/// GET /api/models: the curated models table (a hint, not a hard binding),
/// the claude static list, and `options_by_agent` - the per-agent option list
/// the profile form's model combobox now uses (issue #42 finding 9): the
/// curated table filtered to that agent's vendor, each row annotated
/// `detected` when the agent's local config/detected model list also names
/// it, plus a `detected`-only entry for a detected model the table does not
/// carry. Detection only annotates or extends the list here, it never
/// replaces it - see `apb_core::models_table::model_options_for_agent`.
/// Machine-wide. Powers the model combobox.
pub(crate) async fn list_models_handler() -> impl IntoResponse {
    match apb_core::models_table::load_merged() {
        Ok(t) => {
            let agents = apb_core::detect::detect(false);
            let options_by_agent: std::collections::BTreeMap<
                String,
                Vec<apb_core::models_table::ModelOption>,
            > = agents
                .iter()
                .map(|a| {
                    let detected = a
                        .models
                        .as_ref()
                        .map(|m| m.items.clone())
                        .unwrap_or_default();
                    (
                        a.agent.clone(),
                        apb_core::models_table::model_options_for_agent(&a.agent, &detected, &t),
                    )
                })
                .collect();
            Json(serde_json::json!({
                "as_of": t.as_of,
                "models": t.models,
                "claude_static": t.claude_static_models,
                "options_by_agent": options_by_agent,
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
