use crate::state::*;

use apb_core::profile::ProfileScope;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

pub(crate) fn tag_profile(
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
pub(crate) async fn list_profiles(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
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
pub(crate) struct ProfileRefQuery {
    workspace: Option<String>,
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    force: bool,
}

/// GET /api/profiles/{name}: one profile's full detail (yaml + SOUL + digest),
/// for the edit form. `scope` selects project vs global; `workspace` selects
/// the project for project scope.
pub(crate) async fn get_profile(
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
pub(crate) async fn delete_profile(
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
pub(crate) struct ProfileWriteBody {
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
pub(crate) struct FallbackBody {
    agent: String,
    model: String,
}

pub(crate) fn default_scope() -> String {
    "project".to_string()
}

pub(crate) fn default_soul_req() -> String {
    "any".to_string()
}

/// POST /api/profiles: create/update a profile through the same
/// profile_write logic (validation, CAS lock, auto-approve bundle). A CAS
/// conflict is 409; a validation error is 400.
pub(crate) async fn write_profile(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
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

/// GET /api/skills: skills a profile of the given `scope` could reference, from
/// the project and/or global skills directories. Powers the skills toggle list.
pub(crate) async fn list_skills_handler(
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
