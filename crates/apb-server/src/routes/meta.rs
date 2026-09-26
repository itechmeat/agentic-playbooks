use apb_core::projects::{self};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};

/// GET /api/health: liveness plus the identity of the build serving it, so a
/// dashboard tab can notice that the server was rebuilt under it.
pub(crate) async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "build_id": crate::assets::build_id(),
        "version": env!("CARGO_PKG_VERSION"),
    }))
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

/// GET /api/agents: agents detected on this machine. Machine-wide, so it
/// needs no project root. Same data as `/api/models`' `agents`: both come from
/// `apb_core::agent_catalog::load`, the one source of truth.
pub(crate) async fn list_agents_handler() -> impl IntoResponse {
    match catalog().await {
        Ok(c) => no_store(Json(serde_json::json!({ "agents": c.agents }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// GET /api/models: the whole agent/model catalog the profile editor needs,
/// in one consistent snapshot from `apb_core::agent_catalog::load`: the
/// curated models table (a hint, not a hard binding), the claude and codex
/// static lists, the detected `agents`, and `options_by_agent` - the
/// per-agent option list of the model combobox (issue #42 finding 9, see
/// `apb_core::models_table::model_options_for_agent`). Machine-wide.
pub(crate) async fn list_models_handler() -> impl IntoResponse {
    match catalog().await {
        Ok(c) => no_store(Json(serde_json::json!({
            "as_of": c.table.as_of,
            "models": c.table.models,
            "claude_static": c.table.claude_static_models,
            "codex_static": c.table.codex_static_models,
            "agents": c.agents,
            "options_by_agent": c.options_by_agent,
        }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Loads the catalog off the async runtime: a cold detection spawns the
/// agents' CLIs and waits on them.
async fn catalog() -> Result<apb_core::agent_catalog::Catalog, String> {
    tokio::task::spawn_blocking(|| apb_core::agent_catalog::load(false))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// These lists must never be served from an HTTP cache: the server is the only
/// place that decides them.
fn no_store(body: Json<serde_json::Value>) -> axum::response::Response {
    ([(axum::http::header::CACHE_CONTROL, "no-store")], body).into_response()
}
