//! Moving a playbook's `current` from the dashboard, through the one move
//! of `current` every surface uses (`apb_core::versioning::current`):
//!
//! - `POST /api/playbooks/{id}/current` (`{ version, expected_current,
//!   reason? }`): a person makes `version` current. 200 answers what moved
//!   (`audit_pending` when its record finishes later); 409 answers `stale`
//!   (with `actual`, the value `current` holds now), `frozen`,
//!   `is_candidate` or `record_unreadable`; 404 an unknown playbook or
//!   version; 400 `invalid` a body of the wrong shape.
//!   `expected_current` is required: `null` means the person saw it unset.
//! - `GET /api/playbooks/{id}/current-history` (`?limit=`): the moves,
//!   newest first, with `incomplete` and `pending`.
//!
//! A move is refused (409 `run_step`) when the server itself runs as a
//! step of a run: only a person moves `current` by hand.

use apb_core::step_env::StepCaller;
use apb_core::versioning::current::{
    History, MoveCurrent, MoveError, Moved, MovedBy, Surface, history, move_current,
};
use apb_core::versioning::playbooks_dir;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::*;

/// History lines a request gets by default, and at most.
const HISTORY_DEFAULT: usize = 50;
const HISTORY_MAX: usize = 500;

/// Body of `POST /api/playbooks/{id}/current`.
#[derive(Deserialize)]
pub(crate) struct MoveBody {
    version: String,
    /// The version the person saw as current, `null` when it was unset.
    /// Required: a body without it is refused, never read as "unset".
    #[serde(deserialize_with = "nullable")]
    expected_current: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

fn nullable<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}

#[derive(Deserialize, Default)]
pub(crate) struct HistoryQuery {
    workspace: Option<String>,
    limit: Option<usize>,
}

pub(crate) async fn post_current_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    body: Result<Json<MoveBody>, JsonRejection>,
) -> Response {
    // A body of the wrong shape answers in the route's own JSON; a
    // transport problem (no JSON content type, a body too large) keeps its
    // own answer.
    let body = match body {
        Ok(Json(b)) => b,
        Err(e @ (JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_))) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid",
                    "message": format!(
                        "the body must be {{ \"version\", \"expected_current\" (a version or null), \"reason\"? }}: {}",
                        e.body_text()
                    ),
                })),
            )
                .into_response();
        }
        Err(other) => return other.into_response(),
    };
    by_person(
        &state,
        &id,
        q.workspace.as_deref(),
        PersonMove {
            to: body.version,
            expected: Some(body.expected_current),
            reason: body.reason,
        },
    )
    .await
    .map_or_else(|e| e, |moved| Json(moved).into_response())
}

/// The old `POST /api/playbooks/{id}/versions/{version}/promote`.
/// Deprecated: it moves `current` with no expectation of what it held, so
/// it can undo a move it never saw. Use `POST .../current`.
pub(crate) async fn post_promote_version_handler(
    State(state): State<AppState>,
    AxPath((id, version)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let mv = PersonMove {
        to: version.clone(),
        expected: None,
        reason: None,
    };
    by_person(&state, &id, q.workspace.as_deref(), mv)
        .await
        .map_or_else(
            |e| e,
            |moved| {
                Json(serde_json::json!({ "promoted": version, "moved": moved })).into_response()
            },
        )
}

pub(crate) async fn get_history_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(HISTORY_DEFAULT).min(HISTORY_MAX);
    let dir = match playbook_dir(&state, &id, q.workspace.as_deref()) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match tokio::task::spawn_blocking(move || history(&dir, limit)).await {
        Ok(Ok(h)) => Json::<History>(h).into_response(),
        Ok(Err(e)) => move_error(e),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// What a person asked the dashboard to move.
struct PersonMove {
    to: String,
    /// `None`: no expectation (the deprecated route); `Some(None)`: the
    /// person saw `current` unset.
    expected: Option<Option<String>>,
    reason: Option<String>,
}

/// A person's move on the dashboard. The error is the answer itself, like
/// `resolve_root`'s.
#[allow(clippy::result_large_err)]
async fn by_person(
    state: &AppState,
    id: &str,
    workspace: Option<&str>,
    mv: PersonMove,
) -> Result<Moved, Response> {
    let dir = playbook_dir(state, id, workspace)?;
    if let Some(why) = StepCaller::from_env().refusal("moving `current`") {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "run_step", "message": why })),
        )
            .into_response());
    }
    let moved = tokio::task::spawn_blocking(move || {
        let mut req = MoveCurrent::new(
            &mv.to,
            MovedBy::Person {
                surface: Surface::Dashboard,
            },
        );
        req.expected_from = mv.expected.as_ref().map(Option::as_deref);
        req.reason = mv.reason.as_deref();
        move_current(&dir, &req)
    })
    .await;
    match moved {
        Ok(Ok(m)) => Ok(m),
        Ok(Err(e)) => Err(move_error(e)),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    }
}

#[allow(clippy::result_large_err)]
fn playbook_dir(
    state: &AppState,
    id: &str,
    workspace: Option<&str>,
) -> Result<std::path::PathBuf, Response> {
    if !is_safe_id(id) {
        return Err((StatusCode::NOT_FOUND, "not found").into_response());
    }
    Ok(playbooks_dir(&resolve_root(state, workspace)?).join(id))
}

/// The answer to a refused move: 404 for what is not there, 409 for a
/// conflict a person resolves (`stale`, `frozen`, `is_candidate`).
pub(crate) fn move_error(e: MoveError) -> Response {
    let status = match &e {
        MoveError::NotFound(_) => StatusCode::NOT_FOUND,
        MoveError::Stale { .. }
        | MoveError::Frozen
        | MoveError::IsCandidate { .. }
        | MoveError::ExternalDataRun { .. } => StatusCode::CONFLICT,
        MoveError::ReadOnly => StatusCode::FORBIDDEN,
        MoveError::Record(_) => StatusCode::CONFLICT,
        MoveError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let mut body = serde_json::json!({ "error": e.code(), "message": e.to_string() });
    match &e {
        MoveError::Stale { actual } => body["actual"] = serde_json::json!(actual),
        MoveError::IsCandidate { version } => body["version"] = serde_json::json!(version),
        _ => {}
    }
    (status, Json(body)).into_response()
}
