use crate::state::*;

use apb_core::registry::{PlaybookSummary, Registry, RegistryError};
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_core::versioning::{
    create_version, delete_playbook, list_versions_with_provenance, promote_version, save_layout,
    version_diff,
};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

/// Serializes a playbook summary and tags it with its owning project, so the
/// global list can show playbook-to-project affiliation.
pub(crate) fn tag_summary(
    summary: &PlaybookSummary,
    workspace_id: &str,
    project: &str,
) -> serde_json::Value {
    let mut v = serde_json::to_value(summary).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("workspace_id".into(), serde_json::json!(workspace_id));
        obj.insert("project".into(), serde_json::json!(project));
    }
    v
}

#[derive(Deserialize)]
pub(crate) struct CreatePlaybookBody {
    id: String,
    yaml: String,
}

#[derive(Deserialize)]
pub(crate) struct UpdatePlaybookBody {
    yaml: String,
}

#[derive(Deserialize)]
pub(crate) struct LayoutBody {
    /// YAML string or JSON layout value (coerced to YAML).
    layout: serde_json::Value,
}

#[derive(Deserialize)]
pub(crate) struct LayoutQuery {
    version: String,
    workspace: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DiffQuery {
    from: String,
    to: String,
    workspace: Option<String>,
}

pub(crate) async fn create_playbook(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
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

pub(crate) async fn update_playbook(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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
pub(crate) struct FrozenBody {
    frozen: bool,
}

/// PUT /api/playbooks/{id}/frozen: freeze or unfreeze a playbook. Freeze is an
/// operator action exposed only here (the dashboard button); agents have no
/// path to toggle it. A frozen playbook keeps running but refuses every
/// definition change.
pub(crate) async fn set_frozen_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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
pub(crate) async fn get_input_draft_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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
pub(crate) struct InputDraftBody {
    #[serde(default)]
    instruction: Option<String>,
}

/// PUT /api/playbooks/{id}/input-draft: stores (or, for an empty/absent
/// `instruction`, clears) the run input draft.
pub(crate) async fn put_input_draft_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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
pub(crate) struct RunBody {
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
/// an empty permit - see `RunOptions::expected_connectors`). The same is true
/// one level down (issue #102.1): a `type: playbook` child is spawned with the
/// permit maps its pin carries, so a parent started without pins spawned a
/// connector-binding child with empty maps and the child died fail-closed.
/// Both are computed in ONE pass by
/// `apb_mcp::policy::connector_permit_maps_with_children`, the exact same
/// resolution and trust gate `check_run` runs for its own connector and
/// children steps, rather than duplicating either walk here (anti-TOCTOU: the
/// maps handed to the engine are exactly the maps that were verified).
pub(crate) async fn run_playbook_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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
            // The gate is cheap and a no-op for a playbook that binds no
            // connector and delegates to no sub-playbook (both walks return
            // empty), so it runs unconditionally rather than behind a
            // hand-rolled "does it bind anything" pre-check that would have to
            // stay in sync with the walks.
            match apb_mcp::policy::connector_permit_maps_with_children(
                &root,
                &loaded.playbook,
                &apb_core::scope::Origin::Project { workspace_id: None },
                &id,
            ) {
                Ok(((connectors, connector_accounts), children)) => {
                    opts.expected_connectors = connectors;
                    opts.expected_connector_accounts = connector_accounts;
                    // No sub-playbook node means no pin to carry: keep `None`
                    // so nothing changes for the (vast majority) of playbooks
                    // without children.
                    opts.expected_children = (!children.is_empty()).then_some(children);
                }
                Err(refusal) => return (StatusCode::CONFLICT, Json(refusal)).into_response(),
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

pub(crate) async fn delete_playbook_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match delete_playbook(&root, &id, apb_core::clock::now_ms()) {
        Ok(trashed) => Json(serde_json::json!({
            "trashed": trashed.to_string_lossy(),
        }))
        .into_response(),
        Err(e) => versioning_error(e),
    }
}

pub(crate) async fn put_layout(
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

pub(crate) async fn get_diff(
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

pub(crate) async fn list_versions_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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

pub(crate) async fn promote_version_handler(
    State(state): State<AppState>,
    AxPath((id, version)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
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
pub(crate) async fn list_playbooks(State(state): State<AppState>) -> impl IntoResponse {
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

#[derive(Deserialize)]
pub(crate) struct DetailQuery {
    version: Option<String>,
    workspace: Option<String>,
}

pub(crate) async fn get_playbook(
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
