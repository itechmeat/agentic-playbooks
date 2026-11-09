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
                "parent_workspace_id": e.parent_workspace_id,
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
            "cloud": cloud_block(&c.cloud),
        }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// The `cloud` block of `/api/models`: per provider its catalog, the key
/// variable by name and whether it is set for the server, never its value.
pub(crate) fn cloud_block(
    snaps: &[apb_core::cloud_models::catalog::CatalogSnapshot],
) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = snaps
        .iter()
        .filter_map(|s| {
            let p = apb_core::cloud_models::lookup(&s.provider)?;
            Some(serde_json::json!({
                "provider": p.id,
                "display": p.display,
                "base_url": p.base_url,
                "key_env": p.key_env,
                "key_present": std::env::var_os(p.key_env).is_some_and(|v| !v.is_empty()),
                "source": s.source,
                "fetched_at": s.fetched_at,
                "digest": s.digest,
                "dropped": s.dropped,
                "models": s.models,
            }))
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// POST /api/models/refresh: fetches every cloud catalog's public listing
/// (no credential is sent) off the async runtime and answers the new
/// `cloud` block, or 502 `cloud_catalog_refresh_failed` with the reason;
/// a failure keeps the earlier catalog. Behind [`local_gate`].
pub(crate) async fn refresh_models_handler() -> impl IntoResponse {
    let done = tokio::task::spawn_blocking(|| {
        let results = apb_core::cloud_models::catalog::refresh_all();
        let failures: Vec<String> = results
            .iter()
            .filter_map(|r| r.as_ref().err().map(|e| e.to_string()))
            .collect();
        (failures, apb_core::cloud_models::catalog::load_all())
    })
    .await;
    match done {
        Ok((failures, _)) if !failures.is_empty() => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": apb_core::cloud_models::catalog::REFRESH_FAILED,
                "detail": failures.join("; "),
            })),
        )
            .into_response(),
        Ok((_, snaps)) => no_store(Json(serde_json::json!({ "cloud": cloud_block(&snaps) }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The catalog refresh route, behind [`local_gate`].
pub(crate) fn router() -> axum::Router<crate::state::AppState> {
    axum::Router::new()
        .route(
            "/api/models/refresh",
            axum::routing::post(refresh_models_handler),
        )
        .route_layer(axum::middleware::from_fn(local_gate))
}

/// Refuses a client that is not straight from this machine (the rule of
/// [`crate::state::local_client`]), before the refresh makes a request.
async fn local_gate(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let local = req
        .extensions()
        .get::<crate::auth::ClientCtx>()
        .is_some_and(|ctx| crate::state::local_client(ctx, req.headers()));
    if !local {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "cloud_refresh_local_only",
                "detail": "the model catalog is refreshed only for a client on this machine",
            })),
        )
            .into_response();
    }
    next.run(req).await
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
