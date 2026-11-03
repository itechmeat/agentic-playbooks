//! The keyless demo on the HTTP API, for the dashboard's empty playbook
//! list ("Try the demo"):
//!
//! - `GET /api/demo`: whether the demo is installed in `<config dir>/demo/`
//!   (`installed`, `modified`) and, when it is, its project;
//! - `POST /api/demo`: installs it (idempotent; a demo whose files differ
//!   from the shipped ones is refused 409, its reinstall is `apb demo
//!   --reset`) and registers its project, so the dashboard can open the
//!   playbook page with the returned `workspace_id`.
//!
//! Installing writes only the demo project; starting it is the ordinary
//! Run of its playbook page, with the same acknowledgement as any playbook
//! nobody approved. Both handlers touch files, off the async runtime.

use apb_core::demo::{DemoError, DemoProject, DemoState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;

/// `GET /api/demo`.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct DemoStatus {
    pub installed: bool,
    /// Installed, but its files differ from the shipped ones.
    pub modified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub project: Option<DemoProject>,
}

/// The demo directory, or the answer when this machine has no config dir.
fn demo_dir() -> Result<std::path::PathBuf, Box<Response>> {
    apb_core::config::config_dir()
        .map(|c| apb_core::demo::demo_dir(&c))
        .ok_or_else(|| {
            Box::new(
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "no config directory on this machine",
                )
                    .into_response(),
            )
        })
}

/// The status of the demo in `dir`.
pub fn status_of(dir: &std::path::Path) -> DemoStatus {
    let state = apb_core::demo::state(dir);
    DemoStatus {
        installed: state != DemoState::Absent,
        modified: state == DemoState::Modified,
        project: (state != DemoState::Absent).then(|| apb_core::demo::project(dir)),
    }
}

/// Installs the demo in `dir`, answered as the route answers it.
pub fn install_at(dir: &std::path::Path) -> Response {
    match apb_core::demo::install(dir, false) {
        Ok(p) => Json(p).into_response(),
        Err(e @ DemoError::Modified(_)) => (StatusCode::CONFLICT, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(crate) async fn get_demo_handler() -> Response {
    let dir = match demo_dir() {
        Ok(d) => d,
        Err(r) => return *r,
    };
    match tokio::task::spawn_blocking(move || status_of(&dir)).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(crate) async fn post_demo_handler() -> Response {
    let dir = match demo_dir() {
        Ok(d) => d,
        Err(r) => return *r,
    };
    tokio::task::spawn_blocking(move || install_at(&dir))
        .await
        .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
}
