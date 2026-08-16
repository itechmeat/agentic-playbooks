use std::path::PathBuf;
use std::sync::Arc;

use apb_core::projects::{self, ProjectAccessError};
use apb_core::versioning::VersioningError;
use axum::extract::FromRef;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
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
    /// Server-mode authentication (spec 2026-08-16-server-mode-design).
    /// Disabled by default, which is exactly today's local behavior;
    /// `run_server` attaches a populated state when keys exist.
    pub auth: Arc<crate::auth::AuthState>,
}

impl AppState {
    /// Pinned to a single project root (test harness / backward-compat).
    pub fn new(root: PathBuf) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root: Some(Arc::new(root)),
            events,
            auth: Arc::new(crate::auth::AuthState::disabled()),
        }
    }

    /// The global, machine-wide dashboard: no pinned root, projects resolved
    /// per request from the registry.
    pub fn new_global() -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root: None,
            events,
            auth: Arc::new(crate::auth::AuthState::disabled()),
        }
    }

    /// The global dashboard with server-mode auth attached.
    pub fn new_global_with_auth(auth: Arc<crate::auth::AuthState>) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root: None,
            events,
            auth,
        }
    }

    /// Attaches an auth state to an existing one. The shape the tests use.
    pub fn with_auth(mut self, auth: Arc<crate::auth::AuthState>) -> Self {
        self.auth = auth;
        self
    }
}

/// Lets an axum handler extract just the auth substate (`State<Arc<AuthState>>`)
/// instead of the whole `AppState`. This is what keeps `crate::auth` from
/// needing to import `AppState` back: the dependency only runs one way,
/// `state -> auth`, so the two modules do not cycle.
impl FromRef<AppState> for Arc<crate::auth::AuthState> {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

// Run/playbook identifier: always a single name segment.
// Reject anything that could escape the directory or name one instead of a
// segment (`/`, `\`, `..`, a bare `.`, empty).
pub(crate) fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id != "." && !id.contains('/') && !id.contains('\\') && !id.contains("..")
}

/// Resolves the project root for a request: an explicit `?workspace=<id>` wins
/// (resolved through the registry, with identity binding); otherwise the
/// pinned root is used (test harness). The global server has no pinned root, so
/// omitting `workspace` there is a 400.
///
/// The error is a ready-to-return HTTP `Response` (the natural shape for a
/// request helper), so the large-Err lint does not apply here.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_root(state: &AppState, workspace: Option<&str>) -> Result<PathBuf, Response> {
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
pub(crate) fn resolve_root_for_scope(
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
pub(crate) fn enumerate_workspaces(state: &AppState) -> Vec<(String, String, PathBuf)> {
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
pub(crate) fn find_run_root(state: &AppState, run_id: &str) -> Option<PathBuf> {
    enumerate_workspaces(state)
        .into_iter()
        .map(|(_, _, root)| root)
        .find(|root| root.join(".apb/runs").join(run_id).is_dir())
}

pub(crate) fn versioning_error(e: VersioningError) -> Response {
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

/// Query param carrying the target project for a project-specific request.
#[derive(Deserialize, Default)]
pub(crate) struct WorkspaceQuery {
    pub(crate) workspace: Option<String>,
}
