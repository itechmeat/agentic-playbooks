//! Task 8: a stop that interrupts in-flight work.
//!
//! Three properties, all bounded in wall-clock so a regression fails fast
//! instead of hanging the suite:
//!   (a) a `stop_run` against a live drive interrupts the agent MID-NODE (the
//!       drive returns far sooner than the stub agent's own sleep) and the
//!       journal ends with `RunAborted`;
//!   (b) a run whose driver is gone is finalized by `stop_run` itself;
//!   (c) an already terminal run is left completely untouched.

use crate::common;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::{Control, read_control_after, read_control_cursor};
use apb_engine::error::EngineError;
use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, RunResult, run};
use apb_engine::state::{RunState, RunStatus};
use apb_engine::stop::{StopOutcome, stop_run};

/// The stub agent sleeps this long. An abort that does not actually interrupt
/// the in-flight process would make the drive take at least this long, so the
/// assertion deadline below sits far under it.
const AGENT_SLEEP_SECS: u64 = 30;
/// How long the drive may take to come back after the abort. Killing a process
/// tree plus one drive-loop boundary is a fraction of a second; 10s is a
/// generous ceiling that is still nowhere near `AGENT_SLEEP_SECS`.
const ABORT_DEADLINE: Duration = Duration::from_secs(10);
const POLL_DEADLINE: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(20);

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

fn find_run_id(root: &Path, prefix: &str) -> String {
    poll_until(&format!("a run dir with prefix `{prefix}`"), || {
        let runs_dir = root.join(".apb/runs");
        if !runs_dir.is_dir() {
            return None;
        }
        fs::read_dir(&runs_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .find(|n| n.starts_with(prefix))
    })
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

/// A stub agent that announces itself (so the test knows the node is really
/// in flight) and then sleeps far longer than the test is willing to wait.
fn sleepy_agent(dir: &Path) -> (String, PathBuf) {
    let marker = dir.join("agent_running.marker");
    let path = dir.join("sleepy.sh");
    let body = format!(
        "#!/bin/sh\ntouch '{m}'\nsleep {s}\necho done\n",
        m = marker.display(),
        s = AGENT_SLEEP_SECS
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    (path.to_string_lossy().to_string(), marker)
}

/// Sets `APB_AGENT_CMD` for the lifetime of the guard and removes it on drop,
/// including when the test panics in between. A bare `set_var ... remove_var`
/// pair leaks the variable whenever an assertion or a `recv_timeout` between
/// them fails, and because this suite runs as modules of ONE process that leak
/// would point every later test at a stale stub agent. Holds the shared env
/// lock for the same span.
struct AgentEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl AgentEnv {
    fn set(prog: &str) -> Self {
        let lock = common::env_lock();
        unsafe {
            std::env::set_var("APB_AGENT_CMD", prog);
        }
        Self { _lock: lock }
    }
}

impl Drop for AgentEnv {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
        }
    }
}

const WF: &str = r#"
schema: 1
id: stopflow
name: Stop Flow
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

fn seed(root: &Path, id: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF.replace("stopflow", id)).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

/// (a) The core of the task: an abort posted while an agent is mid-flight must
/// kill that agent's process tree, not wait for it.
#[cfg(unix)]
#[test]
fn stop_interrupts_an_in_flight_agent() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "stopflow");
    let (prog, marker) = sleepy_agent(dir.path());

    let _env = AgentEnv::set(&prog);

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopflow", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "stopflow-");
    // Wait until the agent process is genuinely running, so the abort really
    // lands mid-node rather than before the node ever started.
    poll_until("the stub agent to start", || marker.is_file().then_some(()));

    let started = Instant::now();
    let outcome = stop_run(dir.path(), &run_id).unwrap();
    assert_eq!(
        outcome,
        StopOutcome::SignaledLiveDriver,
        "the drive is running in this very process, so its driver is live"
    );

    let res = rx.recv_timeout(ABORT_DEADLINE).unwrap_or_else(|_| {
        panic!(
            "the drive did not return within {ABORT_DEADLINE:?} of the stop: the in-flight agent was not interrupted"
        )
    });
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(AGENT_SLEEP_SECS),
        "the drive took {elapsed:?}, which means it waited out the agent's {AGENT_SLEEP_SECS}s sleep instead of killing it"
    );
    let res = res.unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted, "the run must end aborted");

    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        matches!(
            events.last().map(|e| &e.payload),
            Some(EventPayload::RunAborted { .. })
        ),
        "the journal must end with RunAborted, got {:?}",
        events.last().map(|e| &e.payload)
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::RunAborted { .. }))
            .count(),
        1,
        "the abort must be applied exactly once"
    );
    // The watcher only OBSERVES control.jsonl. Advancing the cursor is the
    // drive loop's job and happens once, when it applies the abort - so the
    // persisted cursor must name exactly the abort entry the drive consumed,
    // never a value the watcher raced ahead to.
    let posted = read_control_after(&run_dir, None).unwrap();
    let abort_seq = posted
        .iter()
        .find(|e| matches!(e.cmd, Control::Abort { .. }))
        .expect("the stop must have posted an Abort")
        .seq;
    assert_eq!(
        read_control_cursor(&run_dir).unwrap(),
        Some(abort_seq),
        "the drive loop, not the watcher, owns the control cursor"
    );
}

/// The same playbook, except the only way out of `work` is success. A node
/// killed by the stop therefore leaves the run with nowhere to go, which used
/// to be a hard drive error - no terminal event, a run still reading `running`
/// on disk. The stop has to finalize it as aborted regardless of what the
/// interrupted node's wreckage looks like.
const WF_SUCCESS_ONLY: &str = r#"
schema: 1
id: stopdead
name: Stop Dead End
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

#[cfg(unix)]
#[test]
fn stop_aborts_even_when_the_killed_node_has_no_way_forward() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/stopdead/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF_SUCCESS_ONLY).unwrap();
    fs::write(dir.path().join(".apb/playbooks/stopdead/current"), "1.0.0").unwrap();
    common::seed_main(dir.path());
    let (prog, marker) = sleepy_agent(dir.path());

    let _env = AgentEnv::set(&prog);

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopdead", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "stopdead-");
    poll_until("the stub agent to start", || marker.is_file().then_some(()));
    assert_eq!(
        stop_run(dir.path(), &run_id).unwrap(),
        StopOutcome::SignaledLiveDriver
    );

    let res = rx
        .recv_timeout(ABORT_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {ABORT_DEADLINE:?}"));

    let res = res.expect("a stopped run must abort cleanly, not fail the drive with an error");
    assert_eq!(res.outcome, RunStatus::Aborted);
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        matches!(
            events.last().map(|e| &e.payload),
            Some(EventPayload::RunAborted { .. })
        ),
        "the journal must end with RunAborted, got {:?}",
        events.last().map(|e| &e.payload)
    );
}

/// Builds a run directory by hand whose journal stops mid-run, with no
/// `driver.pid`: exactly what a crashed driver leaves behind.
fn seed_abandoned_run(root: &Path, run_id: &str) -> PathBuf {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    let mut log = EventLog::create(&run_dir).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "stopflow".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::NodeStarted {
        node: "work".into(),
        attempt: 1,
    })
    .unwrap();
    run_dir
}

/// (b) A run whose driver is gone cannot observe anything, so `stop_run` has to
/// finalize it itself.
#[test]
fn stop_finalizes_a_run_whose_driver_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let run_dir = seed_abandoned_run(dir.path(), "stopflow-dead");

    let outcome = stop_run(dir.path(), "stopflow-dead").unwrap();
    assert_eq!(outcome, StopOutcome::FinalizedDeadRun);

    let events = read_all(&run_dir).unwrap();
    assert!(
        matches!(
            events.last().map(|e| &e.payload),
            Some(EventPayload::RunAborted { .. })
        ),
        "a dead run must gain a RunAborted event"
    );
    assert_eq!(
        RunState::fold(&events).run_status,
        RunStatus::Aborted,
        "the folded status must be aborted"
    );
}

/// (c) An already terminal run is not touched at all.
#[test]
fn stop_of_a_terminal_run_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let run_dir = seed_abandoned_run(dir.path(), "stopflow-done");
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(EventPayload::RunFinished {
        outcome: "success".into(),
    })
    .unwrap();
    drop(log);

    let before = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    let outcome = stop_run(dir.path(), "stopflow-done").unwrap();
    assert_eq!(outcome, StopOutcome::AlreadyTerminal);
    let after = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert_eq!(before, after, "a terminal run's journal must be untouched");
    assert!(
        !run_dir.join("control.jsonl").is_file(),
        "a terminal run must not even get an abort posted"
    );
}

/// An unknown run id is a not-found error, not a silently created directory.
#[test]
fn stop_of_an_unknown_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    assert!(matches!(
        stop_run(dir.path(), "nope-1"),
        Err(EngineError::NotFound(_))
    ));
    assert!(matches!(
        stop_run(dir.path(), "../escape"),
        Err(EngineError::NotFound(_))
    ));
}
