pub mod lock;
pub mod watch;

use std::path::PathBuf;
use std::sync::Arc;

use apb_core::registry::{Registry, RegistryError};
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_core::versioning::{
    VersioningError, create_version, delete_playbook, list_versions_with_provenance,
    promote_version, save_layout, version_diff,
};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post, put};
use serde::Deserialize;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    pub root: Arc<PathBuf>,
    pub events: broadcast::Sender<String>,
}

impl AppState {
    pub fn new(root: PathBuf) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root: Arc::new(root),
            events,
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/playbooks", get(list_playbooks).post(create_playbook))
        .route(
            "/api/playbooks/{id}",
            get(get_playbook)
                .put(update_playbook)
                .delete(delete_playbook_handler),
        )
        .route("/api/playbooks/{id}/layout", put(put_layout))
        .route("/api/playbooks/{id}/diff", get(get_diff))
        .route("/api/playbooks/{id}/versions", get(list_versions_handler))
        .route(
            "/api/playbooks/{id}/versions/{version}/promote",
            post(promote_version_handler),
        )
        .route("/api/profiles", get(list_profiles).post(write_profile))
        .route("/api/runs", get(list_runs_handler))
        .route("/api/runs/{id}", get(get_run_handler))
        .route("/api/runs/{id}/report", get(get_run_report_handler))
        .route("/api/runs/{id}/review", post(post_review_handler))
        .route("/api/hooks/{run_id}/{secret}", post(post_hook_handler))
        .route("/api/ws", get(ws_handler))
        .fallback(static_handler)
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

// Run/playbook identifier: always a single name segment.
// Reject anything that could escape the directory (`/`, `\`, `..`, empty).
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && !id.contains('/') && !id.contains('\\') && !id.contains("..")
}

fn registry(state: &AppState) -> Result<Registry, (StatusCode, String)> {
    Registry::open(&state.root).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn versioning_error(e: VersioningError) -> Response {
    match e {
        VersioningError::NotFound(what) => (StatusCode::NOT_FOUND, what).into_response(),
        VersioningError::Validation(codes) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "validation", "codes": codes })),
        )
            .into_response(),
        VersioningError::Schema(msg) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "schema", "message": msg })),
        )
            .into_response(),
        VersioningError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        VersioningError::Io(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

#[derive(Deserialize)]
struct CreatePlaybookBody {
    id: String,
    yaml: String,
}

#[derive(Deserialize)]
struct UpdatePlaybookBody {
    yaml: String,
}

#[derive(Deserialize)]
struct LayoutBody {
    /// YAML string or JSON layout value (coerced to YAML).
    layout: serde_json::Value,
}

#[derive(Deserialize)]
struct LayoutQuery {
    version: String,
}

#[derive(Deserialize)]
struct DiffQuery {
    from: String,
    to: String,
}

async fn create_playbook(
    State(state): State<AppState>,
    Json(body): Json<CreatePlaybookBody>,
) -> impl IntoResponse {
    if !is_safe_id(&body.id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match create_version(&state.root, &body.id, &body.yaml, None, true) {
        Ok(version) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "id": body.id, "version": version })),
        )
            .into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn update_playbook(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<UpdatePlaybookBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let dir = state.root.join(".apb/playbooks").join(&id);
    if !dir.is_dir() {
        return (StatusCode::NOT_FOUND, format!("playbook `{id}` not found")).into_response();
    }
    match create_version(&state.root, &id, &body.yaml, None, true) {
        Ok(version) => Json(serde_json::json!({ "id": id, "version": version })).into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn delete_playbook_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match delete_playbook(&state.root, &id, apb_engine::event::now_millis()) {
        Ok(trashed) => Json(serde_json::json!({
            "trashed": trashed.to_string_lossy(),
        }))
        .into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn put_layout(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<LayoutQuery>,
    Json(body): Json<LayoutBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) || !is_safe_id(&q.version) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let layout_yaml = match body.layout {
        serde_json::Value::String(s) => s,
        other => match serde_yaml_ng::to_string(&other) {
            Ok(s) => s,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "schema", "message": e.to_string() })),
                )
                    .into_response();
            }
        },
    };
    match save_layout(&state.root, &id, &q.version, &layout_yaml) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn get_diff(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<DiffQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) || !is_safe_id(&q.from) || !is_safe_id(&q.to) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match version_diff(&state.root, &id, &q.from, &q.to) {
        Ok(diff) => Json(diff).into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn list_versions_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match list_versions_with_provenance(&state.root, &id) {
        Ok(infos) => Json(infos).into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn promote_version_handler(
    State(state): State<AppState>,
    AxPath((id, version)): AxPath<(String, String)>,
) -> impl IntoResponse {
    if !is_safe_id(&id) || !is_safe_id(&version) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match promote_version(&state.root, &id, &version) {
        Ok(()) => Json(serde_json::json!({ "promoted": version })).into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn list_playbooks(State(state): State<AppState>) -> impl IntoResponse {
    let reg = match registry(&state) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    match reg.list() {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct DetailQuery {
    version: Option<String>,
}

async fn get_playbook(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<DetailQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let reg = match registry(&state) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    match reg.load(&id, q.version.as_deref()) {
        Ok(loaded) => {
            let ctx = ValidationContext {
                profiles: reg.profiles(),
                ..Default::default()
            };
            let report = validate(&loaded.playbook, &ctx);
            let validation: Vec<serde_json::Value> = report.issues.iter().map(|i| serde_json::json!({
                "code": i.code,
                "severity": match i.severity { Severity::Error => "error", Severity::Warning => "warning" },
                "message": i.message,
                "node": i.node,
            })).collect();
            Json(serde_json::json!({
                "id": id,
                "version": loaded.version,
                "yaml": loaded.yaml,
                "playbook": loaded.playbook,
                "layout": loaded.layout,
                "validation": validation,
            }))
            .into_response()
        }
        Err(RegistryError::NotFound(what)) => (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/profiles: project and global profiles with trust status (the
/// same shape as the MCP tool profile_list).
async fn list_profiles(State(state): State<AppState>) -> impl IntoResponse {
    match apb_mcp::profile_tools::profile_list(&state.root) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct ProfileWriteBody {
    name: String,
    #[serde(default = "default_scope")]
    scope: String,
    agent: String,
    model: String,
    #[serde(default)]
    fallbacks: Vec<FallbackBody>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    soul: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_soul_req")]
    soul_requirement: String,
    #[serde(default)]
    expected_digest: Option<String>,
}

#[derive(Deserialize)]
struct FallbackBody {
    agent: String,
    model: String,
}

fn default_scope() -> String {
    "project".to_string()
}
fn default_soul_req() -> String {
    "any".to_string()
}

/// POST /api/profiles: create/update a profile through the same
/// profile_write logic (validation, CAS lock, auto-approve bundle). A CAS
/// conflict is 409; a validation error is 400.
async fn write_profile(
    State(state): State<AppState>,
    Json(body): Json<ProfileWriteBody>,
) -> impl IntoResponse {
    use apb_mcp::profile_tools::{self, ExecutorInput, ProfileWrite};
    let soul_requirement = match profile_tools::parse_soul_requirement(Some(&body.soul_requirement))
    {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let res = profile_tools::profile_write(
        &state.root,
        ProfileWrite {
            name: body.name,
            scope: body.scope,
            description: body.description,
            soul_md: body.soul,
            skills: profile_tools::skill_refs(&body.skills),
            executor: ExecutorInput {
                agent: body.agent,
                model: body.model,
                fallbacks: body
                    .fallbacks
                    .into_iter()
                    .map(|f| (f.agent, f.model))
                    .collect(),
            },
            expected_digest: body.expected_digest,
            soul_requirement,
        },
    );
    match res {
        Ok(v) => Json(v).into_response(),
        Err(apb_mcp::tools::ToolError::Conflict(detail)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "conflict", "detail": detail })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "bad_request", "detail": e.to_string() })),
        )
            .into_response(),
    }
}

async fn list_runs_handler(State(state): State<AppState>) -> impl IntoResponse {
    match apb_engine::list_runs(&state.root) {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_run_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let run_dir = state.root.join(".apb/runs").join(&id);
    if !run_dir.is_dir() {
        return (StatusCode::NOT_FOUND, format!("run `{id}` not found")).into_response();
    }
    let events = match apb_engine::event::read_all(&run_dir) {
        Ok(ev) => ev,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let run_state = apb_engine::state::RunState::fold(&events);
    let cfg = apb_engine::run_config::read_run_config(&run_dir).unwrap_or_default();

    // The run's playbook snapshot (may be missing for very old runs).
    let (playbook_json, playbook_id, version) = {
        let path = run_dir.join("playbook.yaml");
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|y| apb_core::schema::Playbook::from_yaml(&y).ok())
        {
            Some(playbook) => (
                serde_json::to_value(&playbook).unwrap_or(serde_json::Value::Null),
                playbook.id.clone(),
                playbook.version.clone(),
            ),
            None => (serde_json::Value::Null, id.clone(), String::new()),
        }
    };

    let nodes: std::collections::BTreeMap<String, String> = run_state
        .nodes
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
        .collect();

    // The run's hooks as map key -> relative path of the signal endpoint.
    let hooks: std::collections::BTreeMap<String, String> = apb_engine::read_hooks(&run_dir)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, secret)| (k, apb_engine::hook_path(&id, &secret)))
        .collect();

    Json(serde_json::json!({
        "run_id": id,
        "playbook": playbook_id,
        "version": version,
        "run_status": run_state.run_status.as_str(),
        "nodes": nodes,
        "outputs": run_state.outputs,
        "instruction": cfg.instruction,
        "params": cfg.params,
        "model": playbook_json,
        "hooks": hooks,
        "events": events,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct ReviewBody {
    node: String,
    decision: String,
    #[serde(default)]
    note: String,
}

async fn post_review_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(body): Json<ReviewBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let run_dir = state.root.join(".apb/runs").join(&id);
    if !run_dir.is_dir() {
        return (StatusCode::NOT_FOUND, format!("run `{id}` not found")).into_response();
    }
    let cmd = apb_engine::ReviewCommand {
        node: body.node,
        decision: body.decision,
        note: body.note,
    };
    match apb_engine::post_review(&run_dir, cmd) {
        Ok(seq) => Json(serde_json::json!({ "posted_seq": seq })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn post_hook_handler(
    State(state): State<AppState>,
    AxPath((run_id, secret)): AxPath<(String, String)>,
) -> impl IntoResponse {
    if !is_safe_id(&run_id) || !is_safe_id(&secret) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let run_dir = state.root.join(".apb/runs").join(&run_id);
    if !run_dir.is_dir() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let hooks = match apb_engine::read_hooks(&run_dir) {
        Ok(h) => h,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // The secret must match one of this run's hooks (otherwise 404 - a
    // foreign or incorrect secret must not accept the signal).
    let Some((key, _)) = hooks.iter().find(|(_, s)| *s == &secret) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    match apb_engine::post_signal(&run_dir, apb_engine::SignalCommand { key: key.clone() }) {
        Ok(seq) => Json(serde_json::json!({ "signalled": key, "posted_seq": seq })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_run_report_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match apb_engine::supervisor_report_or_summary(&state.root, &id) {
        Ok(report) => Json(serde_json::json!({ "report": report })).into_response(),
        Err(apb_engine::EngineError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("run `{id}` not found")).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, state))
}

async fn ws_loop(mut socket: WebSocket, state: AppState) {
    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() { break; }
                }
                Err(_) => break,
            },
            incoming = socket.recv() => {
                if incoming.is_none() { break; } // the client closed the connection
            }
        }
    }
}

#[derive(rust_embed::Embed)]
#[folder = "../../web/dist"]
struct WebAssets;

async fn static_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let asset = WebAssets::get(path).or_else(|| WebAssets::get("index.html"));
    match asset {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                content.data,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "web assets not built").into_response(),
    }
}

pub async fn run_server(root: PathBuf, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState::new(root.clone());
    let _watcher = watch::spawn_watcher(root.clone(), state.events.clone())?;
    let _lock = lock::write_lock(&root, port)?;
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("apb serve: http://127.0.0.1:{port}");
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    // Remove the lock both on normal shutdown and after catching a signal.
    lock::remove_lock(&root)?;
    result?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
