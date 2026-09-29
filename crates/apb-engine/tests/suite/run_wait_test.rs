//! Server-side run waits (`run_wait`, `apb wait`, `supervisor_wait_event`):
//! they must return exactly when the caller has something to act on, never
//! report a gate the caller has just answered, and keep a waiting
//! supervisor's heartbeat fresh so a long wait costs no model turns.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::event::{EventLog, EventPayload, WakeTrigger, read_all};
use apb_engine::review::{ReviewCommand, post_review};
use apb_engine::run_wait::{
    NeedsInput, SupervisorWait, WaitReason, clip_tail, wait_run, wait_run_with,
    wait_supervisor_event_with,
};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const WF_REVIEW: &str = r#"
schema: 1
id: rev
name: Review
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/rev/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), WF_REVIEW).unwrap();
    fs::write(root.join(".apb/playbooks/rev/current"), "1.0.0").unwrap();
}

fn start(root: &Path) -> (mpsc::Receiver<RunResult>, String) {
    let (tx, rx) = mpsc::channel();
    let r = root.to_path_buf();
    std::thread::spawn(move || {
        let res = run(&r, "rev", None, RunOptions::default()).unwrap();
        let _ = tx.send(res);
    });
    let started = Instant::now();
    let run_id = loop {
        let found = fs::read_dir(root.join(".apb/runs")).ok().and_then(|rd| {
            rd.filter_map(|e| e.ok())
                .find(|e| e.path().join("events.jsonl").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
        });
        if let Some(id) = found {
            break id;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "no run dir");
        std::thread::sleep(Duration::from_millis(10));
    };
    (rx, run_id)
}

/// Waits (bounded) until the run journals its review request.
fn wait_for_pending_review(run_dir: &Path) {
    let started = Instant::now();
    loop {
        let requested = read_all(run_dir).is_ok_and(|events| {
            events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::ReviewRequested { .. }))
        });
        if requested {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "timed out after 20s waiting for the review gate to be requested"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn decide(run_dir: &Path, decision: &str) {
    post_review(
        run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: decision.into(),
            note: String::new(),
        },
    )
    .unwrap();
}

#[test]
fn run_wait_reports_a_review_gate_then_the_finish() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let (rx, run_id) = start(dir.path());
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    let res = wait_run_with(
        dir.path(),
        &run_id,
        Duration::from_secs(10),
        Duration::from_millis(100),
        Duration::from_millis(20),
    )
    .unwrap();
    assert_eq!(res.reason, WaitReason::NeedsInput);
    assert_eq!(res.needs, Some(NeedsInput::Review));
    assert_eq!(res.status, RunStatus::Running);

    // Decide and wait again at once, with the production grace: the wait must
    // not hand back the gate that was just answered.
    decide(&run_dir, "approved");
    let res = wait_run(dir.path(), &run_id, Duration::from_secs(10)).unwrap();
    assert_eq!(res.reason, WaitReason::Finished);
    assert_eq!(res.status, RunStatus::Succeeded);
    assert_eq!(res.needs, None);
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn run_wait_times_out_while_a_gate_is_still_inside_its_grace() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let (rx, run_id) = start(dir.path());
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    // The premise: the gate is pending before the timed wait starts. A loaded
    // host can take longer than the 300 ms wait to reach it, and the decision
    // below needs a pending review to land on.
    wait_for_pending_review(&run_dir);

    let started = Instant::now();
    let res = wait_run_with(
        dir.path(),
        &run_id,
        Duration::from_millis(300),
        Duration::from_secs(60),
        Duration::from_millis(20),
    )
    .unwrap();
    assert_eq!(res.reason, WaitReason::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));

    decide(&run_dir, "rejected");
    let res = wait_run(dir.path(), &run_id, Duration::from_secs(10)).unwrap();
    assert_eq!(res.reason, WaitReason::Finished);
    assert_eq!(res.status, RunStatus::Failed);
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn run_wait_rejects_an_unknown_run() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    assert!(wait_run(dir.path(), "nope", Duration::from_millis(10)).is_err());
    assert!(wait_run(dir.path(), "../x", Duration::from_millis(10)).is_err());
}

fn synthetic_run(root: &Path, run_id: &str) -> EventLog {
    let rd = root.join(".apb/runs").join(run_id);
    let mut log = EventLog::create(&rd).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "w".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log
}

fn stored_beat(hb: &Path) -> Option<u128> {
    fs::read_to_string(hb).ok()?.trim().parse().ok()
}

#[test]
fn supervisor_wait_keeps_the_heartbeat_fresh_while_it_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = synthetic_run(dir.path(), "hb");
    let hb = dir.path().join(".apb/runs/hb/supervisor/heartbeat");

    // Readiness, not a wall-clock bound: the wait blocks far longer than the
    // test needs, and the test only asks whether a newer heartbeat lands while
    // it is still blocking. A loaded runner delays the refresh but cannot fake
    // one, so a slow host passes and a wait that beats only on entry fails.
    let root = dir.path().to_path_buf();
    // The wait's own 20 s timeout bounds `waiter.join()` below, so the join
    // cannot hang even if `run_finished` were never seen; it stays under the
    // nextest SLOW period (60 s).
    let waiter = std::thread::spawn(move || {
        wait_supervisor_event_with(
            &root,
            "hb",
            None,
            Duration::from_secs(20),
            Duration::from_millis(50),
        )
    });

    let limit = Instant::now() + Duration::from_secs(15);
    let entry = loop {
        if let Some(beat) = stored_beat(&hb) {
            break beat;
        }
        assert!(Instant::now() < limit, "no heartbeat on entry");
        std::thread::sleep(Duration::from_millis(10));
    };
    loop {
        if stored_beat(&hb).is_some_and(|beat| beat > entry) {
            break;
        }
        assert!(!waiter.is_finished(), "the wait returned before it blocked");
        assert!(
            Instant::now() < limit,
            "the heartbeat was never refreshed while the wait blocked"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    log.append(EventPayload::RunFinished {
        outcome: "success".into(),
    })
    .unwrap();
    assert_eq!(waiter.join().unwrap().unwrap(), SupervisorWait::Ended);
}

#[test]
fn supervisor_wait_returns_wakes_reviews_and_the_end_in_seq_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = synthetic_run(dir.path(), "sv");
    let wake = log
        .append(EventPayload::WakeRaised {
            trigger: WakeTrigger::NodeFailed,
            node: "impl".into(),
            detail: "exit 1".into(),
            triage: None,
        })
        .unwrap();
    let review = log
        .append(EventPayload::ReviewRequested {
            node: "gate".into(),
            options: vec!["approved".into()],
            title: None,
            instruction: "decide".into(),
            prompt: None,
            recommendation: None,
        })
        .unwrap();
    log.append(EventPayload::RunFinished {
        outcome: "failed".into(),
    })
    .unwrap();
    let t = Duration::from_secs(2);
    let beat = Duration::from_secs(10);

    match wait_supervisor_event_with(dir.path(), "sv", None, t, beat).unwrap() {
        SupervisorWait::Wake(w) => {
            assert_eq!(w.seq, wake.seq);
            assert_eq!(w.detail, "exit 1");
        }
        other => panic!("expected the wake, got {other:?}"),
    }
    assert_eq!(
        wait_supervisor_event_with(dir.path(), "sv", Some(wake.seq), t, beat).unwrap(),
        SupervisorWait::Review {
            seq: review.seq,
            node: "gate".into()
        }
    );
    let started = Instant::now();
    assert_eq!(
        wait_supervisor_event_with(dir.path(), "sv", Some(review.seq), t, beat).unwrap(),
        SupervisorWait::Ended
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(read_all(&dir.path().join(".apb/runs/sv")).unwrap().len() >= 4);
}

#[test]
fn clip_tail_keeps_the_end_on_a_char_boundary() {
    assert_eq!(clip_tail("short", 16), ("short".to_string(), false));
    let text = format!("{}ERROR: boom", "é".repeat(100));
    let (kept, clipped) = clip_tail(&text, 20);
    assert!(clipped);
    assert!(kept.ends_with("ERROR: boom"), "{kept}");
    assert!(kept.starts_with("[... "), "{kept}");
}

#[test]
fn a_resumed_waiter_keeps_the_grace_across_short_slices() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let (rx, run_id) = start(dir.path());
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // Slices far shorter than the grace: a waiter that restarted the grace on
    // every slice would never report the gate.
    let mut waiter = apb_engine::run_wait::RunWaiter::new(dir.path(), &run_id).unwrap();
    let started = Instant::now();
    let res = loop {
        let res = waiter.wait(Duration::from_millis(50)).unwrap();
        if res.reason != WaitReason::Timeout {
            break res;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "gate never reported"
        );
    };
    assert_eq!(res.reason, WaitReason::NeedsInput);
    decide(&run_dir, "approved");
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

/// A pid that existed and is provably gone (spawned, waited for, reaped).
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn a throwaway child to borrow a pid from");
    let pid = child.id();
    child.wait().expect("reap the throwaway child");
    pid
}

/// A driver that dies BETWEEN two nodes leaves no open attempt behind, so the
/// journal alone still reads `running`, and nothing will ever write another
/// line to it. The wait used to keep polling that run until its own timeout
/// (`apb wait` without a timeout never returned, MCP `run_wait` answered
/// `timeout` forever). A provably dead driver is a stop: the run is reported
/// `interrupted`, and the wait returns well inside the caller's timeout.
#[test]
fn run_wait_stops_on_a_dead_driver_between_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = synthetic_run(dir.path(), "orphan");
    log.append(EventPayload::NodeStarted {
        node: "start".into(),
        attempt: 1,
    })
    .unwrap();
    log.append(EventPayload::NodeFinished {
        node: "start".into(),
        status: "succeeded".into(),
        attempt: 1,
        output: String::new(),
        artifacts: Vec::new(),
    })
    .unwrap();
    let run_dir = dir.path().join(".apb/runs/orphan");
    fs::write(run_dir.join("driver.pid"), dead_pid().to_string()).unwrap();

    let started = Instant::now();
    let res = wait_run_with(
        dir.path(),
        "orphan",
        Duration::from_secs(20),
        Duration::from_millis(100),
        Duration::from_millis(20),
    )
    .unwrap();

    assert_eq!(res.reason, WaitReason::Stopped);
    assert_eq!(res.status, RunStatus::Interrupted);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a dead driver must end the wait promptly, took {:?}",
        started.elapsed()
    );
}

/// The driver appends `events.jsonl` while waits read it, so a read can land
/// in the middle of a line. That is a normal state of a live run, and it used
/// to fail the whole wait (`apb wait` exited 2 mid-run, `run_wait` answered an
/// engine error). The torn tail is simply not there yet.
#[test]
fn run_wait_reads_through_a_torn_trailing_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = synthetic_run(dir.path(), "torn");
    log.append(EventPayload::RunFinished {
        outcome: "succeeded".into(),
    })
    .unwrap();
    let events = dir.path().join(".apb/runs/torn/events.jsonl");
    let mut file = fs::OpenOptions::new().append(true).open(&events).unwrap();
    std::io::Write::write_all(&mut file, br#"{"seq":9,"ts":9,"type":"node_sta"#).unwrap();

    let res = wait_run(dir.path(), "torn", Duration::from_secs(5)).unwrap();

    assert_eq!(res.reason, WaitReason::Finished);
    assert_eq!(res.status, RunStatus::Succeeded);
}
