use apb_core::registry::init_project;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventLog, EventPayload, read_all};
use apb_engine::scheduler::{
    ResumeReason, RunOptions, StartMode, list_runs, plan_resume, resume, run,
};
use apb_engine::state::{RunState, RunStatus};
use std::fs;
use std::path::Path;

const PLAYBOOK: &str = r#"
schema: 1
id: lin
name: Lin
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "x" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/lin/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/lin/current"), "1.0.0").unwrap();
}

/// A playbook that ends in a failure `finish` node (`bad`) with no outgoing
/// edge: a real run terminates failed at a node that has no successor, which is
/// exactly the shape an `After`-mode resume must refuse.
const FAIL_PLAYBOOK: &str = r#"
schema: 1
id: fail
name: Fail
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "x" }
  - { id: bad, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: bad }
"#;

fn seed_named(root: &std::path::Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn count_events<F: Fn(&EventPayload) -> bool>(root: &Path, run_id: &str, pred: F) -> usize {
    let events = read_all(&root.join(".apb/runs").join(run_id)).unwrap();
    events.iter().filter(|e| pred(&e.payload)).count()
}

#[test]
fn lists_runs_after_a_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    let runs = list_runs(dir.path()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, res.run_id);
    assert_eq!(runs[0].playbook, "lin");
    assert_eq!(runs[0].status, "succeeded");
}

#[test]
fn resume_from_node_reaches_finish() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    // a repeat pass from node `a` finishes with success (the version snapshot lives inside the run)
    let again = resume(dir.path(), &res.run_id, Some("a")).unwrap();
    assert_eq!(again.run_id, res.run_id);
    assert_eq!(again.outcome, RunStatus::Succeeded);
}

// --- Task 3 resume-rework helpers ---

/// Writes a hand-built journal to `.apb/runs/<run_id>/events.jsonl` via the
/// real `EventLog`, so `plan_resume` can fold a specific crash shape.
fn write_journal(root: &Path, run_id: &str, payloads: Vec<EventPayload>) {
    let dir = root.join(".apb/runs").join(run_id);
    let mut log = EventLog::create(&dir).unwrap();
    for p in payloads {
        log.append(p).unwrap();
    }
}

/// Rewrites a real run's journal to keep only the events up to AND including the
/// first one whose payload matches `pred` - simulating a crash that cut the run
/// short at that point. Leaves the rest of the run dir (playbook.yaml, config,
/// manifest) intact so `resume` can drive again.
fn keep_journal_through<F: Fn(&EventPayload) -> bool>(root: &Path, run_id: &str, pred: F) {
    let dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&dir).unwrap();
    let cut = events
        .iter()
        .position(|e| pred(&e.payload))
        .expect("no matching event to truncate at");
    let mut buf = String::new();
    for e in &events[..=cut] {
        buf.push_str(&serde_json::to_string(e).unwrap());
        buf.push('\n');
    }
    fs::write(dir.join("events.jsonl"), buf).unwrap();
}

fn attempt_started(node: &str) -> EventPayload {
    EventPayload::AttemptStarted {
        node: node.into(),
        attempt: 1,
        agent: "claude-code".into(),
        soul_delivery: None,
        skills_mode: None,
        pid: Some(4242),
    }
}

fn node_finished(node: &str, status: &str) -> EventPayload {
    EventPayload::NodeFinished {
        node: node.into(),
        status: status.into(),
        attempt: 1,
        output: String::new(),
        artifacts: Vec::new(),
    }
}

fn run_started() -> EventPayload {
    EventPayload::RunStarted {
        playbook: "lin".into(),
        version: "1.0.0".into(),
    }
}

/// (a) A single interrupted node (journal ends with its `node_started`, no
/// `node_finished`) resumes at that node, `InterruptedRestart` / `Rerun`.
#[test]
fn resume_plan_interrupted_node_restarts() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    write_journal(
        dir.path(),
        "r-a",
        vec![
            run_started(),
            node_finished("start", "succeeded"),
            EventPayload::NodeStarted {
                node: "a".into(),
                attempt: 1,
            },
        ],
    );
    let d = plan_resume(dir.path(), "r-a", None).unwrap();
    assert_eq!(d.start_node, "a");
    assert_eq!(d.mode, StartMode::Rerun);
    assert_eq!(d.reason, ResumeReason::InterruptedRestart);
}

/// (b) A journal ending in `node_finished` for X resumes at X's successor
/// without re-executing X (exactly one `node_finished` for X survives), reason
/// `AdvancePastFinished` / `After`.
#[test]
fn resume_advance_past_finished_does_not_rerun() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    // Cut the journal right after node `a` finished - `done` never ran.
    keep_journal_through(
        dir.path(),
        &res.run_id,
        |p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "a"),
    );

    let d = plan_resume(dir.path(), &res.run_id, None).unwrap();
    assert_eq!(d.start_node, "a");
    assert_eq!(d.mode, StartMode::After);
    assert_eq!(d.reason, ResumeReason::AdvancePastFinished);

    let again = resume(dir.path(), &res.run_id, None).unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);

    // `a` was NOT re-executed: exactly one node_finished for it in the end.
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let a_finishes = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "a"))
        .count();
    assert_eq!(a_finishes, 1, "node `a` must not be re-executed on resume");
}

/// (c) Two or more interrupted nodes resume from `last_node`, reason
/// `ParallelFallback` / `Rerun` (today's behavior).
#[test]
fn resume_plan_two_interrupted_parallel_fallback() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    write_journal(
        dir.path(),
        "r-c",
        vec![
            run_started(),
            node_finished("start", "succeeded"),
            attempt_started("b"),
            attempt_started("c"),
        ],
    );
    let d = plan_resume(dir.path(), "r-c", None).unwrap();
    assert_eq!(d.start_node, "start");
    assert_eq!(d.mode, StartMode::Rerun);
    assert_eq!(d.reason, ResumeReason::ParallelFallback);
}

/// (d) An argument-free resume of a succeeded run errors, mentioning
/// `--from-node`.
#[test]
fn resume_plan_succeeded_needs_from_node() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    let err = plan_resume(dir.path(), &res.run_id, None).unwrap_err();
    assert!(
        err.to_string().contains("--from-node"),
        "expected an error mentioning --from-node, got: {err}"
    );
}

/// (e) An explicit `from_node` override works on a failed terminal run, reason
/// `ExplicitFromNode` / `Rerun`.
#[test]
fn resume_plan_failed_from_node_override() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    write_journal(
        dir.path(),
        "r-e",
        vec![
            run_started(),
            node_finished("a", "failed"),
            EventPayload::RunFinished {
                outcome: "failed".into(),
            },
        ],
    );
    let d = plan_resume(dir.path(), "r-e", Some("a")).unwrap();
    assert_eq!(d.start_node, "a");
    assert_eq!(d.mode, StartMode::Rerun);
    assert_eq!(d.reason, ResumeReason::ExplicitFromNode);
}

/// (f) After a resume the folded run status is running (via `run_resumed`), not
/// paused: the `run_resumed` marker replaces the old `RunPaused` write.
#[test]
fn resume_folds_to_running_not_paused() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    keep_journal_through(
        dir.path(),
        &res.run_id,
        |p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "a"),
    );
    resume(dir.path(), &res.run_id, None).unwrap();

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let resumed = events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .count();
    let paused = events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::RunPaused { .. }))
        .count();
    assert_eq!(resumed, 1, "resume must journal exactly one run_resumed");
    assert_eq!(paused, 0, "resume must not journal a RunPaused marker");

    // Folding the journal through the run_resumed event yields running, not
    // paused - the exact regression the old RunPaused marker caused.
    let cut = events
        .iter()
        .position(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .unwrap();
    let prefix: Vec<Event> = events[..=cut].to_vec();
    let state = RunState::fold(&prefix);
    assert_eq!(state.run_status, RunStatus::Running);
}

/// Regression: a no-arg `resume()` of a failed terminal run whose last node has
/// no matching successor edge must be refused (mentioning `--from-node`) AND
/// must NOT journal a `RunResumed` marker. Otherwise the marker lands after the
/// terminal `RunFinished(failed)` and folds the run to running forever, with a
/// fresh marker appended on every retry.
#[test]
fn resume_failed_terminal_no_successor_is_refused_without_journaling() {
    let dir = tempfile::tempdir().unwrap();
    seed_named(dir.path(), "fail", FAIL_PLAYBOOK);
    let res = run(dir.path(), "fail", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);

    let is_resumed = |p: &EventPayload| matches!(p, EventPayload::RunResumed { .. });
    assert_eq!(count_events(dir.path(), &res.run_id, is_resumed), 0);

    let err = resume(dir.path(), &res.run_id, None).unwrap_err();
    assert!(
        err.to_string().contains("--from-node"),
        "refusal must mention --from-node, got: {err}"
    );
    // Retry to prove the refusal never accumulates a marker.
    let _ = resume(dir.path(), &res.run_id, None);

    assert_eq!(
        count_events(dir.path(), &res.run_id, is_resumed),
        0,
        "a refused After-mode resume must not journal RunResumed"
    );
    let state = RunState::fold(&read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap());
    assert_eq!(
        state.run_status,
        RunStatus::Failed,
        "folded status must stay failed, never Running"
    );
}

/// The `RunResumed` event serializes with the exact snake_case wire tag
/// `"type":"run_resumed"` - guards against a `rename_all` regression.
#[test]
fn run_resumed_serializes_with_snake_case_tag() {
    let payload = EventPayload::RunResumed {
        from_node: "a".into(),
    };
    let json = serde_json::to_string(&payload).unwrap();
    assert!(
        json.contains(r#""type":"run_resumed""#),
        "expected snake_case tag, got: {json}"
    );
}

#[test]
fn resume_traversal_run_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    // The path-traversal check must fire before the check for the run
    // directory's existence, so a valid run is not required here.
    let err = resume(dir.path(), "../../etc", None).unwrap_err();
    assert!(
        matches!(err, EngineError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}
