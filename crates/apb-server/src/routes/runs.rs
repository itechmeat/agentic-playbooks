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
    // --- 0.23.0: run outcome blocks (C1, C7) ---
    /// The playbook's goal with each criterion's result (C1); absent for a
    /// playbook without a goal.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub goal: Option<apb_engine::run_outcome::RunGoal>,
    /// The commits the run's nodes made on a git tree (C7); empty when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        test,
        ts(as = "Option<Vec<apb_engine::run_outcome::NodeCommits>>", optional)
    )]
    pub commits: Vec<apb_engine::run_outcome::NodeCommits>,
    // --- end of the 0.23.0 blocks ---
    /// Each publish node: the pinned target, the latest plan, whether a
    /// consent is awaited, the push's ledger state and the result; empty
    /// for a run without one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        test,
        ts(as = "Option<Vec<apb_engine::run_outcome::RunPublish>>", optional)
    )]
    pub publish: Vec<apb_engine::run_outcome::RunPublish>,
    /// Unread events the run's connector inbox reads found lost to inbox
    /// retention, one per distinct gap; empty when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        test,
        ts(as = "Option<Vec<apb_engine::run_outcome::RunInboxGap>>", optional)
    )]
    pub inbox_gaps: Vec<apb_engine::run_outcome::RunInboxGap>,
    /// The human outcome: what the run did, what the engine checked, what
    /// nobody checked, and what the attempts reported.
    pub outcome: apb_engine::run_outcome::OutcomeSummary,
    /// The baseline check, each observation before and after; absent for a
    /// playbook without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub baseline: Option<apb_engine::run_outcome::RunBaseline>,
    /// The findings and warnings the attempts reported; absent when none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub notes: Option<apb_engine::run_outcome::RunNotes>,
    /// Each list node's visit with its items; empty for a run without a
    /// list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(test, ts(as = "Option<Vec<apb_engine::list::RunList>>", optional))]
    pub lists: Vec<apb_engine::list::RunList>,
    /// The candidate trial this run was (issue #192), with its verdict;
    /// absent for every other run.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub candidate_trial: Option<apb_engine::candidate::CandidateTrial>,
    /// The known problems the run showed its agents and the ones it hid;
    /// absent for a run whose playbook shows none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub known_problems: Option<apb_engine::known_problems::RunKnownProblems>,
    /// The run's pause (issue #200): who paused it, why, since and until
    /// when, and `pausing` while a live driver drains toward a queued
    /// pause. Absent for a run that is neither paused nor pausing.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub pause: Option<apb_engine::pause::PauseInfo>,
    /// The run's budget: limits, spent and left per dimension, and the node
    /// a budget pause holds it before. Absent for a run without a budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub budget: Option<apb_engine::budget::BudgetStatus>,
    /// The run's notifications: whether they are on, where they go, about
    /// what, what was sent, and why a subscription could not be used.
    /// Absent for a run that never had a subscription.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub notify: Option<apb_engine::notify::RunNotify>,
    /// The estimate the run started with (time, tokens, cost from comparable
    /// runs). Absent for a run started before estimates, or a sub-run.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub estimate: Option<apb_engine::run_estimate::RunEstimate>,
    /// The model each attempt actually ran on, against the profile's primary
    /// model (issue #193); empty before the first attempt.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        test,
        ts(as = "Option<Vec<apb_engine::attempt_models::AttemptModel>>", optional)
    )]
    pub attempt_models: Vec<apb_engine::attempt_models::AttemptModel>,
    /// What each isolated attempt's isolation held and did not (level,
    /// backend, network, limits); empty for a run without one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        test,
        ts(
            as = "Option<Vec<apb_engine::run_view::AttemptIsolationRow>>",
            optional
        )
    )]
    pub isolation: Vec<apb_engine::run_view::AttemptIsolationRow>,
    /// Progress and every open gate (reviews, questions, waits, supervisor):
    /// the run page renders its panels from this, never from `events`.
    pub progress: Option<apb_engine::progress::ProgressSummary>,
    pub answer: Option<String>,
    /// Token usage the run's attempts reported; null when none did.
    pub usage: Option<apb_engine::run_view::RunUsage>,
    /// Decision-model totals (issue #165 Part 4); absent when the run
    /// journaled no decision. Each decision itself is a `decision_made` in
    /// `events`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub decisions: Option<apb_engine::run_view::RunDecisions>,
    /// Events of a type this binary does not know (a newer apb wrote them),
    /// left out of `events`; 0 for a journal read in full.
    pub unknown_events: usize,
    // --- host execution mode (0.23.0) ---
    /// Who executes the run's agent steps, from its manifest: `cli` (the
    /// profiles' CLIs) or `host` (the host session's subagents). Read-only.
    #[cfg_attr(test, ts(type = "\"cli\" | \"host\""))]
    pub execution: String,
    /// A `cli` run whose steps fall back to host tasks when no CLI can start.
    pub execution_fallback: bool,
    // --- end host execution mode ---
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
    // Each workspace's listing is newest first; merge them into one newest
    // first order, else "all projects" shows every run of one project above
    // a newer run of the next.
    out.sort_by_key(|e| std::cmp::Reverse(e.run.started_ts));
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
    let hooks = apb_engine::read_hooks(&run_dir)
        .unwrap_or_default()
        .paths(&id);

    let children = view.children(&run_dir);
    let failure_reason = view.failure_reason();
    let usage = view.usage();
    let decisions = view.decisions();
    let commits = view.commits();
    let publish = view.publish();
    let inbox_gaps = view.inbox_gaps();
    let attempt_models = apb_engine::attempt_models::run_attempt_models(&run_dir, &view.events);
    let isolation = view.isolation();
    // The goal, the baseline and the outcome are read against the one
    // playbook snapshot parsed above.
    let readout = view.readout_of(loaded_pb);
    let outcome = apb_engine::run_outcome::OutcomeSummary::from_readout(
        &view,
        &run_dir,
        &readout,
        answer.clone(),
    );
    let goal = readout.goal;
    let baseline = readout.baseline;
    let notes = Some(readout.notes).filter(|n| !n.is_empty());
    let lists = readout.lists;
    let candidate_trial = apb_engine::candidate::trial_of(&run_dir, &view.events);
    let known_problems = apb_mcp::tools::problems::run_block(&view.events);
    let pause = view.pause().cloned();
    let nodes = view.nodes();
    let manifest = apb_engine::manifest::read(&run_dir).ok().flatten();
    let execution = manifest
        .as_ref()
        .and_then(|m| m.execution_mode().ok())
        .unwrap_or_default()
        .as_str()
        .to_string();
    let execution_fallback = manifest.as_ref().is_some_and(|m| m.falls_back_to_host());
    let budget = view.budget(&run_dir);
    let estimate = apb_engine::run_estimate::snapshot::read(&run_dir);
    let notify = apb_mcp::tools::notify::run_block(&run_dir, &view.events);
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
        commits,
        publish,
        inbox_gaps,
        outcome,
        baseline,
        notes,
        lists,
        attempt_models,
        isolation,
        goal,
        candidate_trial,
        known_problems,
        pause,
        budget,
        notify,
        estimate,
        usage,
        decisions,
        unknown_events: view.unknown.len(),
        execution,
        execution_fallback,
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
    /// A publish gate only: the nonce of the plan the person saw.
    #[serde(default)]
    nonce: Option<String>,
}

/// A publish consent is a person's (decision record 0010). A dashboard
/// without keys takes any request that reaches its loopback port, and under
/// `network: open` a sandboxed agent shares that loopback: it could read the
/// open plan's nonce and consent to its own push. So a keyless dashboard
/// never takes a `publish` decision; a `skip` pushes nothing and stays open.
/// The person approves with the CLI, or issues a dashboard key first.
fn keyless_publish_refusal(
    auth: &crate::auth::AuthState,
    run_id: &str,
    body: &ReviewBody,
) -> Option<String> {
    (!auth.enabled() && body.decision == apb_core::publish::OPTION_PUBLISH).then(|| {
        format!(
            "a dashboard without keys does not take a publish consent, since anything on this machine can reach it; approve with `apb publish approve {run_id} --node {} --nonce <nonce>`, or issue a key with `apb server key issue` and sign in",
            body.node
        )
    })
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
    if let Some(why) = keyless_publish_refusal(&state.auth, &id, &body) {
        return (StatusCode::FORBIDDEN, why).into_response();
    }
    let cmd = apb_engine::ReviewCommand {
        node: body.node,
        decision: body.decision,
        note: body.note,
    };
    // The engine owns the node check (issue #103.1) and the publish nonce
    // check, so MCP `review_decide` and the `apb review` CLI inherit them;
    // this maps its client-fault verdicts the way `run_playbook_handler`
    // maps its own. None is a server fault: `NotFound` means the decided
    // node is not a gate of this run's playbook, `Conflict` means no
    // decision is pending on it or the nonce answers an older plan (the
    // message names the current one), `Invalid` means a decision the gate
    // does not offer or a `publish` without the plan's nonce.
    match apb_engine::review::post_decision(&run_dir, cmd, body.nonce) {
        Ok(seq) => Json(serde_json::json!({ "posted_seq": seq })).into_response(),
        Err(apb_engine::EngineError::NotFound(what)) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Err(apb_engine::EngineError::Conflict(what)) => {
            (StatusCode::CONFLICT, what).into_response()
        }
        Err(apb_engine::EngineError::Invalid(what)) => {
            (StatusCode::BAD_REQUEST, what).into_response()
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

/// Body of `POST /api/runs/{id}/pause` (issue #200). Every field is optional:
/// an empty body is a drain pause with no reason, like `apb pause <id>`.
#[derive(Deserialize, Default)]
pub(crate) struct PauseBody {
    /// `drain` (default) or `now`.
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    /// RFC 3339 timestamp or a duration from now (`90m`, `2h`).
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    keep_lock: bool,
}

/// POST /api/runs/{id}/pause: the dashboard's Pause button. The same
/// `apb_engine::pause::request_pause` the CLI and MCP `run_pause` call, as
/// `by: operator`. 404 for an unknown run, 409 for a run that already ended,
/// 422 for a bad `mode` or `until`.
pub(crate) async fn post_pause_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    body: Option<Json<PauseBody>>,
) -> impl IntoResponse {
    use apb_engine::pause::{PauseOutcome, PauseRequest};
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if !root.join(".apb/runs").join(&id).is_dir() {
        return (StatusCode::NOT_FOUND, format!("run `{id}` not found")).into_response();
    }
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let req = match PauseRequest::operator(
        body.mode.as_deref(),
        body.reason,
        body.until.as_deref(),
        body.keep_lock,
        apb_core::clock::now_ms_u64(),
    ) {
        Ok(r) => r,
        Err(what) => return (StatusCode::UNPROCESSABLE_ENTITY, what).into_response(),
    };
    match apb_engine::pause::request_pause(&root, &id, &req) {
        Ok(outcome) => {
            let seq = match outcome {
                PauseOutcome::Posted { seq } | PauseOutcome::Journaled { seq } => seq,
            };
            Json(serde_json::json!({
                "run_id": id,
                "outcome": outcome.as_str(),
                "mode": req.mode.as_str(),
                "seq": seq,
                "until": req.until,
            }))
            .into_response()
        }
        Err(apb_engine::EngineError::NotFound(what)) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        // A run that already ended cannot be paused.
        Err(apb_engine::EngineError::Invalid(what) | apb_engine::EngineError::Conflict(what)) => {
            (StatusCode::CONFLICT, what).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Body of `POST /api/runs/{id}/resume`.
#[derive(Deserialize, Default)]
pub(crate) struct ResumeBody {
    #[serde(default)]
    from_node: Option<String>,
    #[serde(default)]
    allow_environment_drift: bool,
    /// The `consent_nonce` of the 409 refusal the consent dialog showed, for
    /// a run with no valid consent to its irreversible effects recorded (one
    /// an older apb started): the same confirmation MCP `run_resume` takes.
    #[serde(default)]
    confirm_irreversible: Option<apb_engine::consent::Confirmation>,
    /// The budget change a run its budget paused needs (raise or set a
    /// limit, another `on_no_estimate`, or `once`); a plain resume of such a
    /// run is refused (409 `budget_paused`, with the run's budget).
    #[serde(default)]
    budget: Option<super::estimate::ResumeBudgetBody>,
}

/// POST /api/runs/{id}/resume: the dashboard's Resume button, for a paused,
/// interrupted or driverless run. It goes through the gate MCP `run_resume`
/// uses: `check_resume` (the run was created here, its snapshot is trusted;
/// the person clicking Resume is the trust confirmation, as clicking Run is)
/// and the irreversible-consent check, whose refusal (409,
/// `irreversible_requires_confirmation`, with `consent_nonce`) the dashboard
/// answers with its consent dialog. The driver then runs detached. 404 for an
/// unknown run, 409 for a run that cannot be resumed (finished, refused by
/// the gate, environment drift).
pub(crate) async fn post_resume_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    body: Option<Json<ResumeBody>>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let body = body.map(|Json(b)| b).unwrap_or_default();
    if let Err(refusal) = apb_engine::gate::check_resume(&root, &id, true) {
        let status = if refusal["policy"] == "not_found" {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::CONFLICT
        };
        return (status, Json(refusal)).into_response();
    }
    let deprecation = match apb_engine::gate::check_resume_consent(
        &root,
        &id,
        body.confirm_irreversible.as_ref(),
        "dashboard",
    ) {
        Ok(note) => note,
        Err(refusal) => return (StatusCode::CONFLICT, Json(refusal)).into_response(),
    };
    let budget = match super::estimate::resume_budget(body.budget.as_ref()) {
        Ok(b) => b,
        Err(refusal) => return refusal.into_response(),
    };
    let from_node = body.from_node.as_deref();
    // Computed before the drive so the answer says where the run resumes, and
    // so a finished run is refused here (the argument-free plan would).
    let decision = match apb_engine::plan_resume(&root, &id, from_node) {
        Ok(d) => d,
        Err(e) => return resume_error(e),
    };
    // A paused run whose `until` is still ahead is resumed early.
    let early_until = apb_engine::run_view::RunView::load(&root.join(".apb/runs").join(&id), &id)
        .ok()
        .and_then(|v| v.pause().filter(|p| !p.pausing).and_then(|p| p.until))
        .filter(|u| *u > apb_core::clock::now_ms_u64());
    let req = apb_engine::ResumeRequest {
        from_node,
        allow_environment_drift: body.allow_environment_drift,
        budget: budget.as_ref(),
        ..apb_engine::ResumeRequest::operator()
    };
    match apb_engine::resume_detached_req(&root, &id, &req) {
        Ok(_) => {
            let mut answer = serde_json::json!({
                "run_id": id,
                "resumed_from": decision.start_node,
                "reason": decision.reason.as_str(),
                "detached": true,
            });
            if let Some(until) = early_until {
                answer["resumed_early"] = serde_json::json!(true);
                answer["pause_until"] = serde_json::json!(until);
            }
            if let Some(note) = deprecation {
                answer["deprecation"] = serde_json::json!(note);
            }
            Json(answer).into_response()
        }
        Err(apb_engine::EngineError::Conflict(detail))
            if let Some(refusal) = super::estimate::resume_refusal(&root, &id, &detail) =>
        {
            refusal
        }
        Err(e) => resume_error(e),
    }
}

fn resume_error(e: apb_engine::EngineError) -> axum::response::Response {
    match e {
        apb_engine::EngineError::NotFound(what) => (StatusCode::NOT_FOUND, what).into_response(),
        apb_engine::EngineError::Invalid(what) | apb_engine::EngineError::Conflict(what) => {
            (StatusCode::CONFLICT, what).into_response()
        }
        // Another run holds the busy lock of the tree this run works in: the
        // resume is refused before a driver starts, so the run stays paused.
        apb_engine::EngineError::WorkdirBusy(what) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "workdir_busy", "detail": what })),
        )
            .into_response(),
        other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()).into_response(),
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
    // foreign or incorrect secret must not accept the signal), compared in
    // constant time by `RunHooks::key_for`.
    let Some(key) = hooks.key_for(&secret) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    match apb_engine::post_signal(
        &run_dir,
        apb_engine::SignalCommand {
            key: key.to_string(),
        },
    ) {
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

/// GET /api/runs/{id}/report.html: the run report as one self-contained
/// page to save (the dashboard's "Save report"), the same page `apb runs
/// <id> --html` writes. Served as a download with its own Content Security
/// Policy (nothing loads, nothing runs, only its pinned stylesheet applies)
/// and `nosniff`, so it is never rendered as part of the dashboard.
pub(crate) async fn get_run_report_html_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    use axum::http::header;
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
    let page =
        tokio::task::spawn_blocking(move || apb_engine::report_html::render(&root, &run_dir)).await;
    match page {
        Ok(Ok(html)) => {
            let csp = apb_engine::report_html::markup::csp(apb_engine::report_html::STYLESHEET);
            let disposition = format!("attachment; filename=\"apb-run-{id}.html\"");
            (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                    (header::CONTENT_SECURITY_POLICY, csp),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                    (header::CONTENT_DISPOSITION, disposition),
                    (header::CACHE_CONTROL, "no-store".to_string()),
                ],
                html,
            )
                .into_response()
        }
        Ok(Err(apb_engine::EngineError::NotFound(what))) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
