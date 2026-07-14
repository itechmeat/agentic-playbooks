mod common;
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
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

// Scenario 3: autonomous mode is unchanged. The same failing playbook, but now
// node `work` has only an edge conditioned on success (no fallback and no failure
// branch) - as before Phase 4a, next_node finds no matching edge and returns
// EngineError::Invalid; run() must return that same error rather than silently proceed.
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
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    // RunOptions::default() -> RunMode::Autonomous: behavior must match
    // what it was before Phase 4a - we don't pass any mode by name.
    let err = run(dir.path(), "autoflow", None, RunOptions::default()).unwrap_err();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    match err {
        EngineError::Invalid(msg) => {
            assert!(
                msg.contains("no outgoing edge"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected EngineError::Invalid, got {other:?}"),
    }
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
