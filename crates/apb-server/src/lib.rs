pub mod lock;
pub mod watch;

use std::path::PathBuf;
use std::sync::Arc;

use apb_core::profile::ProfileScope;
use apb_core::projects::{self, ProjectAccessError};
use apb_core::registry::{PlaybookSummary, Registry, RegistryError};
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
    /// A pinned single project root. `None` is the production, global-only
    /// dashboard: there is no project-scoped server, and every project-specific
    /// request resolves its root from the `?workspace=<id>` param through the
    /// project registry. `Some` exists only for the pinned-root test harness
    /// (and keeps the older single-project handler tests unchanged): with a
    /// pinned root, a request that omits `workspace` falls back to it.
    pub root: Option<Arc<PathBuf>>,
    pub events: broadcast::Sender<String>,
}

impl AppState {
    /// Pinned to a single project root (test harness / backward-compat).
    pub fn new(root: PathBuf) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root: Some(Arc::new(root)),
            events,
        }
    }

    /// The global, machine-wide dashboard: no pinned root, projects resolved
    /// per request from the registry.
    pub fn new_global() -> Self {
        let (events, _) = broadcast::channel(64);
        Self { root: None, events }
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/projects", get(list_projects_handler))
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
        .route("/api/playbooks/{id}/frozen", put(set_frozen_handler))
        .route(
            "/api/playbooks/{id}/input-draft",
            get(get_input_draft_handler).put(put_input_draft_handler),
        )
        .route("/api/playbooks/{id}/run", post(run_playbook_handler))
        .route("/api/profiles", get(list_profiles).post(write_profile))
        .route(
            "/api/profiles/{name}",
            get(get_profile).delete(delete_profile),
        )
        .route("/api/agents", get(list_agents_handler))
        .route("/api/models", get(list_models_handler))
        .route("/api/skills", get(list_skills_handler))
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

/// Query param carrying the target project for a project-specific request.
#[derive(Deserialize, Default)]
struct WsQuery {
    workspace: Option<String>,
}

/// Resolves the project root for a request: an explicit `?workspace=<id>` wins
/// (resolved through the registry, with identity binding); otherwise the
/// pinned root is used (test harness). The global server has no pinned root, so
/// omitting `workspace` there is a 400.
///
/// The error is a ready-to-return HTTP `Response` (the natural shape for a
/// request helper), so the large-Err lint does not apply here.
#[allow(clippy::result_large_err)]
fn resolve_root(state: &AppState, workspace: Option<&str>) -> Result<PathBuf, Response> {
    if let Some(ws) = workspace {
        if !is_safe_id(ws) {
            return Err((StatusCode::BAD_REQUEST, "invalid workspace").into_response());
        }
        return projects::resolve_root(ws).map_err(|e| match e {
            ProjectAccessError::Unknown(w) => {
                (StatusCode::NOT_FOUND, format!("workspace `{w}` not found")).into_response()
            }
            ProjectAccessError::Unreachable { workspace_id, path } => (
                StatusCode::GONE,
                format!("workspace `{workspace_id}` is unreachable (path `{path}`)"),
            )
                .into_response(),
        });
    }
    match &state.root {
        Some(r) => Ok(r.as_ref().clone()),
        None => Err((
            StatusCode::BAD_REQUEST,
            "missing required `workspace` query parameter",
        )
            .into_response()),
    }
}

/// Resolves a root for a profile operation. Global-scope profiles live in a
/// single shared store that ignores the project root, so any reachable root
/// works; project-scope profiles need the specific project via `workspace`.
#[allow(clippy::result_large_err)]
fn resolve_root_for_scope(
    state: &AppState,
    workspace: Option<&str>,
    scope: &str,
) -> Result<PathBuf, Response> {
    if scope == "global" {
        if let Some(r) = &state.root {
            return Ok(r.as_ref().clone());
        }
        // Global-scope operations (global profile store under <config_dir>,
        // global skills under ~/.agents/skills) are root-independent: the
        // callees never read this path. Prefer a real reachable project when
        // one exists (keeps behavior identical when projects are present), but
        // fall back to a throwaway existing dir so a machine with zero
        // registered projects can still manage global profiles/skills instead
        // of being locked out with a 409.
        return Ok(enumerate_workspaces(state)
            .into_iter()
            .next()
            .map(|(_, _, root)| root)
            .unwrap_or_else(std::env::temp_dir));
    }
    resolve_root(state, workspace)
}

/// The set of (workspace_id, project_name, root) to enumerate for aggregate
/// endpoints. A pinned root yields one anonymous entry (test harness); the
/// global server yields every reachable project from the registry.
fn enumerate_workspaces(state: &AppState) -> Vec<(String, String, PathBuf)> {
    match &state.root {
        Some(r) => vec![(String::new(), String::new(), r.as_ref().clone())],
        None => projects::list_reachable()
            .into_iter()
            .map(|e| (e.workspace_id, e.name, PathBuf::from(e.path)))
            .collect(),
    }
}

/// Finds which project owns a given run, by locating `.apb/runs/<run_id>`
/// among the enumerated workspaces. Used where the caller cannot pass a
/// workspace (external webhooks).
fn find_run_root(state: &AppState, run_id: &str) -> Option<PathBuf> {
    enumerate_workspaces(state)
        .into_iter()
        .map(|(_, _, root)| root)
        .find(|root| root.join(".apb/runs").join(run_id).is_dir())
}

/// Serializes a playbook summary and tags it with its owning project, so the
/// global list can show playbook-to-project affiliation.
fn tag_summary(summary: &PlaybookSummary, workspace_id: &str, project: &str) -> serde_json::Value {
    let mut v = serde_json::to_value(summary).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("workspace_id".into(), serde_json::json!(workspace_id));
        obj.insert("project".into(), serde_json::json!(project));
    }
    v
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
        VersioningError::Frozen(what) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "frozen", "playbook": what })),
        )
            .into_response(),
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
    workspace: Option<String>,
}

#[derive(Deserialize)]
struct DiffQuery {
    from: String,
    to: String,
    workspace: Option<String>,
}

async fn create_playbook(
    State(state): State<AppState>,
    Query(q): Query<WsQuery>,
    Json(body): Json<CreatePlaybookBody>,
) -> impl IntoResponse {
    if !is_safe_id(&body.id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match create_version(&root, &body.id, &body.yaml, None, true) {
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
    Query(q): Query<WsQuery>,
    Json(body): Json<UpdatePlaybookBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let dir = root.join(".apb/playbooks").join(&id);
    if !dir.is_dir() {
        return (StatusCode::NOT_FOUND, format!("playbook `{id}` not found")).into_response();
    }
    match create_version(&root, &id, &body.yaml, None, true) {
        Ok(version) => Json(serde_json::json!({ "id": id, "version": version })).into_response(),
        Err(e) => versioning_error(e),
    }
}

#[derive(Deserialize)]
struct FrozenBody {
    frozen: bool,
}

/// PUT /api/playbooks/{id}/frozen: freeze or unfreeze a playbook. Freeze is an
/// operator action exposed only here (the dashboard button); agents have no
/// path to toggle it. A frozen playbook keeps running but refuses every
/// definition change.
async fn set_frozen_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
    Json(body): Json<FrozenBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match reg.set_frozen(&id, body.frozen) {
        Ok(()) => Json(serde_json::json!({ "id": id, "frozen": body.frozen })).into_response(),
        Err(RegistryError::NotFound(what)) => (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/playbooks/{id}/input-draft: the saved run "input prompt" draft for
/// this playbook (spec A), or `null` if none has been saved yet.
async fn get_input_draft_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match reg.read_instruction_draft(&id) {
        Ok(v) => Json(serde_json::json!({ "instruction": v })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct InputDraftBody {
    #[serde(default)]
    instruction: Option<String>,
}

/// PUT /api/playbooks/{id}/input-draft: stores (or, for an empty/absent
/// `instruction`, clears) the run input draft.
async fn put_input_draft_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
    Json(body): Json<InputDraftBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let text = body.instruction.unwrap_or_default();
    match reg.write_instruction_draft(&id, &text) {
        Ok(()) => {
            let out = if text.is_empty() { None } else { Some(text) };
            Json(serde_json::json!({ "instruction": out })).into_response()
        }
        Err(RegistryError::NotFound(what)) => (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize, Default)]
struct RunBody {
    #[serde(default)]
    instruction: Option<String>,
    #[serde(default)]
    params: std::collections::BTreeMap<String, String>,
}

/// POST /api/playbooks/{id}/run: starts an autonomous run in the background and
/// returns its run_id immediately, so the dashboard can jump straight to the
/// run view. Mirrors the CLI/MCP background-run path.
async fn run_playbook_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
    Json(body): Json<RunBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let opts = apb_engine::RunOptions {
        instruction: body.instruction,
        params: body.params,
        ..Default::default()
    };
    match apb_engine::run_background(&root, &id, None, opts) {
        Ok(run_id) => Json(serde_json::json!({ "run_id": run_id })).into_response(),
        Err(apb_engine::EngineError::NotFound(what)) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn delete_playbook_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match delete_playbook(&root, &id, apb_engine::event::now_millis()) {
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
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
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
    match save_layout(&root, &id, &q.version, &layout_yaml) {
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
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match version_diff(&root, &id, &q.from, &q.to) {
        Ok(diff) => Json(diff).into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn list_versions_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match list_versions_with_provenance(&root, &id) {
        Ok(infos) => Json(infos).into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn promote_version_handler(
    State(state): State<AppState>,
    AxPath((id, version)): AxPath<(String, String)>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) || !is_safe_id(&version) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match promote_version(&root, &id, &version) {
        Ok(()) => Json(serde_json::json!({ "promoted": version })).into_response(),
        Err(e) => versioning_error(e),
    }
}

/// Aggregated playbook list across every reachable project (global server) or
/// the single pinned root (test harness). Each entry is tagged with its owning
/// workspace_id and project name so the dashboard shows affiliation. A project
/// that fails to open is skipped rather than failing the whole list.
async fn list_playbooks(State(state): State<AppState>) -> impl IntoResponse {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for (workspace_id, project, root) in enumerate_workspaces(&state) {
        let Ok(reg) = Registry::open(&root) else {
            continue;
        };
        let Ok(list) = reg.list() else {
            continue;
        };
        for summary in &list {
            out.push(tag_summary(summary, &workspace_id, &project));
        }
    }
    Json(out).into_response()
}

/// GET /api/projects: the reachable projects the global dashboard aggregates.
async fn list_projects_handler() -> impl IntoResponse {
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

#[derive(Deserialize)]
struct DetailQuery {
    version: Option<String>,
    workspace: Option<String>,
}

async fn get_playbook(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<DetailQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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
                "frozen": reg.is_frozen(&id),
            }))
            .into_response()
        }
        Err(RegistryError::NotFound(what)) => (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn tag_profile(
    profile: &serde_json::Value,
    workspace_id: &str,
    project: &str,
) -> serde_json::Value {
    let mut v = profile.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("workspace_id".into(), serde_json::json!(workspace_id));
        obj.insert("project".into(), serde_json::json!(project));
    }
    v
}

/// GET /api/profiles: profiles with trust status. With `?workspace=<id>` it
/// returns that one project's profiles (used by the node executor selector);
/// without it, the global profiles page gets an aggregate across every
/// reachable project, each entry tagged with its owning project. Global-scope
/// profiles live in a single shared store, so they are emitted once (tagged as
/// the `global` project) rather than repeated per project.
async fn list_profiles(
    State(state): State<AppState>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    if let Some(ws) = q.workspace.as_deref() {
        let root = match resolve_root(&state, Some(ws)) {
            Ok(r) => r,
            Err(e) => return e,
        };
        return match apb_mcp::profile_tools::profile_list(&root) {
            Ok(v) => Json(v).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
    }

    let mut out: Vec<serde_json::Value> = Vec::new();

    // Global-scope profiles live in a single shared store read independently of
    // any project root, so emit them once up front through a root-independent
    // resolution. This keeps them visible even when no projects are reachable
    // (an empty workspace enumeration would otherwise drop them entirely).
    if let Ok(global_root) = resolve_root_for_scope(&state, None, "global")
        && let Ok(v) = apb_mcp::profile_tools::profile_list(&global_root)
        && let Some(arr) = v.get("profiles").and_then(|p| p.as_array())
    {
        for p in arr {
            if p.get("scope").and_then(|s| s.as_str()) == Some("global") {
                out.push(tag_profile(p, "", "global"));
            }
        }
    }

    // Each reachable project contributes only its project-scope profiles.
    for (workspace_id, project, root) in enumerate_workspaces(&state) {
        let Ok(v) = apb_mcp::profile_tools::profile_list(&root) else {
            continue;
        };
        let Some(arr) = v.get("profiles").and_then(|p| p.as_array()) else {
            continue;
        };
        for p in arr {
            if p.get("scope").and_then(|s| s.as_str()) != Some("global") {
                out.push(tag_profile(p, &workspace_id, &project));
            }
        }
    }
    Json(serde_json::json!({ "profiles": out })).into_response()
}

#[derive(Deserialize)]
struct ProfileRefQuery {
    workspace: Option<String>,
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    force: bool,
}

/// GET /api/profiles/{name}: one profile's full detail (yaml + SOUL + digest),
/// for the edit form. `scope` selects project vs global; `workspace` selects
/// the project for project scope.
async fn get_profile(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<ProfileRefQuery>,
) -> impl IntoResponse {
    let root = match resolve_root_for_scope(&state, q.workspace.as_deref(), &q.scope) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match apb_mcp::profile_tools::profile_get(&root, &name, &q.scope) {
        Ok(v) => Json(v).into_response(),
        Err(apb_mcp::tools::ToolError::NotFound(what)) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// DELETE /api/profiles/{name}: remove a profile. Refuses (409) if playbooks
/// still reference it unless `force=true`.
async fn delete_profile(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<ProfileRefQuery>,
) -> impl IntoResponse {
    let root = match resolve_root_for_scope(&state, q.workspace.as_deref(), &q.scope) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match apb_mcp::profile_tools::profile_delete(&root, &name, &q.scope, q.force) {
        Ok(v) => Json(v).into_response(),
        Err(apb_mcp::tools::ToolError::NotFound(what)) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Err(apb_mcp::tools::ToolError::Engine(detail)) => {
            // Referenced-by-playbooks (or another engine refusal) maps to 409 so
            // the client can offer a force delete.
            (StatusCode::CONFLICT, detail).into_response()
        }
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
    Query(q): Query<WsQuery>,
    Json(body): Json<ProfileWriteBody>,
) -> impl IntoResponse {
    use apb_mcp::profile_tools::{self, ExecutorInput, ProfileWrite};
    let root = match resolve_root_for_scope(&state, q.workspace.as_deref(), &body.scope) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let soul_requirement = match profile_tools::parse_soul_requirement(Some(&body.soul_requirement))
    {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let res = profile_tools::profile_write(
        &root,
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

/// GET /api/agents: agents detected on this machine (free detection, cached).
/// Machine-wide, so it needs no project root. Powers the profile form's agent
/// combobox.
async fn list_agents_handler() -> impl IntoResponse {
    Json(serde_json::json!({ "agents": apb_core::detect::detect(false) })).into_response()
}

/// GET /api/models: the curated models table (a hint, not a hard binding) plus
/// the claude static list. Machine-wide. Powers the model combobox.
async fn list_models_handler() -> impl IntoResponse {
    match apb_core::models_table::load_merged() {
        Ok(t) => Json(serde_json::json!({
            "as_of": t.as_of,
            "models": t.models,
            "claude_static": t.claude_static_models,
        }))
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/skills: skills a profile of the given `scope` could reference, from
/// the project and/or global skills directories. Powers the skills toggle list.
async fn list_skills_handler(
    State(state): State<AppState>,
    Query(q): Query<ProfileRefQuery>,
) -> impl IntoResponse {
    let root = match resolve_root_for_scope(&state, q.workspace.as_deref(), &q.scope) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let profile_scope = if q.scope == "global" {
        ProfileScope::Global
    } else {
        ProfileScope::Project
    };
    let skills = apb_core::skills::list_available(&root, profile_scope);
    Json(serde_json::json!({ "skills": skills })).into_response()
}

async fn list_runs_handler(State(state): State<AppState>) -> impl IntoResponse {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for (workspace_id, project, root) in enumerate_workspaces(&state) {
        let Ok(list) = apb_engine::list_runs(&root) else {
            continue;
        };
        for run in &list {
            let mut v = serde_json::to_value(run).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("workspace_id".into(), serde_json::json!(workspace_id));
                obj.insert("project".into(), serde_json::json!(project));
            }
            out.push(v);
        }
    }
    Json(out).into_response()
}

async fn get_run_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let run_dir = root.join(".apb/runs").join(&id);
    if !run_dir.is_dir() {
        return (StatusCode::NOT_FOUND, format!("run `{id}` not found")).into_response();
    }
    let events = match apb_engine::event::read_all(&run_dir) {
        Ok(ev) => ev,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let run_state = apb_engine::state::RunState::fold(&events);
    let cfg = apb_engine::run_config::read_run_config(&run_dir).unwrap_or_default();

    // The run's playbook snapshot (may be missing for very old runs). Kept in
    // scope because it also feeds the graph JSON and layout lookup below.
    let loaded_pb = apb_engine::progress::load_run_playbook(&run_dir);
    let (playbook_json, playbook_id, version) = match &loaded_pb {
        Some(playbook) => (
            serde_json::to_value(playbook).unwrap_or(serde_json::Value::Null),
            playbook.id.clone(),
            playbook.version.clone(),
        ),
        None => (serde_json::Value::Null, id.clone(), String::new()),
    };
    let progress = loaded_pb
        .as_ref()
        .map(|pb| apb_engine::progress::compute(pb, &events));

    // The saved graph layout for the run's playbook version, so the run view
    // shows the same node arrangement the author laid out in the editor rather
    // than a fresh auto-layout. Best-effort: an old/removed version simply has
    // no stored layout and the client falls back to auto-layout.
    let layout = Registry::open(&root)
        .ok()
        .filter(|_| !version.is_empty())
        .and_then(|reg| reg.load(&playbook_id, Some(&version)).ok())
        .and_then(|loaded| loaded.layout);

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
        "layout": layout,
        "hooks": hooks,
        "events": events,
        "progress": progress,
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
    Query(q): Query<WsQuery>,
    Json(body): Json<ReviewBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let run_dir = root.join(".apb/runs").join(&id);
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
    // Webhook callers cannot pass a workspace, so the owning project is found
    // by locating the run across reachable projects (run ids are unique).
    let Some(root) = find_run_root(&state, &run_id) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let run_dir = root.join(".apb/runs").join(&run_id);
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
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match apb_engine::supervisor_report_or_summary(&root, &id) {
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

/// Runs the global, machine-wide dashboard: one server, no project binding.
/// Playbooks and runs are aggregated across every reachable project in the
/// registry; project-specific requests carry `?workspace=<id>`. A single
/// instance lock lives in the config dir so two global dashboards cannot race
/// on the same port.
pub async fn run_server(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState::new_global();
    let cfg = apb_core::config::config_dir()
        .ok_or_else(|| std::io::Error::other("no config dir for the global server lock"))?;
    std::fs::create_dir_all(&cfg)?;
    // Bind the port BEFORE writing the lock file: the port bind is the real
    // mutual exclusion (a second server on the same port fails here), so if it
    // fails we must return without having written a lock that no cleanup path
    // would then remove.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let _lock = lock::write_global_lock(&cfg, port)?;
    // Real-time updates across all projects: a filesystem watcher broadcasts
    // change pings on the shared channel that the dashboard's WebSocket relays.
    // Best-effort: if it cannot start, the server still serves (the UI just
    // falls back to refetch-on-navigation).
    let _watcher = match watch::spawn_global_watcher(state.events.clone()) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("apb serve: real-time watcher unavailable: {e}");
            None
        }
    };
    let app = build_router(state);
    println!("apb serve (global): http://127.0.0.1:{port}");
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    // Remove the lock both on normal shutdown and after catching a signal.
    lock::remove_global_lock(&cfg)?;
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
