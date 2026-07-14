use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::{Control, read_control_after};
use apb_engine::error::EngineError;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run_background, run_cancel};
use apb_engine::state::{RunState, RunStatus};
use apb_engine::workdir::acquire;

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(20);

/// Polls `f` until it returns Some(..) or the deadline elapses; otherwise panics
/// with a clear message instead of hanging.
fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if start.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for: {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

// A pipeline without agent_task: start -> prompt -> finish (the same pattern as in
// scheduler_test.rs::NOAGENT). No real agent is needed - the background run
// proceeds entirely synchronously inside the spawned thread.
const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hello {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
}

// A pipeline with a script node (is_write=true in prepare_run) - unlike NOAGENT
// above, this is the only scenario in the file where Some(WorkdirGuard) is actually taken.
// The script sleeps for a second so the test has a window in which to check that
// the workdir lock is held for the WHOLE background run, not released right after
// run_background returns the run_id.
const SLOWSCRIPT: &str = r#"
schema: 1
id: slowscript
name: Slow Script
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn seed_slowscript(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/slowscript/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), SLOWSCRIPT).unwrap();
    fs::write(vdir.join("scripts/slow.sh"), "#!/bin/sh\nsleep 1\n").unwrap();
    fs::write(root.join(".apb/playbooks/slowscript/current"), "1.0.0").unwrap();
}

// Scenario A: run_background returns the run_id immediately (without waiting for
// completion); the run itself finishes successfully in the background thread.
#[test]
fn run_background_returns_run_id_and_finishes_succeeded() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let mut opts = RunOptions::default();
    opts.params.insert("who".into(), "world".into());

    let run_id = run_background(dir.path(), "noagent", None, opts).unwrap();
    assert!(
        run_id.starts_with("noagent-"),
        "unexpected run_id: {run_id}"
    );

    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    poll_until("a RunFinished event in events.jsonl", || {
        let events = read_all(&run_dir).ok()?;
        events
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .map(|_| ())
    });

    let events = read_all(&run_dir).unwrap();
    let state = RunState::fold(&events);
    assert_eq!(
        state.run_status,
        RunStatus::Succeeded,
        "expected background run to finish succeeded"
    );
}

// Scenario B: run_cancel on a nonexistent run and on a traversal path is
// equally rejected as NotFound.
#[test]
fn run_cancel_rejects_missing_and_traversal_run_id() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err = run_cancel(dir.path(), "ghost-1").unwrap_err();
    match err {
        EngineError::NotFound(_) => {}
        other => panic!("expected NotFound for missing run, got {other:?}"),
    }

    let err = run_cancel(dir.path(), "../../etc").unwrap_err();
    match err {
        EngineError::NotFound(_) => {}
        other => panic!("expected NotFound for traversal run_id, got {other:?}"),
    }
}

// Scenario C: a contract check - run_cancel on an actually existing
// (background) run appends Abort to that run's control.jsonl. Deterministic:
// we don't try to catch the run "in flight" mid-execution, we only check
// that the command channel receives Abort, which drive (proven in supervised_drive_test.rs
// scenario 4) must carry through to RunStatus::Aborted at the next iteration boundary.
#[test]
fn run_cancel_posts_abort_to_existing_run_control_channel() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let mut opts = RunOptions::default();
    opts.params.insert("who".into(), "world".into());
    let run_id = run_background(dir.path(), "noagent", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // Wait until run_dir and its control.jsonl infrastructure are definitely ready
    // (the directory is created synchronously in prepare_run, so it already exists now,
    // but we wait anyway using the same timeout pattern as the other tests, just in case).
    poll_until("run_dir exists on disk", || {
        if run_dir.is_dir() { Some(()) } else { None }
    });

    run_cancel(dir.path(), &run_id).unwrap();

    let entries = read_control_after(&run_dir, None).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| matches!(e.cmd, Control::Abort { .. })),
        "expected control.jsonl to contain an Abort entry after run_cancel"
    );

    // Idempotency: the second call does not panic or error.
    run_cancel(dir.path(), &run_id).unwrap();
    let entries_after = read_control_after(&run_dir, None).unwrap();
    assert!(
        entries_after.len() >= entries.len(),
        "second run_cancel call must not reduce the control channel"
    );

    // Let the background run reach a terminal event so the thread doesn't
    // linger alive after the test (noagent is a fast linear run; Abort
    // may arrive before or after its natural finish - both outcomes are
    // terminal and fit within the deadline).
    poll_until(
        "a terminal event (RunFinished or RunAborted) in events.jsonl",
        || {
            let events = read_all(&run_dir).ok()?;
            events
                .iter()
                .find(|e| {
                    matches!(
                        e.payload,
                        EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. }
                    )
                })
                .map(|_| ())
        },
    );
}

// Scenario D: a regression test for a bug with a partial move-capture in run_background.
// slowscript is an is_write playbook (script node), so prepare_run actually
// takes Some(WorkdirGuard). Before the fix, the closure in run_background captured
// per RFC 2229 only the used fields of Prepared (playbook, run_dir, log, cfg,
// start_node, run_id, mode), while p.guard remained in run_background's
// stack frame and got dropped (releasing the lock) the moment it returned -
// i.e. the lock disappeared BEFORE the background drive could do anything. With the fix
// (`let mut p = p;` as the closure's first line), the whole Prepared moves
// into the thread, and the lock is held until drive finishes.
#[test]
fn run_background_holds_workdir_lock_for_the_whole_run() {
    let dir = tempfile::tempdir().unwrap();
    seed_slowscript(dir.path());

    let lock_path = dir.path().join(".apb/workdir.lock");
    assert!(
        !lock_path.is_file(),
        "lock must not exist before the run starts"
    );

    let opts = RunOptions::default();
    let run_id = run_background(dir.path(), "slowscript", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // Check immediately after run_background returns, while the background script
    // is still sleeping: with the bug, the lock is already released at this point
    // (the guard was dropped in run_background's frame); with the fix it is still held.
    assert!(
        lock_path.is_file(),
        "workdir lock must still be held right after run_background returns, while the background run is in flight"
    );

    // The same fact from another angle: a repeated acquire in this same process
    // must run into the busy lock (the lock stores the current process's pid, which is
    // definitely alive) while the background run has not yet finished.
    match acquire(dir.path(), false) {
        Err(EngineError::WorkdirBusy(_)) => {}
        other => {
            panic!("expected WorkdirBusy while the background run is still live, got {other:?}")
        }
    }

    poll_until("a RunFinished event in events.jsonl", || {
        let events = read_all(&run_dir).ok()?;
        events
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .map(|_| ())
    });

    let events = read_all(&run_dir).unwrap();
    let state = RunState::fold(&events);
    assert_eq!(
        state.run_status,
        RunStatus::Succeeded,
        "expected slowscript background run to finish succeeded"
    );

    // RunFinished is written inside drive() before the closure in run_background
    // returns and p (with the guard) is actually dropped, so there's a short window
    // between the event and the lock release - so we poll rather than check once.
    poll_until(
        "workdir lock released after the background run finished",
        || {
            if lock_path.is_file() { None } else { Some(()) }
        },
    );
}
