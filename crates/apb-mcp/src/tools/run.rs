//! Run lifecycle tools: starting a run (foreground, background, supervised),
//! reading its status, events and report, resuming, stopping, and answering
//! the two gates a run can open (an interactive question, a human review).

use std::collections::BTreeMap;
use std::path::Path;

use super::{ToolError, resolve_run_dir};
use apb_core::registry::is_safe_segment;
use apb_engine::control::Control;
use apb_engine::event::read_all;
use apb_engine::run_config::ChildExpectation;
use apb_engine::state::{FailureReason, RunState, RunStatus};
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
) -> Result<Value, ToolError> {
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Autonomous,
        supervisor_expected: false,
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
) -> Result<Value, ToolError> {
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Autonomous,
        supervisor_expected: false,
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
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let state = RunState::fold(&events);
    // Liveness overlay (Task 9 / issue #45 findings 9 and 10). The pure fold
    // is replayable from the journal alone; these read the process table (and
    // parent-drive markers) at request time, which is precisely why they are
    // applied here rather than folded into `RunState`.
    //
    // `reported_*` re-promotes a live open attempt from the pure-fold
    // `interrupted` crash shape back to `running`, and maps a dead attempt
    // pid to `lost`. `driver_alive` also understands parent-driven children.
    let node_times = apb_engine::liveness::node_times(&events);
    let driver_alive = apb_engine::liveness::driver_alive(&dir, run_id);
    let nodes = apb_engine::liveness::reported_node_statuses(&events);
    let run_status = apb_engine::liveness::reported_run_status(&events);
    let progress = apb_engine::progress::from_run_dir(&dir, &events);
    // Lifted out of `progress` to the top level (spec 2026-07-20-interactive-
    // nodes, Task 8): callers that only care about the pending question
    // (`run_answer`'s caller, the web) do not have to drill into `progress`.
    // `progress` itself still carries it too (`progress.pending_question`),
    // unchanged.
    let cfg = apb_engine::run_config::read_run_config(&dir).unwrap_or_default();
    let pending_question = progress.as_ref().and_then(|p| p.pending_question.clone());
    // Lifted to the top level like `pending_question` (issue #42 finding 4):
    // a human_review gate must be first-class here so an intermediary that
    // calls `run_status` is forced to see the pending decision, its options,
    // and how to answer - the gate no longer waits silently forever.
    let pending_review = progress.as_ref().and_then(|p| p.pending_review.clone());
    // Supervised failure/timeout park (issue #45 finding 4): same first-class
    // lift so the wake is never only buried under a silent "running" status.
    let pending_supervisor = progress.as_ref().and_then(|p| p.pending_supervisor.clone());
    let answer = apb_engine::progress::run_answer(&dir, &events);
    let children: Vec<Value> = events
        .iter()
        .filter_map(|e| match &e.payload {
            apb_engine::event::EventPayload::ChildRunStarted { node_id, run_id } => {
                let child_dir = dir.parent().map(|p| p.join(run_id));
                let status = child_dir
                    .and_then(|d| read_all(&d).ok())
                    .map(|ev| {
                        apb_engine::liveness::reported_run_status(&ev)
                            .as_str()
                            .to_string()
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                Some(json!({ "node_id": node_id, "run_id": run_id, "status": status }))
            }
            _ => None,
        })
        .collect();
    // The verbatim reason behind a `failed` run (issue #42 finding 3): every
    // scheduler/prepare path that fails a run now appends a `RunError` before
    // its terminal `run_finished(failed)`, so an operator reads why directly
    // from run_status instead of grepping events.jsonl by hand. `None` for
    // anything other than a failed run, and for a failed run whose log
    // predates this fix (no `RunError` was ever appended for it).
    let failure_reason = (run_status == RunStatus::Failed)
        .then(|| state.failure_reason.as_ref().map(FailureReason::display))
        .flatten();
    Ok(json!({
        "run_id": run_id,
        "run_status": run_status.as_str(),
        "nodes": nodes,
        "node_times": node_times,
        "driver_alive": driver_alive,
        "outputs": state.outputs,
        "progress": progress,
        "pending_question": pending_question,
        "pending_review": pending_review,
        "pending_supervisor": pending_supervisor,
        "answer": answer,
        "children": children,
        "continued_from": cfg.continued_from,
        "superseded_by": cfg.superseded_by,
        "failure_reason": failure_reason,
    }))
}

pub fn run_events(root: &Path, run_id: &str, from_seq: Option<u64>) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let from = from_seq.unwrap_or(0);
    let filtered: Vec<&_> = events.iter().filter(|e| e.seq >= from).collect();
    Ok(
        json!({ "events": serde_json::to_value(filtered).map_err(|e| ToolError::Engine(e.to_string()))? }),
    )
}

fn node_kind_label(kind: &apb_core::schema::NodeKind) -> &'static str {
    use apb_core::schema::NodeKind::*;
    match kind {
        Start => "start",
        AgentTask { .. } => "agent_task",
        Script { .. } => "script",
        Prompt { .. } => "prompt",
        Condition { .. } => "condition",
        HumanReview { .. } => "human_review",
        Wait { .. } => "wait",
        Finish { .. } => "finish",
        Playbook { .. } => "playbook",
    }
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
                "kind": node_kind_label(&n.kind),
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
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let state = RunState::fold(&events);
    let pb = apb_engine::progress::load_run_playbook(&dir);
    let progress = pb
        .as_ref()
        .map(|p| apb_engine::progress::compute(p, &events));
    let answer = apb_engine::progress::run_answer(&dir, &events);

    let nodes: BTreeMap<String, String> = state
        .nodes
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
        .collect();
    let mut base = json!({
        "run_id": run_id,
        "run_status": state.run_status.as_str(),
        "nodes": nodes,
        "outputs": state.outputs,
        "progress": progress,
        "answer": answer,
    });

    // duration_table is always present (empty when there is no snapshot), as
    // before; it is now built from the single events read above.
    let table = match &pb {
        Some(playbook) => {
            let measured = apb_engine::progress::node_durations_seconds(&events);
            build_duration_table_from(playbook, &measured)
        }
        None => Vec::new(),
    };
    if let Some(obj) = base.as_object_mut() {
        obj.insert("duration_table".into(), json!(table));
    }
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
) -> Result<Value, ToolError> {
    // supervise:"self" does not spawn a separate supervisor agent process - the supervisor here is the same
    // MCP session that called playbook_run, hence supervisor_expected: false
    // (heartbeat oversight in drive does not touch this path).
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Supervised,
        supervisor_expected: false,
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

    #[test]
    fn run_report_propagates_unreadable_events() {
        // B7: an unreadable/corrupt event log surfaces as an error, not an
        // empty duration table masquerading as "no measurements".
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n").unwrap();
        std::fs::write(run_dir.join("events.jsonl"), "this is not json\n").unwrap();
        let err = run_report(tmp.path(), "r1").unwrap_err();
        assert!(matches!(err, ToolError::Engine(_)), "got {err:?}");
    }
}
