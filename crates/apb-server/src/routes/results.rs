//! A run's result snapshots on the HTTP API, over the tool layer MCP and
//! the CLI use (`apb_mcp::tools::results`), so every surface serves the
//! same bytes and digests and says `corrupt` alike:
//!
//! - `GET /api/runs/{id}/results` (`node`): every stamped snapshot by
//!   node, execution and item, and the refs recorded without bytes;
//! - `GET /api/runs/{id}/results/{snapshot}?entry=<entry>` (`offset`,
//!   `limit`): one entry's text as JSON, at most the display cap;
//! - `GET /api/runs/{id}/results/{snapshot}/raw?entry=<entry>`: the whole
//!   entry as a download (`text/plain; charset=utf-8`, `nosniff`,
//!   `attachment`), never rendered by the dashboard;
//! - `GET /api/runs/{id}/results/{snapshot}/live?entry=<entry>`: how a
//!   stored workspace file compares with its live copy, a word only.
//!
//! Every query takes `workspace`. Nothing here writes; no path is taken
//! from a request (the snapshot is an id, the entry the core grammar). A
//! keyless dashboard serves these reads to any allowed host, as it serves
//! run detail.

use apb_core::result_store::Citation;
use apb_mcp::tools::results::{
    parse_entry, run_result_live, run_result_raw, run_result_read, run_results,
};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::state::*;

/// The query of the listing.
#[derive(Deserialize, Default)]
pub(crate) struct ResultsQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    node: Option<String>,
}

/// The query of an entry route.
#[derive(Deserialize, Default)]
pub(crate) struct EntryQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    entry: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

pub(crate) async fn get_results_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<ResultsQuery>,
) -> Response {
    let run = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        run_results(&root, &run, q.node.as_deref())
    })
    .await
}

pub(crate) async fn get_result_entry_handler(
    State(state): State<AppState>,
    AxPath((id, snapshot)): AxPath<(String, String)>,
    Query(q): Query<EntryQuery>,
) -> Response {
    let run = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let (snapshot_id, entry) = parse_entry(&snapshot, &q.entry)?;
        let reference = Citation {
            snapshot_id,
            entry,
            range: None,
        }
        .to_string();
        run_result_read(&root, &run, &reference, q.offset, q.limit, false)
    })
    .await
}

pub(crate) async fn get_result_live_handler(
    State(state): State<AppState>,
    AxPath((id, snapshot)): AxPath<(String, String)>,
    Query(q): Query<EntryQuery>,
) -> Response {
    let run = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        let (snapshot_id, entry) = parse_entry(&snapshot, &q.entry)?;
        let live = run_result_live(&root, &run, &snapshot_id, &entry)?;
        Ok(serde_json::json!({ "live": live }))
    })
    .await
}

pub(crate) async fn get_result_raw_handler(
    State(state): State<AppState>,
    AxPath((id, snapshot)): AxPath<(String, String)>,
    Query(q): Query<EntryQuery>,
) -> Response {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let read = tokio::task::spawn_blocking(move || {
        let (snapshot_id, entry) = parse_entry(&snapshot, &q.entry)?;
        run_result_raw(&root, &id, &snapshot_id, &entry, None)
    })
    .await;
    match read {
        Ok(Ok(content)) => {
            let name = content
                .digest
                .strip_prefix("sha256:")
                .unwrap_or(&content.digest)
                .chars()
                .take(16)
                .collect::<String>();
            (
                [
                    (
                        header::CONTENT_TYPE,
                        "text/plain; charset=utf-8".to_string(),
                    ),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"result-{name}.txt\""),
                    ),
                    (header::CACHE_CONTROL, "no-store".to_string()),
                ],
                content.text,
            )
                .into_response()
        }
        Ok(Err(e)) => tool_refusal(e),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
