use apb_engine::event::{EventLog, EventPayload, WakeTrigger, read_all};
use apb_engine::state::{RunState, RunStatus};

#[test]
fn wake_and_abort_round_trip_and_fold() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::create(dir.path()).unwrap();
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
    log.append(EventPayload::SupervisorAction {
        action: "node_retry".into(),
        node: Some("impl".into()),
        detail: "retry with hint".into(),
    })
    .unwrap();
    log.append(EventPayload::RunAborted {
        reason: "user cancel".into(),
    })
    .unwrap();

    let events = read_all(dir.path()).unwrap();
    assert_eq!(events.len(), 4);
    // serialization of the type tag is in snake_case
    let raw = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
    assert!(raw.contains("\"type\":\"wake_raised\""));
    assert!(raw.contains("\"trigger\":\"node_failed\""));
    assert!(raw.contains("\"type\":\"supervisor_action\""));
    assert!(raw.contains("\"type\":\"run_aborted\""));

    let state = RunState::fold(&events);
    assert_eq!(state.run_status, RunStatus::Aborted);
}
