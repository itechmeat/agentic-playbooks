use crate::state::*;

use apb_core::registry::Registry;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

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
    let workspaces = match q.workspace.as_deref() {
        None => enumerate_workspaces(&state),
        Some(ws) => {
            let root = match resolve_root(&state, Some(ws)) {
                Ok(r) => r,
                Err(e) => return e,
            };
            // The project name comes from the registry when the requested
            // workspace is one of the enumerated ones; a pinned-root harness
            // has no name to give, exactly as in the aggregate. Matched on the
            // workspace id, never on the path: `resolve_root` canonicalizes
            // and the registry stores the path as it was registered, so on any
            // symlinked root (every macOS `/var` temp dir, for one) a path
            // comparison would silently drop the name.
            let project = enumerate_workspaces(&state)
                .into_iter()
                .find(|(wid, _, _)| wid == ws)
                .map(|(_, name, _)| name)
                .unwrap_or_default();
            vec![(ws.to_string(), project, root)]
        }
    };
    let mut out: Vec<serde_json::Value> = Vec::new();
    for (workspace_id, project, root) in workspaces {
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
    // Tolerant of a torn trailing line (issue #103.3): the drive appends to
    // `events.jsonl` a line at a time, so a detail request that lands between
    // the bytes of a line and its newline is a normal transient state of a
    // live run. The strict `read_all` failed the whole request with a 500 in
    // that window, which is what makes "every poll during execution failed"
    // look like a broken body to a polling client. Only a read-only reporting
    // surface may do this; the engine itself stays strict.
    let events = match apb_engine::event::read_all_lossy_tail(&run_dir) {
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

    // Live reporting, on the same terms the run listing and MCP `run_status`
    // already use (#85.4, #102.4 cause A). The pure fold calls every open
    // attempt `interrupted`, so before this the detail view read a healthy
    // in-flight run as interrupted while the run list beside it read running,
    // and it never showed that a driverless run needs a resume at all.
    // `driver_alive` is `liveness::driver_alive`, not a bare pid check: a
    // sub-playbook child follows its parent's drive claim.
    let driver_alive = apb_engine::liveness::driver_alive(&run_dir, &id);
    let waiting = progress.as_ref().is_some_and(|p| p.waiting_on.is_some());
    let run_status = apb_engine::liveness::reported_run_status(&events, waiting, driver_alive);
    let nodes = apb_engine::liveness::reported_node_statuses(&events);

    // The run's hooks as map key -> relative path of the signal endpoint.
    let hooks: std::collections::BTreeMap<String, String> = apb_engine::read_hooks(&run_dir)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, secret)| (k, apb_engine::hook_path(&id, &secret)))
        .collect();

    // Why the run failed, on the same terms MCP `run_status` reports it: the
    // last `RunError` folded from the journal. Without this the dashboard shows
    // a red run and no explanation, and the reason is only reachable through
    // `apb doctor --run` or an MCP call. Only for a failed run: a reason folded
    // from an earlier, recovered anomaly is not why the run ended.
    let failure_reason = (run_status == apb_engine::state::RunStatus::Failed)
        .then(|| {
            run_state
                .failure_reason
                .as_ref()
                .map(apb_engine::state::FailureReason::display)
        })
        .flatten();

    Json(serde_json::json!({
        "run_id": id,
        "playbook": playbook_id,
        "version": version,
        "run_status": run_status.as_str(),
        "failure_reason": failure_reason,
        "driver_alive": driver_alive,
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
