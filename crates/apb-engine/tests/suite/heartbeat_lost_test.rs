use apb_engine::error::EngineError;
use apb_engine::event::{EventLog, EventPayload};
use apb_engine::inspect::{
    find_session_by_token, heartbeat_age_ms, should_declare_lost, touch_heartbeat,
    write_supervisor_session,
};
use apb_engine::state::RunState;

fn run_dir(root: &std::path::Path, run_id: &str) -> std::path::PathBuf {
    root.join(".apb/runs").join(run_id)
}

// Scenario 1: write_supervisor_session + find_session_by_token, including
// the traversal guard and a miss on an unknown token.
#[test]
fn write_and_find_session_by_token_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let rd1 = run_dir(dir.path(), "run-a");
    let rd2 = run_dir(dir.path(), "run-b");
    EventLog::create(&rd1).unwrap();
    EventLog::create(&rd2).unwrap();

    write_supervisor_session(
        dir.path(),
        "run-a",
        "the-token",
        &["observe".to_string(), "retry".to_string()],
    )
    .unwrap();

    let found = find_session_by_token(dir.path(), "the-token").unwrap();
    let (run_id, caps) = found.expect("expected the session to be found");
    assert_eq!(run_id, "run-a");
    assert_eq!(caps, vec!["observe".to_string(), "retry".to_string()]);

    let missing = find_session_by_token(dir.path(), "unknown-token").unwrap();
    assert!(
        missing.is_none(),
        "unknown token must resolve to None, got {missing:?}"
    );

    let err = write_supervisor_session(dir.path(), "../../etc", "tok", &[]).unwrap_err();
    assert!(
        matches!(err, EngineError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

// find_session_by_token must not panic if the runs directory does not exist at all.
#[test]
fn find_session_by_token_missing_runs_dir_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let found = find_session_by_token(dir.path(), "whatever").unwrap();
    assert!(found.is_none());
}

// Scenario 2: touch_heartbeat then heartbeat_age_ms - a small age;
// absent heartbeat - None; traversal - NotFound.
#[test]
fn touch_then_age_is_small_absent_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let rd = run_dir(dir.path(), "run-hb");
    EventLog::create(&rd).unwrap();

    let before = heartbeat_age_ms(dir.path(), "run-hb").unwrap();
    assert!(
        before.is_none(),
        "no heartbeat file yet, expected None, got {before:?}"
    );

    touch_heartbeat(dir.path(), "run-hb").unwrap();
    let age = heartbeat_age_ms(dir.path(), "run-hb").unwrap();
    let age = age.expect("expected Some(age) right after touch_heartbeat");
    assert!(
        age < 5_000,
        "age should be small right after touch, got {age}"
    );

    let err = touch_heartbeat(dir.path(), "../../etc").unwrap_err();
    assert!(
        matches!(err, EngineError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );

    let err = heartbeat_age_ms(dir.path(), "../../etc").unwrap_err();
    assert!(
        matches!(err, EngineError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

// Scenario 3: the pure decision function should_declare_lost.
#[test]
fn should_declare_lost_decision_table() {
    assert!(
        !should_declare_lost(None, 60_000, false),
        "absent heartbeat must never be a loss"
    );
    assert!(
        should_declare_lost(Some(120_000), 60_000, false),
        "stale heartbeat past threshold must be a loss"
    );
    assert!(
        !should_declare_lost(Some(120_000), 60_000, true),
        "already-logged loss must not repeat"
    );
    assert!(
        !should_declare_lost(Some(1_000), 60_000, false),
        "fresh heartbeat must not be a loss"
    );
}

// Scenario 4: SupervisorLost round-trip through EventLog + RunState::fold -
// fold does not panic, status does not change, the event serializes with type=supervisor_lost.
#[test]
fn supervisor_lost_roundtrip_is_journal_only() {
    let dir = tempfile::tempdir().unwrap();
    let rd = run_dir(dir.path(), "run-lost");
    let mut log = EventLog::create(&rd).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "w".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::SupervisorLost {
        detail: "no heartbeat for 120s".into(),
    })
    .unwrap();

    let events = apb_engine::event::read_all(&rd).unwrap();
    let lost_event = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::SupervisorLost { .. }))
        .expect("expected a SupervisorLost event in the log");
    let json = serde_json::to_string(lost_event).unwrap();
    assert!(
        json.contains("\"type\":\"supervisor_lost\""),
        "expected supervisor_lost tag, got {json}"
    );

    let state = RunState::fold(&events);
    assert_eq!(
        state.run_status.as_str(),
        "running",
        "SupervisorLost must not change run_status"
    );
}
