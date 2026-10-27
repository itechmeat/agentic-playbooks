//! Issue #200, stage A2: the operator pause end to end through the engine.
//!
//! Properties pinned here, every wait bounded so a regression fails fast:
//!   (AC1) a drain pause while an agent node runs starts no new node, the run
//!         reads paused when the node ends, and the resume continues from the
//!         next node without running a finished node again;
//!   (AC2) a `--now` pause interrupts a running agent node and a running
//!         script node (SIGTERM first): the attempt is `interrupted`, there is
//!         no `node_finished` and no `retry_started`, and the resume runs it
//!         again;
//!   - a `--now` pause of a parallel batch reruns every interrupted member;
//!   - `--keep-lock` keeps the workdir lock across the pause (a second
//!     write-run is refused), its stop releases it, its own resume takes it
//!     back, and a default pause releases it;
//!   - the waker of a pause with `until` exits when the run is resumed first;
//!   - `resume_all` resumes a due paused run, marks a driverless run it cannot
//!     resume, leaves a run paused into the future, and skips children.

use crate::common;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventLog, EventPayload, read_all};
use apb_engine::pause::{
    PauseBy, PauseMode, PauseOutcome, PauseRequest, ResumeAllAction, ResumeCause, WakeOutcome,
    request_pause, resume_all, run_waker,
};
use apb_engine::scheduler::{
    ResumeReason, RunOptions, RunResult, plan_resume, resume, resume_by, run,
};
use apb_engine::state::RunStatus;
use apb_engine::stop::stop_run;
use apb_engine::workdir::{LockOwner, acquire_tree, lock_owner};

const DEADLINE: Duration = Duration::from_secs(20);
const POLL_STEP: Duration = Duration::from_millis(20);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if start.elapsed() > DEADLINE {
            panic!("timed out after {DEADLINE:?} waiting for: {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn find_run_id(root: &Path, prefix: &str) -> String {
    poll_until(&format!("a run dir with prefix `{prefix}`"), || {
        fs::read_dir(root.join(".apb/runs"))
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

/// Holds the shared env lock and the variables this suite sets, removing them
/// on drop (a panic included).
struct Env {
    _lock: std::sync::MutexGuard<'static, ()>,
    keys: Vec<&'static str>,
}

impl Env {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let lock = common::env_lock();
        for (k, v) in vars {
            unsafe { std::env::set_var(k, v) };
        }
        Self {
            _lock: lock,
            keys: vars.iter().map(|(k, _)| *k).collect(),
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for k in &self.keys {
            unsafe { std::env::remove_var(k) };
        }
    }
}

fn seed_playbook(root: &Path, id: &str, yaml: &str, scripts: &[(&str, String)]) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    common::seed_main(root);
}

/// A stub agent: counts its calls, announces itself, and sleeps `slow_secs`
/// while the file `slow` exists.
fn stub_agent(dir: &Path, slow_secs: u32) -> String {
    let path = dir.join("agent.sh");
    let d = dir.display();
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\necho x >> '{d}/calls'\ntouch '{d}/agent_running'\nif [ -f '{d}/slow' ]; then sleep {slow_secs}; fi\necho done\n"
        ),
    );
    set_executable(&path);
    path.to_string_lossy().into_owned()
}

/// A stub agent: counts its calls, announces itself, and waits while the
/// file `gate` exists, so a test decides when the node ends.
fn gated_agent(dir: &Path) -> String {
    let path = dir.join("agent.sh");
    let d = dir.display();
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\necho x >> '{d}/calls'\ntouch '{d}/agent_running'\nwhile [ -f '{d}/gate' ]; do sleep 0.05; done\necho done\n"
        ),
    );
    set_executable(&path);
    path.to_string_lossy().into_owned()
}

/// A script that announces itself as `<name>_running`, records a SIGTERM as
/// `term_seen`, and sleeps while the file `slow` exists.
fn slow_script(root: &Path, name: &str) -> String {
    let d = root.display();
    format!(
        "trap 'touch \"{d}/term_seen\"; exit 1' TERM\ntouch \"{d}/{name}_running\"\nif [ -f \"{d}/slow\" ]; then sleep 30 & wait $!; fi\necho ok\n"
    )
}

fn spawn_run(root: &Path, id: &str) -> mpsc::Receiver<Result<RunResult, EngineError>> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    let id = id.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, &id, None, RunOptions::default()));
    });
    rx
}

fn finished(rx: &mpsc::Receiver<Result<RunResult, EngineError>>) -> RunResult {
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {DEADLINE:?}"))
        .expect("the drive must not fail")
}

fn events(root: &Path, run_id: &str) -> Vec<Event> {
    read_all(&root.join(".apb/runs").join(run_id)).unwrap()
}

fn node_finishes(events: &[Event], node: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n, status, ..
            } if n == node => Some(status.clone()),
            _ => None,
        })
        .collect()
}

fn node_started(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
}

fn interrupted_attempts(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::AttemptFinished { node: n, status, .. }
                if n == node && status == "interrupted")
        })
        .count()
}

fn retries(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::RetryStarted { .. }))
        .count()
}

fn last_pause(events: &[Event]) -> (Option<PauseBy>, Option<PauseMode>, bool) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunPaused {
                by,
                mode,
                keep_lock,
                ..
            } => Some((*by, *mode, *keep_lock)),
            _ => None,
        })
        .expect("a run_paused event")
}

fn resumed_by(events: &[Event]) -> Option<PauseBy> {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunResumed { by, .. } => Some(*by),
            _ => None,
        })
        .flatten()
}

fn operator(mode: PauseMode) -> PauseRequest {
    PauseRequest {
        reason: Some("later".into()),
        by: Some(PauseBy::Operator),
        mode,
        ..PauseRequest::default()
    }
}

const TWO_AGENTS: &str = r#"
schema: 1
id: drainpb
name: Drain
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "first" }
  - { id: b, type: agent_task, prompt: "second" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: b, to: done }
"#;

/// AC1: drain while an agent node runs.
#[cfg(unix)]
#[test]
fn a_drain_pause_lets_the_running_node_finish_and_the_resume_continues() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(root, "drainpb", TWO_AGENTS, &[]);
    let prog = gated_agent(root);
    let _env = Env::set(&[("APB_AGENT_CMD", &prog)]);
    fs::write(root.join("gate"), "").unwrap();

    let rx = spawn_run(root, "drainpb");
    let run_id = find_run_id(root, "drainpb-");
    poll_until("agent a to start", || {
        root.join("agent_running").is_file().then_some(())
    });
    // Agent a is held until the pause is queued, so the pause always lands
    // while it runs.
    let out = request_pause(root, &run_id, &operator(PauseMode::Drain)).unwrap();
    assert!(matches!(out, PauseOutcome::Posted { .. }), "{out:?}");
    fs::remove_file(root.join("gate")).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);

    let ev = events(root, &run_id);
    assert_eq!(node_finishes(&ev, "a"), vec!["succeeded"]);
    assert!(!node_started(&ev, "b"), "a drain pause starts no new node");
    assert_eq!(
        last_pause(&ev),
        (Some(PauseBy::Operator), Some(PauseMode::Drain), false)
    );

    let res = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &run_id);
    assert_eq!(node_finishes(&ev, "a"), vec!["succeeded"], "a ran once");
    assert_eq!(node_finishes(&ev, "b"), vec!["succeeded"]);
    assert_eq!(resumed_by(&ev), Some(PauseBy::Operator));
}

const ONE_AGENT: &str = r#"
schema: 1
id: nowagent
name: Now Agent
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do", max_retries: 3 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// AC2, agent half: `--now` interrupts the agent, spends no retry, and the
/// resume runs the node again.
#[cfg(unix)]
#[test]
fn a_now_pause_interrupts_an_agent_node_without_spending_a_retry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(root, "nowagent", ONE_AGENT, &[]);
    let prog = stub_agent(root, 30);
    let _env = Env::set(&[("APB_AGENT_CMD", &prog), ("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("slow"), "").unwrap();

    let rx = spawn_run(root, "nowagent");
    let run_id = find_run_id(root, "nowagent-");
    poll_until("the agent to start", || {
        root.join("agent_running").is_file().then_some(())
    });
    let started = Instant::now();
    request_pause(root, &run_id, &operator(PauseMode::Now)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the agent was not interrupted"
    );

    let ev = events(root, &run_id);
    assert_eq!(interrupted_attempts(&ev, "work"), 1);
    assert!(node_finishes(&ev, "work").is_empty(), "no node_finished");
    assert_eq!(retries(&ev), 0, "a pause spends no retry");
    assert_eq!(last_pause(&ev).1, Some(PauseMode::Now));

    fs::remove_file(root.join("slow")).unwrap();
    let res = resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &run_id);
    assert_eq!(node_finishes(&ev, "work"), vec!["succeeded"]);
    assert_eq!(retries(&ev), 0);
}

const ONE_SCRIPT: &str = r#"
schema: 1
id: nowscript
name: Now Script
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: "scripts/s.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: done }
"#;

/// AC2, script half: the script gets SIGTERM first, the attempt is
/// interrupted, and the resume runs it again.
#[cfg(unix)]
#[test]
fn a_now_pause_sends_sigterm_to_a_script_and_reruns_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "5000")]);
    fs::write(root.join("slow"), "").unwrap();

    let rx = spawn_run(root, "nowscript");
    let run_id = find_run_id(root, "nowscript-");
    poll_until("the script to start", || {
        root.join("s_running").is_file().then_some(())
    });
    request_pause(root, &run_id, &operator(PauseMode::Now)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    assert!(
        root.join("term_seen").is_file(),
        "the script must get SIGTERM before any SIGKILL"
    );
    let ev = events(root, &run_id);
    assert_eq!(interrupted_attempts(&ev, "s"), 1);
    assert!(node_finishes(&ev, "s").is_empty());

    fs::remove_file(root.join("slow")).unwrap();
    let res = resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        node_finishes(&events(root, &run_id), "s"),
        vec!["succeeded"]
    );
}

const PARALLEL: &str = r#"
schema: 1
id: nowpar
name: Now Parallel
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/a.sh", runner: sh }
  - { id: b, type: script, script: "scripts/b.sh", runner: sh }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }
"#;

/// A `--now` pause of a parallel batch: every interrupted member runs again
/// on resume, and no finished node does.
#[cfg(unix)]
#[test]
fn a_now_pause_of_a_parallel_batch_reruns_every_interrupted_member() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowpar",
        PARALLEL,
        &[
            ("a.sh", slow_script(root, "a")),
            ("b.sh", slow_script(root, "b")),
        ],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("slow"), "").unwrap();

    let rx = spawn_run(root, "nowpar");
    let run_id = find_run_id(root, "nowpar-");
    poll_until("both members to start", || {
        (root.join("a_running").is_file() && root.join("b_running").is_file()).then_some(())
    });
    request_pause(root, &run_id, &operator(PauseMode::Now)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    let ev = events(root, &run_id);
    for n in ["a", "b"] {
        assert_eq!(interrupted_attempts(&ev, n), 1, "member {n}");
        assert!(node_finishes(&ev, n).is_empty(), "member {n}");
    }
    let plan = plan_resume(root, &run_id, None).unwrap();
    assert_eq!(plan.reason, ResumeReason::PausedParallelRestart);

    fs::remove_file(root.join("slow")).unwrap();
    let res = resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &run_id);
    for n in ["a", "b", "j"] {
        assert_eq!(node_finishes(&ev, n), vec!["succeeded"], "node {n}");
    }
    assert_eq!(node_finishes(&ev, "start").len(), 1, "start never reruns");
}

/// Runs `nowscript` until its script is in flight and pauses it `--now` with
/// `keep_lock`; returns the run id once the drive has ended paused.
fn pause_script_run(root: &Path, keep_lock: bool) -> String {
    for f in ["s_running", "term_seen"] {
        let _ = fs::remove_file(root.join(f));
    }
    fs::write(root.join("slow"), "").unwrap();
    let rx = spawn_run(root, "nowscript");
    let run_id = poll_until("a new run to start its script", || {
        if !root.join("s_running").is_file() {
            return None;
        }
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .find(|id| {
                read_all(&root.join(".apb/runs").join(id)).is_ok_and(|ev| {
                    apb_engine::state::RunState::fold(&ev).run_status == RunStatus::Running
                })
            })
    });
    let req = PauseRequest {
        keep_lock,
        ..operator(PauseMode::Now)
    };
    request_pause(root, &run_id, &req).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    fs::remove_file(root.join("slow")).unwrap();
    run_id
}

/// `--keep-lock`: the paused run keeps the workdir lock, a second write-run
/// is refused, a stop releases it; its own resume takes it back; a default
/// pause releases it.
#[cfg(unix)]
#[test]
fn keep_lock_holds_the_workdir_until_the_run_is_resumed_or_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200")]);
    let lock = root.join(".apb/workdir.lock");

    // Kept, then released by a stop.
    let kept = pause_script_run(root, true);
    assert_eq!(lock_owner(&lock), Some(LockOwner::PausedRun(kept.clone())));
    assert!(last_pause(&events(root, &kept)).2);
    let busy = acquire_tree(root, None, false).unwrap_err();
    assert!(matches!(busy, EngineError::WorkdirBusy(_)), "{busy}");
    stop_run(root, &kept).unwrap();
    assert_eq!(lock_owner(&lock), None, "a stop releases the kept lock");

    // Kept, then taken back by the run's own resume.
    let kept = pause_script_run(root, true);
    assert_eq!(lock_owner(&lock), Some(LockOwner::PausedRun(kept.clone())));
    let res = resume(root, &kept, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(lock_owner(&lock), None, "the finished resume released it");

    // A default pause releases the lock.
    let released = pause_script_run(root, false);
    assert_eq!(lock_owner(&lock), None);
    drop(acquire_tree(root, None, false).expect("the workdir is free"));
    stop_run(root, &released).unwrap();
}

/// The waker exits without acting when the run is resumed before `until`.
#[cfg(unix)]
#[test]
fn the_waker_exits_when_the_run_is_resumed_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200"), ("APB_WAKER_POLL_MS", "50")]);
    let run_id = pause_script_run(root, false);

    // Pausing a paused run updates it: a new `run_paused` with the `until`.
    let until = u64::try_from(apb_core::clock::now_ms()).unwrap() + 600_000;
    let req = PauseRequest {
        until: Some(until),
        ..operator(PauseMode::Drain)
    };
    let PauseOutcome::Journaled { seq } = request_pause(root, &run_id, &req).unwrap() else {
        panic!("a paused run is updated in place");
    };
    let (tx, rx) = mpsc::channel();
    let (r, id) = (root.to_path_buf(), run_id.clone());
    std::thread::spawn(move || {
        let _ = tx.send(run_waker(&r, &id, seq));
    });
    // Smoke for the in-loop path: whether the waker's first journal read
    // lands before or after the resume, the outcome is `Superseded` (both
    // paths end there), so this pause only makes the sleeping-waker path the
    // likely one; it is not a synchronization the assertion depends on.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        resume(root, &run_id, None).unwrap().outcome,
        RunStatus::Succeeded
    );
    let woke = rx
        .recv_timeout(DEADLINE)
        .expect("the waker must notice the early resume")
        .unwrap();
    assert_eq!(woke, WakeOutcome::Superseded);
}

/// A terminal run cannot be paused.
#[test]
fn pausing_a_finished_run_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run_dir = seed_journal(root, "r-done", &[]);
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(EventPayload::RunFinished {
        outcome: "succeeded".into(),
    })
    .unwrap();
    let err = request_pause(root, "r-done", &operator(PauseMode::Drain)).unwrap_err();
    assert!(err.to_string().contains("already succeeded"), "{err}");
}

/// A hand-written run directory: `run_started`, then `extra`.
fn seed_journal(root: &Path, run_id: &str, extra: &[EventPayload]) -> PathBuf {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    let mut log = EventLog::create(&run_dir).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "nowscript".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    for p in extra {
        log.append(p.clone()).unwrap();
    }
    run_dir
}

/// `resume_all`: a due paused run is resumed, a driverless run the gate
/// refuses is marked paused (driverless), a run paused into the future is
/// left alone, and a sub-playbook child is skipped.
#[cfg(unix)]
#[test]
fn resume_all_picks_due_and_driverless_runs_and_skips_children() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    // An installation key, so the runs started here are stamped as local.
    let cfg = tempfile::tempdir().unwrap();
    let cfg_dir = cfg.path().to_string_lossy().into_owned();
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200"), ("APB_CONFIG_DIR", &cfg_dir)]);
    apb_core::run_origin::ensure_key().unwrap();
    let now = u64::try_from(apb_core::clock::now_ms()).unwrap();

    // A real run (created here, so the gate passes) paused until the past.
    let due = pause_script_run(root, false);
    request_pause(
        root,
        &due,
        &PauseRequest {
            until: Some(now.saturating_sub(1_000)),
            ..operator(PauseMode::Drain)
        },
    )
    .unwrap();
    // A real run paused until far in the future.
    let later = pause_script_run(root, false);
    request_pause(
        root,
        &later,
        &PauseRequest {
            until: Some(now + 3_600_000),
            ..operator(PauseMode::Drain)
        },
    )
    .unwrap();
    // A hand-written run left `running` with no driver: not created by apb
    // here, so the gate refuses to resume it.
    let started = EventPayload::NodeStarted {
        node: "s".into(),
        attempt: 1,
    };
    seed_journal(root, "r-orphan", std::slice::from_ref(&started));
    // A sub-playbook child left the same way: it follows its parent.
    let child = seed_journal(root, "r-child", &[started]);
    fs::write(child.join("run.yaml"), "parent_run: r-orphan\n").unwrap();

    let entries = resume_all(root).unwrap();
    let action = |id: &str| {
        entries
            .iter()
            .find(|e| e.run_id == id)
            .map(|e| e.action.clone())
    };
    assert!(
        matches!(
            action(&due),
            Some(ResumeAllAction::Resumed {
                cause: ResumeCause::Due,
                ..
            })
        ),
        "{entries:?}"
    );
    assert_eq!(action(&later), Some(ResumeAllAction::LeftPaused));
    assert!(
        matches!(
            action("r-orphan"),
            Some(ResumeAllAction::MarkedDriverless { .. })
        ),
        "{entries:?}"
    );
    assert_eq!(action("r-child"), None, "children follow their parent");

    let orphan = events(root, "r-orphan");
    match orphan.last().map(|e| &e.payload) {
        Some(EventPayload::RunPaused { reason, by, .. }) => {
            assert_eq!(reason, "driverless");
            assert_eq!(*by, Some(PauseBy::Engine));
        }
        other => panic!("expected run_paused driverless, got {other:?}"),
    }
    // Marked paused, it is left alone by the next pass.
    let again = resume_all(root).unwrap();
    assert!(
        again
            .iter()
            .any(|e| e.run_id == "r-orphan" && e.action == ResumeAllAction::LeftPaused),
        "{again:?}"
    );
}

/// A run whose driver died mid-attempt leaves an open `attempt_started`. A
/// pause with `until` journaled on it reads paused, not interrupted, so its
/// waker resumes the run at `until`.
#[cfg(unix)]
#[test]
fn a_timed_pause_of_a_run_whose_driver_died_mid_attempt_is_resumed_by_its_waker() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let cfg = tempfile::tempdir().unwrap();
    let cfg_dir = cfg.path().to_string_lossy().into_owned();
    let _env = Env::set(&[
        ("APB_PAUSE_GRACE_MS", "200"),
        ("APB_WAKER_POLL_MS", "50"),
        ("APB_CONFIG_DIR", &cfg_dir),
    ]);
    apb_core::run_origin::ensure_key().unwrap();
    // A real run (stamped here), then the journal a driver that resumed it
    // and died mid-attempt leaves: an attempt whose process is gone.
    let run_id = pause_script_run(root, false);
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = gone.id();
    gone.wait().unwrap();
    let run_dir = root.join(".apb/runs").join(&run_id);
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(EventPayload::RunResumed {
        from_node: "s".into(),
        by: Some(PauseBy::Operator),
        explicit: false,
    })
    .unwrap();
    log.append(EventPayload::AttemptStarted {
        item: None,
        node: "s".into(),
        attempt: 2,
        agent: "script".into(),
        soul_delivery: None,
        skills_mode: None,
        pid: Some(dead_pid),
        spawn_ms: None,
        model: None,
        workdir: None,
        transcript: None,
        token: None,
    })
    .unwrap();
    drop(log);

    let until = u64::try_from(apb_core::clock::now_ms()).unwrap() + 300;
    let req = PauseRequest {
        until: Some(until),
        ..operator(PauseMode::Drain)
    };
    let PauseOutcome::Journaled { seq } = request_pause(root, &run_id, &req).unwrap() else {
        panic!("a run nothing drives is paused on the spot");
    };
    assert_eq!(
        apb_engine::state::RunState::fold(&events(root, &run_id)).run_status,
        RunStatus::Paused
    );
    let woke = run_waker(root, &run_id, seq).unwrap();
    // The detached driver needs the shipped `apb` binary; that the waker
    // handed the run to one is the property here.
    assert!(matches!(woke, WakeOutcome::Resumed { .. }), "{woke:?}");
}

/// `resume_all` leaves a driverless run as it is while another run holds the
/// busy lock of its tree (a later pass resumes it), and a driverless run it
/// can neither resume nor mark paused is reported refused while the pass
/// goes on to the other runs.
#[cfg(unix)]
#[test]
fn resume_all_waits_out_a_busy_workdir_and_reports_a_run_it_cannot_mark() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let cfg = tempfile::tempdir().unwrap();
    let cfg_dir = cfg.path().to_string_lossy().into_owned();
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200"), ("APB_CONFIG_DIR", &cfg_dir)]);
    apb_core::run_origin::ensure_key().unwrap();

    // A real run whose driver is gone after a resume: it reads running.
    let busy_run = pause_script_run(root, false);
    let mut log = EventLog::open(&root.join(".apb/runs").join(&busy_run)).unwrap();
    log.append(EventPayload::RunResumed {
        from_node: "s".into(),
        by: Some(PauseBy::Operator),
        explicit: false,
    })
    .unwrap();
    drop(log);
    // A hand-written driverless run the gate refuses, whose journal cannot
    // be written.
    let orphan = seed_journal(root, "r-locked", &[]);
    let journal = orphan.join("events.jsonl");
    assert!(journal.is_file(), "the journal file name changed");
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o444)).unwrap();

    let held = acquire_tree(root, None, false).unwrap();
    let entries = resume_all(root).unwrap();
    let action = |id: &str| {
        entries
            .iter()
            .find(|e| e.run_id == id)
            .map(|e| e.action.clone())
    };
    assert!(
        matches!(action(&busy_run), Some(ResumeAllAction::Busy { .. })),
        "{entries:?}"
    );
    assert!(
        matches!(action("r-locked"), Some(ResumeAllAction::Refused { .. })),
        "{entries:?}"
    );
    assert!(
        !events(root, &busy_run).iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunPaused { reason, .. } if reason == "driverless"
        )),
        "a busy workdir does not mark the run driverless"
    );
    drop(held);
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o644)).unwrap();
}

/// The engine's resume of one pause journals `run_resumed` only while that
/// pause still holds the run: a newer pause in between makes it a no-op.
#[cfg(unix)]
#[test]
fn the_engine_resume_of_a_replaced_pause_journals_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200")]);
    let run_id = pause_script_run(root, false);
    let first = latest_pause_seq(&events(root, &run_id));
    let PauseOutcome::Journaled { seq: second } =
        request_pause(root, &run_id, &operator(PauseMode::Drain)).unwrap()
    else {
        panic!("a paused run is updated in place");
    };
    let err = apb_engine::scheduler::resume_pause(root, &run_id, first).unwrap_err();
    assert!(
        matches!(&err, EngineError::Conflict(m) if m == apb_engine::scheduler::PAUSE_SUPERSEDED),
        "{err:?}"
    );
    assert!(resumed_by(&events(root, &run_id)).is_none());
    let res = apb_engine::scheduler::resume_pause(root, &run_id, second).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(resumed_by(&events(root, &run_id)), Some(PauseBy::Engine));
}

/// The engine's resume of a pause that a stop or a newer pause replaced
/// returns the superseded conflict before anything else can refuse it (and
/// make the detached driver journal a misleading error): not a resume-plan
/// refusal after a stop, not a busy workdir.
#[cfg(unix)]
#[test]
fn the_engine_resume_of_a_replaced_pause_refuses_before_any_other_check() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200")]);
    let superseded = |err: &EngineError| matches!(err, EngineError::Conflict(m) if m == apb_engine::scheduler::PAUSE_SUPERSEDED);

    // Replaced by a newer pause while another run holds the workdir.
    let run_id = pause_script_run(root, false);
    let first = latest_pause_seq(&events(root, &run_id));
    request_pause(root, &run_id, &operator(PauseMode::Drain)).unwrap();
    let held = acquire_tree(root, None, false).unwrap();
    let err = apb_engine::scheduler::resume_pause(root, &run_id, first).unwrap_err();
    assert!(superseded(&err), "{err:?}");
    drop(held);

    // Ended by a stop.
    let stopped = pause_script_run(root, false);
    let seq = latest_pause_seq(&events(root, &stopped));
    stop_run(root, &stopped).unwrap();
    let before = events(root, &stopped).len();
    let err = apb_engine::scheduler::resume_pause(root, &stopped, seq).unwrap_err();
    assert!(superseded(&err), "{err:?}");
    assert_eq!(events(root, &stopped).len(), before, "nothing journaled");
}

/// A resume journals `run_resumed` under the run's event lock and only while
/// the run is as it planned: an operator resume that loses the race to
/// another resume journals nothing, so the run has one `run_resumed`.
#[cfg(unix)]
#[test]
fn two_racing_resumes_journal_one_run_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200")]);
    let run_id = pause_script_run(root, false);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let lock_file = root.join(".apb/workdir.lock");

    let event_lock = apb_core::fsutil::lock_dir(&run_dir, "events.jsonl.lock").unwrap();
    let (tx, rx) = mpsc::channel();
    let (r, id) = (root.to_path_buf(), run_id.clone());
    std::thread::spawn(move || {
        let _ = tx.send(resume(&r, &id, None));
    });
    // The operator resume took the workdir lock and waits for the event lock.
    poll_until("the resume to take the workdir lock", || {
        (lock_owner(&lock_file) == Some(LockOwner::Pid(std::process::id()))).then_some(())
    });
    // Another resume wins the race.
    EventLog::open(&run_dir)
        .unwrap()
        .append(EventPayload::RunResumed {
            from_node: "s".into(),
            by: Some(PauseBy::Engine),
            explicit: false,
        })
        .unwrap();
    drop(event_lock);

    let err = rx.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert!(
        matches!(&err, EngineError::Conflict(m) if m == apb_engine::scheduler::RESUME_SUPERSEDED),
        "{err:?}"
    );
    let resumed = events(root, &run_id)
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .count();
    assert_eq!(resumed, 1);
    assert_eq!(lock_owner(&lock_file), None, "the refused resume lets go");
}

/// An engine resume that took a lock its run had parked, then found that a
/// newer `--keep-lock` pause replaced the one it was for, parks the lock
/// again instead of removing it.
#[cfg(unix)]
#[test]
fn a_replaced_engine_resume_gives_a_kept_lock_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200")]);
    let run_id = pause_script_run(root, true);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let lock_file = root.join(".apb/workdir.lock");
    let seq = latest_pause_seq(&events(root, &run_id));
    assert_eq!(
        lock_owner(&lock_file),
        Some(LockOwner::PausedRun(run_id.clone()))
    );

    let event_lock = apb_core::fsutil::lock_dir(&run_dir, "events.jsonl.lock").unwrap();
    let (tx, rx) = mpsc::channel();
    let (r, id) = (root.to_path_buf(), run_id.clone());
    std::thread::spawn(move || {
        let _ = tx.send(apb_engine::scheduler::resume_pause(&r, &id, seq));
    });
    poll_until("the engine resume to take the parked lock", || {
        (lock_owner(&lock_file) == Some(LockOwner::Pid(std::process::id()))).then_some(())
    });
    // A newer `--keep-lock` pause lands before the resume journals.
    EventLog::open(&run_dir)
        .unwrap()
        .append(
            PauseRequest {
                keep_lock: true,
                ..operator(PauseMode::Drain)
            }
            .to_event(),
        )
        .unwrap();
    drop(event_lock);

    let err = rx.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert!(
        matches!(&err, EngineError::Conflict(m) if m == apb_engine::scheduler::PAUSE_SUPERSEDED),
        "{err:?}"
    );
    assert_eq!(
        lock_owner(&lock_file),
        Some(LockOwner::PausedRun(run_id.clone())),
        "the newer pause keeps the lock"
    );
    stop_run(root, &run_id).unwrap();
}

/// A resume of a `--keep-lock` pause that cannot journal its `run_resumed`
/// (the run's event lock stays busy) parks the lock it took back under the
/// run, so the still-paused run keeps its workdir.
#[cfg(unix)]
#[test]
fn a_resume_that_cannot_reach_the_journal_gives_a_kept_lock_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(
        root,
        "nowscript",
        ONE_SCRIPT,
        &[("s.sh", slow_script(root, "s"))],
    );
    let _env = Env::set(&[("APB_PAUSE_GRACE_MS", "200")]);
    let run_id = pause_script_run(root, true);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let lock_file = root.join(".apb/workdir.lock");

    let event_lock = apb_core::fsutil::lock_dir(&run_dir, "events.jsonl.lock").unwrap();
    let err = resume(root, &run_id, None).unwrap_err();
    assert!(matches!(err, EngineError::Io(_)), "{err:?}");
    assert_eq!(
        lock_owner(&lock_file),
        Some(LockOwner::PausedRun(run_id.clone())),
        "the paused run keeps its lock"
    );
    assert_eq!(
        apb_engine::state::RunState::fold(&events(root, &run_id)).run_status,
        RunStatus::Paused
    );
    drop(event_lock);
    stop_run(root, &run_id).unwrap();
}

fn latest_pause_seq(events: &[Event]) -> u64 {
    events
        .iter()
        .rev()
        .find_map(|e| matches!(e.payload, EventPayload::RunPaused { .. }).then_some(e.seq))
        .expect("a run_paused event")
}

/// AC2, agent session: the attempt a `--now` pause cuts short keeps the
/// node's agent session, and the resumed attempt continues that same
/// session. The first attempt fails (its session is captured), the retry
/// continues it and is paused mid-work, and the resume continues it again.
#[cfg(unix)]
#[test]
fn a_now_pause_keeps_the_agent_session_for_the_resume() {
    use crate::token_economy_test::{invocations, recording_stub};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    crate::token_economy_test::seed_playbook(
        root,
        "sess",
        "schema: 2\nid: sess\nname: Sess\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"do\", max_retries: 1 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
    );
    common::seed_profile(root, "main", "claude", "haiku", &[]);
    let d = root.display();
    let stub = recording_stub(
        root,
        &format!(
            "if [ \"$N\" = 1 ]; then printf '\\n```yaml\\nstatus: failure\\nsummary: not yet\\n```\\n'; exit 0; fi\n\
             if [ \"$N\" = 2 ]; then touch '{d}/agent_running'; while [ -f '{d}/gate' ]; do sleep 0.05; done; fi\n\
             printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'"
        ),
    );
    let _env = Env::set(&[
        ("APB_AGENT_CMD", &stub),
        ("APB_INFRA_BACKOFF_MS", "1,1"),
        ("APB_PAUSE_GRACE_MS", "300"),
    ]);
    fs::write(root.join("gate"), "").unwrap();

    let rx = spawn_run(root, "sess");
    let run_id = find_run_id(root, "sess-");
    poll_until("the retry to start", || {
        root.join("agent_running").is_file().then_some(())
    });
    request_pause(root, &run_id, &operator(PauseMode::Now)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    fs::remove_file(root.join("gate")).unwrap();
    let res = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let inv = invocations(root);
    let work: Vec<_> = inv.iter().filter(|i| i.node == "work").collect();
    assert_eq!(work.len(), 3, "fail, paused retry, resumed attempt");
    let sid = work[0]
        .flag("--session-id")
        .expect("the first attempt opens a session");
    assert_eq!(
        work[1].flag("--resume"),
        Some(sid),
        "the retry continues it"
    );
    assert_eq!(
        work[2].flag("--resume"),
        Some(sid),
        "the resumed attempt continues the same session"
    );
    assert!(
        work[2].prompt().contains("stopped by a pause"),
        "{}",
        work[2].prompt()
    );
    let ev = events(root, &run_id);
    assert_eq!(interrupted_attempts(&ev, "work"), 1);
}

/// The pause session of a FIRST attempt: an attempt killed by a `--now`
/// pause journals no finish of its own, so its session comes from the
/// attempt itself (here the id apb assigned at launch), not from an earlier
/// attempt; the resumed attempt continues it.
#[cfg(unix)]
#[test]
fn a_now_pause_of_a_first_attempt_keeps_its_session() {
    use crate::token_economy_test::{invocations, recording_stub};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    crate::token_economy_test::seed_playbook(
        root,
        "sess",
        "schema: 2\nid: sess\nname: Sess\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
    );
    common::seed_profile(root, "main", "claude", "haiku", &[]);
    let d = root.display();
    let stub = recording_stub(
        root,
        &format!(
            "if [ \"$N\" = 1 ]; then touch '{d}/agent_running'; while [ -f '{d}/gate' ]; do sleep 0.05; done; fi\n\
             printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'"
        ),
    );
    let _env = Env::set(&[("APB_AGENT_CMD", &stub), ("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("gate"), "").unwrap();

    let rx = spawn_run(root, "sess");
    let run_id = find_run_id(root, "sess-");
    poll_until("the first attempt to start", || {
        root.join("agent_running").is_file().then_some(())
    });
    request_pause(root, &run_id, &operator(PauseMode::Now)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    fs::remove_file(root.join("gate")).unwrap();
    let res = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let inv = invocations(root);
    let work: Vec<_> = inv.iter().filter(|i| i.node == "work").collect();
    assert_eq!(work.len(), 2, "the paused attempt and the resumed one");
    let sid = work[0]
        .flag("--session-id")
        .expect("the first attempt opens a session");
    assert_eq!(
        work[1].flag("--resume"),
        Some(sid),
        "the resumed attempt continues the paused attempt's session"
    );
}

/// An isolated node's agent session is bound to the directory it ran in:
/// the resumed attempt runs in the interrupted attempt's own directory, as
/// that attempt left it, not in a fresh (emptied) one.
#[cfg(unix)]
#[test]
fn a_resumed_isolated_node_continues_its_session_in_the_same_directory() {
    use crate::token_economy_test::{invocations, recording_stub};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    crate::token_economy_test::seed_playbook(
        root,
        "sess",
        "schema: 2\nid: sess\nname: Sess\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"do\", isolation: best_effort }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
    );
    common::seed_profile(root, "main", "claude", "haiku", &[]);
    let d = root.display();
    let stub = recording_stub(
        root,
        &format!(
            "pwd > '{d}/cwd-'$N\n\
             if [ \"$N\" = 1 ]; then touch session_work; touch '{d}/agent_running'; while [ -f '{d}/gate' ]; do sleep 0.05; done; fi\n\
             if [ -f session_work ]; then touch '{d}/work_found'; fi\n\
             printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'"
        ),
    );
    let _env = Env::set(&[("APB_AGENT_CMD", &stub), ("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(root.join("gate"), "").unwrap();

    let rx = spawn_run(root, "sess");
    let run_id = find_run_id(root, "sess-");
    poll_until("the first attempt to start", || {
        root.join("agent_running").is_file().then_some(())
    });
    request_pause(root, &run_id, &operator(PauseMode::Now)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    fs::remove_file(root.join("gate")).unwrap();
    let res = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let work: Vec<_> = invocations(root)
        .into_iter()
        .filter(|i| i.node == "work")
        .collect();
    assert_eq!(work.len(), 2, "the paused attempt and the resumed one");
    assert!(
        work[1].resumed(),
        "the resumed attempt continues the session"
    );
    let cwd = |n: usize| fs::read_to_string(root.join(format!("cwd-{n}"))).unwrap();
    assert_eq!(cwd(2), cwd(1), "the session's own directory");
    assert!(
        root.join("work_found").is_file(),
        "the directory still holds what the interrupted attempt left"
    );
}
