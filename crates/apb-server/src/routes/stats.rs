//! `GET /api/stats` (C3): cross-run metrics of one project's runs, the same
//! report `apb stats --json` prints. Read-only, from journals only.

use crate::state::*;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct StatsQuery {
    workspace: Option<String>,
    playbook: Option<String>,
    /// A date (2026-09-20) or a duration back from now (30d).
    since: Option<String>,
    compare: Option<String>,
}

/// GET /api/stats?workspace=<id>&playbook=<id>&since=30d&compare=<ver>.
pub(crate) async fn get_stats_handler(
    State(state): State<AppState>,
    Query(q): Query<StatsQuery>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let since_ms = match q.since.as_deref() {
        None | Some("") => None,
        Some(s) => match apb_engine::decision::report::parse_since(s, apb_core::clock::now_ms()) {
            Some(ms) => Some(ms),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    "since takes a date (2026-09-20) or a duration (30d, 24h)",
                )
                    .into_response();
            }
        },
    };
    let filter = apb_engine::run_stats::StatsFilter {
        playbook: q.playbook.filter(|p| !p.is_empty()),
        since_ms,
        compare: q.compare.filter(|c| !c.is_empty()),
    };
    // Like `apb stats --compare` (exit 2): a version is one playbook's.
    if filter.compare.is_some() && filter.playbook.is_none() {
        return (StatusCode::BAD_REQUEST, "compare needs playbook").into_response();
    }
    // Reading every journal is blocking file IO.
    match tokio::task::spawn_blocking(move || apb_engine::run_stats::stats(&[root], &filter)).await
    {
        Ok(report) => Json(report).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
