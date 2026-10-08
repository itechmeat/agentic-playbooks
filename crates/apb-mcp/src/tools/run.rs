//! Run lifecycle tools: starting a run (foreground, background, supervised),
//! reading its status, events and report, resuming, stopping, and answering
//! the two gates a run can open (an interactive question, a human review).

use std::collections::BTreeMap;
use std::path::Path;

use super::{ToolError, resolve_run_dir};
use apb_core::registry::is_safe_segment;
use apb_engine::control::Control;
use apb_engine::run_config::ChildExpectation;
use apb_engine::run_view::read_events;
use apb_engine::{
    RunMode, RunOptions, list_runs, plan_resume, post_supervisor_command, run, stop_run,
};
use serde_json::{Value, json};

#[allow(clippy::too_many_arguments)]
pub fn playbook_run(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
    expected_digest: Option<String>,
    expected_profile_bundles: Option<BTreeMap<String, String>>,
    expected_children: Option<BTreeMap<String, ChildExpectation>>,
    expected_connectors: BTreeMap<String, String>,
    expected_connector_accounts: BTreeMap<String, String>,
    continued_from: Option<String>,
    worktree: Option<String>,
    // host execution mode (0.23.0)
    execution: apb_core::execution::ExecutionRequest,
    // 0.24.0: the irreversible consent the caller obtained from the person.
    consent: Option<apb_engine::consent::RunConsent>,
    // Issue #192: why the gate left a waiting candidate out (the permit's
    // `candidate_skipped`), recorded in the manifest for `run_status`.
    candidate_skipped: Option<String>,
) -> Result<Value, ToolError> {
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Autonomous,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        expected_digest,
        expected_profile_bundles,
        parent_run: None,
        continued_from,
        depth: 0,
        expected_children,
        expected_connectors,
        expected_connector_accounts,
        cache: Default::default(),
        max_parallel: None,
        // Fail-fast on a busy workdir: this caller is a person waiting on the
        // answer, who can retry, not an event source whose event dies with the
        // refusal (see `RunOptions::workdir_queue_wait`).
        workdir_queue_wait: None,
        worktree,
        execution,
        consent,
        eval: None,
        candidate_skipped,
    };
    let res = run(root, id, version, opts)?;
    Ok(json!({ "run_id": res.run_id, "outcome": res.outcome.as_str() }))
}

/// A non-blocking run start for a regular (non-supervised) MCP client:
/// starts the playbook (autonomous) and returns run_id immediately. The client
/// then polls `run_status`/`run_events` and resolves reviews via `review_decide`.
/// Needed because some hosts (e.g. ChatGPT Apps) have a tool-call timeout of
/// ~60s, while a run can take minutes (design doc, section 13.5).
///
/// The run is driven by a DETACHED process, not a thread of this one: the
/// policy gate, permit verification and manifest snapshot all complete here,
/// in-process, and only the drive loop is handed across - so an `apb mcp`
/// bound to a chat session that dies no longer takes the run with it.
#[allow(clippy::too_many_arguments)]
pub fn playbook_run_background(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
    expected_digest: Option<String>,
    expected_profile_bundles: Option<BTreeMap<String, String>>,
    expected_children: Option<BTreeMap<String, ChildExpectation>>,
    expected_connectors: BTreeMap<String, String>,
    expected_connector_accounts: BTreeMap<String, String>,
    continued_from: Option<String>,
    worktree: Option<String>,
    // host execution mode (0.23.0)
    execution: apb_core::execution::ExecutionRequest,
    // 0.24.0: the irreversible consent the caller obtained from the person.
    consent: Option<apb_engine::consent::RunConsent>,
    // Issue #192: why the gate left a waiting candidate out (the permit's
    // `candidate_skipped`), recorded in the manifest for `run_status`.
    candidate_skipped: Option<String>,
) -> Result<Value, ToolError> {
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Autonomous,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        expected_digest,
        expected_profile_bundles,
        parent_run: None,
        continued_from,
        depth: 0,
        expected_children,
        expected_connectors,
        expected_connector_accounts,
        cache: Default::default(),
        max_parallel: None,
        // Fail-fast on a busy workdir: this caller is a person waiting on the
        // answer, who can retry, not an event source whose event dies with the
        // refusal (see `RunOptions::workdir_queue_wait`).
        workdir_queue_wait: None,
        worktree,
        execution,
        consent,
        eval: None,
        candidate_skipped,
    };
    let run_id = apb_engine::start_detached(root, id, version, opts)?;
    Ok(json!({ "run_id": run_id }))
}

pub fn runs_list(root: &Path) -> Result<Value, ToolError> {
    let runs = list_runs(root)?;
    serde_json::to_value(runs).map_err(|e| ToolError::Engine(e.to_string()))
}

pub fn run_status(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    // The run view every status surface reports from (`apb runs`, `apb wait`,
    // the dashboard): the pure fold is replayable from the journal alone, and
    // the liveness overlay on top of it reads the process table (and
    // parent-drive markers) at request time. A live open attempt or a run
    // parked on a wait with a live driver reads `running`, a dead attempt
    // pid `lost`, a dead driver `interrupted`.
    let view = apb_engine::run_view::RunView::load(&dir, run_id)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    let node_times = apb_engine::liveness::node_times(&view.events);
    let progress = view.progress.as_ref();
    let cfg = apb_engine::run_config::read_run_config(&dir).unwrap_or_default();
    // Lifted out of `progress` to the top level (spec 2026-07-20-interactive-
    // nodes, Task 8; issue #42 finding 4; issue #45 finding 4): an
    // intermediary that calls `run_status` must see a pending question,
    // human_review gate or supervisor decision first-class, not buried under
    // a silent "running". `progress` still carries them too.
    let pending_question = progress.and_then(|p| p.pending_question.clone());
    let pending_review = progress.and_then(|p| p.pending_review.clone());
    let pending_supervisor = progress.and_then(|p| p.pending_supervisor.clone());
    let answer = apb_engine::progress::run_answer(&dir, &view.events);
    // The verbatim reason behind a `failed` run (issue #42 finding 3), read
    // straight from the journal's last `RunError`.
    let failure_reason = view.failure_reason();
    let mut out = json!({
        "run_id": run_id,
        "run_status": view.run_status.as_str(),
        "nodes": view.nodes(),
        "node_times": node_times,
        "driver_alive": view.driver_alive,
        // The run's working tree (issue #67 item 8): where its agent and
        // script nodes run and what its busy lock covers; null means the
        // project root.
        "worktree": view.state.worktree,
        "outputs": view.state.outputs,
        "progress": view.progress,
        "pending_question": pending_question,
        "pending_review": pending_review,
        "pending_supervisor": pending_supervisor,
        "answer": answer,
        "children": view.children(&dir),
        "continued_from": cfg.continued_from,
        "superseded_by": cfg.superseded_by,
        "failure_reason": failure_reason,
    });
    // Host execution mode (0.23.0): the run's execution block (host mode, or
    // a `cli` run with the host fallback) and the tasks the host executes,
    // prompts inline. `pending_tasks` is there only while a task waits, as in
    // `run_wait` and `supervisor_wait_event`, so a `cli` run's status reads
    // as before whenever nothing fell back to the host.
    let mut execution = run_execution(&dir);
    if let Some(obj) = execution.as_object_mut() {
        // 0.24.0: the nodes that actually fell back, so the JSON carries what
        // the `apb runs <id>` text line names.
        let fell_back = apb_engine::run_view::fell_back_nodes(&view.events);
        if !fell_back.is_empty() {
            obj.insert("fell_back".to_string(), json!(fell_back));
        }
    }
    if !execution.is_null() {
        out["execution"] = execution;
    }
    if let Some(p) = progress.filter(|p| !p.pending_tasks.is_empty()) {
        out["pending_tasks"] = json!(p.pending_tasks);
    }
    add_journal_extras(&mut out, &view);
    add_outcome_blocks(&mut out, &view, &dir);
    add_candidate_trial(&mut out, &view, &dir);
    Ok(out)
}

/// Issue #192: the candidate trial this run was, with its verdict; absent
/// for every other run.
fn add_candidate_trial(out: &mut Value, view: &apb_engine::run_view::RunView, dir: &Path) {
    if let Some(t) = apb_engine::candidate::trial_of(dir, &view.events) {
        out["candidate_trial"] = json!(t);
    }
    if let Some(why) = apb_engine::candidate::skipped_of(dir) {
        out["candidate_skipped"] = json!(why);
    }
}

// --- 0.23.0: run outcome blocks (C1, C7) ---

/// The run's goal with the criteria results (C1) and the commits its nodes
/// made (C7), each only when there is one: a run of a playbook without a
/// goal on a tree without commits reads as before. Shared by `run_status`
/// and `run_report`.
fn add_outcome_blocks(out: &mut Value, view: &apb_engine::run_view::RunView, dir: &Path) {
    if let Some(goal) = view.goal(dir) {
        out["goal"] = json!(goal);
    }
    let commits = view.commits();
    if !commits.is_empty() {
        out["commits"] = json!(commits);
    }
    // Issue #193: the model each attempt actually ran on.
    let models = apb_engine::attempt_models::run_attempt_models(dir, &view.events);
    if !models.is_empty() {
        let mismatches = models.iter().filter(|a| a.mismatch).count();
        out["attempt_models"] = json!(models);
        if mismatches > 0 {
            out["model_mismatch"] = json!(mismatches);
        }
    }
}

// --- end of the 0.23.0 blocks ---

/// The fields a run view carries only when they apply: the token usage its
/// attempts reported, the decision-model totals, and the note about events a newer apb wrote that this
/// binary skipped. Shared by `run_status` and `run_report`.
fn add_journal_extras(out: &mut Value, view: &apb_engine::run_view::RunView) {
    if let Some(usage) = view.usage() {
        out["usage"] = json!(usage);
    }
    // One compact object; each decision's detail stays in `run_events`.
    if let Some(decisions) = view.decisions() {
        out["decisions"] = json!(decisions);
    }
    if !view.unknown.is_empty() {
        out["unknown_events"] = json!(view.unknown.len());
        let n = view.unknown.len();
        let events = if n == 1 { "event" } else { "events" };
        out["unknown_events_note"] = json!(format!(
            "{n} unknown {events} (newer apb?): skipped by this binary"
        ));
    }
}

pub use apb_engine::run_wait::{RUN_WAIT_DEFAULT_MS, RUN_WAIT_MAX_MS};

// --- host execution mode (0.23.0) ---
/// `run_wait`'s and `supervisor_wait_event`'s `next` when the run waits for
/// the host to execute tasks. It opens with the engine's execution contract
/// (`apb_engine::host_task_contract`), the one wording every surface shares.
pub const HOST_TASK_NEXT: &str = concat!(
    "execute pending_tasks. ",
    apb_engine::host_task_contract!(),
    " For each task tell your subagent to read prompt_path as its task and role_path as its system context (role_prompt and prompt carry the same text inline unless you passed inline_prompt: false), have it load skills and work in workdir with env set (hint_note labels model_hint; fallback_of says why the previous step closed). Submit the final reply verbatim with run_task_submit (status succeeded, failed, or blocked with the question for the user), then call run_wait again. Independent tasks may run concurrently"
);

/// Drops the inline `prompt` and `role_prompt` of every pending task in a
/// `run_wait` or `supervisor_wait_event` answer, for a host that passes
/// `inline_prompt: false` and reads `prompt_path` and `role_path` instead
/// (issue #193).
pub fn drop_inline_prompts(out: &mut Value) {
    if let Some(tasks) = out.get_mut("pending_tasks").and_then(Value::as_array_mut) {
        for t in tasks.iter_mut().filter_map(Value::as_object_mut) {
            t.remove("prompt");
            t.remove("role_prompt");
        }
    }
}

/// The execution block of a run's manifest, for `run_status`: `null` for a
/// plain `cli` run.
fn run_execution(run_dir: &Path) -> Value {
    match apb_engine::manifest::read(run_dir) {
        Ok(Some(m)) => json!(m.execution),
        _ => Value::Null,
    }
}

/// Records a host's reply to a pending host task (`run_task_submit`). The
/// task may belong to the run or to one of its sub-playbook runs.
#[allow(clippy::too_many_arguments)]
pub fn run_task_submit(
    root: &Path,
    run_id: &str,
    task_id: &str,
    status: &str,
    output: String,
    usage: Option<apb_engine::host_task::SubmittedUsage>,
    note: Option<String>,
    submitted_by: &str,
    client: Option<String>,
    model: Option<String>,
) -> Result<Value, ToolError> {
    let Some(status) = apb_engine::host_task::SubmitStatus::parse(status) else {
        return Ok(json!({
            "error": "unknown_status",
            "detail": format!("status must be succeeded, failed or blocked, got `{status}`"),
        }));
    };
    let receipt = apb_engine::host_task::submit_to_run(
        root,
        run_id,
        apb_engine::host_task::SubmitRequest {
            task_id: task_id.to_string(),
            status,
            output,
            usage,
            note,
            submitted_by: submitted_by.to_string(),
            client,
            model,
        },
    )?;
    Ok(json!({
        "run_id": run_id,
        "task_id": receipt.task_id,
        "node": receipt.node,
        "status": receipt.status,
        "next": "call run_wait again: it returns the next pending task, a question, a gate, or the end of the run",
    }))
}
// --- end host execution mode ---

/// Clamps a caller's `timeout_ms` for `run_wait`/`supervisor_wait_event`.
pub fn wait_timeout(timeout_ms: Option<u64>) -> std::time::Duration {
    std::time::Duration::from_millis(
        timeout_ms
            .unwrap_or(RUN_WAIT_DEFAULT_MS)
            .min(RUN_WAIT_MAX_MS),
    )
}

/// The compact `run_wait` answer: why it returned and only what the caller
/// needs to act on (the gate to answer, the final answer, the failure), not
/// the full `run_status` with every node output. A caller that wants the
/// detail calls `run_status`/`run_report` once, not on every wake.
pub fn run_wait_result(
    root: &Path,
    run_id: &str,
    res: &apb_engine::run_wait::RunWaitResult,
) -> Result<Value, ToolError> {
    use apb_engine::run_wait::WaitReason;
    // Built from the observation the wait decided on, not a second read: a
    // gate decided in between must not leave `reason: needs_input` without
    // the `pending_*` it is about.
    let view = &res.view;
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for s in view.nodes().into_values() {
        *counts.entry(s).or_default() += 1;
    }
    let next = match res.reason {
        WaitReason::Finished => {
            "done: the run is over; call run_report only if you need the details"
        }
        WaitReason::NeedsInput => match res.needs {
            Some(apb_engine::run_wait::NeedsInput::Question) => {
                "answer pending_question with run_answer, then call run_wait again"
            }
            Some(apb_engine::run_wait::NeedsInput::Review) => {
                "relay pending_review to the user, record it with review_decide, then call run_wait again"
            }
            Some(apb_engine::run_wait::NeedsInput::HostTask) => HOST_TASK_NEXT,
            _ => "the run is parked for a supervisor decision (pending_supervisor)",
        },
        WaitReason::Stopped if res.driver_alive == Some(false) => {
            "the run's driver is dead, so the run is interrupted; run_resume continues it"
        }
        WaitReason::Stopped => "the run is paused or has no live driver; run_resume continues it",
        WaitReason::Timeout => {
            "still running: call run_wait again with the same arguments; do not poll run_status"
        }
    };
    let mut out = json!({
        "run_id": run_id,
        "reason": res.reason,
        // The status the wait decided on, not a second read's: a run that
        // stopped on a dead driver must never come back as `running`.
        "run_status": res.status.as_str(),
        "driver_alive": res.driver_alive,
        "waited_ms": res.waited.as_millis() as u64,
        "nodes": counts,
        "next": next,
    });
    // Which answer channel a `needs_input` is about (`question`, `review`,
    // `supervisor`, `host_task`), as the docs and the tool description name it.
    if res.reason == WaitReason::NeedsInput
        && let Some(needs) = res.needs
    {
        out["needs"] = json!(needs.as_str());
    }
    let progress = view.progress.as_ref();
    let dir = resolve_run_dir(root, run_id)?;
    let fields = [
        (
            "pending_question",
            json!(progress.and_then(|p| p.pending_question.clone())),
        ),
        (
            "pending_review",
            json!(progress.and_then(|p| p.pending_review.clone())),
        ),
        (
            "pending_supervisor",
            json!(progress.and_then(|p| p.pending_supervisor.clone())),
        ),
        (
            "pending_tasks",
            progress
                .filter(|p| !p.pending_tasks.is_empty())
                .map(|p| json!(p.pending_tasks))
                .unwrap_or(Value::Null),
        ),
        ("failure_reason", json!(view.failure_reason())),
        (
            "answer",
            json!(apb_engine::progress::run_answer(&dir, &view.events)),
        ),
    ];
    for (key, value) in fields {
        if !value.is_null() {
            out[key] = value;
        }
    }
    Ok(out)
}

pub fn run_events(root: &Path, run_id: &str, from_seq: Option<u64>) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let events = read_events(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let from = from_seq.unwrap_or(0);
    let filtered: Vec<&_> = events.iter().filter(|e| e.seq >= from).collect();
    Ok(
        json!({ "events": serde_json::to_value(filtered).map_err(|e| ToolError::Engine(e.to_string()))? }),
    )
}

/// Per-node expected vs measured durations for calibration (spec 5). Measured
/// comes from the run's events; expected from the playbook version bound to
/// the run. The maintaining agent uses this to update estimates via
/// playbook_update; the engine never rewrites the playbook.
pub(crate) fn build_duration_table_from(
    playbook: &apb_core::schema::Playbook,
    measured: &BTreeMap<String, u64>,
) -> Vec<Value> {
    playbook
        .nodes
        .iter()
        .map(|n| {
            json!({
                "node": n.id,
                "kind": n.kind.type_str(),
                "expected_seconds": n.expected_seconds(),
                "measured_seconds": measured.get(&n.id),
            })
        })
        .collect()
}

pub fn run_report(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    // There is no supervisor agent in Phase 3: the report is a light state
    // summary. The full supervisor report is Phase 4. events.jsonl is read once
    // and the playbook snapshot parsed once here; a failing events read
    // propagates as a ToolError rather than masquerading as an empty duration
    // table (B7). The base object mirrors `run_status`'s JSON shape exactly.
    let dir = resolve_run_dir(root, run_id)?;
    // The same run view `run_status` reports from, so the two tools never
    // disagree about one run: status and nodes with the liveness overlay,
    // progress with the pending question and child credit.
    let view = apb_engine::run_view::RunView::load(&dir, run_id)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    let events = &view.events;
    let pb = apb_engine::progress::load_run_playbook(&dir);
    let answer = apb_engine::progress::run_answer(&dir, events);
    let mut base = json!({
        "run_id": run_id,
        "run_status": view.run_status.as_str(),
        "nodes": view.nodes(),
        "outputs": view.state.outputs,
        "progress": view.progress,
        "answer": answer,
    });
    add_journal_extras(&mut base, &view);
    add_outcome_blocks(&mut base, &view, &dir);

    // duration_table is always present (empty when there is no snapshot), as
    // before; it is now built from the single events read above.
    let table = match &pb {
        Some(playbook) => {
            let measured = apb_engine::progress::node_durations_seconds(events);
            build_duration_table_from(playbook, &measured)
        }
        None => Vec::new(),
    };
    if let Some(obj) = base.as_object_mut() {
        obj.insert("duration_table".into(), json!(table));
    }

    // The goal with each criterion's result (C1) is in `add_outcome_blocks`,
    // shared with `run_status`.

    Ok(base)
}

pub fn run_resume(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    allow_environment_drift: bool,
) -> Result<Value, ToolError> {
    // Compute the resume decision up front so the ack reports where and why the
    // run resumes. This must run BEFORE the drive: once the run reaches a
    // terminal state, an argument-free `plan_resume` would refuse it.
    let decision = plan_resume(root, run_id, from_node)?;
    // The drive itself happens in a separate OS process: this session may be a
    // chat host that dies at any moment, and a resumed run must not die with
    // it. The ack is what the caller gets back, immediately - the run's
    // progress is read afterwards through `run_status` / `run_events`.
    // A stop still sitting unapplied in the control queue is consumed by the
    // resumed drive BEFORE it executes anything, so the run stops again
    // immediately. Read it before spawning the driver (afterwards the driver
    // races us to consume it) and say so in the ack, or the caller sees a
    // successful resume followed by a run that never moved.
    let pending_stop =
        apb_engine::control::pending_stop_seq(&root.join(".apb/runs").join(run_id))?.is_some();
    // The drift preflight runs inside resume_detached_with: a drift the caller
    // did not allow is returned as an Err HERE (issue #45 finding 3), instead
    // of the old detached spawn whose child failed its own check on null stdio
    // and left this ack reporting `detached: true` for a run that never moved.
    apb_engine::resume_detached_with(root, run_id, from_node, allow_environment_drift)?;
    let mut ack = json!({
        "run_id": run_id,
        "resumed_from": decision.start_node,
        "reason": decision.reason.as_str(),
        "detached": true,
    });
    if allow_environment_drift && let Some(obj) = ack.as_object_mut() {
        obj.insert(
            "note".into(),
            json!(
                "environment drift override accepted: an agent binary changed since run start, and resume is proceeding anyway; the accepted drift is recorded in the run event log"
            ),
        );
    }
    if pending_stop && let Some(obj) = ack.as_object_mut() {
        obj.insert("stops_on_pending_abort".into(), json!(true));
        obj.insert(
            "note".into(),
            json!(
                "a stop was still pending on this run, so this resume applies it and the run stops again without executing anything; call run_resume once more to continue past it"
            ),
        );
    }
    Ok(ack)
}

/// Starts a playbook in supervised mode without waiting for it to finish, on a
/// detached driver process (see `playbook_run_background`). The supervisor
/// access token is minted by the server layer (Phase 4b, Task 3), not this
/// function.
#[allow(clippy::too_many_arguments)]
pub fn playbook_run_supervised(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
    expected_digest: Option<String>,
    expected_profile_bundles: Option<BTreeMap<String, String>>,
    expected_children: Option<BTreeMap<String, ChildExpectation>>,
    expected_connectors: BTreeMap<String, String>,
    expected_connector_accounts: BTreeMap<String, String>,
    continued_from: Option<String>,
    worktree: Option<String>,
    // host execution mode (0.23.0)
    execution: apb_core::execution::ExecutionRequest,
    // 0.24.0: the irreversible consent the caller obtained from the person.
    consent: Option<apb_engine::consent::RunConsent>,
    // Issue #192: why the gate left a waiting candidate out (the permit's
    // `candidate_skipped`), recorded in the manifest for `run_status`.
    candidate_skipped: Option<String>,
) -> Result<Value, ToolError> {
    // supervise:"self" does not spawn a separate supervisor agent process - the supervisor here is the same
    // MCP session that called playbook_run, hence RunMode::Supervised, not AgentSupervised
    // (heartbeat oversight in drive does not touch this path).
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Supervised,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        expected_digest,
        expected_profile_bundles,
        parent_run: None,
        continued_from,
        depth: 0,
        expected_children,
        expected_connectors,
        expected_connector_accounts,
        cache: Default::default(),
        max_parallel: None,
        // Fail-fast on a busy workdir: this caller is a person waiting on the
        // answer, who can retry, not an event source whose event dies with the
        // refusal (see `RunOptions::workdir_queue_wait`).
        workdir_queue_wait: None,
        worktree,
        execution,
        consent,
        eval: None,
        candidate_skipped,
    };
    let run_id = apb_engine::start_detached(root, id, version, opts)?;
    Ok(json!({ "run_id": run_id }))
}

/// Stops a run and reports what that took: signaling a live driver (whose
/// watcher interrupts the in-flight node), finalizing a run whose driver is
/// gone, or nothing at all for an already terminal run. Unlike
/// `supervisor_run_abort` this needs no supervisor session - it is the
/// operator-facing stop, the same one `apb stop` calls.
pub fn run_stop(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let outcome = stop_run(root, run_id)?;
    Ok(json!({ "run_id": run_id, "outcome": outcome.as_str() }))
}

/// Answers a pending interactive question on a run (spec
/// 2026-07-20-interactive-nodes): writes a command into the run's
/// answers.jsonl channel via `apb_engine::post_answer`. `node` omitted
/// resolves to the single pending question. The `answer_by` policy (a node
/// declaring `answer_by: human` rejects `answered_by: "supervisor"`, with an
/// error instructing the supervisor to relay the question to the user) is
/// enforced inside `post_answer`, not here - every facade (this MCP tool,
/// `apb answer`, the web API) shares that one enforcement point, so it
/// cannot be bypassed by a facade that forgets to check it.
pub fn run_answer(
    root: &Path,
    run_id: &str,
    node: Option<&str>,
    answer: &str,
    answered_by: &str,
) -> Result<Value, ToolError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let seq = apb_engine::post_answer(&run_dir, node, answer, answered_by)?;
    Ok(json!({ "posted_seq": seq }))
}

/// A human_review node decision: writes a command into the run's reviews.jsonl channel.
/// A regular run tool (not supervised): takes run_id directly.
pub fn review_decide(
    root: &Path,
    run_id: &str,
    node: &str,
    decision: &str,
    note: &str,
) -> Result<Value, ToolError> {
    if !is_safe_segment(run_id) {
        return Err(ToolError::NotFound(run_id.to_string()));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(ToolError::NotFound(run_id.to_string()));
    }
    let seq = apb_engine::post_review(
        &run_dir,
        apb_engine::ReviewCommand {
            node: node.to_string(),
            decision: decision.to_string(),
            note: note.to_string(),
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

/// Reports cycle progress for the run's currently executing node group. Posts
/// a `Control::Progress` command; drive stamps the node and appends the
/// `RunProgress` event (single-writer). Callable by the executing agent or the
/// supervisor.
pub fn run_progress_report(
    root: &Path,
    run_id: &str,
    done: u64,
    total: u64,
    label: Option<String>,
    node: Option<String>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Progress {
            done,
            total,
            label,
            node,
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn run_progress_report_posts_a_command() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        // minimal events + playbook so resolve_run_dir + run_status succeed
        std::fs::write(
            run_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n",
        )
        .unwrap();
        let out = run_progress_report(tmp.path(), "r1", 2, 5, Some("x".into()), None).unwrap();
        assert!(out.get("posted_seq").is_some());
        let control = std::fs::read_to_string(run_dir.join("control.jsonl")).unwrap();
        assert!(control.contains("\"cmd\":\"progress\""));
    }

    #[test]
    fn run_report_includes_duration_table() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n").unwrap();
        std::fs::write(run_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":1000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}\n{\"seq\":2,\"ts\":6000,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n").unwrap();
        let out = run_report(tmp.path(), "r1").unwrap();
        let table = out
            .get("duration_table")
            .and_then(|v| v.as_array())
            .unwrap();
        let a = table.iter().find(|e| e["node"] == "a").unwrap();
        assert_eq!(a["expected_seconds"], 100);
        assert_eq!(a["measured_seconds"], 5);
    }

    /// Issue #165 Part 4: `run_status` and `run_report` carry the decision
    /// totals as one compact object, and only when the run journaled one.
    #[test]
    fn run_status_and_report_carry_decisions_only_when_journaled() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        let start = "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n";
        std::fs::write(run_dir.join("events.jsonl"), start).unwrap();
        assert!(
            run_status(tmp.path(), "r1")
                .unwrap()
                .get("decisions")
                .is_none()
        );
        assert!(
            run_report(tmp.path(), "r1")
                .unwrap()
                .get("decisions")
                .is_none()
        );
        let decision = "{\"seq\":1,\"ts\":1,\"type\":\"decision_made\",\"use_site\":\"completion_check\",\"node\":\"a\",\"attempt\":1,\"provider\":\"main\",\"model\":\"m\",\"calibrated\":true,\"mode\":\"shadow\",\"answers\":{},\"would_change\":true,\"latency_ms\":200,\"cost_usd\":0.0001,\"cached\":false,\"error\":null}\n";
        std::fs::write(run_dir.join("events.jsonl"), format!("{start}{decision}")).unwrap();
        for out in [
            run_status(tmp.path(), "r1").unwrap(),
            run_report(tmp.path(), "r1").unwrap(),
        ] {
            let d = &out["decisions"];
            assert_eq!(d["decisions"], 1);
            assert_eq!(d["requests"], 1);
            assert_eq!(d["p50_latency_ms"], 200);
            assert_eq!(d["by_use"]["completion_check"]["shadow_would_change"], 1);
        }
    }

    /// The report shows the goal with each criterion's result (C1): before
    /// the run reached a finish node a checked criterion reads `pending`.
    #[test]
    fn run_report_includes_the_goal_with_pending_criteria() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\n\
             goal:\n  statement: \"the invoice is filed and sent for approval\"\n  \
             criteria:\n    - description: \"invoice appears in the tracking sheet\"\n      \
             check: { type: marker, marker: FILED }\n\
             nodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi }\n  \
             - { id: f, type: finish, outcome: success }\n\
             edges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
        )
        .unwrap();
        std::fs::write(
            run_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n",
        )
        .unwrap();
        let out = run_report(tmp.path(), "r1").unwrap();
        let goal = out.get("goal").expect("goal block must be present");
        assert_eq!(
            goal["statement"],
            "the invoice is filed and sent for approval"
        );
        assert_eq!(goal["checked"], false);
        let criteria = goal["criteria"].as_array().unwrap();
        assert_eq!(
            criteria[0]["description"],
            "invoice appears in the tracking sheet"
        );
        assert_eq!(criteria[0]["check"], "marker");
        assert_eq!(criteria[0]["status"], "pending");
    }

    /// A playbook snapshot with no `goal` block emits no `goal` key at all,
    /// rather than a null or empty placeholder.
    #[test]
    fn run_report_omits_goal_when_playbook_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n").unwrap();
        std::fs::write(
            run_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n",
        )
        .unwrap();
        let out = run_report(tmp.path(), "r1").unwrap();
        assert!(out.get("goal").is_none());
    }

    /// Issue #42 finding 3: `run_status` must expose the terminal error for a
    /// failed run directly, rather than making an operator open events.jsonl
    /// by hand to find the `run_error` event.
    #[test]
    fn run_status_exposes_failure_reason_for_a_failed_run() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":0,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":1000,"type":"node_started","node":"a","attempt":1}"#,
                "\n",
                r#"{"seq":2,"ts":2000,"type":"node_finished","node":"a","status":"failed","attempt":1,"output":"boom"}"#,
                "\n",
                r#"{"seq":3,"ts":2500,"type":"run_error","node":"a","reason":"node `a` has no outgoing edge and is not finish"}"#,
                "\n",
                r#"{"seq":4,"ts":3000,"type":"run_finished","outcome":"failed"}"#,
                "\n",
            ),
        )
        .unwrap();
        let out = run_status(tmp.path(), "r1").unwrap();
        assert_eq!(out["run_status"], "failed");
        let reason = out["failure_reason"]
            .as_str()
            .expect("failure_reason must be a string for a failed run with a recorded RunError");
        assert!(reason.contains("no outgoing edge"));
        assert!(reason.contains("node `a`"));
    }

    /// `failure_reason` stays absent (JSON `null`) for a run that is not
    /// failed - it must not appear on a succeeded/running/paused run.
    #[test]
    fn run_status_omits_failure_reason_for_a_succeeded_run() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":0,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":1000,"type":"run_finished","outcome":"succeeded"}"#,
                "\n",
            ),
        )
        .unwrap();
        let out = run_status(tmp.path(), "r1").unwrap();
        assert_eq!(out["run_status"], "succeeded");
        assert!(out["failure_reason"].is_null());
    }

    /// #85 finding 4: `runs_list` just wraps `list_runs`, so `driver_dead`
    /// arrives for free through `RunSummary`'s serde shape; this pins that it
    /// actually does - present and `true` for a dead-driver run, absent for a
    /// run with no drive claim at all.
    #[test]
    fn runs_list_serializes_driver_dead_and_omits_it_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_dir = tmp.path().join(".apb/runs");

        let healthy_dir = runs_dir.join("good-1");
        std::fs::create_dir_all(&healthy_dir).unwrap();
        std::fs::write(
            healthy_dir.join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":1,"type":"run_started","playbook":"good","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":2,"type":"run_finished","outcome":"succeeded"}"#,
                "\n",
            ),
        )
        .unwrap();

        // A pid that existed and is provably gone: spawn, wait, reap, reuse.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn a throwaway child to borrow a pid from");
        let dead_pid = child.id();
        child.wait().expect("reap the throwaway child");

        let dead_dir = runs_dir.join("dead-1");
        std::fs::create_dir_all(&dead_dir).unwrap();
        std::fs::write(
            dead_dir.join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":1,"type":"run_started","playbook":"dead","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":2,"type":"node_started","node":"start","attempt":1}"#,
                "\n",
            ),
        )
        .unwrap();
        std::fs::write(dead_dir.join("driver.pid"), format!("{dead_pid}\n")).unwrap();

        let out = runs_list(tmp.path()).unwrap();
        let runs = out.as_array().expect("runs_list returns an array");
        let dead = runs
            .iter()
            .find(|r| r["run_id"] == "dead-1")
            .expect("dead-1 listed");
        assert_eq!(dead["driver_dead"], serde_json::json!(true));
        let good = runs
            .iter()
            .find(|r| r["run_id"] == "good-1")
            .expect("good-1 listed");
        assert!(
            good.get("driver_dead").is_none(),
            "a run with no drive claim at all must omit driver_dead, got: {good:?}"
        );
    }

    #[test]
    fn run_report_propagates_unreadable_events() {
        // B7: an unreadable/corrupt event log surfaces as an error, not an
        // empty duration table masquerading as "no measurements". The broken
        // line has a line after it: a broken LAST line is a line the driver is
        // still appending, which every read-only surface reads through.
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n").unwrap();
        std::fs::write(
            run_dir.join("events.jsonl"),
            concat!(
                "this is not json\n",
                r#"{"seq":1,"ts":2,"type":"run_finished","outcome":"succeeded"}"#,
                "\n",
            ),
        )
        .unwrap();
        let err = run_report(tmp.path(), "r1").unwrap_err();
        assert!(matches!(err, ToolError::Engine(_)), "got {err:?}");
    }
}
