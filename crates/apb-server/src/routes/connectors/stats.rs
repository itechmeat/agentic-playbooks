//! Usage statistics for one connector, aggregated read-only from run event
//! logs. Scanning is capped so the endpoint's cost does not grow with a
//! project's whole history.

use super::connector_roots;
use crate::state::*;
use std::path::Path;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};

/// Runs scanned per `GET /api/connectors/{name}/stats` call, most recent
/// first (spec 9's usage-stats bullet: "aggregated from existing run event
/// logs", read-only, no new engine state). Unbounded history scanning would
/// make this endpoint cost grow with the whole project's lifetime, so it is
/// capped to the latest N runs by start time - the same ordering
/// `apb_engine::list_runs` already sorts by. `runs_scanned` in the response
/// reports how many were actually read, so a caller can tell a small number
/// apart from "there were more but we capped".
pub(crate) const STATS_RUN_CAP: usize = 50;

/// Running totals of one connector's `ConnectorCall` events, summed over one
/// or more project roots. Kept as a struct so the per-root scan is a single
/// method the handler calls in a loop, rather than five mutable locals threaded
/// through it.
#[derive(Default)]
pub(crate) struct ConnectorStatsAcc {
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
pub(crate) async fn connector_stats_handler(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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
