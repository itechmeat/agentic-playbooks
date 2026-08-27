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

/// Anti-hang ceiling for the polls below, not a performance budget: nothing here
/// asserts that the background driver is FAST, only that it gets there. Widened
/// from 5s after two observed spurious failures on a clean tree, where a 1s
/// script node took longer than that to finish because every fresh `sh` stub and
/// the detached driver binary pay a per-launch macOS security scan
/// (BUILD-OPTIMIZATION rule 8; the same scan measured 3.9s to 53.5s of spawn
/// stall in the timeout suites). A genuine hang is still caught: 30s is well
/// inside nextest's 60s SLOW period, so a wedged poll fails by name.
const POLL_DEADLINE: Duration = Duration::from_secs(30);
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

// The workdir queue, end to end. An inbound-event bridge posts an event while
// some other write-run holds the lock; before this, the start was refused, the
// bridge turned the refusal into a 502, and the event - the only copy of a
// customer's message - was gone. Now the start is ADMITTED: the run directory
// and the caller's parameters are persisted immediately, the journal says the
// run is queued, and the run executes as soon as the holder lets go.
#[test]
fn a_queued_background_run_is_admitted_and_runs_once_the_workdir_frees() {
    let dir = tempfile::tempdir().unwrap();
    seed_slowscript(dir.path());

    // Stand in for the run that is already using the workdir. Acquired the same
    // way `prepare` acquires it (a live pid, this process's own), so the queue
    // is exercised deterministically rather than by racing a second real run.
    let holder = acquire(dir.path(), false).unwrap().unwrap();

    let opts = RunOptions {
        workdir_queue_wait: Some(Duration::from_secs(60)),
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "slowscript", None, opts)
        .expect("a busy workdir must queue the start, not refuse it");
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // Admission is what makes the event durable: everything the caller handed
    // over is on disk before the lock is anywhere in sight.
    assert!(run_dir.is_dir(), "a queued run must be persisted at once");
    let queued = read_all(&run_dir).unwrap();
    assert!(
        queued
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RunQueued { .. })),
        "a queued start must journal why it has not begun, got {queued:?}"
    );
    // Queued is not paused: the run reads as running to every observer.
    assert_eq!(RunState::fold(&queued).run_status, RunStatus::Running);
    assert!(
        !queued
            .iter()
            .any(|e| matches!(e.payload, EventPayload::NodeStarted { .. })),
        "no node may start while another run holds the workdir"
    );

    drop(holder);

    poll_until("the queued run to finish once the workdir freed", || {
        let events = read_all(&run_dir).ok()?;
        events
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .map(|_| ())
    });
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        RunState::fold(&events).run_status,
        RunStatus::Succeeded,
        "a queued run must execute normally once it gets the workdir"
    );
}

// The queue is bounded, and running out is a visible failure rather than a
// thread parked forever. The run was already admitted, so it has to be closed
// out in its own journal: an admitted event that never executes must still be
// findable, with the reason attached.
#[test]
fn a_queued_background_run_that_never_gets_the_workdir_fails_in_its_own_journal() {
    let dir = tempfile::tempdir().unwrap();
    seed_slowscript(dir.path());
    let _holder = acquire(dir.path(), false).unwrap().unwrap();

    let opts = RunOptions {
        workdir_queue_wait: Some(Duration::from_millis(300)),
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "slowscript", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    poll_until("the queued run to give up", || {
        let events = read_all(&run_dir).ok()?;
        events
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .map(|_| ())
    });
    let events = read_all(&run_dir).unwrap();
    assert_eq!(RunState::fold(&events).run_status, RunStatus::Failed);
    let reason = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunError { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("a queue that runs out must journal why");
    assert!(
        reason.contains("workdir queue"),
        "the recorded reason must name the queue, got {reason}"
    );
}

// Nothing changes for a caller that did not ask to be queued: a busy workdir
// still refuses the start outright, before any run directory exists.
#[test]
fn without_a_queue_wait_a_busy_workdir_still_refuses_the_start() {
    let dir = tempfile::tempdir().unwrap();
    seed_slowscript(dir.path());
    let _holder = acquire(dir.path(), false).unwrap().unwrap();

    match run_background(dir.path(), "slowscript", None, RunOptions::default()) {
        Err(EngineError::WorkdirBusy(_)) => {}
        other => panic!("expected WorkdirBusy without a queue wait, got {other:?}"),
    }
    let runs = dir.path().join(".apb/runs");
    assert!(
        !runs.is_dir() || fs::read_dir(&runs).unwrap().next().is_none(),
        "a refused start must leave no run behind"
    );
}

// A stop against a queued run must not have to wait for an unrelated run to
// finish before it takes effect, and the run it ends is `aborted` rather than
// `failed`: somebody asked for this outcome, the engine did not run out of
// patience.
#[test]
fn a_stop_against_a_queued_run_ends_the_wait_and_records_an_abort() {
    let dir = tempfile::tempdir().unwrap();
    seed_slowscript(dir.path());
    let _holder = acquire(dir.path(), false).unwrap().unwrap();

    let opts = RunOptions {
        // Long enough that a give-up by ceiling cannot be what ends this wait.
        workdir_queue_wait: Some(Duration::from_secs(600)),
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "slowscript", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    run_cancel(dir.path(), &run_id).unwrap();

    let status = poll_until("the queued run to end on the stop", || {
        let events = read_all(&run_dir).ok()?;
        let status = RunState::fold(&events).run_status;
        status.is_terminal().then_some(status)
    });
    assert_eq!(status, RunStatus::Aborted);
}
