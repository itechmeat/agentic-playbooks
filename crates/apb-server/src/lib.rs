pub mod lock;
pub mod watch;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use apb_core::connector::{config, secrets, store};
use apb_core::profile::ProfileScope;
use apb_core::projects::{self, ProjectAccessError};
use apb_core::registry::{PlaybookSummary, Registry, RegistryError};
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
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
        .route("/api/connectors", get(list_connectors_handler))
        .route("/api/connectors/approve", post(approve_connector_handler))
        .route(
            "/api/connectors/available",
            get(available_connectors_handler),
        )
        .route("/api/connectors/{name}", get(get_connector_handler))
        .route(
            "/api/connectors/{name}/install",
            post(install_connector_handler),
        )
        .route(
            "/api/connectors/{name}/uninstall",
            post(uninstall_connector_handler),
        )
        .route("/api/connectors/{name}/stats", get(connector_stats_handler))
        .route(
            "/api/connectors/{name}/healthcheck/{account}",
            post(healthcheck_connector_handler),
        )
        .route("/api/connectors/{name}/call", post(call_connector_handler))
        .route("/api/agents", get(list_agents_handler))
        .route("/api/models", get(list_models_handler))
        .route("/api/skills", get(list_skills_handler))
        .route("/api/runs", get(list_runs_handler))
        .route("/api/runs/{id}", get(get_run_handler))
        .route("/api/runs/{id}/report", get(get_run_report_handler))
        .route("/api/runs/{id}/review", post(post_review_handler))
        .route("/api/runs/{id}/answer", post(post_answer_handler))
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
        VersioningError::Validation(issues) => {
            let codes: Vec<&str> = issues.iter().map(|i| i.code).collect();
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "validation", "codes": codes })),
            )
                .into_response()
        }
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
    /// Run id to continue as a fresh run (issue #42 finding 10).
    #[serde(default)]
    continued_from: Option<String>,
}

/// POST /api/playbooks/{id}/run: starts an autonomous run in the background and
/// returns its run_id immediately, so the dashboard can jump straight to the
/// run view. Mirrors the CLI/MCP background-run path.
///
/// A connector-binding playbook additionally needs its two connector permit
/// maps computed server-side first (Task 15 review follow-up): the dashboard
/// has no MCP tool call in front of it to run `policy::check_run`, so without
/// this the engine would see empty `expected_connectors`/
/// `expected_connector_accounts` maps and refuse ANY connector-binding
/// playbook (a playbook that binds connectors is never permitted to run with
/// an empty permit - see `RunOptions::expected_connectors`). This reuses
/// `apb_mcp::policy::connector_permit_maps`, the exact same resolution and
/// trust gate `check_run` runs for its own connector step, rather than
/// duplicating that logic here.
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
    let mut opts = apb_engine::RunOptions {
        instruction: body.instruction,
        params: body.params,
        continued_from: body.continued_from,
        ..Default::default()
    };

    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match reg.load(&id, None) {
        Ok(loaded) => {
            let binds_connectors = loaded
                .playbook
                .nodes
                .iter()
                .any(|n| !n.kind.connector_bindings().is_empty());
            if binds_connectors {
                match apb_mcp::policy::connector_permit_maps(&root, &loaded.playbook) {
                    Ok((connectors, connector_accounts)) => {
                        opts.expected_connectors = connectors;
                        opts.expected_connector_accounts = connector_accounts;
                    }
                    Err(refusal) => return (StatusCode::CONFLICT, Json(refusal)).into_response(),
                }
            }
        }
        Err(RegistryError::NotFound(what)) => return (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }

    match apb_engine::run_background(&root, &id, None, opts) {
        Ok(run_id) => Json(serde_json::json!({ "run_id": run_id })).into_response(),
        Err(apb_engine::EngineError::NotFound(what)) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Err(apb_engine::EngineError::Conflict(what)) => {
            (StatusCode::CONFLICT, what).into_response()
        }
        // Client precondition failures (e.g. cross-playbook continued_from)
        // must not look like server faults.
        Err(apb_engine::EngineError::Invalid(what)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, what).into_response()
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

/// Trust status of a digest against the trust store: `"approved"` when the
/// current digest is approved, `"changed"` when some OTHER digest of the same
/// `id` was approved before (content moved since), else `"unapproved"`.
/// Shared by the connector-level and account-level trust fields below.
fn digest_trust_status(trust: &TrustStore, digest: &str, id: &str, kind: Kind) -> &'static str {
    if trust.is_approved(digest) {
        "approved"
    } else if trust.approved_record_ids(kind).iter().any(|x| x == id) {
        "changed"
    } else {
        "unapproved"
    }
}

/// The project roots a connector READ should merge account config from.
/// `Some(workspace)` is the strict single-project view and still errors on an
/// unknown, unreachable or malformed workspace; `None` is the machine-wide
/// view: every reachable project, which on a machine with no registered
/// project is legitimately empty (connectors themselves are installed
/// machine-wide, so an empty root set means "no per-project account config",
/// never "no connectors").
#[allow(clippy::result_large_err)]
fn connector_roots(state: &AppState, workspace: Option<&str>) -> Result<Vec<PathBuf>, Response> {
    match workspace {
        Some(ws) => Ok(vec![resolve_root(state, Some(ws))?]),
        None => Ok(enumerate_workspaces(state)
            .into_iter()
            .map(|(_, _, root)| root)
            .collect()),
    }
}

/// One connector's configured accounts across `roots`, paired with the root
/// they were read from (secret resolution is root-scoped). Keyed by account
/// name so the machine-wide view never lists the same account twice:
/// `config::load_merged` folds the shared global account store into every
/// project's, so a global account would otherwise reappear once per project.
/// First reachable project wins for a name configured in several, which keeps
/// the single-root case byte-identical to a plain `load_merged`.
///
/// A project whose account config fails to parse is skipped when aggregating
/// several roots; with exactly one root the error is returned so the strict
/// single-project view still surfaces it.
#[allow(clippy::result_large_err)]
fn merged_accounts(
    roots: &[PathBuf],
    name: &str,
) -> Result<Vec<(PathBuf, config::Account)>, apb_core::connector::ConnectorError> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out: Vec<(PathBuf, config::Account)> = Vec::new();
    for root in roots {
        let accounts = match config::load_merged(root, name) {
            Ok(a) => a,
            Err(e) if roots.len() == 1 => return Err(e),
            Err(_) => continue,
        };
        for a in accounts {
            if seen.insert(a.name.clone()) {
                out.push((root.clone(), a));
            }
        }
    }
    Ok(out)
}

/// GET /api/connectors: installed connectors with their storefront summary,
/// trust status, and account configuration readiness (spec 9). With
/// `?workspace=<id>` the account numbers describe that one project; without
/// it, the machine-wide connectors page gets an aggregate across every
/// reachable project. Connectors are installed machine-wide and their trust is
/// root-independent, so `store::list` is walked once and every connector
/// appears exactly once no matter how many projects are reachable - only the
/// account counts aggregate, and a machine with no reachable project simply
/// reports zero accounts instead of erroring.
///
/// `trust` is the connector's OWN digest trust (`approved` | `changed` |
/// `unapproved` | `invalid`); `accounts_ready` counts configured accounts
/// whose required secret env vars all currently resolve - a configuration
/// signal, not a trust signal (a ready account can still be untrusted, and
/// vice versa). `store::list` only parses `connector.yaml`, so a connector
/// that gets this far already has a manifest that parses; if `store::load`
/// still fails here (for example the whole-tree digest walk hits an escaping
/// symlink), the connector is fundamentally broken, not merely
/// un-trust-decided - report `invalid` rather than `unapproved` so the
/// dashboard can tell the two apart (spec 9's fourth trust state).
async fn list_connectors_handler(
    State(state): State<AppState>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    let roots = match connector_roots(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let trust = TrustStore::load();
    let mut out = Vec::new();
    for summary in store::list() {
        let loaded = store::load(&summary.name);
        let trust_state = match &loaded {
            Ok(l) => digest_trust_status(&trust, &l.digest, &summary.name, Kind::Connector),
            Err(_) => "invalid",
        };
        let accounts = merged_accounts(&roots, &summary.name).unwrap_or_default();
        let accounts_ready = match &loaded {
            Ok(l) => accounts
                .iter()
                .filter(|(root, a)| {
                    let vars: Vec<String> = config::env_refs(&l.doc, a).into_values().collect();
                    secrets::missing_vars(root, &vars).is_empty()
                })
                .count(),
            Err(_) => 0,
        };
        out.push(serde_json::json!({
            "name": summary.name,
            "version": summary.version,
            "display_name": summary.meta.display_name,
            "summary": summary.meta.summary,
            "tags": summary.meta.tags,
            "trust": trust_state,
            "accounts_total": accounts.len(),
            "accounts_ready": accounts_ready,
        }));
    }
    Json(out).into_response()
}

/// GET /api/connectors/available: the embedded official connectors that are
/// NOT currently installed, so the dashboard can offer them for connecting.
/// Each entry carries the same storefront fields the installed listing exposes
/// (`name`, `version`, `display_name`, `summary`, `tags`), read from the
/// embedded `PUBLIC.md` rather than from disk.
///
/// Like `GET /api/connectors`, this is machine-wide: the embedded set comes out
/// of the binary and the store is global, so no project root and therefore no
/// `?workspace=` parameter is involved at all.
async fn available_connectors_handler() -> impl IntoResponse {
    let installed: std::collections::BTreeSet<String> =
        store::list().into_iter().map(|s| s.name).collect();
    let out: Vec<serde_json::Value> = apb_core::connector::official::list()
        .into_iter()
        .filter(|o| !installed.contains(&o.name))
        .map(|o| {
            let meta = o.meta();
            serde_json::json!({
                "name": o.name,
                "version": o.version,
                "display_name": meta.display_name,
                "summary": meta.summary,
                "tags": meta.tags,
            })
        })
        .collect();
    Json(out).into_response()
}

/// Query params of the install endpoint. `force` overwrites a target that
/// already exists and differs from the embedded version; without it that case
/// is a 409 so the dashboard can ask the user before clobbering local edits.
#[derive(Deserialize, Default)]
struct ConnectorInstallQuery {
    #[serde(default)]
    force: Option<bool>,
}

/// Builds the `{ "error": ..., "detail": ... }` body every failing connector
/// lifecycle response carries. A JSON body in every case (never a bare string)
/// is what lets the dashboard render a specific message per failure instead of
/// parsing prose.
fn lifecycle_error(status: StatusCode, code: &str, detail: String) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": code, "detail": detail })),
    )
        .into_response()
}

/// POST /api/connectors/{name}/install: installs the embedded official
/// connector `name` into the global store and records its trust as `Bundled`,
/// through the same `apb_core::connector::install::install_official` the CLI
/// runs. Machine-wide like the store itself, so it needs no `?workspace=`.
///
/// 200 with `no_op: true` when the exact same tree digest is already installed
/// (a reinstall is idempotent, not an error), 400 for a name that is not a
/// valid slug, 404 when no embedded connector carries that name, 409 when a
/// DIFFERING version is already installed and `?force=true` was not passed, and
/// 500 when there is no config directory or a filesystem step fails.
async fn install_connector_handler(
    AxPath(name): AxPath<String>,
    Query(q): Query<ConnectorInstallQuery>,
) -> impl IntoResponse {
    match apb_core::connector::install::install_official(&name, q.force.unwrap_or(false)) {
        Ok(report) => Json(serde_json::json!({
            "ok": true,
            "name": report.name,
            "version": report.version,
            "digest": report.digest,
            "no_op": report.no_op,
            "trust_recorded": report.trust_warning.is_none(),
            "trust_warning": report.trust_warning,
        }))
        .into_response(),
        Err(e) => {
            use apb_core::connector::install::InstallError;
            let (status, code) = match &e {
                InstallError::InvalidName { .. } => (StatusCode::BAD_REQUEST, "invalid_name"),
                InstallError::NotEmbedded(_) => (StatusCode::NOT_FOUND, "not_found"),
                InstallError::NeedsForce { .. } => (StatusCode::CONFLICT, "needs_force"),
                InstallError::NoConfigDir => (StatusCode::INTERNAL_SERVER_ERROR, "no_config_dir"),
                InstallError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io_error"),
            };
            lifecycle_error(status, code, e.to_string())
        }
    }
}

/// POST /api/connectors/{name}/uninstall: removes `<config_dir>/connectors/
/// {name}/` and nothing else, through `apb_core::connector::install::uninstall`.
/// Account configuration lives in a separate `connector-config/` tree and is
/// deliberately left alone, so disconnecting keeps the user's accounts and
/// reconnecting picks them straight back up; the trust record is left in place
/// for the same reason (a reinstall of the same version digests identically).
///
/// 200 with `no_op: true` when the connector was not installed to begin with
/// (removing what is already gone is what the caller asked for), 400 for a name
/// that is not a valid slug, and 500 when there is no config directory or the
/// directory cannot be removed. There is no 404: an absent connector is a
/// successful no-op, not a missing resource.
async fn uninstall_connector_handler(AxPath(name): AxPath<String>) -> impl IntoResponse {
    match apb_core::connector::install::uninstall(&name) {
        Ok(report) => Json(serde_json::json!({
            "ok": true,
            "name": report.name,
            "no_op": report.no_op,
        }))
        .into_response(),
        Err(e) => {
            use apb_core::connector::install::UninstallError;
            let (status, code) = match &e {
                UninstallError::InvalidName { .. } => (StatusCode::BAD_REQUEST, "invalid_name"),
                UninstallError::NoConfigDir => (StatusCode::INTERNAL_SERVER_ERROR, "no_config_dir"),
                UninstallError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io_error"),
            };
            lifecycle_error(status, code, e.to_string())
        }
    }
}

/// Runs scanned per `GET /api/connectors/{name}/stats` call, most recent
/// first (spec 9's usage-stats bullet: "aggregated from existing run event
/// logs", read-only, no new engine state). Unbounded history scanning would
/// make this endpoint cost grow with the whole project's lifetime, so it is
/// capped to the latest N runs by start time - the same ordering
/// `apb_engine::list_runs` already sorts by. `runs_scanned` in the response
/// reports how many were actually read, so a caller can tell a small number
/// apart from "there were more but we capped".
const STATS_RUN_CAP: usize = 50;

/// Running totals of one connector's `ConnectorCall` events, summed over one
/// or more project roots. Kept as a struct so the per-root scan is a single
/// method the handler calls in a loop, rather than five mutable locals threaded
/// through it.
#[derive(Default)]
struct ConnectorStatsAcc {
    /// (function, account) -> (calls, errors, total_duration_ms). A BTreeMap
    /// keeps the response's `by_function` order deterministic across runs, and
    /// across roots: the same function/account pair used in two projects sums
    /// into one row.
    by_fn: std::collections::BTreeMap<(String, String), (u64, u64, u64)>,
    by_outcome: std::collections::BTreeMap<String, u64>,
    total_calls: u64,
    runs_scanned: u64,
}

impl ConnectorStatsAcc {
    /// Folds the most recent `STATS_RUN_CAP` runs of one project root into the
    /// totals. The cap is per root, so the machine-wide view scans at most that
    /// many runs from each project rather than truncating older projects away.
    /// A run whose event log cannot be read is skipped.
    fn scan_root(&mut self, root: &Path, name: &str) -> Result<(), apb_engine::EngineError> {
        let runs = apb_engine::list_runs(root)?;
        let runs_dir = root.join(".apb/runs");
        for run in runs.iter().take(STATS_RUN_CAP) {
            self.runs_scanned += 1;
            let Ok(events) = apb_engine::event::read_all(&runs_dir.join(&run.run_id)) else {
                continue;
            };
            for event in &events {
                let apb_engine::event::EventPayload::ConnectorCall {
                    connector,
                    function,
                    account,
                    outcome,
                    duration_ms,
                    ..
                } = &event.payload
                else {
                    continue;
                };
                if connector != name {
                    continue;
                }
                self.total_calls += 1;
                let entry = self
                    .by_fn
                    .entry((function.clone(), account.clone()))
                    .or_insert((0, 0, 0));
                entry.0 += 1;
                if outcome != "ok" {
                    entry.1 += 1;
                }
                entry.2 += duration_ms;
                *self.by_outcome.entry(outcome.clone()).or_insert(0) += 1;
            }
        }
        Ok(())
    }
}

/// GET /api/connectors/{name}/stats: usage stats for one connector,
/// aggregated by scanning the `ConnectorCall` events (`apb-engine`'s
/// `event.rs`) of the most recent `STATS_RUN_CAP` runs (spec 9). Calls, error
/// rate, and duration are broken down per function/account pair as well as
/// summed as `by_outcome`. Purely read-only: no engine state is written, and
/// `ConnectorCall` events never carry request/response bodies or secrets by
/// construction (`event.rs`), so this cannot leak anything the run log itself
/// does not already hold.
///
/// Scoped like the list and detail endpoints: `?workspace=<id>` is the strict
/// single-project view and still 500s when that project's runs cannot be
/// listed, while without it (the connector page is machine-wide and pins no
/// project) the totals are the sum across every reachable project, and a
/// project whose run log cannot be read is skipped rather than failing the
/// whole request. A connector with no recorded calls - including one that is
/// not installed - is an empty result and a 200, never an error.
async fn connector_stats_handler(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    let roots = match connector_roots(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let strict = q.workspace.is_some();
    let mut acc = ConnectorStatsAcc::default();
    for root in &roots {
        match acc.scan_root(root, &name) {
            Ok(()) => {}
            Err(e) if strict => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
            Err(_) => continue,
        }
    }

    let ConnectorStatsAcc {
        by_fn,
        by_outcome,
        total_calls,
        runs_scanned,
    } = acc;

    let by_function: Vec<serde_json::Value> = by_fn
        .into_iter()
        .map(
            |((function, account), (calls, errors, total_duration_ms))| {
                let avg_duration_ms = if calls > 0 {
                    total_duration_ms as f64 / calls as f64
                } else {
                    0.0
                };
                serde_json::json!({
                    "function": function,
                    "account": account,
                    "calls": calls,
                    "errors": errors,
                    "avg_duration_ms": avg_duration_ms,
                })
            },
        )
        .collect();

    Json(serde_json::json!({
        "connector": name,
        "runs_scanned": runs_scanned,
        "calls": total_calls,
        "by_function": by_function,
        "by_outcome": by_outcome,
    }))
    .into_response()
}

/// The installation-independent half of the connector detail response: the
/// parsed manifest plus the storefront page. It comes off disk for an
/// installed connector and straight out of the binary for an embedded official
/// one that is merely not installed, which is why the two are resolved
/// together here rather than at each use site.
struct ConnectorPublic {
    doc: apb_core::connector::def::ConnectorDoc,
    meta: store::PublicMeta,
    body_md: String,
}

/// The installation state of a connector, resolved alongside its public half.
/// `Invalid` is kept distinct from `NotInstalled` so a broken installed
/// connector (its `store::load` fails with a non-`NotFound` error) surfaces as
/// installed with trust `invalid`, rather than masquerading as a
/// not-yet-connected embedded connector. That keeps the detail endpoint in step
/// with the list endpoint, which already reports `invalid` for the very same
/// connector.
///
/// The loaded connector is boxed to keep the enum small (the parseable variant
/// would otherwise dwarf the two unit variants).
enum InstallState {
    /// Installed and parseable; trust comes from the tree digest.
    Installed(Box<store::LoadedConnector>),
    /// Not installed; the public half is served from the embedded official
    /// manifest.
    NotInstalled,
    /// Installed but broken (unparseable connector.yaml, digest or containment
    /// error): reported as installed with trust `invalid`.
    Invalid,
}

/// The public half of an embedded official connector, or `None` when `name` is
/// not embedded. (Its embedded manifest failing to parse cannot happen for the
/// shipped set, but is handled as `None` all the same.)
fn embedded_public(name: &str) -> Option<ConnectorPublic> {
    let embedded = apb_core::connector::official::get(name)?;
    Some(ConnectorPublic {
        doc: embedded.doc()?,
        meta: embedded.meta(),
        body_md: embedded.public_body(),
    })
}

/// The manifest and storefront page of `name`, plus how it is installed. `None`
/// for a name that is neither installed nor embedded - the only case the detail
/// endpoint answers with a 404.
///
/// Not being installed is a state, not an error: every official connector's
/// manifest is baked into this binary and none of it is private, so the public
/// half is served either way and only the on-disk `LoadedConnector` is absent.
///
/// A connector whose `store::load` fails with a non-`NotFound` error IS
/// installed, just broken (unparseable connector.yaml, a digest or containment
/// error). It must not be swallowed the way `NotFound` is and reported as a
/// connectable embedded connector: that would disagree with the list endpoint,
/// which already reports the same connector's trust as `invalid`. It resolves
/// to `InstallState::Invalid` instead, with the storefront half filled from the
/// embedded manifest when the connector is official, so the two views agree.
fn connector_public(name: &str) -> Option<(ConnectorPublic, InstallState)> {
    match store::load(name) {
        Ok(loaded) => {
            let public = ConnectorPublic {
                doc: loaded.doc.clone(),
                meta: store::public_meta(&loaded.dir),
                body_md: store::public_body(&loaded.dir),
            };
            Some((public, InstallState::Installed(Box::new(loaded))))
        }
        // Not installed: nothing on disk. Fall back to the embedded official
        // manifest so the storefront can show what the connector does before it
        // is connected. A name that is neither installed nor embedded is the
        // one case that 404s (the `?` below).
        Err(apb_core::connector::ConnectorError::NotFound(_)) => {
            Some((embedded_public(name)?, InstallState::NotInstalled))
        }
        // Installed but broken: report installed + `invalid`, mirroring the
        // list endpoint. Serve the storefront half from the embedded manifest
        // when this is an official connector, else a minimal one so the
        // endpoint still answers 200 with an honest invalid state.
        Err(_) => {
            let public = embedded_public(name).unwrap_or_else(|| ConnectorPublic {
                doc: apb_core::connector::def::ConnectorDoc {
                    name: name.to_string(),
                    version: String::new(),
                    healthcheck: None,
                    auth: None,
                    account_fields: Vec::new(),
                    functions: Vec::new(),
                },
                meta: store::public_meta_from_str("", name),
                body_md: String::new(),
            });
            Some((public, InstallState::Invalid))
        }
    }
}

/// GET /api/connectors/{name}: the manifest (functions, account fields),
/// storefront body, and the merged account list with non-secret fields,
/// missing env var NAMES (never values), and per-account trust status (spec
/// 9). `missing_env` never carries a value, only the env var name.
///
/// With `?workspace=<id>` the account rows are that one project's; without it
/// (the machine-wide connectors page links to a connector without pinning a
/// project) the connector identity, manifest and trust are read the same way,
/// since all three are root-independent, and the account rows are the union
/// across every reachable project, each account listed once. A machine with no
/// reachable project still gets the connector, with an empty account list.
///
/// A connector that is not installed but IS embedded answers with the same
/// shape and `installed: false`, so the dashboard can show what a connector
/// does before the user connects it. Account rows are still real there:
/// account config lives in a separate `connector-config/` tree that survives
/// (and precedes) installation. `trust` describes bytes on disk that do not
/// exist yet, so it reports its own `not_installed` state rather than
/// borrowing `unapproved`, which would read as a trust decision nobody made.
/// 404 is reserved for a name that is neither installed nor embedded.
async fn get_connector_handler(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    let roots = match connector_roots(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let Some((public, install)) = connector_public(&name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("connector `{name}` is not installed and is not an official connector"),
        )
            .into_response();
    };
    let accounts_cfg = match merged_accounts(&roots, &name) {
        Ok(a) => a,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let trust = TrustStore::load();
    let secret_fields = public.doc.secret_fields();

    let functions: Vec<serde_json::Value> = public
        .doc
        .functions
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "description": f.description,
                "read_only": f.read_only,
                "deprecated": f.deprecated,
                "args_schema": f.args_schema,
            })
        })
        .collect();

    let accounts: Vec<serde_json::Value> = accounts_cfg
        .iter()
        .map(|(root, a)| {
            let vars: Vec<String> = config::env_refs(&public.doc, a).into_values().collect();
            let missing_env = secrets::missing_vars(root, &vars);
            // Non-secret fields only: a secret field's config value is the raw
            // `{{env.VAR}}` reference, not the value itself, but the detail
            // endpoint must never surface anything secret-shaped, even by proxy.
            let fields: serde_json::Map<String, serde_json::Value> = a
                .fields
                .iter()
                .filter(|(k, _)| !secret_fields.iter().any(|s| s == *k))
                .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect();
            let digest = config::account_digest(a);
            let id = account_trust_id(&name, &a.name);
            let acct_trust = digest_trust_status(&trust, &digest, &id, Kind::ConnectorAccount);
            serde_json::json!({
                "name": a.name,
                "default": a.default,
                "fields": serde_json::Value::Object(fields),
                "missing_env": missing_env,
                "trust": acct_trust,
            })
        })
        .collect();

    let connector_trust = match &install {
        InstallState::Installed(l) => {
            digest_trust_status(&trust, &l.digest, &name, Kind::Connector)
        }
        InstallState::NotInstalled => "not_installed",
        InstallState::Invalid => "invalid",
    };
    // On disk either way (parseable or broken); only `NotInstalled` is absent.
    let installed = !matches!(install, InstallState::NotInstalled);

    Json(serde_json::json!({
        "name": name,
        "version": public.doc.version,
        "installed": installed,
        "trust": connector_trust,
        "meta": public.meta,
        "body_md": public.body_md,
        "functions": functions,
        "accounts": accounts,
    }))
    .into_response()
}

/// POST /api/connectors/{name}/healthcheck/{account}: runs the connector's
/// declared healthcheck function LIVE (spec 9's dashboard probe button) and
/// returns the call executor's JSON verbatim. A mock healthcheck needs no
/// network; an HTTP healthcheck actually reaches the URL - that live
/// reachability probe is the point of the button.
async fn healthcheck_connector_handler(
    State(state): State<AppState>,
    AxPath((name, account)): AxPath<(String, String)>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let (value, _ok) = apb_engine::connector_call::healthcheck(&root, &name, &account);
    Json(value).into_response()
}

#[derive(Deserialize)]
struct ConnectorCallBody {
    function: String,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    args: serde_json::Value,
    #[serde(default)]
    dry_run: bool,
    /// `--full` bypasses the function's `response_pick` projection (spec 4.5
    /// / 2026-07-19-official-connectors-design section 7 post-review fix);
    /// omitted or `false` (the default) applies the projection like a
    /// normal agent call.
    #[serde(default)]
    full: bool,
}

/// POST /api/connectors/{name}/call: the dashboard playground's manual call
/// (spec 2026-07-19-official-connectors-design section 7). Wraps the same
/// live execution path the healthcheck probe uses
/// (`apb_engine::connector_call::play_call`), extended with an arbitrary
/// function name, args, a dry-run flag, and a `full` flag. Like the
/// healthcheck probe, the server answers HTTP 200 even for a refused or
/// failed call - the outcome is carried in the body's `ok`/`error`, never as
/// an HTTP error status. Account defaulting (an omitted or null `account`)
/// is resolved inside `play_call`, mirroring the CLI's single-or-default
/// selection rule.
async fn call_connector_handler(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<WsQuery>,
    Json(body): Json<ConnectorCallBody>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    // An absent/null `args` in the request body deserializes to
    // `Value::Null`; the executor's schema validation and template
    // rendering both expect an object, so normalize here rather than push
    // that concern into the engine.
    let args = if body.args.is_null() {
        serde_json::json!({})
    } else {
        body.args
    };
    let (value, _ok) = apb_engine::connector_call::play_call(
        &root,
        &name,
        body.account.as_deref(),
        &body.function,
        &args,
        body.dry_run,
        body.full,
    );
    Json(value).into_response()
}

#[derive(Deserialize)]
struct ConnectorApproveBody {
    name: String,
    #[serde(default)]
    account: Option<String>,
}

/// POST /api/connectors/approve: approves the connector's current tree
/// digest, or (with `account` set) that account's current non-secret-field
/// digest instead - the dashboard's approve flow for the trust gate that
/// guards secret egress (spec 7/9). Mirrors `apb connector approve`.
async fn approve_connector_handler(
    State(state): State<AppState>,
    Query(q): Query<WsQuery>,
    Json(body): Json<ConnectorApproveBody>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let loaded = match store::load(&body.name) {
        Ok(l) => l,
        Err(e) => return (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    };
    let mut trust = TrustStore::load();
    match body.account.as_deref() {
        None => {
            if let Err(e) = trust.approve_kind(
                &loaded.digest,
                &body.name,
                Kind::Connector,
                OriginKind::LocallyApproved,
            ) {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
        Some(acct_name) => {
            let accounts = match config::load_merged(&root, &body.name) {
                Ok(a) => a,
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                }
            };
            let Some(account) = accounts.iter().find(|a| a.name == acct_name) else {
                return (
                    StatusCode::NOT_FOUND,
                    format!("account `{acct_name}` not configured for `{}`", body.name),
                )
                    .into_response();
            };
            let digest = config::account_digest(account);
            let id = account_trust_id(&body.name, acct_name);
            if let Err(e) = trust.approve_kind(
                &digest,
                &id,
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            ) {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// GET /api/agents: agents detected on this machine (free detection, cached).
/// Machine-wide, so it needs no project root. Powers the profile form's agent
/// combobox.
async fn list_agents_handler() -> impl IntoResponse {
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
async fn list_models_handler() -> impl IntoResponse {
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
    let progress = apb_engine::progress::from_run_dir(&run_dir, &events);
    let answer = apb_engine::progress::run_answer(&run_dir, &events);

    // Child runs started from this run (spec review R1-I6): mirrors MCP
    // `run_status`'s pattern exactly - one entry per `ChildRunStarted` event,
    // with the child's current status folded from its own run dir. An
    // unreadable child event log (deleted/corrupt run dir) reports `"unknown"`
    // rather than failing the parent's detail read.
    let children: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| match &e.payload {
            apb_engine::event::EventPayload::ChildRunStarted { node_id, run_id } => {
                let child_dir = run_dir.parent().map(|p| p.join(run_id));
                let status = child_dir
                    .and_then(|d| apb_engine::event::read_all(&d).ok())
                    .map(|ev| {
                        apb_engine::state::RunState::fold(&ev)
                            .run_status
                            .as_str()
                            .to_string()
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                Some(serde_json::json!({ "node_id": node_id, "run_id": run_id, "status": status }))
            }
            _ => None,
        })
        .collect();

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
        "answer": answer,
        "children": children,
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

/// Body of `POST /api/runs/{id}/answer`: `node` is the interactive node to
/// answer, defaulting (when omitted) to the single node with a pending
/// question, exactly like `apb_engine::post_answer`'s own `node: Option<&str>`
/// resolution (spec 2026-07-20-interactive-nodes).
#[derive(Deserialize)]
struct AnswerBody {
    #[serde(default)]
    node: Option<String>,
    answer: String,
}

/// POST /api/runs/{id}/answer: the web facade for answering an interactive
/// `agent_task` node's pending question, always posted as `answered_by:
/// "human"` (the dashboard is a human-facing surface; a supervisor answers
/// through its own MCP tool instead). Delegates to `apb_engine::post_answer`,
/// which owns the `answer_by` policy and the pending-node resolution, so this
/// handler mirrors `post_review_handler` exactly: on failure the engine
/// error's message (including the policy's relay-instruction text) is
/// surfaced verbatim as the response body.
async fn post_answer_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
    Json(body): Json<AnswerBody>,
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
    match apb_engine::post_answer(&run_dir, body.node.as_deref(), &body.answer, "human") {
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
            eprintln!("apb dashboard: real-time watcher unavailable: {e}");
            None
        }
    };
    let app = build_router(state);
    println!("apb dashboard (global): http://127.0.0.1:{port}");
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
