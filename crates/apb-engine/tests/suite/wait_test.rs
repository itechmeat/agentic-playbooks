use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::signals::{SignalCommand, post_signal};
use apb_engine::state::RunStatus;

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(10);

// The timer fires right away (seconds: 0) and goes to a success finish.
const WF_TIMER: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: timer, seconds: 0 }, timeout_seconds: 60 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: wait }
  - { from: wait, to: done }
"#;

// Waits for a webhook signal; success on success, failure on timeout (fallback).
const WF_WEBHOOK: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 60 }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: wait }
  - { from: wait, to: ok, condition: { type: node_status, node: wait, equals: success } }
  - { from: wait, to: no, fallback: true }
"#;

// The timeout fires immediately (timeout_seconds: 0), no signal -> failure.
const WF_TIMEOUT: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 0 }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: wait }
  - { from: wait, to: ok, condition: { type: node_status, node: wait, equals: success } }
  - { from: wait, to: no, fallback: true }
"#;

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/w/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/w/current"), "1.0.0").unwrap();
}

fn latest_run_dir(root: &Path) -> std::path::PathBuf {
    poll_until("run dir", || {
        let entry = fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())?;
        Some(entry.path())
    })
}

#[test]
fn timer_wait_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_TIMER);
    let result = run(dir.path(), "w", None, RunOptions::default()).unwrap();
    assert_eq!(result.outcome, RunStatus::Succeeded);
}

#[test]
fn webhook_signal_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_WEBHOOK);
    let (tx, rx) = mpsc::channel::<RunResult>();
    let root = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "w", None, RunOptions::default()).unwrap());
    });
    let run_dir = latest_run_dir(dir.path());
    poll_until("wait_started", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitStarted { .. }))
            .then_some(())
    });
    post_signal(&run_dir, SignalCommand { key: "ci".into() }).unwrap();
    let result = rx.recv_timeout(POLL_DEADLINE).expect("run finished");
    assert_eq!(result.outcome, RunStatus::Succeeded);
    assert!(
        read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitSignalled { .. }))
    );
}

#[test]
fn webhook_timeout_fails() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_TIMEOUT);
    let result = run(dir.path(), "w", None, RunOptions::default()).unwrap();
    assert_eq!(result.outcome, RunStatus::Failed);
    let run_dir = latest_run_dir(dir.path());
    assert!(
        read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitTimeout { .. }))
    );
}

// ------------------------------------------------------------------ wait park

use crate::common::{self, EnvVar};
use apb_engine::control::{self, Control, post_control};
use apb_engine::driver::read_driver_pid;
use apb_engine::event::EventLog;
use apb_engine::pause::kind::{PauseBy, PauseMode};
use apb_engine::pause::{PauseOutcome, PauseRequest, WakeOutcome, request_pause, run_waker};
use apb_engine::scheduler::resume;
use apb_engine::workdir::{LockOwner, lock_owner};
use std::path::PathBuf;

const WAIT_PARK_DEADLINE: Duration = Duration::from_secs(20);

// A write run: a script node takes the workdir lock, then a long timer wait.
const WP_WRITE: &str = r#"
schema: 1
id: w
name: WP
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/go.sh", runner: sh }
  - { id: pause, type: wait, wait_for: { type: timer, seconds: 3 }, timeout_seconds: 3600 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: pause }
  - { from: pause, to: done }
"#;

// A pilot list that goes on by itself (`then: continue`), then a long
// timer wait: the run guard names `list_pilot` and `wait_park`, in that
// order.
const WP_PILOT: &str = r#"
schema: 2
id: w
name: WP
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: work
    type: script
    script: "scripts/go.sh"
    runner: sh
    list:
      items: { inline: [ { id: i1 }, { id: i2 }, { id: i3 } ] }
      batch: { size: 1, parallel: 1, retries: 0 }
      pilot: { items: 1, then: continue }
  - { id: pause, type: wait, wait_for: { type: timer, seconds: 3 }, timeout_seconds: 3600 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: pause }
  - { from: pause, to: done }
"#;

// The same with a timer under the release threshold (with
// APB_RELEASE_AFTER_MS=1500 the wait never parks).
const WP_SHORT: &str = r#"
schema: 1
id: w
name: WP
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/go.sh", runner: sh }
  - { id: pause, type: wait, wait_for: { type: timer, seconds: 1 }, timeout_seconds: 3600 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: pause }
  - { from: pause, to: done }
"#;

// A webhook wait with a long timeout: never a park.
const WP_WEBHOOK: &str = r#"
schema: 1
id: w
name: WP
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/go.sh", runner: sh }
  - { id: pause, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 60 }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: pause }
  - { from: pause, to: ok, condition: { type: node_status, node: pause, equals: success } }
  - { from: pause, to: no, fallback: true }
"#;

// A read-only run: no node takes the workdir lock.
const WP_RO: &str = r#"
schema: 1
id: w
name: WP
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: pause, type: wait, wait_for: { type: timer, seconds: 3 }, timeout_seconds: 3600 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: pause }
  - { from: pause, to: done }
"#;

// A script node that holds while the file `hold` exists, so a test can
// write to the journal before the run reaches the wait.
const HOLD_SH: &str = "while [ -f hold ]; do sleep 0.05; done\necho go\n";

const GO_SH: &str = "echo go\n";

/// The project probation ledger's entries of run `run_id` of playbook `w`;
/// none when the ledger does not exist yet.
fn probation_entries(root: &Path, run_id: &str) -> Vec<apb_core::probation::ProbationEntry> {
    apb_core::probation::read(root, apb_core::scope::Scope::Project, "w")
        .map(|l| l.file.entries)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.run == run_id)
        .collect()
}

fn seed_scripts(root: &Path, yaml: &str, scripts: &[(&str, &str)]) {
    seed(root, yaml);
    let dir = root.join(".apb/playbooks/w/1.0.0/scripts");
    fs::create_dir_all(&dir).unwrap();
    for (name, body) in scripts {
        fs::write(dir.join(name), body).unwrap();
    }
}

fn spawn_wp(
    root: &Path,
    opts: RunOptions,
) -> mpsc::Receiver<Result<RunResult, apb_engine::error::EngineError>> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(apb_engine::scheduler::run(&root, "w", None, opts));
    });
    rx
}

fn wait_result(
    rx: &mpsc::Receiver<Result<RunResult, apb_engine::error::EngineError>>,
) -> RunResult {
    rx.recv_timeout(WAIT_PARK_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return in time"))
        .expect("the drive must not fail")
}

fn latest_run(root: &Path) -> PathBuf {
    poll_until("run dir", || {
        let mut dirs: Vec<_> = fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("events.jsonl").is_file())
            .collect();
        dirs.sort_by_key(|p| fs::metadata(p).and_then(|m| m.created()).ok());
        dirs.pop()
    })
}

/// One journaled pause: its seq, reason, `by`, `until` and `keep_lock`.
type ParkRow = (u64, String, Option<PauseBy>, Option<u64>, bool);

fn parks(events: &[Event]) -> Vec<ParkRow> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunPaused {
                reason,
                by,
                until,
                keep_lock,
                ..
            } => Some((e.seq, reason.clone(), *by, *until, *keep_lock)),
            _ => None,
        })
        .collect()
}

fn wait_parks(events: &[Event]) -> Vec<ParkRow> {
    parks(events)
        .into_iter()
        .filter(|(_, reason, _, _, _)| reason.starts_with("wait ("))
        .collect()
}

fn guard_before_and_after(events: &[Event], at: u64) -> bool {
    let idx = events.iter().position(|e| e.seq == at).unwrap();
    let names = |e: &Event| {
        matches!(&e.payload, EventPayload::RunGuard { requires, .. }
            if requires.iter().any(|f| f == "wait_park"))
    };
    // Right after the park, in the same write (the park is a checkpoint).
    if !events.get(idx + 1).is_some_and(names) {
        return false;
    }
    // Before it: the latest guard of the journal so far (the one after the
    // last checkpoint) names the feature too.
    events[..idx]
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::RunGuard { .. }))
        .is_some_and(names)
}

fn run_waker_outcome(
    root: &Path,
    run_id: &str,
    seq: u64,
) -> mpsc::Receiver<Result<WakeOutcome, apb_engine::error::EngineError>> {
    let (tx, rx) = mpsc::channel();
    let (r, id) = (root.to_path_buf(), run_id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(run_waker(&r, &id, seq));
    });
    rx
}

/// Waits until the wall clock reaches `until`, a park's wake time: a wake
/// time more than a minute ahead fails at once instead of waiting it out
/// (a short timer that parked with a long `until` is a regression).
fn wait_until_ms(until: u64) {
    let left = u128::from(until).saturating_sub(apb_core::clock::now_ms());
    assert!(left <= 60_000, "the park's wake time is {left} ms ahead");
    while apb_core::clock::now_ms() < u128::from(until) {
        std::thread::sleep(POLL_STEP);
    }
}

/// Waits until the pause waker of `run_id` completed a whole poll that
/// began after this call (two more polls than it had), to a bound.
fn wait_for_a_waker_poll(root: &Path, run_id: &str) {
    let run_dir = root.join(".apb/runs").join(run_id);
    let target = apb_engine::pause::waker::polls::count(&run_dir) + 2;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while apb_engine::pause::waker::polls::count(&run_dir) < target {
        assert!(
            std::time::Instant::now() < deadline,
            "the waker did not poll"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn operator_pause_request() -> PauseRequest {
    PauseRequest {
        reason: Some("looking".into()),
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    }
}

/// The run id of the run directory (its last path component).
fn run_id_of(run_dir: &Path) -> String {
    run_dir.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn a_long_timer_wait_parks_and_wakes_when_due() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The run's origin stamp: the waker's engine resume verifies it.
    apb_core::run_origin::ensure_key().unwrap();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");

    let rx = spawn_wp(root, RunOptions::default());
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);

    let events = read_all(&run_dir).unwrap();
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1, "one park: {}", events.len());
    let (seq, reason, by, until, keep_lock) = wp[0].clone();
    let until = until.unwrap();
    // No pause before the park: the wake time is the wait's start plus
    // the 3 s timer, read from the journal.
    let wait_ts = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == "pause"))
        .unwrap()
        .ts;
    assert_eq!(
        u128::from(until),
        wait_ts + 3_000,
        "the park carries the wait's due time"
    );
    assert_eq!(reason, "wait (pause)");
    assert_eq!(by, Some(PauseBy::Engine));
    assert!(keep_lock, "a write run keeps the lock");
    assert!(
        guard_before_and_after(&events, seq),
        "a run_guard naming wait_park stands before and after the park"
    );
    assert!(read_driver_pid(&run_dir).is_none(), "no driver.pid");
    let lock = root.join(".apb/workdir.lock");
    assert_eq!(
        lock_owner(&lock),
        Some(LockOwner::PausedRun(run_id.clone())),
        "the tree's lock names the parked run"
    );
    // The control cursor stands at the park's entry.
    let entries = control::read_control_after(&run_dir, None).unwrap();
    assert_eq!(entries.len(), 1, "only the park is in the control file");
    assert!(matches!(&entries[0].cmd, Control::Pause(req)
        if req.reason.as_deref() == Some("wait (pause)")));
    assert_eq!(
        control::read_control_cursor(&run_dir).unwrap(),
        Some(entries[0].seq),
        "the cursor stands at the park's entry"
    );
    // A parked run has not ended: the probation ledger (written by the
    // journal's terminal observer on `run_finished` or `run_aborted` only)
    // holds no entry for it.
    assert!(
        probation_entries(root, &run_id).is_empty(),
        "a parked run writes no probation entry"
    );

    // The engine resumes the run at `until`, and it ends succeeded. (The
    // waker's process hand-over is the CLI test; a test binary cannot
    // re-exec `__drive-run`, so the wake here is the engine's own
    // in-process resume of the pause at `seq`, `resume_pause`, which
    // journals the same `run_resumed` by `engine`.)
    wait_until_ms(until);
    let resumed = apb_engine::scheduler::resume_pause(root, &run_id, seq, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        events
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                EventPayload::RunFinished { outcome } => Some(outcome.clone()),
                _ => None,
            })
            .as_deref(),
        Some("succeeded")
    );
    let starts = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "pause"))
        .count();
    assert_eq!(starts, 1, "the wait is not started again");
    // The engine's resume of the park journals the same definition, so the
    // attended run that parked and woke counts toward probation once.
    let entries = probation_entries(root, &run_id);
    assert_eq!(entries.len(), 1, "one probation entry: {entries:?}");
    assert_eq!(entries[0].outcome, "succeeded", "{entries:?}");
    assert!(
        entries[0].eligible,
        "a park does not make the run ineligible: {entries:?}"
    );
    let waits = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == "pause"))
        .count();
    assert_eq!(waits, 1, "one wait_started");
    let started_ts = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == "pause"))
        .unwrap()
        .ts;
    let signalled = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::WaitSignalled { .. }))
        .unwrap();
    assert!(
        signalled.ts >= started_ts + 3_000,
        "wait_signalled no earlier than the timer's active length"
    );
}

/// A pilot list before the wait changes nothing about the park: the run's
/// guard names `list_pilot` then `wait_park` (one fixed order), the clean
/// pilot slice goes on without a gate, and the timer wait parks and wakes
/// through the same rules as without the list.
#[test]
fn a_pilot_list_before_the_wait_parks_like_any_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::run_origin::ensure_key().unwrap();
    seed_scripts(root, WP_PILOT, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");

    let rx = spawn_wp(root, RunOptions::default());
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);
    let events = read_all(&run_dir).unwrap();
    let guards: Vec<Vec<String>> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunGuard { requires } => Some(requires.clone()),
            _ => None,
        })
        .collect();
    assert!(!guards.is_empty(), "the run carries a guard");
    for g in &guards {
        assert_eq!(
            g,
            &vec!["list_pilot".to_string(), "wait_park".to_string()],
            "every guard names both, the pilot first"
        );
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ReviewRequested { .. })),
        "a clean slice under `then: continue` asks nobody"
    );
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1, "one park");
    let (seq, reason, by, until, _) = wp[0].clone();
    assert_eq!(reason, "wait (pause)");
    assert_eq!(by, Some(PauseBy::Engine));
    assert!(guard_before_and_after(&events, seq));

    wait_until_ms(until.unwrap());
    let resumed = apb_engine::scheduler::resume_pause(root, &run_id, seq, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
}

#[test]
fn a_short_wait_keeps_its_driver() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_SHORT, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "1500");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(root, RunOptions::default());
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let run_dir = latest_run(root);
    assert!(wait_parks(&read_all(&run_dir).unwrap()).is_empty());
}

#[test]
fn a_webhook_wait_keeps_its_driver() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WEBHOOK, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(root, RunOptions::default());
    let run_dir = poll_until("wait_started", || {
        let rd = latest_run(root);
        let events = read_all(&rd).ok()?;
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitStarted { .. }))
            .then_some(rd)
    });
    // A webhook wait never parks, whatever its length.
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        wait_parks(&read_all(&run_dir).unwrap()).is_empty(),
        "a webhook wait keeps its driver"
    );
    post_signal(&run_dir, SignalCommand { key: "ci".into() }).unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    assert!(wait_parks(&read_all(&run_dir).unwrap()).is_empty());
}

#[test]
fn an_unattended_run_keeps_its_driver() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(
        root,
        RunOptions {
            unattended: true,
            ..RunOptions::default()
        },
    );
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let run_dir = latest_run(root);
    assert!(wait_parks(&read_all(&run_dir).unwrap()).is_empty());
}

#[test]
fn a_run_without_the_guard_keeps_its_driver() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", HOLD_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    fs::write(root.join("hold"), "").unwrap();
    let rx = spawn_wp(root, RunOptions::default());
    let run_dir: PathBuf = poll_until("work started", || {
        let rd = latest_run(root);
        let events = read_all(&rd).ok()?;
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "work"))
            .then_some(rd)
    });
    // The run's guard ends: the latest run_guard names no feature.
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(EventPayload::RunGuard {
        requires: Vec::new(),
    })
    .unwrap();
    drop(log);
    fs::remove_file(root.join("hold")).unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    assert!(wait_parks(&read_all(&run_dir).unwrap()).is_empty());
}

#[test]
fn an_early_resume_parks_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(root, RunOptions::default());
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);
    let first = wait_parks(&read_all(&run_dir).unwrap());
    let until1 = first[0].3.unwrap();

    // A person's resume well before `until`: the drive re-enters the wait
    // and parks again for the time left.
    let resumed = resume(root, &run_id, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunResumed {
                by: Some(PauseBy::Operator),
                ..
            }
        )),
        "the journal holds the person's run_resumed"
    );
    let second = wait_parks(&events);
    assert_eq!(second.len(), 2, "a second park");
    let until2 = second[1].3.unwrap();
    assert!(
        until2.abs_diff(until1) <= 100,
        "the second park's until is within one poll of the first: {until1} vs {until2}"
    );
}

#[test]
fn a_pause_during_the_park_freezes_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The run's origin stamp: the waker's engine resume verifies it.
    apb_core::run_origin::ensure_key().unwrap();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(root, RunOptions::default());
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);
    let first = wait_parks(&read_all(&run_dir).unwrap());
    let seq = first[0].0;
    let until1 = first[0].3.unwrap();

    // A person's pause on the parked run: journaled directly, the park's
    // waker exits superseded.
    let req = PauseRequest {
        keep_lock: true,
        ..operator_pause_request()
    };
    let PauseOutcome::Journaled { .. } = request_pause(root, &run_id, &req).unwrap() else {
        panic!("the pause is journaled");
    };
    let woke = run_waker_outcome(root, &run_id, seq)
        .recv_timeout(WAIT_PARK_DEADLINE)
        .unwrap()
        .unwrap();
    assert_eq!(woke, WakeOutcome::Superseded, "the park's waker exits");

    // A resume later parks again with the remaining active time: the
    // person's pause did not run the timer.
    std::thread::sleep(Duration::from_millis(600));
    let resumed = resume(root, &run_id, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    let second = wait_parks(&events);
    assert_eq!(second.len(), 2, "a second park after the person's pause");
    let until2 = second[1].3.unwrap();
    // The wake time moved by exactly the person's paused interval, read
    // from the journal: the person's run_paused to the resume's
    // run_resumed.
    let paused_ts = events
        .iter()
        .find(|e| {
            matches!(
                &e.payload,
                EventPayload::RunPaused {
                    by: Some(PauseBy::Operator),
                    ..
                }
            )
        })
        .unwrap()
        .ts;
    let resumed_ts = events
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::RunResumed { .. }))
        .unwrap()
        .ts;
    assert_eq!(
        u128::from(until2 - until1),
        resumed_ts - paused_ts,
        "the timer did not run while the person paused: {until1} vs {until2}"
    );
}

#[test]
fn a_read_only_wait_parks_without_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The run's origin stamp: the waker's engine resume verifies it.
    apb_core::run_origin::ensure_key().unwrap();
    seed_scripts(root, WP_RO, &[]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(root, RunOptions::default());
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);
    let first = wait_parks(&read_all(&run_dir).unwrap());
    assert_eq!(first.len(), 1);
    assert!(!first[0].4, "keep_lock false without a busy lock");
    assert!(
        !root.join(".apb/workdir.lock").exists(),
        "no lock file exists"
    );
    // The engine resumes it at `until` (in-process; see the note in
    // `a_long_timer_wait_parks_and_wakes_when_due`) and it ends succeeded.
    let (seq, _, _, until, _) = first[0];
    wait_until_ms(until.unwrap());
    let resumed = apb_engine::scheduler::resume_pause(root, &run_id, seq, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        events
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                EventPayload::RunFinished { outcome } => Some(outcome.clone()),
                _ => None,
            })
            .as_deref(),
        Some("succeeded")
    );
}

#[test]
fn a_shared_workdir_wait_parks_without_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The run's origin stamp: the waker's engine resume verifies it.
    apb_core::run_origin::ensure_key().unwrap();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(
        root,
        RunOptions {
            allow_shared_workdir: true,
            ..RunOptions::default()
        },
    );
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);
    let first = wait_parks(&read_all(&run_dir).unwrap());
    assert_eq!(first.len(), 1);
    assert!(!first[0].4, "keep_lock false on a shared workdir");
    assert!(!root.join(".apb/workdir.lock").exists(), "no lock file");
    // The engine resumes it at `until` (in-process; see the note in
    // `a_long_timer_wait_parks_and_wakes_when_due`) and it ends succeeded.
    let (seq, _, _, until, _) = first[0];
    wait_until_ms(until.unwrap());
    let resumed = apb_engine::scheduler::resume_pause(root, &run_id, seq, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        events
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                EventPayload::RunFinished { outcome } => Some(outcome.clone()),
                _ => None,
            })
            .as_deref(),
        Some("succeeded")
    );
}

// ---------------------------------------------------------- the park yields

/// The pid of a process that already exited: a dead driver.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    child.id()
}

/// A run directory written directly: the write playbook's snapshot, a
/// journal up to the open timer wait on `pause`, a valid origin stamp and a
/// dead `driver.pid`. Control entries go in afterwards, with `post_control`
/// calls made one after the other.
fn seed_waiting(root: &Path, run_id: &str) -> PathBuf {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    let yaml = fs::read_to_string(root.join(".apb/playbooks/w/1.0.0/playbook.yaml")).unwrap();
    fs::write(run_dir.join("playbook.yaml"), &yaml).unwrap();
    // Every run apb prepares has its `run.yaml`: a patch reads the run's
    // mode from it and refuses a run whose mode cannot be told.
    fs::write(run_dir.join("run.yaml"), "unattended: false\n").unwrap();
    let mut log = EventLog::create(&run_dir).unwrap();
    for payload in [
        EventPayload::RunStarted {
            playbook: "w".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        },
        EventPayload::RunProvenance {
            origin: Some("project".into()),
            digest: None,
            execution_root: None,
            scripts_digest: None,
            profiles: Vec::new(),
            remembered: Vec::new(),
            apb: Some(env!("CARGO_PKG_VERSION").into()),
            limits: Vec::new(),
            previous_success: None,
            untrusted: Vec::new(),
            deadline: None,
            start_key: None,
            concurrency: None,
            eval_doubles: false,
        },
        EventPayload::RunGuard {
            requires: vec!["wait_park".into()],
        },
        EventPayload::NodeStarted {
            node: "start".into(),
            attempt: 1,
        },
        EventPayload::NodeFinished {
            node: "start".into(),
            status: "succeeded".into(),
            attempt: 1,
            output: String::new(),
            artifacts: Vec::new(),
            result: None,
        },
        EventPayload::RunGuard {
            requires: vec!["wait_park".into()],
        },
        EventPayload::EdgeTraversed {
            from: "start".into(),
            to: "work".into(),
            uncounted: true,
            edges: vec!["e:start>work#1".into()],
            via_policy: false,
        },
        EventPayload::NodeStarted {
            node: "work".into(),
            attempt: 1,
        },
        EventPayload::NodeFinished {
            node: "work".into(),
            status: "succeeded".into(),
            attempt: 1,
            output: "go".into(),
            artifacts: Vec::new(),
            result: None,
        },
        EventPayload::RunGuard {
            requires: vec!["wait_park".into()],
        },
        EventPayload::EdgeTraversed {
            from: "work".into(),
            to: "pause".into(),
            uncounted: true,
            edges: vec!["e:work>pause#1".into()],
            via_policy: false,
        },
        EventPayload::NodeStarted {
            node: "pause".into(),
            attempt: 1,
        },
        EventPayload::WaitStarted {
            node: "pause".into(),
            kind: "timer".into(),
        },
    ] {
        log.append(payload).unwrap();
    }
    drop(log);
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::run_origin::stamp(&run_dir, run_id).unwrap();
    fs::write(run_dir.join("driver.pid"), dead_pid().to_string()).unwrap();
    run_dir
}

/// The engine park's control request for `until`.
fn park_entry(until: u64, node: &str) -> Control {
    Control::Pause(PauseRequest {
        reason: Some(apb_engine::pause::cause::wait_reason(node)),
        by: Some(PauseBy::Engine),
        mode: PauseMode::Drain,
        until: Some(until),
        keep_lock: true,
    })
}

fn operator_pause_entry(keep_lock: bool) -> Control {
    Control::Pause(PauseRequest {
        reason: Some("looking".into()),
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        until: None,
        keep_lock,
    })
}

/// Whether `events` hold a `run_paused` with the wait reason and `until`.
fn has_wait_pause(events: &[Event], until: Option<u64>) -> bool {
    wait_parks(events).iter().any(|(_, _, _, u, _)| *u == until)
}

/// The due time of the seeded wait on `pause` as the scan computes it again
/// over the journal at this moment (one `now` for the elapsed time and the
/// wake time): a park posted with it passes the due-time rule, so a discard
/// test fails exactly when its own rule is removed.
fn due_now(run_dir: &Path, timer_ms: u128) -> u64 {
    let events = read_all(run_dir).unwrap();
    let now = apb_core::clock::now_ms();
    let ws_ts = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == "pause"))
        .unwrap()
        .ts;
    let active = apb_engine::pause::active_elapsed_ms(&events, ws_ts, now, now);
    u64::try_from(now + timer_ms - active).unwrap()
}

#[test]
fn a_pause_after_the_park_is_applied_instead() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let run_dir = seed_waiting(root, "w-p1");
    // The two posts made one after the other: the park, then the person's
    // pause. They get N and N + 1 in that order.
    let park_until = due_now(&run_dir, 3_000);
    let n = post_control(&run_dir, park_entry(park_until, "pause")).unwrap();
    let n1 = post_control(&run_dir, operator_pause_entry(true)).unwrap();
    assert_eq!(n1, n + 1, "two numbers, in append order");

    // The first scan discards the park and applies the person's pause.
    let resumed = resume(root, "w-p1", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        !has_wait_pause(&events, Some(park_until)),
        "no park applied: {}",
        events.len()
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunPaused {
                by: Some(PauseBy::Operator),
                keep_lock: true,
                ..
            }
        )),
        "the person's run_paused is journaled"
    );
    assert_eq!(
        control::read_control_cursor(&run_dir).unwrap(),
        Some(n1),
        "the cursor stands at the operator's entry"
    );

    // The timer does not run while a person paused: the wait's active time
    // has not grown since the operator's run_paused.
    let ws_ts = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == "pause"))
        .unwrap()
        .ts;
    let now = apb_core::clock::now_ms();
    let active = apb_engine::pause::active_elapsed_ms(&events, ws_ts, now, now);
    std::thread::sleep(Duration::from_millis(2_000));
    let events = read_all(&run_dir).unwrap();
    let now = apb_core::clock::now_ms();
    let active2 = apb_engine::pause::active_elapsed_ms(&events, ws_ts, now, now);
    assert_eq!(
        active, active2,
        "the timer is frozen under the person's pause"
    );

    // A resume parks again for the remaining time, never the old `until`.
    let resumed = resume(root, "w-p1", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1, "exactly one park, after the resumes");
    let until2 = wp[0].3.unwrap();
    assert!(
        until2 > park_until,
        "the new park's wake time leaves out the paused time: {park_until} vs {until2}"
    );
}

#[test]
fn a_stop_after_the_park_is_applied_instead() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let run_dir = seed_waiting(root, "w-p2");
    let park_until = due_now(&run_dir, 3_000);
    let n = post_control(&run_dir, park_entry(park_until, "pause")).unwrap();
    let n1 = post_control(
        &run_dir,
        Control::Abort {
            reason: "stop requested".into(),
            by: Some(PauseBy::Operator),
            graceful: false,
        },
    )
    .unwrap();
    assert_eq!(n1, n + 1);

    let resumed = resume(root, "w-p2", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Aborted);
    let events = read_all(&run_dir).unwrap();
    assert!(
        !has_wait_pause(&events, Some(park_until)),
        "no park applied"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunAborted {
                by: Some(PauseBy::Operator),
                ..
            }
        )),
        "run_aborted by the operator"
    );
}

#[test]
fn a_patch_before_the_park_discards_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let run_dir = seed_waiting(root, "w-p3");
    // A patch to a published version whose wait has a 3600 s timer, then
    // the park of the old 3 s timer.
    let new_yaml = WP_WRITE.replace("seconds: 3", "seconds: 3600");
    let version = apb_core::versioning::create_patch_version(
        root,
        "w",
        "1.0.0",
        &new_yaml,
        "w-p3",
        "workaround",
    )
    .unwrap();
    let n = post_control(
        &run_dir,
        Control::Patch {
            version,
            classification: "workaround".into(),
            continue_from: "pause".into(),
        },
    )
    .unwrap();
    let park_until = due_now(&run_dir, 3_000);
    let n1 = post_control(&run_dir, park_entry(park_until, "pause")).unwrap();
    assert_eq!(n1, n + 1);

    // The patch applies, the old park is discarded, and the wait decides
    // again on the patched definition: it parks for the 3600 s timer.
    let resumed = resume(root, "w-p3", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "patch_applied is journaled"
    );
    assert!(
        !has_wait_pause(&events, Some(park_until)),
        "the old park is never applied"
    );
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1, "exactly one park, on the patched definition");
    let until2 = wp[0].3.unwrap();
    let now = apb_core::clock::now_ms();
    assert!(
        until2 > u64::try_from(now).unwrap() + 3_000_000,
        "the park follows the new 3600 s timer: {until2} at {now}"
    );
    assert!(
        control::read_control_cursor(&run_dir).unwrap() > Some(n1),
        "the old park's entry is behind the cursor"
    );
}

#[test]
fn a_control_posted_in_the_exit_window_wakes_the_run_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The run's origin stamp: the waker's engine resume verifies it.
    apb_core::run_origin::ensure_key().unwrap();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    // A 30 s timer: the park's `until` is far ahead.
    let yaml = WP_WRITE.replace("seconds: 3", "seconds: 30");
    seed_scripts(root, &yaml, &[("go.sh", GO_SH)]);
    let rx = spawn_wp(root, RunOptions::default());
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);
    let events = read_all(&run_dir).unwrap();
    let (seq, _, _, until, _) = wait_parks(&events)[0].clone();
    let until = until.unwrap();

    // A stop posted after the park was applied: the entry a stop leaves when
    // it found the driver still claiming the run.
    let n = post_control(
        &run_dir,
        Control::Abort {
            reason: "stop requested".into(),
            by: Some(PauseBy::Operator),
            graceful: false,
        },
    )
    .unwrap();

    // The park's waker resumes the run at its first poll, well before
    // `until`. (The resumed drive itself runs in this process: a test
    // binary cannot re-exec `__drive-run`, so the waker's spawn is a child
    // that exits at once and the engine's own `resume_pause` drives the
    // run; the hand-over with real processes is the CLI test.)
    let started = Instant::now();
    let woke = run_waker_outcome(root, &run_id, seq)
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert!(
        matches!(woke, WakeOutcome::Resumed { .. }),
        "the waker resumed the run: {woke:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "well before `until` ({} ms)",
        started.elapsed().as_millis()
    );
    assert!(apb_core::clock::now_ms() < u128::from(until));
    let resumed = apb_engine::scheduler::resume_pause(root, &run_id, seq, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Aborted);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunResumed {
                by: Some(PauseBy::Engine),
                ..
            }
        )),
        "one run_resumed by engine precedes the abort"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunAborted {
                by: Some(PauseBy::Operator),
                ..
            }
        )),
        "the resumed drive's scan journaled the operator's abort"
    );
    assert_eq!(
        control::read_control_cursor(&run_dir).unwrap(),
        Some(n),
        "the cursor stands at the abort's entry"
    );
}

#[test]
fn a_half_written_control_line_does_not_end_the_waker() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The run's origin stamp: the waker's engine resume verifies it.
    apb_core::run_origin::ensure_key().unwrap();
    let yaml = WP_WRITE.replace("seconds: 3", "seconds: 30");
    seed_scripts(root, &yaml, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(root, RunOptions::default());
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);
    let (seq, _, _, until, _) = wait_parks(&read_all(&run_dir).unwrap())[0].clone();
    let until = until.unwrap();

    let woke = run_waker_outcome(root, &run_id, seq);
    // A writer in the middle of its append: the first half of a note's
    // line, no newline. The waker's read fails; it must keep running.
    let half = r#"{"seq":1,"cmd":"context_append","note":"half of a no"#.to_string();
    use std::io::Write as _;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("control.jsonl"))
        .unwrap();
    f.write_all(half.as_bytes()).unwrap();
    f.flush().unwrap();
    // A whole poll that read the torn line later the waker has not
    // returned (the channel still holds no result: it is in its loop).
    wait_for_a_waker_poll(root, &run_id);
    assert!(
        woke.try_recv().is_err(),
        "the torn line must not end the waker"
    );
    // The rest of the line, and its newline.
    f.write_all(b"te\"}\n").unwrap();
    f.flush().unwrap();
    let woke = woke
        .recv_timeout(Duration::from_secs(30))
        .expect("the waker returns once the line is whole");
    assert!(matches!(woke, Ok(WakeOutcome::Resumed { .. })), "{woke:?}");
    assert!(
        apb_core::clock::now_ms() < u128::from(until),
        "before `until`"
    );
    // The resumed drive applies the note.
    let resumed = apb_engine::scheduler::resume_pause(root, &run_id, seq, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::SupervisorAction { action, .. }
                if action == apb_engine::event::supervisor_action::CONTEXT_APPEND)),
        "the note is applied"
    );
}

#[test]
fn a_note_posted_after_the_waker_settled_reaches_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The run's origin stamp: the waker's engine resume verifies it.
    apb_core::run_origin::ensure_key().unwrap();
    let yaml = WP_WRITE.replace("seconds: 3", "seconds: 30");
    seed_scripts(root, &yaml, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(root, RunOptions::default());
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = latest_run(root);
    let run_id = run_id_of(&run_dir);
    let (seq, _, _, until, _) = wait_parks(&read_all(&run_dir).unwrap())[0].clone();
    let until = until.unwrap();

    let woke = run_waker_outcome(root, &run_id, seq);
    // A whole poll of the waker's loop, then a note posted while the run
    // is parked: only the per-poll check can see it before `until`.
    wait_for_a_waker_poll(root, &run_id);
    post_control(
        &run_dir,
        Control::ContextAppend {
            note: "look at the logs".into(),
        },
    )
    .unwrap();
    let woke = woke
        .recv_timeout(Duration::from_secs(30))
        .expect("the waker returns for the pending entry");
    assert!(matches!(woke, Ok(WakeOutcome::Resumed { .. })), "{woke:?}");
    assert!(
        apb_core::clock::now_ms() < u128::from(until),
        "before `until`"
    );
    // The resumed drive applies the note, and the wait parks again for the
    // time left.
    let resumed = apb_engine::scheduler::resume_pause(root, &run_id, seq, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::SupervisorAction { action, .. }
                if action == apb_engine::event::supervisor_action::CONTEXT_APPEND)),
        "the note is applied"
    );
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 2, "a second park for the time left");
    let until2 = wp[1].3.unwrap();
    assert!(
        until2.abs_diff(until) <= 100,
        "within one poll of the first park's until: {until} vs {until2}"
    );
    // The entries are the first park (0), the note (1) and the second park
    // (2); the cursor stands at the second park's entry.
    assert_eq!(
        control::read_control_cursor(&run_dir).unwrap(),
        Some(2),
        "the cursor stands at the second park's entry"
    );
}

#[test]
fn a_resume_after_the_patch_limit_pause_applies_no_stale_park() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let run_dir = seed_waiting(root, "w-p5");
    // The run's patch limit is exhausted.
    fs::write(run_dir.join("run.yaml"), "max_patches_per_run: 0\n").unwrap();
    // A patch, then the park of the 3 s timer with `until` 1 s ahead.
    let new_yaml = WP_WRITE.replace("seconds: 3", "seconds: 3600");
    let version = apb_core::versioning::create_patch_version(
        root,
        "w",
        "1.0.0",
        &new_yaml,
        "w-p5",
        "workaround",
    )
    .unwrap();
    post_control(
        &run_dir,
        Control::Patch {
            version,
            classification: "workaround".into(),
            continue_from: "pause".into(),
        },
    )
    .unwrap();
    let park_until = due_now(&run_dir, 3_000);
    let n1 = post_control(&run_dir, park_entry(park_until, "pause")).unwrap();

    // The first scan journals the patch-limit pause and discards the park
    // in the same cursor write.
    let resumed = resume(root, "w-p5", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::RunPaused { reason, by: Some(PauseBy::Engine), .. }
                if reason.starts_with("max patches per run exhausted"))),
        "the engine's patch-limit pause is journaled"
    );
    assert!(
        !has_wait_pause(&events, Some(park_until)),
        "no park with the old until"
    );
    assert_eq!(
        control::read_control_cursor(&run_dir).unwrap(),
        Some(n1),
        "the cursor stands at the park's entry"
    );

    // Past the old park's `until`, a person's resume: the wait decides
    // again with its remaining active time (the patch-limit pause is paused
    // time) and parks for it, never with the old `until`.
    wait_until_ms(park_until + 1);
    let resumed = resume(root, "w-p5", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1, "exactly one park, after the patch-limit pause");
    let until2 = wp[0].3.unwrap();
    // The due time from the journal: the wait's start, its timer and every
    // paused interval recorded since (the patch-limit pause and the park
    // itself, whose ongoing interval only adds the moments since it began).
    let due = due_now(&run_dir, 3_000);
    assert!(
        until2 <= due && due - until2 <= 1_000,
        "the wait parks again with its remaining active time: {until2}, due {due}"
    );
    assert!(
        until2 > park_until,
        "never the old until: {until2} vs {park_until}"
    );
}

#[test]
fn a_pause_queued_before_the_park_leaves_no_stale_park() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let run_dir = seed_waiting(root, "w-p6");
    // The two posts made one after the other: the person's pause first,
    // the park second. They get N and N + 1 in that order.
    let n = post_control(&run_dir, operator_pause_entry(true)).unwrap();
    let park_until = due_now(&run_dir, 3_000);
    let n1 = post_control(&run_dir, park_entry(park_until, "pause")).unwrap();
    assert_eq!(n1, n + 1);

    // The person's pause is applied, the park is discarded in the same
    // cursor write.
    let resumed = resume(root, "w-p6", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunPaused {
                by: Some(PauseBy::Operator),
                ..
            }
        )),
        "the person's run_paused is journaled"
    );
    assert!(
        !has_wait_pause(&events, Some(park_until)),
        "no park applied"
    );
    assert_eq!(
        control::read_control_cursor(&run_dir).unwrap(),
        Some(n1),
        "the cursor stands past the park's entry"
    );

    // A person's resume 2 s later parks again for the remaining time,
    // never with the old `until`.
    std::thread::sleep(Duration::from_millis(2_000));
    let resumed = resume(root, "w-p6", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1);
    let until2 = wp[0].3.unwrap();
    assert_ne!(until2, park_until, "never the old until");
    let now = u64::try_from(apb_core::clock::now_ms()).unwrap();
    assert!(
        until2 > now + 1_000,
        "for the remaining time: {until2} at {now}"
    );
}

#[test]
fn a_park_for_another_node_is_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let run_dir = seed_waiting(root, "w-p7");
    let park_until = due_now(&run_dir, 3_000);
    let n = post_control(&run_dir, park_entry(park_until, "gone")).unwrap();

    let resumed = resume(root, "w-p7", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        !events.iter().any(
            |e| matches!(&e.payload, EventPayload::RunPaused { reason, .. }
                if reason == "wait (gone)")
        ),
        "no park for the other node is journaled"
    );
    assert!(
        control::read_control_cursor(&run_dir).unwrap() > Some(n),
        "the cursor stands past that entry"
    );
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1, "the wait parks as itself");
    assert_eq!(wp[0].1, "wait (pause)");
}

#[test]
fn a_park_pending_behind_a_direct_pause_is_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(root, WP_WRITE, &[("go.sh", GO_SH)]);
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let run_dir = seed_waiting(root, "w-p8");
    // The park the driver posted and died before applying: its `until` is
    // the wait's due time of the journal at this moment (one `now` for the
    // elapsed time and the wake time, as the wait branch computes it).
    let events = read_all(&run_dir).unwrap();
    let ws_ts = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == "pause"))
        .unwrap()
        .ts;
    let due = due_now(&run_dir, 3_000);
    let n = post_control(&run_dir, park_entry(due, "pause")).unwrap();

    // A person's pause finds no live driver and is journaled directly.
    let req = PauseRequest {
        keep_lock: true,
        ..operator_pause_request()
    };
    let PauseOutcome::Journaled { .. } = request_pause(root, "w-p8", &req).unwrap() else {
        panic!("the direct pause is journaled");
    };

    // The person resumes 2 s later: the park's until is stale by the paused
    // interval, so the scan discards it and the wait parks again with the
    // new due time.
    std::thread::sleep(Duration::from_millis(2_000));
    let resumed = resume(root, "w-p8", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(
        !has_wait_pause(&events, Some(due)),
        "no park with the old until"
    );
    assert!(
        control::read_control_cursor(&run_dir).unwrap() > Some(n),
        "the cursor stands past the park's entry"
    );
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1, "the wait parks again");
    let until2 = wp[0].3.unwrap();
    assert!(
        until2 >= due + 2_000,
        "at least the paused 2 s later than the old one: {due} vs {until2}"
    );
    // It is the due time of the journal at the second park: the park is
    // active time, so a later `now` computes the same value.
    let now = apb_core::clock::now_ms();
    let active2 = apb_engine::pause::active_elapsed_ms(&events, ws_ts, now, now);
    let due2 = u64::try_from(now + 3_000 - active2).unwrap();
    assert_eq!(until2, due2, "equal to the recomputed due time");
}

/// The positive control of the discard tests (crash row P2): a park the
/// driver posted and died before applying, with the wait's real due time
/// and nothing behind it, is applied by the next drive. (A test binary
/// cannot re-exec `__drive-run`, so the next drive is the in-process
/// `resume`, the drive `resume_all` would start.)
#[test]
fn a_pending_park_is_applied_by_the_next_drive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_scripts(
        root,
        &WP_WRITE.replace("seconds: 3", "seconds: 30"),
        &[("go.sh", GO_SH)],
    );
    let _env = common::env_lock();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let run_dir = seed_waiting(root, "w-p9");
    let due = due_now(&run_dir, 30_000);
    let n = post_control(&run_dir, park_entry(due, "pause")).unwrap();

    let resumed = resume(root, "w-p9", None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    let wp = wait_parks(&events);
    assert_eq!(wp.len(), 1, "the pending park is the one applied");
    assert_eq!(wp[0].3, Some(due), "with the posted until");
    assert_eq!(wp[0].2, Some(PauseBy::Engine));
    assert_eq!(
        control::read_control_cursor(&run_dir).unwrap(),
        Some(n),
        "the cursor stands at the park's entry"
    );
    assert_eq!(
        control::read_control_after(&run_dir, None).unwrap().len(),
        1,
        "no second park was posted"
    );
}

// A list that may skip, then a long timer wait.
const WP_SKIP: &str = r#"
schema: 2
id: w
name: WP
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: work
    type: script
    script: "scripts/go.sh"
    runner: sh
    list:
      items: { inline: [ { id: i1 } ] }
      skip: { passed_in: previous_success }
  - { id: pause, type: wait, wait_for: { type: timer, seconds: 3 }, timeout_seconds: 3600 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: pause }
  - { from: pause, to: done }
"#;

/// A parked run's drive has left: its scripts copy is writable again, so a
/// plain removal of the run directory works; nothing under the project is
/// left read-only.
#[test]
fn a_parked_run_leaves_its_scripts_writable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The key is created under the env lock, held through the drive and
    // the cleanup: a test that redirects `APB_CONFIG_DIR` holds that lock.
    let _env = common::env_lock();
    apb_core::run_origin::ensure_key().unwrap();
    seed_scripts(root, WP_SKIP, &[("go.sh", GO_SH)]);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = spawn_wp(root, RunOptions::default());
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused, "parked");
    let run_dir = latest_run(root);
    assert_eq!(wait_parks(&read_all(&run_dir).unwrap()).len(), 1);
    let scripts = run_dir.join("scripts");
    for p in [scripts.clone(), scripts.join("go.sh")] {
        assert_ne!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o200,
            0,
            "{}",
            p.display()
        );
    }
    apb_engine::stop::stop_run(root, &run_id_of(&run_dir)).ok();
}
