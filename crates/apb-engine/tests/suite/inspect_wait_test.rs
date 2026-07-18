use std::time::Duration;

use apb_engine::error::EngineError;
use apb_engine::event::{EventLog, EventPayload, WakeTrigger};
use apb_engine::inspect::{run_inspect, wait_wake};

fn run_dir(root: &std::path::Path, run_id: &str) -> std::path::PathBuf {
    root.join(".apb/runs").join(run_id)
}

#[test]
fn wait_wake_returns_none_promptly_when_run_already_finished() {
    let dir = tempfile::tempdir().unwrap();
    let rd = run_dir(dir.path(), "r1");
    let mut log = EventLog::create(&rd).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "w".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::RunFinished {
        outcome: "succeeded".into(),
    })
    .unwrap();

    let started = std::time::Instant::now();
    // The timeout is deliberately larger than detecting the terminal event should take,
    // but the test must not wait it out fully - completion is visible right on the first poll.
    let res = wait_wake(dir.path(), "r1", None, Duration::from_secs(1)).unwrap();
    assert!(res.is_none());
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "should return promptly, not wait out the full timeout"
    );
}

#[test]
fn wait_wake_finds_wake_then_none_after_cursor_past_it() {
    let dir = tempfile::tempdir().unwrap();
    let rd = run_dir(dir.path(), "r2");
    let mut log = EventLog::create(&rd).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "w".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    let wake_event = log
        .append(EventPayload::WakeRaised {
            trigger: WakeTrigger::NodeFailed,
            node: "impl".into(),
            detail: "exit 1".into(),
        })
        .unwrap();

    let found = wait_wake(dir.path(), "r2", None, Duration::from_secs(1)).unwrap();
    let wake = found.expect("expected a wake event");
    assert_eq!(wake.seq, wake_event.seq);
    assert!(matches!(wake.trigger, WakeTrigger::NodeFailed));
    assert_eq!(wake.node, "impl");
    assert_eq!(wake.detail, "exit 1");

    log.append(EventPayload::RunFinished {
        outcome: "failed".into(),
    })
    .unwrap();

    // The cursor is already past the found wake - only the terminal event follows it.
    let none = wait_wake(dir.path(), "r2", Some(wake.seq), Duration::from_millis(300)).unwrap();
    assert!(none.is_none());
}

#[test]
fn run_inspect_reports_wakes_status_and_context() {
    let dir = tempfile::tempdir().unwrap();
    let rd = run_dir(dir.path(), "r3");
    let mut log = EventLog::create(&rd).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "w".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::WakeRaised {
        trigger: WakeTrigger::NodeFailed,
        node: "impl".into(),
        detail: "exit 1".into(),
    })
    .unwrap();

    let value = run_inspect(dir.path(), "r3").unwrap();
    let wakes = value
        .get("wakes")
        .and_then(|v| v.as_array())
        .expect("wakes array");
    assert!(
        !wakes.is_empty(),
        "expected at least one wake in the summary"
    );
    let run_status = value
        .get("run_status")
        .and_then(|v| v.as_str())
        .expect("run_status string");
    assert!(!run_status.is_empty());
    let context = value
        .get("context")
        .and_then(|v| v.as_str())
        .expect("context string");
    // context.md was not written in this test - must be an empty string, not a missing field.
    assert_eq!(context, "");
}

#[test]
fn traversal_run_id_is_not_found_for_both_primitives() {
    let dir = tempfile::tempdir().unwrap();

    let wait_err =
        wait_wake(dir.path(), "../../etc", None, Duration::from_millis(200)).unwrap_err();
    assert!(
        matches!(wait_err, EngineError::NotFound(_)),
        "expected NotFound, got {wait_err:?}"
    );

    let inspect_err = run_inspect(dir.path(), "../../etc").unwrap_err();
    assert!(
        matches!(inspect_err, EngineError::NotFound(_)),
        "expected NotFound, got {inspect_err:?}"
    );
}
