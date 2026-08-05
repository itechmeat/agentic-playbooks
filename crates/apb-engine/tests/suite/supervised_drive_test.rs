use crate::common;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::{Control, post_control};
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventPayload, WakeTrigger, read_all};
use apb_engine::scheduler::{RunMode, RunOptions, RunResult, resume, run};
use apb_engine::state::RunStatus;

// Cargo runs #[test] fns in parallel threads within one process, so tests that
// mutate the shared global env var APB_AGENT_CMD race with each other unless
// serialized. Hold this lock across the entire set_var..run..remove_var span,
// including the whole background-thread + poll + post_control span for the
// threaded scenarios below (see retry_test.rs for the same idiom).

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

/// Finds the run directory whose run_id starts with `prefix` (the playbook id),
/// appearing under `.apb/runs`. The run_id is generated inside `run()` and is not
/// known to the test in advance, so we detect it once the directory is created.
fn find_run_dir(root: &Path, prefix: &str) -> PathBuf {
    poll_until(
        &format!("run dir with prefix `{prefix}` under .apb/runs"),
        || {
            let runs_dir = root.join(".apb/runs");
            if !runs_dir.is_dir() {
                return None;
            }
            std::fs::read_dir(&runs_dir)
                .ok()?
                .filter_map(|e| e.ok())
                .find(|e| e.file_name().to_string_lossy().starts_with(prefix))
                .map(|e| e.path())
        },
    )
}

fn wait_for_wake(run_dir: &Path) -> Event {
    poll_until("a WakeRaised event in events.jsonl", || {
        read_all(run_dir)
            .ok()?
            .into_iter()
            .find(|e| matches!(e.payload, EventPayload::WakeRaised { .. }))
    })
}

/// Runs `run(...)` in a separate thread and returns a channel with the result,
/// so it can be retrieved via `recv_timeout` without risking the test hanging.
fn run_in_background(
    root: PathBuf,
    id: &'static str,
    opts: RunOptions,
) -> mpsc::Receiver<Result<RunResult, EngineError>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = run(&root, id, None, opts);
        let _ = tx.send(res);
    });
    rx
}

fn recv_result(
    rx: &mpsc::Receiver<Result<RunResult, EngineError>>,
) -> Result<RunResult, EngineError> {
    rx.recv_timeout(POLL_DEADLINE).unwrap_or_else(|_| {
        panic!("background drive thread did not finish within {POLL_DEADLINE:?}")
    })
}

// A stub that always fails - as in retry_test.rs::always_fail_agent.
fn always_fail_agent(dir: &Path) -> String {
    let path = dir.join("always_fail.sh");
    fs::write(&path, "#!/bin/sh\necho boom 1>&2\nexit 1\n").unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

// Stub: fails on the first invocation, leaves a marker file, succeeds on all following ones.
// The same trick as in retry_test.rs::flaky_agent, just naming the marker differently
// so it's not confused with the internal retry/fallback engine's execution.
fn flaky_agent(dir: &Path) -> String {
    let marker = dir.join("sup_flaky.marker");
    let path = dir.join("sup_flaky.sh");
    let body = format!(
        "#!/bin/sh\nif [ -f '{m}' ]; then echo ok; exit 0; else touch '{m}'; echo firstfail 1>&2; exit 1; fi\n",
        m = marker.display()
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

fn seed(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

// The only unconditional edge `work -> done`: in supervised mode a node failure
// does not go into next_node, but raises wake and waits for a command, so a fallback edge
// is not needed here (unlike the autonomous tests in retry_test.rs).
const WF_SUPERVISED: &str = r#"
schema: 1
id: supflow
name: Supervised
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

// Scenario 1: Supervised + a failing agent_task, no command pre-seeded.
// drive raises WakeRaised and waits; the test posts Abort from the main thread,
// the run must end Aborted, and events must contain WakeRaised{node_failed}.
#[test]
fn supervised_wake_without_command_then_abort() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "supflow1", WF_SUPERVISED);

    let prog = always_fail_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let rx = run_in_background(dir.path().to_path_buf(), "supflow1", opts);

    let run_dir = find_run_dir(dir.path(), "supflow1-");
    let wake = wait_for_wake(&run_dir);
    match &wake.payload {
        EventPayload::WakeRaised { trigger, .. } => {
            assert_eq!(
                *trigger,
                WakeTrigger::NodeFailed,
                "expected node_failed trigger, got {trigger:?}"
            );
        }
        other => panic!("expected WakeRaised, got {other:?}"),
    }

    post_control(
        &run_dir,
        Control::Abort {
            reason: "test abort".into(),
        },
    )
    .unwrap();

    let res = recv_result(&rx).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(
        res.outcome,
        RunStatus::Aborted,
        "expected run aborted after supervisor Abort command"
    );
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::WakeRaised {
                trigger: WakeTrigger::NodeFailed,
                ..
            }
        )),
        "expected a WakeRaised{{node_failed}} event in the log"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RunAborted { .. })),
        "expected a RunAborted event in the log"
    );
}

// Scenario 2: Supervised + an agent that fails on the first invocation and succeeds on the second.
// After wake, the supervisor sends Retry{node: work}; drive must restart the node,
// this time the stub returns success, and the run reaches finish.
#[test]
fn supervised_retry_recovers_after_wake() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "supflow2", WF_SUPERVISED);

    let prog = flaky_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let rx = run_in_background(dir.path().to_path_buf(), "supflow2", opts);

    let run_dir = find_run_dir(dir.path(), "supflow2-");
    wait_for_wake(&run_dir);

    post_control(
        &run_dir,
        Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    )
    .unwrap();

    let res = recv_result(&rx).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "retry after wake must let the run reach the finish node"
    );
    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::SupervisorAction { action, .. } if action == "node_retry")),
        "expected a SupervisorAction{{action: node_retry}} event in the log");
}

// Scenario 2b: a supervised FAN-OUT. Both branches are batched (spec 1.3), and
// the failing one raises its wake only after the whole batch has completed; a
// posted Retry then recovers exactly as it does after a sequential node.
const WF_SUPERVISED_DIAMOND: &str = r#"
schema: 1
id: supdia
name: Supervised diamond
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "branch a steady" }
  - { id: b, type: agent_task, prompt: "branch b flaky" }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

/// One stub for both branches: the adapter passes `-p <prompt> --model <model>`,
/// so `$2` is the prompt and the branch is told apart by its text. The flaky
/// branch fails once (marker file) and succeeds afterwards.
fn branch_agent(dir: &Path) -> String {
    let marker = dir.join("branch_b.marker");
    let path = dir.join("branch_agent.sh");
    let body = format!(
        "#!/bin/sh\ncase \"$2\" in\n  *flaky*)\n    if [ -f '{m}' ]; then echo ok; exit 0; fi\n    touch '{m}'\n    echo 'branch b boom' 1>&2\n    exit 1\n    ;;\nesac\necho ok\n",
        m = marker.display()
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

fn index_of(events: &[Event], what: &str, pred: impl Fn(&EventPayload) -> bool) -> usize {
    events
        .iter()
        .position(|e| pred(&e.payload))
        .unwrap_or_else(|| panic!("expected a {what} event in the log"))
}

fn node_started_at(events: &[Event], node: &str) -> usize {
    index_of(
        events,
        &format!("node_started for {node}"),
        |p| matches!(p, EventPayload::NodeStarted { node: n, .. } if n == node),
    )
}

fn node_finished_at(events: &[Event], node: &str) -> usize {
    index_of(
        events,
        &format!("node_finished for {node}"),
        |p| matches!(p, EventPayload::NodeFinished { node: n, .. } if n == node),
    )
}

#[test]
fn supervised_batch_failure_wakes_after_the_batch_and_retry_recovers() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "supdia", WF_SUPERVISED_DIAMOND);

    let prog = branch_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let rx = run_in_background(dir.path().to_path_buf(), "supdia", opts);

    let run_dir = find_run_dir(dir.path(), "supdia-");
    let wake = wait_for_wake(&run_dir);
    match &wake.payload {
        EventPayload::WakeRaised { trigger, node, .. } => {
            assert_eq!(*trigger, WakeTrigger::NodeFailed);
            assert_eq!(node, "b", "the wake must name the failed branch");
        }
        other => panic!("expected WakeRaised, got {other:?}"),
    }

    post_control(
        &run_dir,
        Control::Retry {
            node: "b".into(),
            prompt_override: None,
        },
    )
    .unwrap();

    let res = recv_result(&rx).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "a retry of the failed batch branch must let the run reach finish"
    );
    let events = read_all(&run_dir).unwrap();
    // Both branches were in flight together: `b` started before `a` finished.
    assert!(
        node_started_at(&events, "b") < node_finished_at(&events, "a"),
        "a supervised fan-out must batch its branches, not serialize them"
    );
    // The wake belongs to the batch TAIL: it comes after every member finished.
    let wake_at = index_of(&events, "wake_raised", |p| {
        matches!(p, EventPayload::WakeRaised { .. })
    });
    for n in ["a", "b"] {
        assert!(
            node_finished_at(&events, n) < wake_at,
            "the wake must be raised only after batch member {n} finished"
        );
    }
    // The retry re-ran only the failed branch, and the join then executed.
    let started_b = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "b"))
        .count();
    assert_eq!(started_b, 2, "branch b must run again after the retry");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "a"))
            .count(),
        1,
        "branch a must not be re-executed by the retry of b"
    );
    node_finished_at(&events, "j");
}

// Scenario 2c: TWO failures in one supervised batch. Each is presented on its
// own, in batch order, and the run only moves on once the supervisor has answered
// both - the contract that replaces per-branch live parking (spec 1.3).
const WF_SUPERVISED_TWO_FAILURES: &str = r#"
schema: 1
id: supdia2
name: Supervised two failures
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "branch a steady" }
  - { id: b1, type: agent_task, prompt: "branch flaky1" }
  - { id: b2, type: agent_task, prompt: "branch flaky2" }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b1 }
  - { from: start, to: b2 }
  - { from: a, to: j, join: all }
  - { from: b1, to: j, join: all }
  - { from: b2, to: j, join: all }
  - { from: j, to: done }
"#;

/// Two independently flaky branches (one marker each) plus a steady one.
fn two_flaky_branches_agent(dir: &Path) -> String {
    let path = dir.join("two_flaky.sh");
    let body = format!(
        "#!/bin/sh\ncase \"$2\" in\n\
         \x20 *flaky1*) m='{m1}' ;;\n\
         \x20 *flaky2*) m='{m2}' ;;\n\
         \x20 *) echo ok; exit 0 ;;\n\
         esac\n\
         if [ -f \"$m\" ]; then echo ok; exit 0; fi\n\
         touch \"$m\"\n\
         echo 'first attempt boom' 1>&2\n\
         exit 1\n",
        m1 = dir.join("flaky1.marker").display(),
        m2 = dir.join("flaky2.marker").display(),
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

/// Waits for a wake that names `node`, so a test can follow the batch-order
/// sequence of wakes instead of only seeing the first one.
fn wait_for_wake_on(run_dir: &Path, node: &'static str) -> Event {
    poll_until(&format!("a WakeRaised event for node {node}"), || {
        read_all(run_dir)
            .ok()?
            .into_iter()
            .find(|e| matches!(&e.payload, EventPayload::WakeRaised { node: n, .. } if n == node))
    })
}

#[test]
fn two_failures_in_one_batch_park_one_at_a_time_in_batch_order() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "supdia2", WF_SUPERVISED_TWO_FAILURES);

    let prog = two_flaky_branches_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let rx = run_in_background(dir.path().to_path_buf(), "supdia2", opts);
    let run_dir = find_run_dir(dir.path(), "supdia2-");

    // Batch order is `current` then the frontier in edge-declaration order, so
    // b1's wake comes first and b2 is not presented until b1 is answered.
    wait_for_wake_on(&run_dir, "b1");
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WakeRaised { node, .. } if node == "b2")),
        "only one wake may be outstanding at a time"
    );
    post_control(
        &run_dir,
        Control::Retry {
            node: "b1".into(),
            prompt_override: None,
        },
    )
    .unwrap();

    wait_for_wake_on(&run_dir, "b2");
    post_control(
        &run_dir,
        Control::Retry {
            node: "b2".into(),
            prompt_override: None,
        },
    )
    .unwrap();

    let res = recv_result(&rx).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "both retried branches must reach the barrier and the run must finish"
    );
    let events = read_all(&run_dir).unwrap();
    for n in ["b1", "b2"] {
        assert_eq!(
            events
                .iter()
                .filter(
                    |e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == n)
                )
                .count(),
            2,
            "branch {n} must run once in the batch and once for its retry"
        );
    }
    node_finished_at(&events, "j");
}

// Scenario 3: autonomous mode is unchanged. The same failing playbook, but now
// node `work` has only an edge conditioned on success (no fallback and no failure
// branch) - as before Phase 4a, next_node finds no matching edge. Before issue
// #42 finding 3 was fixed, that bubbled as a raw `EngineError::Invalid` out of
// `run()`, which has no fallback of its own - the run was left `running`
// forever with no explanatory event. `run()` now returns `Ok` with the run
// recorded `Failed`, and the event log carries a `RunError` naming why.
const WF_AUTONOMOUS_NO_FALLBACK: &str = r#"
schema: 1
id: autoflow
name: Autonomous no fallback
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
"#;

#[test]
fn autonomous_mode_unchanged_errors_without_fallback_edge() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "autoflow", WF_AUTONOMOUS_NO_FALLBACK);

    let prog = always_fail_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    // RunOptions::default() -> RunMode::Autonomous: behavior must match
    // what it was before Phase 4a - we don't pass any mode by name.
    let res = run(dir.path(), "autoflow", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    // Issue #42 finding 3: `run()` no longer surfaces this as a raw `Err`
    // (which left the run `running` forever with no terminal event, since
    // `run()` has no fallback of its own) - it comes back `Ok` with the run
    // recorded `Failed`.
    assert_eq!(res.outcome, RunStatus::Failed);
    let run_dir = find_run_dir(dir.path(), "autoflow-");
    let events = read_all(&run_dir).unwrap();
    let reason = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunError { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    let reason = reason.unwrap_or_else(|| {
        panic!("expected a RunError event explaining the failure, got {events:?}")
    });
    assert!(
        reason.contains("no outgoing edge"),
        "unexpected reason: {reason}"
    );
    // The RunError comes before the terminal run_finished(failed), not after.
    let run_error_seq = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunError { .. }))
        .unwrap()
        .seq;
    let run_finished_seq = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
        .unwrap()
        .seq;
    assert!(
        run_error_seq < run_finished_seq,
        "RunError must be journaled before run_finished"
    );
}

// Scenario 4: Abort also works in autonomous mode. We place an Abort into
// control.jsonl in advance in an already existing run directory (getting the run_id from a first
// successful run), then resume() with an autonomous drive must, at the very first loop boundary
// (before executing any node), see Abort and return RunStatus::Aborted - without threads
// or polling, fully deterministically.
const WF_LINEAR: &str = r#"
schema: 1
id: lin4
name: Linear
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "x" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

#[test]
fn abort_control_ends_autonomous_drive_as_aborted() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "lin4", WF_LINEAR);

    // A normal autonomous run to completion - only needed to get a real run_id/run_dir.
    let first = run(dir.path(), "lin4", None, RunOptions::default()).unwrap();
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&first.run_id);

    post_control(
        &run_dir,
        Control::Abort {
            reason: "pre-seeded abort".into(),
        },
    )
    .unwrap();

    // resume() is always autonomous (Phase 4a); drive checks Abort at the entry of EVERY
    // iteration, including the very first one, before executing node `a`.
    let res = resume(dir.path(), &first.run_id, Some("a")).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Aborted,
        "pre-seeded Abort must end autonomous drive as Aborted"
    );

    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::RunAborted { reason } if reason == "pre-seeded abort")),
        "expected a RunAborted event carrying the posted reason");
}
