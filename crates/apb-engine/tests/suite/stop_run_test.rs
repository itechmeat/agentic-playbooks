//! Task 8: a stop that interrupts in-flight work.
//!
//! Four properties, all bounded in wall-clock so a regression fails fast
//! instead of hanging the suite:
//!   (a) a `stop_run` against a live drive interrupts the agent MID-NODE (the
//!       drive returns far sooner than the stub agent's own sleep) and the
//!       journal ends with `RunAborted`;
//!   (b) a run whose driver is gone is finalized by `stop_run` itself;
//!   (c) an already terminal run is left completely untouched;
//!   (d) a stop posted while a CONCURRENT batch is running stops the batch from
//!       admitting the groups still queued behind it.

use crate::common;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::{Control, read_control_after, read_control_cursor};
use apb_engine::error::EngineError;
use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, RunResult, post_supervisor_command, resume, run};
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
///
/// The journal ends with an OPEN attempt (`attempt_started` and no
/// `attempt_finished`, no `node_finished`), which is the crash shape this
/// branch actually produces now that attempts are journaled at SPAWN time. That
/// shape is the one the fold used to downgrade back to `Interrupted` after the
/// stop had already written `RunAborted`.
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
    log.append(EventPayload::AttemptStarted {
        node: "work".into(),
        attempt: 1,
        agent: "claude".into(),
        soul_delivery: None,
        skills_mode: None,
        pid: Some(999_999),
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

/// A stop has to STICK. Twice over: the folded status of the run must actually
/// be `aborted` afterwards (before the fold's open-attempt override exempted
/// `Aborted`, the crash shape this branch produces - a journal ending in an
/// open `attempt_started` - was downgraded straight back to `interrupted`,
/// so the stop was invisible to `run_status`, `apb runs`, the dashboard and
/// `doctor --run`), and a second stop must therefore find a terminal run and do
/// nothing, instead of passing the terminal check again and appending a SECOND
/// `RunAborted`.
#[test]
fn a_second_stop_of_a_finalized_dead_run_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let run_dir = seed_abandoned_run(dir.path(), "stopflow-twice");

    assert_eq!(
        stop_run(dir.path(), "stopflow-twice").unwrap(),
        StopOutcome::FinalizedDeadRun
    );
    assert_eq!(
        RunState::fold(&read_all(&run_dir).unwrap()).run_status,
        RunStatus::Aborted,
        "an explicitly aborted run must not be downgraded by its leftover open attempt"
    );

    assert_eq!(
        stop_run(dir.path(), "stopflow-twice").unwrap(),
        StopOutcome::AlreadyTerminal,
        "the run is terminal now, so the second stop has nothing to finalize"
    );

    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::RunAborted { .. }))
            .count(),
        1,
        "two stops must leave exactly one RunAborted"
    );
    assert_eq!(
        RunState::fold(&events).run_status,
        RunStatus::Aborted,
        "and the run must still read aborted"
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

/// A fan-out of four script branches with two slots: the batch is admitted in
/// two chunks of two. The first chunk holds until the test releases it, the
/// second chunk records the fact that it ran at all.
const BATCH_FANOUT: &str = r#"
schema: 1
id: stopbatch
name: Stop Batch
version: 1.0.0
defaults:
  max_parallel: 2
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/hold.sh", runner: sh }
  - { id: b, type: script, script: "scripts/hold.sh", runner: sh }
  - { id: c, type: script, script: "scripts/mark.sh", runner: sh }
  - { id: d, type: script, script: "scripts/mark.sh", runner: sh }
  - { id: m, type: prompt, prompt: "merged" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: start, to: c }
  - { from: start, to: d }
  - { from: a, to: m }
  - { from: b, to: m }
  - { from: c, to: m }
  - { from: d, to: m }
  - { from: m, to: done }
"#;

/// The first chunk's script: announce that the chunk is genuinely in flight, wait
/// (bounded) for the test to release it, and then linger one second. The linger
/// is what makes the assertion about the SECOND chunk meaningful rather than a
/// race: the release happens only after the abort is posted, and one second is
/// several `stop::WATCH_INTERVAL`s, so the abort watcher has latched the run-level
/// cancel flag long before the admission gate for the next chunk is reached.
fn hold_script(root: &Path) -> String {
    format!(
        "touch '{root}/chunk1_started'\n\
         i=0\n\
         while [ ! -f '{root}/release' ]; do\n\
         \x20 i=$((i + 1))\n\
         \x20 if [ \"$i\" -gt 200 ]; then break; fi\n\
         \x20 sleep 0.05\n\
         done\n\
         sleep 1\n\
         touch '{root}/chunk1_finished_naturally'\n",
        root = root.display(),
    )
}

/// The status on a node's `NodeFinished`, for "did it really finish this way"
/// checks. Mirrors `parallel_e2e_test::finish_status`; kept local because each
/// file in this suite is its own module.
fn finish_status(events: &[apb_engine::event::Event], node: &str) -> String {
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n, status, ..
            } if n == node => Some(status.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("node `{node}` never finished"))
}

fn seed_batch(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/stopbatch/1.0.0");
    let scripts = vdir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(vdir.join("playbook.yaml"), BATCH_FANOUT).unwrap();
    fs::write(root.join(".apb/playbooks/stopbatch/current"), "1.0.0").unwrap();
    fs::write(scripts.join("hold.sh"), hold_script(root)).unwrap();
    fs::write(
        scripts.join("mark.sh"),
        format!("printf x >> '{}/chunk2_ran'\n", root.display()),
    )
    .unwrap();
}

/// (d) A stop must reach the CONCURRENT batch, not only the node in hand.
///
/// A fan-out wider than `max_parallel` is admitted in chunks, and the admission
/// gate used to consult only the batch-local `join: any` cancel flag. An abort
/// posted while the first chunk was running therefore still admitted and SPAWNED
/// every remaining chunk in full before the run aborted at the batch tail: a
/// 12-node fan-out at `max_parallel: 4` started eight fresh agent processes after
/// the operator said stop. Killing the members ALREADY in flight is covered
/// separately by `a_stop_during_a_batch_kills_the_members_in_flight`; this test
/// is only about the members still QUEUED behind the chunk in flight.
#[cfg(unix)]
#[test]
fn a_stop_during_a_batch_does_not_admit_the_queued_chunks() {
    let dir = tempfile::tempdir().unwrap();
    seed_batch(dir.path());

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopbatch", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "stopbatch-");
    poll_until("the first chunk to start", || {
        dir.path().join("chunk1_started").is_file().then_some(())
    });

    assert_eq!(
        stop_run(dir.path(), &run_id).unwrap(),
        StopOutcome::SignaledLiveDriver
    );
    // Only now may the first chunk finish, so the abort is provably in
    // control.jsonl while the batch is still mid-flight.
    fs::write(dir.path().join("release"), "go").unwrap();

    let res = rx
        .recv_timeout(ABORT_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {ABORT_DEADLINE:?}"));
    let res = res.expect("a stopped batch must abort cleanly, not fail the drive");
    assert_eq!(res.outcome, RunStatus::Aborted, "the run must end aborted");

    assert!(
        !dir.path().join("chunk2_ran").exists(),
        "the queued chunk was spawned after the operator said stop"
    );
    let events = read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    for n in ["c", "d"] {
        // A queued member now gets a paired NodeStarted (the unified cancelled
        // journal shape), so `!NodeStarted` is no longer the honest property.
        // What actually matters, and still holds, is that it was never
        // SPAWNED: no `AttemptStarted`, and (checked once for both above via
        // `chunk2_ran`) its script body never ran.
        assert!(
            !events.iter().any(
                |e| matches!(&e.payload, EventPayload::AttemptStarted { node, .. } if node == n)
            ),
            "queued member {n} must never be spawned once the run is cancelled"
        );
        assert!(
            events.iter().any(|e| matches!(
                &e.payload,
                EventPayload::NodeFinished { node, status, .. } if node == n && status == "cancelled"
            )),
            "queued member {n} must be journaled cancelled, not left pending"
        );
    }
    assert!(
        matches!(
            events.last().map(|e| &e.payload),
            Some(EventPayload::RunAborted { .. })
        ),
        "the journal must end with RunAborted, got {:?}",
        events.last().map(|e| &e.payload)
    );
}

/// #77(A): an abort must kill the members already in flight, not only stop
/// admission. Negative evidence, not a wall clock: `hold.sh` touches
/// `chunk1_finished_naturally` only after its post-release linger, so the
/// marker's ABSENCE proves the members were killed rather than allowed to end.
#[cfg(unix)]
#[test]
fn a_stop_during_a_batch_kills_the_members_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    seed_batch(dir.path());
    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopbatch", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "stopbatch-");
    poll_until("the first chunk to start", || {
        dir.path().join("chunk1_started").is_file().then_some(())
    });
    assert_eq!(
        stop_run(dir.path(), &run_id).unwrap(),
        StopOutcome::SignaledLiveDriver
    );

    let res = rx
        .recv_timeout(ABORT_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {ABORT_DEADLINE:?}"));
    let res = res.expect("a stopped batch must abort cleanly, not fail the drive");
    assert_eq!(res.outcome, RunStatus::Aborted);
    assert!(
        !dir.path().join("chunk1_finished_naturally").exists(),
        "the in-flight members ran to their own end instead of being killed"
    );
    let events = read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    for n in ["a", "b"] {
        assert_eq!(
            finish_status(&events, n),
            "cancelled",
            "an in-flight member killed by the abort is journaled cancelled"
        );
    }
}

/// #77(B), the sharpest trap: a PAUSE stops admission and writes NOTHING off.
/// `Cancelled` is terminal for `parallel::is_terminal`, so journaling a queued
/// member cancelled here would make the resume skip it forever and silently
/// drop that branch.
#[cfg(unix)]
#[test]
fn a_pause_during_a_batch_stops_admission_without_writing_members_off() {
    let dir = tempfile::tempdir().unwrap();
    seed_batch(dir.path());
    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopbatch", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "stopbatch-");
    poll_until("the first chunk to start", || {
        dir.path().join("chunk1_started").is_file().then_some(())
    });
    post_supervisor_command(dir.path(), &run_id, Control::Pause).unwrap();
    std::fs::write(dir.path().join("release"), "go").unwrap();

    let res = rx
        .recv_timeout(ABORT_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {ABORT_DEADLINE:?}"))
        .expect("a paused batch must not fail the drive");
    assert_eq!(res.outcome, RunStatus::Paused);
    assert!(
        !dir.path().join("chunk2_ran").exists(),
        "the queued chunk was admitted after the pause"
    );
    let events = read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    for n in ["c", "d"] {
        assert!(
            !events.iter().any(|e| matches!(
                &e.payload,
                EventPayload::NodeFinished { node, .. } if node == n
            )),
            "a paused run must not write member {n} off: the resume still has to run it"
        );
    }
}

/// The resume half of the pause test: the members the pause deferred really do
/// run afterwards. This is what the "no NodeFinished on pause" rule buys, and
/// without it the pause would silently drop those branches for good.
#[cfg(unix)]
#[test]
fn a_paused_batch_resumes_and_runs_the_deferred_members() {
    let dir = tempfile::tempdir().unwrap();
    seed_batch(dir.path());
    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopbatch", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "stopbatch-");
    poll_until("the first chunk to start", || {
        dir.path().join("chunk1_started").is_file().then_some(())
    });
    post_supervisor_command(dir.path(), &run_id, Control::Pause).unwrap();
    std::fs::write(dir.path().join("release"), "go").unwrap();
    let paused = rx
        .recv_timeout(ABORT_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {ABORT_DEADLINE:?}"))
        .expect("a paused batch must not fail the drive");
    assert_eq!(paused.outcome, RunStatus::Paused);
    // Premise of the test, asserted so a collapsed race fails loudly instead of
    // passing vacuously: the deferred members really had not run yet.
    assert!(!dir.path().join("chunk2_ran").exists());

    let resumed = resume(dir.path(), &run_id, None).expect("a paused run must resume");
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    assert!(
        dir.path().join("chunk2_ran").exists(),
        "the members the pause deferred must actually run on resume"
    );
    let events = read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    for n in ["c", "d"] {
        assert_eq!(
            finish_status(&events, n),
            "succeeded",
            "member {n} was deferred by the pause and must succeed on resume"
        );
    }
}

/// #77(D): one journal shape. Every cancelled member has a paired NodeStarted
/// before its NodeFinished, and every such finish carries `output: "cancelled"`.
/// The pairing is what `cache::verify_connector_calls`,
/// `journal::current_visit_start_seq`, `progress::node_durations_seconds` and
/// the interactive re-entry guard all assume; the single output text is what a
/// `{{nodes.<id>.output}}` read renders on both paths.
#[cfg(unix)]
#[test]
fn a_cancelled_member_journals_a_paired_start_and_a_cancelled_output() {
    let dir = tempfile::tempdir().unwrap();
    seed_batch(dir.path());
    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopbatch", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "stopbatch-");
    poll_until("the first chunk to start", || {
        dir.path().join("chunk1_started").is_file().then_some(())
    });
    stop_run(dir.path(), &run_id).unwrap();
    let _ = rx
        .recv_timeout(ABORT_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {ABORT_DEADLINE:?}"));

    let events = read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    let cancelled: Vec<(usize, String, String)> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.payload {
            EventPayload::NodeFinished {
                node,
                status,
                output,
                ..
            } if status == "cancelled" => Some((i, node.clone(), output.clone())),
            _ => None,
        })
        .collect();
    assert!(
        !cancelled.is_empty(),
        "the abort fixture must produce at least one cancelled member"
    );
    for (at, node, output) in cancelled {
        assert!(
            events[..at].iter().any(|e| matches!(
                &e.payload,
                EventPayload::NodeStarted { node: n, .. } if *n == node
            )),
            "cancelled member {node} has no preceding NodeStarted"
        );
        assert_eq!(
            output, "cancelled",
            "both cancel paths must render the same output text for {node}"
        );
    }
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

/// A stop must not eat the operator's OTHER pending commands.
///
/// The control cursor is a single scalar, so setting it to the Abort's seq
/// marks every lower-numbered entry applied as well. On the dead-run path those
/// lower entries are exactly the ones the crashed driver never got to - so a
/// crashed driver plus `apb note` plus `apb stop` plus `apb resume` silently
/// lost the note. The stop therefore leaves the cursor alone and lets the
/// terminal `RunAborted` guard the replay: the resumed drive applies the note
/// first and only then re-reads the abort.
#[cfg(unix)]
#[test]
fn a_note_the_crashed_driver_never_applied_survives_a_stop() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "stopflow");
    let stub = dir.path().join("quick.sh");
    fs::write(&stub, "#!/bin/sh\necho done\n").unwrap();
    set_executable(&stub);
    let _env = AgentEnv::set(&stub.to_string_lossy());

    // A real prepared run (snapshot, manifest, run config - everything a resume
    // needs), then the crash shape by hand: a node started, an attempt opened,
    // nothing closed, no driver.pid.
    let prepared = apb_engine::prepare_supervised_background(
        dir.path(),
        "stopflow",
        None,
        RunOptions::default(),
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    drop(prepared);
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    {
        let mut log = EventLog::open(&run_dir).unwrap();
        log.append(EventPayload::NodeStarted {
            node: "work".into(),
            attempt: 1,
        })
        .unwrap();
        log.append(EventPayload::AttemptStarted {
            node: "work".into(),
            attempt: 1,
            agent: "claude".into(),
            soul_delivery: None,
            skills_mode: None,
            pid: Some(999_999),
        })
        .unwrap();
    }

    // The operator posts a note, then stops the run. The note is queued ahead
    // of the stop's own Abort and nothing has applied it.
    let note = "remember the failing fixture";
    apb_engine::scheduler::post_supervisor_command(
        dir.path(),
        &run_id,
        Control::ContextAppend { note: note.into() },
    )
    .unwrap();
    assert_eq!(
        stop_run(dir.path(), &run_id).unwrap(),
        StopOutcome::FinalizedDeadRun
    );
    assert_eq!(
        read_control_cursor(&run_dir).unwrap(),
        None,
        "the stop must not mark the unapplied note consumed"
    );

    // The later drive applies it - exactly once - and then honours the stop.
    let res = apb_engine::scheduler::resume(dir.path(), &run_id, None).expect("resume returns");
    assert_eq!(
        res.outcome,
        RunStatus::Aborted,
        "the resumed drive must re-read the stop and stay stopped"
    );
    let applied = read_all(&run_dir)
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::SupervisorAction { action, detail, .. }
                if action == "context_append" && detail == note)
        })
        .count();
    assert_eq!(applied, 1, "the note must be applied exactly once");
    assert!(
        fs::read_to_string(run_dir.join("context.md"))
            .unwrap_or_default()
            .contains(note),
        "the note must reach the run context"
    );
}

/// A stop must produce an ABORTED run even when the operator left an
/// unconsumable command queued ahead of it.
///
/// `supervisor_node_retry` posted outside a wake is such a command: the
/// top-of-loop scan stops at it without advancing the cursor, so it can never
/// reach an Abort queued behind it. The abort watcher reads the raw file and
/// does see the abort, so the run-level cancel flag latches and every later
/// node returns `Cancelled` instantly - which used to fall through edge
/// selection, match nothing, and fail the drive with "has no outgoing edge",
/// stamping the run FAILED. An operator who asked for a stop must get a stop.
#[cfg(unix)]
#[test]
fn a_stop_queued_behind_a_retry_still_aborts_the_run() {
    let dir = tempfile::tempdir().unwrap();
    // The success-only playbook: a node killed by the stop has nowhere to go,
    // which is what turned the latched cancel into a hard drive error and so
    // into a run_finished(failed).
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/stopretry/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        WF_SUCCESS_ONLY.replace("stopdead", "stopretry"),
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/stopretry/current"), "1.0.0").unwrap();
    common::seed_main(dir.path());
    let (prog, marker) = sleepy_agent(dir.path());
    let _env = AgentEnv::set(&prog);

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopretry", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "stopretry-");
    poll_until("the stub agent to start", || marker.is_file().then_some(()));

    // Ordering is the whole point: the Retry lands FIRST, so the scan breaks on
    // it and never reaches the Abort behind it.
    apb_engine::scheduler::post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    )
    .unwrap();
    assert_eq!(
        stop_run(dir.path(), &run_id).unwrap(),
        StopOutcome::SignaledLiveDriver
    );

    let res = rx
        .recv_timeout(ABORT_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {ABORT_DEADLINE:?}"));
    let res = res.expect("a stop must abort cleanly, not fail the drive with an error");
    assert_eq!(
        res.outcome,
        RunStatus::Aborted,
        "a stop queued behind a retry must still abort, not fail"
    );

    let events = read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    assert!(
        matches!(
            events.last().map(|e| &e.payload),
            Some(EventPayload::RunAborted { .. })
        ),
        "the journal must end with RunAborted, got {:?}",
        events.last().map(|e| &e.payload)
    );
    assert_eq!(
        RunState::fold(&events).run_status,
        RunStatus::Aborted,
        "and the folded run must read aborted"
    );
}

/// The stop that supersedes a queued Retry must be CONSUMED, not merely
/// applied. The arm that finalizes a run whose pending Abort the scan cannot
/// reach writes the control cursor like every other applied command, so the
/// stopped run can be resumed normally afterwards. Without that write the
/// stale Retry stayed pending with the cursor below it forever: every later
/// resume re-entered the same arm and appended another RunAborted, with no way
/// out short of hand-editing control.jsonl - exactly the class of workaround
/// this release exists to remove. The discarded Retry is journaled as a
/// `retry_superseded_by_stop` supervisor action so the loss is visible.
#[cfg(unix)]
#[test]
fn a_stop_that_supersedes_a_retry_leaves_the_run_resumable() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/stopresume/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        WF_SUCCESS_ONLY.replace("stopdead", "stopresume"),
    )
    .unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/stopresume/current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(dir.path());
    let (prog, marker) = sleepy_agent(dir.path());
    let _env = AgentEnv::set(&prog);

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "stopresume", None, RunOptions::default()));
    });
    let run_id = find_run_id(dir.path(), "stopresume-");
    poll_until("the stub agent to start", || marker.is_file().then_some(()));

    apb_engine::scheduler::post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    )
    .unwrap();
    stop_run(dir.path(), &run_id).unwrap();
    let res = rx
        .recv_timeout(ABORT_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {ABORT_DEADLINE:?}"));
    assert_eq!(res.unwrap().outcome, RunStatus::Aborted);

    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(
                |e| matches!(&e.payload, EventPayload::SupervisorAction { action, node, .. }
                if action == "retry_superseded_by_stop" && node.as_deref() == Some("work"))
            )
            .count(),
        1,
        "the discarded retry must be journaled, not dropped silently"
    );
    // The stop consumed everything up to and including its own Abort, so
    // nothing is left pending for the next drive to trip over.
    assert_eq!(
        apb_engine::control::pending_stop_seq(&run_dir).unwrap(),
        None,
        "the applied stop must not still read as pending"
    );

    // The run is resumable: swap the agent for one that returns at once (the
    // binary changed, hence the drift allowance) and let it finish. The killed
    // node is journaled `cancelled` rather than interrupted and its only edge
    // needs success, so this is the `--from-node` recovery the docs prescribe -
    // what matters here is that the resumed drive PROGRESSES instead of
    // re-entering the abort arm.
    fs::write(&prog, "#!/bin/sh\necho done\n").unwrap();
    set_executable(Path::new(&prog));
    let resumed = apb_engine::scheduler::resume_with(dir.path(), &run_id, Some("work"), true)
        .expect("a stopped run must stay resumable");
    assert_eq!(
        resumed.outcome,
        RunStatus::Succeeded,
        "the resumed run must actually progress instead of re-aborting"
    );
    assert_eq!(
        read_all(&run_dir)
            .unwrap()
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::RunAborted { .. }))
            .count(),
        1,
        "resuming a stopped run must not append another RunAborted"
    );
}

/// The stop, note, resume pattern the release notes teach, in the order they
/// teach it: the note is posted AFTER the stop and so carries a HIGHER seq than
/// the Abort. The first resume therefore hits the Abort first, applies it and
/// returns without ever reaching the note - so the operator has to resume a
/// second time. The note must survive that first resume intact and be applied
/// exactly once by the second, and `pending_stop_seq` (what `apb resume` and
/// the `run_resume` ack report) must be what tells the operator which of the
/// two resumes they are looking at.
#[cfg(unix)]
#[test]
fn a_note_posted_after_a_stop_survives_the_resume_that_applies_the_stop() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "stopflow");
    let stub = dir.path().join("quick.sh");
    fs::write(&stub, "#!/bin/sh\necho done\n").unwrap();
    set_executable(&stub);
    let _env = AgentEnv::set(&stub.to_string_lossy());

    let prepared = apb_engine::prepare_supervised_background(
        dir.path(),
        "stopflow",
        None,
        RunOptions::default(),
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    drop(prepared);
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    {
        let mut log = EventLog::open(&run_dir).unwrap();
        log.append(EventPayload::NodeStarted {
            node: "work".into(),
            attempt: 1,
        })
        .unwrap();
        log.append(EventPayload::AttemptStarted {
            node: "work".into(),
            attempt: 1,
            agent: "claude".into(),
            soul_delivery: None,
            skills_mode: None,
            pid: Some(999_999),
        })
        .unwrap();
    }

    // The documented ordering: stop first, then leave the note.
    assert_eq!(
        stop_run(dir.path(), &run_id).unwrap(),
        StopOutcome::FinalizedDeadRun
    );
    let note = "the fixture path moved";
    apb_engine::scheduler::post_supervisor_command(
        dir.path(),
        &run_id,
        Control::ContextAppend { note: note.into() },
    )
    .unwrap();

    // Resume 1 consumes the pending stop and nothing else. This is what the
    // CLI and the run_resume ack now announce, so the operator knows to go
    // again rather than concluding the resume did nothing.
    assert!(
        apb_engine::control::pending_stop_seq(&run_dir)
            .unwrap()
            .is_some(),
        "the stop is still pending before the first resume"
    );
    let first = apb_engine::scheduler::resume(dir.path(), &run_id, None).expect("resume returns");
    assert_eq!(
        first.outcome,
        RunStatus::Aborted,
        "the first resume applies the pending stop and stops again"
    );
    let after_first = read_all(&run_dir).unwrap();
    assert_eq!(
        after_first
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::SupervisorAction { detail, .. } if detail == note))
            .count(),
        0,
        "the note sits behind the abort, so the first resume must not have reached it"
    );

    // Resume 2 gets past it: the stop is consumed, the note is applied once.
    assert_eq!(
        apb_engine::control::pending_stop_seq(&run_dir).unwrap(),
        None,
        "the stop is consumed now, so the second resume proceeds"
    );
    let second = apb_engine::scheduler::resume(dir.path(), &run_id, None).expect("resume returns");
    assert_eq!(
        second.outcome,
        RunStatus::Succeeded,
        "the second resume runs the playbook out"
    );
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::SupervisorAction { detail, .. } if detail == note))
            .count(),
        1,
        "the note must survive the stop-applying resume and be applied exactly once"
    );
    assert!(
        fs::read_to_string(run_dir.join("context.md"))
            .unwrap_or_default()
            .contains(note),
        "the note must reach the run context"
    );
}
