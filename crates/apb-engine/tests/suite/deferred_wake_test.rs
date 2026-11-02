//! Waking a run whose retry waits: a release or a pause is resumed only
//! while it still holds the run, one waker per wait, nothing a person asks
//! for in the release window is lost, and a retry-now waives the delay
//! only with a resume that went through (live, released and paused runs).

use crate::common;
use crate::deferred_retry_test::{
    DEADLINE, LONG_DELAY, agent, attempts_of, deferrals, poll_until, released, run_dir, seed,
    wait_driver_gone,
};
use std::fs;
use std::path::Path;
use std::sync::mpsc;

use apb_engine::error::EngineError;
use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::pause::{
    PauseBy, PauseMode, PauseOutcome, PauseRequest, ResumeAllAction, WakeHold, WakeOutcome,
    request_pause, resume_all, run_release_waker,
};
use apb_engine::scheduler::deferred::{ExpediteBy, expedite_live, expedite_run};
use apb_engine::scheduler::{RELEASE_SUPERSEDED, ResumeRequest, RetryNow, RunOptions, RunResult};
use apb_engine::state::{RunState, RunStatus};
use apb_engine::waker::{Lease, WakerFor};

/// One node whose only retry waits an hour.
const HOUR_DELAY: &str = r#"
schema: 2
id: hourpb
name: Hour
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: { initial: 1h } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn operator_pause() -> PauseRequest {
    PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    }
}

fn retry_now() -> ResumeRequest<'static> {
    ResumeRequest {
        retry_now: Some(RetryNow {
            node: Some("work"),
            by: ExpediteBy::Operator,
        }),
        ..ResumeRequest::operator()
    }
}

fn kinds(events: &[apb_engine::event::Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            serde_json::to_value(&e.payload).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

fn expedites(rd: &Path) -> usize {
    read_all(rd)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::RetryExpedited { .. }))
        .count()
}

/// Starts `id` on a drive thread that waits in-process for its retry (no
/// release); returns the run's id, its directory and the drive's result.
fn drive_in_thread(
    root: &Path,
    id: &'static str,
) -> (
    String,
    std::path::PathBuf,
    mpsc::Receiver<Result<RunResult, EngineError>>,
) {
    let (tx, rx) = mpsc::channel();
    {
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(apb_engine::scheduler::run(
                &root,
                id,
                None,
                RunOptions::default(),
            ));
        });
    }
    let runs = root.join(".apb/runs");
    let run_id = poll_until("the run directory", || {
        fs::read_dir(&runs)
            .ok()?
            .flatten()
            .find(|e| e.path().join("events.jsonl").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
    });
    let rd = runs.join(&run_id);
    poll_until("the deferral", || {
        let events = read_all(&rd).ok()?;
        (!deferrals(&events, "work").is_empty()).then_some(())
    });
    (run_id, rd, rx)
}

/// A released run a person paused after its waker looked: the driver the
/// waker started finds the release gone under the run's event lock and
/// journals nothing, so the pause stands.
#[test]
fn a_stale_release_never_resumes_over_a_newer_pause() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "relpb", LONG_DELAY, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let _release = common::EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let res = apb_engine::scheduler::run(root, "relpb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Running);
    let rd = run_dir(root, &res);
    let release = released(&rd);
    assert_eq!(
        request_pause(root, &res.run_id, &operator_pause()).unwrap(),
        PauseOutcome::Journaled {
            seq: release.seq + 1
        }
    );
    let before = read_all(&rd).unwrap().len();
    let err =
        apb_engine::scheduler::resume_held(root, &res.run_id, WakeHold::Release(release.seq), None)
            .unwrap_err();
    assert!(
        matches!(&err, EngineError::Conflict(why) if why == RELEASE_SUPERSEDED),
        "{err:?}"
    );
    let events = read_all(&rd).unwrap();
    assert_eq!(events.len(), before, "nothing journaled");
    assert_eq!(RunState::fold(&events).run_status, RunStatus::Paused);
}

/// A journal of a run released at `wake_at_ms`, its driver pid file naming
/// this (live) process: the window between the release and the driver's
/// exit.
fn seed_released(root: &Path, run_id: &str, wake_at_ms: u64) -> std::path::PathBuf {
    apb_core::registry::init_project(root).unwrap();
    let rd = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&rd).unwrap();
    let mut log = EventLog::create(&rd).unwrap();
    for p in [
        EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
        },
        EventPayload::RetryDeferred {
            node: "work".into(),
            after_attempt: 0,
            retry: 1,
            step: 0,
            binding: "b".repeat(64),
            delay_ms: 3_600_000,
            failure_kind: None,
        },
        EventPayload::DriveReleased {
            wake_at_ms,
            nodes: vec!["work".into()],
        },
    ] {
        log.append(p).unwrap();
    }
    fs::write(
        apb_engine::driver::driver_pid_path(&rd),
        std::process::id().to_string(),
    )
    .unwrap();
    rd
}

/// In the window between a release and its driver's exit nothing is handed
/// to that driver: a pause is journaled at once, a live retry-now is
/// refused (the caller resumes instead), and the plan says not live.
#[test]
fn nothing_asked_in_the_release_window_is_left_to_the_exiting_driver() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let wake = apb_core::clock::now_ms_u64() + 3_600_000;
    let rd = seed_released(root, "r-window", wake);
    let plan = apb_engine::retry_wait::plan_retry_now(&rd, "r-window", None).unwrap();
    assert!(!plan.live, "a released run is not driven");
    assert_eq!(
        expedite_live(&rd, "r-window", None, ExpediteBy::Operator).unwrap(),
        None
    );
    assert_eq!(expedites(&rd), 0);
    let outcome = request_pause(root, "r-window", &operator_pause()).unwrap();
    assert!(
        matches!(outcome, PauseOutcome::Journaled { .. }),
        "{outcome:?}"
    );
    assert!(
        apb_engine::control::read_control_after(&rd, None)
            .unwrap()
            .is_empty(),
        "nothing posted"
    );
}

/// A `--keep-lock` pause in the release window is not refused by the busy
/// lock its own departing driver still holds: the lock is parked under the
/// run, so the driver's exit leaves it in place.
#[test]
fn a_keep_lock_pause_in_the_release_window_parks_the_drivers_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let wake = apb_core::clock::now_ms_u64() + 3_600_000;
    let rd = seed_released(root, "r-keep", wake);
    fs::write(rd.join("playbook.yaml"), HOUR_DELAY).unwrap();
    let lock = root.join(".apb/workdir.lock");
    fs::write(&lock, std::process::id().to_string()).unwrap();
    let req = PauseRequest {
        keep_lock: true,
        ..operator_pause()
    };
    let outcome = request_pause(root, "r-keep", &req).unwrap();
    assert!(
        matches!(outcome, PauseOutcome::Journaled { .. }),
        "{outcome:?}"
    );
    assert_eq!(fs::read_to_string(&lock).unwrap(), "paused:r-keep");
}

/// One waker per wait: while a waker holds a release's lease, a second
/// waker of it ends at once and `resume --all` leaves the due run to it. An
/// expedite makes a released run due at once.
#[test]
fn a_held_lease_keeps_other_wakers_off_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let wake = apb_core::clock::now_ms_u64() + 3_600_000;
    let rd = seed_released(root, "r-lease", wake);
    fs::remove_file(apb_engine::driver::driver_pid_path(&rd)).unwrap();
    let seq = released(&rd).seq;
    let lease = Lease::try_take(&rd, WakerFor::Release(seq))
        .unwrap()
        .expect("free");
    assert_eq!(
        run_release_waker(root, "r-lease", seq).unwrap(),
        WakeOutcome::Superseded,
        "a second waker ends at once"
    );
    expedite_run(&rd, Some("work"), Some(ExpediteBy::Supervisor)).unwrap();
    let entries = resume_all(root).unwrap();
    assert_eq!(
        entries.iter().map(|e| &e.action).collect::<Vec<_>>(),
        vec![&ResumeAllAction::Busy {
            detail: "its waker is resuming it".into()
        }],
        "the expedited run is due, and left to its waker"
    );
    drop(lease);
    assert!(!Lease::held(&rd, WakerFor::Release(seq)));
}

/// A live drive starts the delayed attempt as soon as a retry-now lands,
/// long before its delay.
#[test]
fn retry_now_makes_a_live_drive_start_the_delayed_attempt_early() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "hourpb", HOUR_DELAY, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let _release = common::EnvVar::set("APB_RELEASE_AFTER_MS", "600000000");
    let (run_id, rd, rx) = drive_in_thread(root, "hourpb");
    let nodes = poll_until("a live driver", || {
        expedite_live(&rd, &run_id, Some("work"), ExpediteBy::Operator).unwrap()
    });
    assert_eq!(nodes, vec!["work".to_string()]);
    let done = rx.recv_timeout(DEADLINE).unwrap().unwrap();
    assert_eq!(done.outcome, RunStatus::Succeeded);
    let events = read_all(&rd).unwrap();
    let deferred_at = events[deferrals(&events, "work")[0]].ts;
    let second = attempts_of(&events, "work")[1].ts;
    assert!(
        second < deferred_at + 60_000,
        "started long before its delay"
    );
}

/// A paused run: a retry-now resume refused (another run holds the busy
/// lock) waives nothing; the one that goes through journals the expedite
/// right before its `run_resumed` and starts the attempt at once.
#[test]
fn retry_now_on_a_paused_run_waives_the_delay_only_with_its_resume() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "hourpb", HOUR_DELAY, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let _release = common::EnvVar::set("APB_RELEASE_AFTER_MS", "600000000");
    let (run_id, rd, rx) = drive_in_thread(root, "hourpb");
    request_pause(root, &run_id, &operator_pause()).unwrap();
    let paused = rx.recv_timeout(DEADLINE).unwrap().unwrap();
    assert_eq!(paused.outcome, RunStatus::Paused);

    let busy = root.join(".apb/workdir.lock");
    fs::write(&busy, std::process::id().to_string()).unwrap();
    let err = apb_engine::resume_req(root, &run_id, &retry_now()).unwrap_err();
    assert!(matches!(err, EngineError::WorkdirBusy(_)), "{err:?}");
    assert_eq!(expedites(&rd), 0, "a refused resume waives no delay");
    fs::remove_file(&busy).unwrap();

    let done = apb_engine::resume_req(root, &run_id, &retry_now()).unwrap();
    assert_eq!(done.outcome, RunStatus::Succeeded);
    let events = read_all(&rd).unwrap();
    let k = kinds(&events);
    let at = k.iter().position(|t| t == "retry_expedited").unwrap();
    assert_eq!(k[at + 1], "run_resumed", "{k:?}");
    assert_eq!(
        serde_json::to_value(&events[at].payload).unwrap()["by"],
        "operator"
    );
    assert_eq!(attempts_of(&events, "work").len(), 2);
}

/// A released run: a live retry-now leaves it to the resume, which starts
/// the attempt at once.
#[test]
fn retry_now_resumes_a_released_run_with_the_retry_due() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "hourpb", HOUR_DELAY, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let _release = common::EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let res = apb_engine::scheduler::run(root, "hourpb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Running, "released");
    let rd = run_dir(root, &res);
    wait_driver_gone(&rd, &res.run_id);
    assert_eq!(
        expedite_live(&rd, &res.run_id, Some("work"), ExpediteBy::Operator).unwrap(),
        None
    );
    let done = apb_engine::resume_req(root, &res.run_id, &retry_now()).unwrap();
    assert_eq!(done.outcome, RunStatus::Succeeded);
    let events = read_all(&rd).unwrap();
    assert_eq!(attempts_of(&events, "work").len(), 2);
    assert_eq!(expedites(&rd), 1);
}
