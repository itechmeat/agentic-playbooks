use crate::state::*;

use apb_core::registry::Registry;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};

/// One row of `GET /api/runs`: the engine's run summary stamped with the
/// project it belongs to. Typed so the dashboard's TypeScript is generated
/// from it (`web/src/lib/api.gen.ts`, see `ts_contract`).
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct RunListEntry {
    #[serde(flatten)]
    pub run: apb_engine::RunSummary,
    /// Owning project (global dashboard). Empty on the pinned-root test server.
    pub workspace_id: String,
    pub project: String,
}

/// `GET /api/runs/{id}`: everything the run page shows, every run fact read
/// through one [`apb_engine::run_view::RunView`] (the same model `apb wait`
/// and MCP `run_status`/`run_wait` report from).
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct RunDetail {
    pub run_id: String,
    pub playbook: String,
    pub version: String,
    pub run_status: apb_engine::state::RunStatus,
    /// Why a failed run ended (`node \`x\`: reason`); null unless failed.
    pub failure_reason: Option<String>,
    /// Whether a process really drives the run; null when nothing claims to.
    pub driver_alive: Option<bool>,
    /// Per-node reported status (`lost` and `interrupted` included).
    pub nodes: std::collections::BTreeMap<String, String>,
    pub outputs: std::collections::BTreeMap<String, String>,
    pub instruction: Option<String>,
    pub params: std::collections::BTreeMap<String, String>,
    /// The run's working tree (issue #67 item 8), once resolved; null means
    /// the nodes run in the project root.
    pub worktree: Option<String>,
    /// The run's playbook snapshot; null for very old runs without one.
    #[cfg_attr(
        test,
        ts(
            type = "{ id: string; name: string; nodes: PlaybookNode[]; edges: PlaybookEdge[]; defaults?: { on_failure?: string } | null } | null"
        )
    )]
    pub model: serde_json::Value,
    #[cfg_attr(test, ts(type = "WfLayout | null"))]
    pub layout: Option<serde_json::Value>,
    pub hooks: std::collections::BTreeMap<String, String>,
    /// Sub-runs started by a `playbook` node, one per `ChildRunStarted`.
    pub children: Vec<apb_engine::run_view::ChildRun>,
    /// Progress and every open gate (reviews, questions, waits, supervisor):
    /// the run page renders its panels from this, never from `events`.
    pub progress: Option<apb_engine::progress::ProgressSummary>,
    pub answer: Option<String>,
    /// Token usage the run's attempts reported; null when none did.
    pub usage: Option<apb_engine::run_view::RunUsage>,
    /// Events of a type this binary does not know (a newer apb wrote them),
    /// left out of `events`; 0 for a journal read in full.
    pub unknown_events: usize,
    #[cfg_attr(test, ts(type = "WfEvent[]"))]
    pub events: Vec<apb_engine::event::Event>,
}

/// GET /api/runs: every reachable project's runs by default, or exactly one
/// project's when `?workspace=<id>` is given (issue #103.2).
///
/// The aggregate stays the no-param default - that is what the dashboard
/// calls. The filter exists because the listing stamps each row with its
/// `workspace_id` while `GET /api/runs/{id}` requires that same id to resolve
/// the run, so without it a caller could not narrow the listing to the
/// workspace it was about to ask about. An unknown id is a 404 through
/// `resolve_root`, exactly like the detail endpoint.
pub(crate) async fn list_runs_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    let workspaces = match selected_workspaces(&state, q.workspace.as_deref()) {
        Ok(w) => w,
        Err(e) => return e,
    };
    let mut out: Vec<RunListEntry> = Vec::new();
    for (workspace_id, project, root) in workspaces {
        let Ok(list) = apb_engine::list_runs(&root) else {
            continue;
        };
        out.extend(list.into_iter().map(|run| RunListEntry {
            run,
            workspace_id: workspace_id.clone(),
            project: project.clone(),
        }));
    }
    Json(out).into_response()
}

pub(crate) async fn get_run_handler(
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
    let run_dir = root.join(".apb/runs").join(&id);
    if !run_dir.is_dir() {
        return (StatusCode::NOT_FOUND, format!("run `{id}` not found")).into_response();
    }
    // The run view every status surface reports from (`apb runs`, `apb
    // wait`, MCP `run_status`): the journal read tolerant of a line the drive
    // is still appending (issue #103.3), and the liveness overlay applied once
    // (#85.4, #102.4 cause A) - a live open attempt reads running, a dead
    // driver interrupted, a sub-playbook child follows its parent's drive.
    let view = match apb_engine::run_view::RunView::load(&run_dir, &id) {
        Ok(view) => view,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
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
    let answer = apb_engine::progress::run_answer(&run_dir, &view.events);

    // The saved graph layout for the run's playbook version, so the run view
    // shows the same node arrangement the author laid out in the editor rather
    // than a fresh auto-layout. Best-effort: an old/removed version simply has
    // no stored layout and the client falls back to auto-layout.
    let layout = Registry::open(&root)
        .ok()
        .filter(|_| !version.is_empty())
        .and_then(|reg| reg.load(&playbook_id, Some(&version)).ok())
        .and_then(|loaded| loaded.layout);

    // The run's hooks as map key -> relative path of the signal endpoint.
    let hooks: std::collections::BTreeMap<String, String> = apb_engine::read_hooks(&run_dir)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, secret)| (k, apb_engine::hook_path(&id, &secret)))
        .collect();

    let children = view.children(&run_dir);
    let failure_reason = view.failure_reason();
    let usage = view.usage();
    let nodes = view.nodes();
    Json(RunDetail {
        run_id: id,
        playbook: playbook_id,
        version,
        run_status: view.run_status,
        failure_reason,
        driver_alive: view.driver_alive,
        nodes,
        worktree: view.state.worktree.clone(),
        outputs: view.state.outputs,
        instruction: cfg.instruction,
        params: cfg.params,
        model: playbook_json,
        layout,
        hooks,
        children,
        usage,
        unknown_events: view.unknown.len(),
        progress: view.progress,
        answer,
        events: view.events,
    })
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct ReviewBody {
    node: String,
    decision: String,
    #[serde(default)]
    note: String,
}

pub(crate) async fn post_review_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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
    // The engine owns the node check (issue #103.1), so MCP `review_decide`
    // and the `apb review` CLI inherit it; this maps its two client-fault
    // verdicts the way `run_playbook_handler` maps its own. Neither is a
    // server fault: `NotFound` means the decided node is not a `human_review`
    // node of this run's playbook, `Conflict` means no decision is pending
    // on it.
    match apb_engine::post_review(&run_dir, cmd) {
        Ok(seq) => Json(serde_json::json!({ "posted_seq": seq })).into_response(),
        Err(apb_engine::EngineError::NotFound(what)) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Err(apb_engine::EngineError::Conflict(what)) => {
            (StatusCode::CONFLICT, what).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Body of `POST /api/runs/{id}/answer`: `node` is the interactive node to
/// answer, defaulting (when omitted) to the single node with a pending
/// question, exactly like `apb_engine::post_answer`'s own `node: Option<&str>`
/// resolution (spec 2026-07-20-interactive-nodes).
#[derive(Deserialize)]
pub(crate) struct AnswerBody {
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
pub(crate) async fn post_answer_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
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

pub(crate) async fn post_hook_handler(
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
    // foreign or incorrect secret must not accept the signal). Only the
    // comparison changes: a plain `==` leaks a live secret's bytes through
    // response timing, so each candidate is compared in constant time. The
    // first-match semantics of the previous `find` are preserved exactly,
    // including the break, so a run whose hooks somehow share a secret still
    // signals the same key it always did.
    let mut matched: Option<&String> = None;
    for (key, candidate) in hooks.iter() {
        if apb_core::server_auth::ct_eq_str(candidate, &secret) {
            matched = Some(key);
            break;
        }
    }
    let Some(key) = matched else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    match apb_engine::post_signal(&run_dir, apb_engine::SignalCommand { key: key.clone() }) {
        Ok(seq) => Json(serde_json::json!({ "signalled": key, "posted_seq": seq })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(crate) async fn get_run_report_handler(
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
    match apb_engine::supervisor_report_or_summary(&root, &id) {
        Ok(report) => Json(serde_json::json!({ "report": report })).into_response(),
        Err(apb_engine::EngineError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("run `{id}` not found")).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
