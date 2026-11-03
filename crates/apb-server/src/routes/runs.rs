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
    /// The run's params as shown: every sensitive value (a param declared
    /// secret, a secret-shaped name, a credential-shaped value) reads
    /// `<redacted>`.
    pub params: std::collections::BTreeMap<String, String>,
    /// The names of the params whose value `params` hides.
    pub redacted_params: Vec<String>,
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
    /// What the journal proves about every node and edge of the
    /// definitions the run executed, through the run's redactor; a string
    /// when the run's text is withheld.
    #[cfg_attr(test, ts(type = "RunGraph | string | null"))]
    pub graph: Option<serde_json::Value>,
    /// The definition of the graph's shown segment (the latest known one):
    /// what the run canvas draws so edge states match the bytes that ran;
    /// null when no definition of the run is established.
    #[cfg_attr(
        test,
        ts(
            type = "{ id: string; name: string; nodes: PlaybookNode[]; edges: PlaybookEdge[]; defaults?: { on_failure?: string } | null } | null"
        )
    )]
    pub graph_model: serde_json::Value,
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
    /// The declared run outputs projected from the journal; absent for a
    /// playbook that declares none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub run_outputs: Option<apb_engine::run_outputs::RunOutputs>,
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
    /// The run's launch authorization: who launched it, how an `ask` or
    /// `deny` launch was confirmed, the policy and its sources. Absent for a
    /// run an older apb started.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub launch: Option<apb_core::launch::LaunchRecord>,
    /// The run's pause (issue #200): who paused it, why, since and until
    /// when, and `pausing` while a live driver drains toward a queued
    /// pause. Absent for a run that is neither paused nor pausing.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub pause: Option<apb_engine::pause::PauseInfo>,
    /// The nodes whose retry waits out its `retry_delay`, each with when it
    /// is due (an absolute time, or frozen while the run is paused); empty
    /// when none waits.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        test,
        ts(as = "Option<Vec<apb_engine::retry_wait::RetryWait>>", optional)
    )]
    pub retry_waits: Vec<apb_engine::retry_wait::RetryWait>,
    /// The run's driver let go while only deferred retries were left: a
    /// waker resumes it at `wake_at_ms`. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub released: Option<apb_engine::state::Released>,
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
    /// Each attempt's identity: the model apb requested, what a host
    /// reported, the chain position and why a step was entered, against the
    /// profile's primary model; empty before the first attempt.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        test,
        ts(
            as = "Option<Vec<apb_engine::execution_identity::fold::ExecutionIdentityView>>",
            optional
        )
    )]
    pub attempt_models: Vec<apb_engine::execution_identity::fold::ExecutionIdentityView>,
    /// How often a host followed the hints of the run's host attempts;
    /// absent when no host attempt had one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub hint_follow: Option<apb_engine::run_stats::executors::HintFollow>,
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
    /// Each read-only node's check per attempt (tier, what it covered) and
    /// the changes that failed it; empty for a run without one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        test,
        ts(as = "Option<Vec<apb_engine::run_view::ReadOnlyRow>>", optional)
    )]
    pub read_only: Vec<apb_engine::run_view::ReadOnlyRow>,
    /// The run is unattended (`apb run --unattended`, MCP and HTTP
    /// `unattended: true`), from its write-once manifest: every wait without
    /// a declared outcome fails it. Read-only; left out for an attended run.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(test, ts(optional, as = "Option<bool>"))]
    pub unattended: bool,
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
    /// "driven by apb X, resumed by apb Y": the binaries that drove the run,
    /// from the versions its journal recorded; absent when none did.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub driven_by: Option<String>,
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
    /// What the run and its live sub-playbook runs are ready for, each
    /// action with the run id it acts on, from the same read as the rest.
    pub next_actions: apb_engine::next_action::RunActions,
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
    // The listing reads every run's files and resolves the workspaces'
    // connector secrets: off the async runtime.
    match tokio::task::spawn_blocking(move || listed_runs(workspaces)).await {
        Ok(Ok(rows)) => Json(rows).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Every run of `workspaces`, each redacted with its own values (and its
/// ancestors'), newest first across the workspaces.
fn listed_runs(
    workspaces: Vec<(String, String, std::path::PathBuf)>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut out: Vec<(u128, serde_json::Value)> = Vec::new();
    for (workspace_id, project, root) in workspaces {
        let Ok(list) = apb_engine::list_runs(&root) else {
            continue;
        };
        // The workspace's connector secrets are resolved once.
        let redaction = apb_engine::run_redact::WorkspaceRedaction::new(&root);
        for run in list {
            let redactor = redaction.run(&run.run_id);
            let started = run.started_ts;
            let run_id = run.run_id.clone();
            let entry = RunListEntry {
                run,
                workspace_id: workspace_id.clone(),
                project: project.clone(),
            };
            // A row that does not serialize fails the listing, as it did
            // when the whole list was serialized at once: a run never
            // vanishes from the dashboard without a word.
            let mut v = serde_json::to_value(&entry).map_err(|e| format!("run `{run_id}`: {e}"))?;
            redactor.redact_json(&mut v);
            out.push((started, v));
        }
    }
    // Each workspace's listing is newest first; merge them into one newest
    // first order, else "all projects" shows every run of one project above
    // a newer run of the next.
    out.sort_by_key(|(started, _)| std::cmp::Reverse(*started));
    Ok(out.into_iter().map(|(_, v)| v).collect())
}

pub(crate) async fn get_run_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    channel: Option<axum::Extension<crate::auth::RequestChannel>>,
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
        Ok(view) => std::sync::Arc::new(view),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let cfg = apb_engine::run_config::read_run_config(&run_dir).unwrap_or_default();
    // Each action redacted with the values of the run it acts on (a child's
    // with its own), before the whole detail is redacted with this run's.
    let redaction = apb_engine::run_redact::WorkspaceRedaction::new(&root);
    let mut next_actions = apb_engine::next_action::for_surface(
        &redaction,
        &apb_engine::next_action::Observation::from_view(&run_dir, &id, view.clone()),
    );
    // What this request's credential may not take: the server cannot see a
    // caller's run step, so only the channel counts here.
    apb_engine::next_action::mark_for_caller(
        &mut next_actions,
        &root.join(".apb/runs"),
        crate::auth::RequestChannel::of(channel.map(|c| c.0)),
        &apb_core::step_env::StepCaller::default(),
    );

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
    let attempt_models = apb_engine::execution_identity::fold::views(&run_dir, &view.events);
    let hint_follow = apb_engine::run_stats::executors::HintFollow::of_views(&attempt_models);
    let isolation = view.isolation();
    let read_only = view.read_only();
    let unattended = apb_engine::run_view::run_unattended(&run_dir);
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
    let run_outputs = view.run_outputs(&run_dir);
    let candidate_trial = apb_engine::candidate::trial_of(&run_dir, &view.events);
    let known_problems = apb_mcp::tools::problems::run_block(&view.events);
    let pause = view.pause().cloned();
    let retry_waits = apb_engine::retry_wait::of_view(&view, apb_core::clock::now_ms_u64());
    let released = view.released().cloned();
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
    let redactor = redaction.run_dir(&run_dir);
    let (params, redacted_params) = redactor.params(&cfg.params);
    let (run_graph, shown_definition) = view.graph_and_definition(&run_dir);
    let graph = Some(run_graph.present(&redactor));
    let graph_model = shown_definition
        .and_then(|p| serde_json::to_value(p).ok())
        .unwrap_or(serde_json::Value::Null);
    let (run_status, driver_alive) = (view.run_status, view.driver_alive);
    let worktree = view.state.worktree.clone();
    let unknown_events = view.unknown.len();
    let driven_by = view.driven_by();
    // The journal, the outputs and the progress move into the answer when
    // this handler holds the one view the actions were read from (the
    // observation is gone), and are copied from that same view otherwise.
    let (events, outputs, progress) = match std::sync::Arc::try_unwrap(view) {
        Ok(view) => (view.events, view.state.outputs, view.progress),
        Err(shared) => (
            shared.events.clone(),
            shared.state.outputs.clone(),
            shared.progress.clone(),
        ),
    };
    let detail = RunDetail {
        run_id: id,
        playbook: playbook_id,
        version,
        run_status,
        failure_reason,
        driver_alive,
        nodes,
        worktree,
        outputs,
        instruction: cfg.instruction,
        params,
        redacted_params,
        model: playbook_json,
        layout,
        graph,
        graph_model,
        hooks,
        children,
        commits,
        publish,
        inbox_gaps,
        outcome,
        baseline,
        notes,
        run_outputs,
        lists,
        attempt_models,
        hint_follow,
        isolation,
        read_only,
        unattended,
        goal,
        candidate_trial,
        known_problems,
        launch: apb_engine::gate::launch::run_authorization(&run_dir),
        pause,
        retry_waits,
        released,
        budget,
        notify,
        estimate,
        usage,
        decisions,
        unknown_events,
        driven_by,
        execution,
        execution_fallback,
        progress,
        answer,
        events,
        next_actions,
    };
    // Everything else the page shows (outputs, events, progress, the
    // outcome) without the run's secret values; numbers and paths kept.
    match serde_json::to_value(&detail) {
        Ok(mut v) => {
            redactor.redact_json(&mut v);
            Json(v).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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
    /// The journal seq of the review request this decides (a next
    /// action's `target.request.seq`).
    #[serde(default)]
    request: Option<u64>,
}

/// The answer to a gate's engine refusal: 404 for a node or request that
/// is not there, 409 when the request changed or was already answered,
/// 400 for a decision the gate does not take, 403 for a channel that may
/// not decide it (a person-only gate without a signed-in session);
/// anything else is a server fault.
fn gate_refusal(e: apb_engine::EngineError) -> axum::response::Response {
    use apb_engine::EngineError;
    match e {
        EngineError::NotFound(what) => (StatusCode::NOT_FOUND, what).into_response(),
        EngineError::Conflict(what) => (StatusCode::CONFLICT, what).into_response(),
        e @ EngineError::AlreadyAnswered(_) => {
            (StatusCode::CONFLICT, e.to_string()).into_response()
        }
        EngineError::Invalid(what) => (StatusCode::BAD_REQUEST, what).into_response(),
        EngineError::ChannelRefused { message, .. } => {
            (StatusCode::FORBIDDEN, message).into_response()
        }
        e => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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
    channel: Option<axum::Extension<crate::auth::RequestChannel>>,
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
    // The channel is the request's credential: only a signed-in browser
    // session decides a person-only gate (a keyless dashboard and a bearer
    // key are 403).
    let channel = crate::auth::RequestChannel::of(channel.map(|c| c.0));
    match apb_engine::review::post_decision_via(&run_dir, cmd, body.nonce, body.request, channel) {
        Ok(seq) => Json(serde_json::json!({ "posted_seq": seq })).into_response(),
        Err(e) => gate_refusal(e),
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
    /// The journal seq of the question this answers (a next action's
    /// `target.request.seq`).
    #[serde(default)]
    request: Option<u64>,
}

/// POST /api/runs/{id}/answer: the web facade for answering an interactive
/// `agent_task` node's pending question, always posted as `answered_by:
/// "human"` (the dashboard is a human-facing surface; a supervisor answers
/// through its own MCP tool instead). Delegates to `apb_engine::post_answer`,
/// which owns the `answer_by` policy and the pending-node resolution, so this
/// handler mirrors `post_review_handler` exactly: on failure the engine
/// error's message (including the policy's relay-instruction text) is
/// surfaced verbatim as the response body, with the status of
/// [`gate_refusal`] (404 when no question waits, 409 when `request` names
/// a question a later round replaced or one already answered).
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
    match apb_engine::question::post_answer_to(
        &run_dir,
        body.node.as_deref(),
        &body.answer,
        "human",
        body.request,
    ) {
        Ok(seq) => Json(serde_json::json!({ "posted_seq": seq })).into_response(),
        Err(e) => gate_refusal(e),
    }
}

/// Body of `POST /api/runs/{id}/intents/{intent}/resolve`.
#[derive(Deserialize)]
pub(crate) struct IntentResolveBody {
    /// The send went out (`true`) or it did not.
    sent: bool,
    #[serde(default)]
    note: Option<String>,
}

/// POST /api/runs/{id}/intents/{intent}/resolve: a person's decision on a
/// send nobody can tell went out, through the same tool `apb intent
/// resolve` and MCP `run_intent_resolve` use. 404 for an unknown run or
/// intent, 409 for an intent whose outcome is known or still in flight.
pub(crate) async fn post_intent_resolve_handler(
    State(state): State<AppState>,
    AxPath((id, intent)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<IntentResolveBody>,
) -> impl IntoResponse {
    if let Some(why) = keyless_intent_refusal(&state.auth, &id, &intent) {
        return (StatusCode::FORBIDDEN, why).into_response();
    }
    let run_id = id.clone();
    answer(&state, &id, q.workspace.as_deref(), move |root| {
        apb_mcp::tools::intent::run_intent_resolve(
            &root,
            &run_id,
            &intent,
            body.sent,
            body.note,
            apb_mcp::tools::intent::IntentCaller::person("dashboard"),
        )
    })
    .await
    .into_response()
}

/// Why a dashboard without keys refuses a decision on an unknown send:
/// anything on this machine reaches it, a step's agent included, and the
/// decision lets a repeat of the send go out again or answers it as sent,
/// as a publish consent would push ([`keyless_publish_refusal`]). It points
/// at the CLI; the ids are quoted as shell words, never trusted as written.
fn keyless_intent_refusal(
    auth: &crate::auth::AuthState,
    run_id: &str,
    intent: &str,
) -> Option<String> {
    (!auth.enabled()).then(|| {
        let line = apb_engine::next_action::command::resolve_intent(run_id, Some(intent), None);
        format!(
            "a dashboard without keys does not record a decision on a send, since anything on this machine can reach it; check the service, then run `{}`, or issue a key with `apb server key issue` and sign in",
            line.line()
        )
    })
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
    /// A dashboard without keys resumes a never-authorized `ask` run only
    /// with the `launch_nonce` of its 409 refusal (see
    /// `routes::playbooks::dashboard_launch`).
    #[serde(default)]
    confirm_launch: Option<String>,
    /// Start a deferred retry now (the `retry_now` next action): on a run a
    /// live driver holds the retry starts at once and nothing else happens;
    /// a released or paused run is resumed with the retry due. 409
    /// `no_deferred_retry` when nothing waits to retry.
    #[serde(default)]
    retry_now: bool,
    /// With `retry_now`: only this node's waiting retry (400
    /// `node_without_retry_now` without it).
    #[serde(default)]
    node: Option<String>,
}

/// Who the dashboard's retry-now is journaled as (`retry_expedited.by`): a
/// person (`operator`) only on a dashboard with keys, where every request
/// was signed in. A dashboard without keys takes any request that reaches
/// its loopback port, so, as the launch policy decides it like the CLI
/// without a terminal, its retry-now is recorded as `host`, never a person.
fn retry_now_by(auth: &crate::auth::AuthState) -> apb_engine::scheduler::deferred::ExpediteBy {
    use apb_engine::scheduler::deferred::ExpediteBy;
    if auth.enabled() {
        ExpediteBy::Operator
    } else {
        ExpediteBy::Host
    }
}

/// The retry-now half of a resume request, after the run gate passed:
/// `Err` with the answer when the request ends here (nothing waits to
/// retry, or expedited on a run a live driver holds), else the nodes the
/// resume then starts now. `unused` names the resume arguments a live
/// expedite, being the whole action, leaves unapplied. It reads and folds
/// the journal and may wait on the run's event lock, so it runs off the
/// async runtime.
fn retry_now_first(
    root: &std::path::Path,
    id: &str,
    node: Option<&str>,
    by: apb_engine::scheduler::deferred::ExpediteBy,
    unused: &[(&str, bool)],
) -> Result<Vec<String>, Box<axum::response::Response>> {
    match apb_mcp::tools::run::retry_now_plan(root, id, node) {
        Err(e) => Err(Box::new(crate::state::tool_refusal(e))),
        Ok(Err(refusal)) => {
            let status = if refusal["error"] == "not_found" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::CONFLICT
            };
            Err(Box::new((status, Json(refusal)).into_response()))
        }
        // A driver that let go of the run meanwhile leaves the retry to the
        // resume.
        Ok(Ok(plan)) if plan.live => {
            match apb_mcp::tools::run::retry_now_live(root, id, node, by) {
                Ok(Some(mut v)) => {
                    apb_mcp::tools::run::note_not_applied(&mut v, unused);
                    Err(Box::new(Json(v).into_response()))
                }
                Ok(None) => Ok(plan.nodes),
                Err(e) => Err(Box::new(crate::state::tool_refusal(e))),
            }
        }
        Ok(Ok(plan)) => Ok(plan.nodes),
    }
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
    channel: Option<axum::Extension<crate::auth::RequestChannel>>,
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
    // The order of refusals is MCP `run_resume`'s: the arguments, then the
    // run gate, then "nothing waits to retry".
    if let Some(refusal) = apb_mcp::tools::run::retry_now_arg_refusal(
        body.retry_now,
        body.from_node.is_some(),
        body.node.is_some(),
    ) {
        return (StatusCode::BAD_REQUEST, Json(refusal)).into_response();
    }
    // The run gate first, for a live retry-now too: it writes to the run's
    // journal like any resume.
    if let Err(refusal) = apb_engine::gate::check_resume(&root, &id, true) {
        let status = if refusal["policy"] == "not_found" {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::CONFLICT
        };
        return (status, Json(refusal)).into_response();
    }
    let expedited = if body.retry_now {
        let unused = [
            ("budget", body.budget.is_some()),
            ("confirm_launch", body.confirm_launch.is_some()),
            ("confirm_irreversible", body.confirm_irreversible.is_some()),
            ("allow_environment_drift", body.allow_environment_drift),
        ];
        let (root, id, node) = (root.clone(), id.clone(), body.node.clone());
        let by = retry_now_by(&state.auth);
        let first = tokio::task::spawn_blocking(move || {
            retry_now_first(&root, &id, node.as_deref(), by, &unused)
        })
        .await;
        match first {
            Ok(Ok(nodes)) => Some(nodes),
            Ok(Err(answer)) => return *answer,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    } else {
        None
    };
    // The launch policy of a run never authorized: the click is the
    // person's launch with keys, the CLI without a terminal's without; the
    // grant reaches the detached driver.
    let launch = super::playbooks::dashboard_launch(
        &state.auth,
        apb_engine::gate::launch::LaunchKind::Resume { run_id: id.clone() },
        body.confirm_launch.clone(),
        None,
        None,
    );
    let grant = match apb_engine::resume_launch::check_resume_launch(&root, &id, &launch) {
        Ok(g) => g,
        Err(refusal) => return (StatusCode::CONFLICT, Json(refusal)).into_response(),
    };
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
    // A resume from a node past a person-only or approving gate no decision
    // passed: only a signed-in session goes past it.
    if let Some(n) = from_node
        && let Err(e) = apb_engine::approval::refuse_resume_skip(
            &root.join(".apb/runs").join(&id),
            n,
            crate::auth::RequestChannel::of(channel.map(|c| c.0)),
        )
    {
        return gate_refusal(e);
    }
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
        launch: grant.as_ref(),
        // The waiting retry is due with `run_resumed`, once every check of
        // the resume passed.
        retry_now: expedited.as_ref().map(|_| apb_engine::scheduler::RetryNow {
            node: body.node.as_deref(),
            by: retry_now_by(&state.auth),
        }),
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
            if let Some(nodes) = expedited {
                answer["retry_now"] = serde_json::json!(nodes);
            }
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
        Ok(report) => {
            let run_dir = root.join(".apb/runs").join(&id);
            let report =
                apb_engine::run_redact::RunRedactor::for_run(&root, &run_dir).redact_str(&report);
            Json(serde_json::json!({ "report": report })).into_response()
        }
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
