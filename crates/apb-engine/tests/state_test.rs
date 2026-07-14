use apb_engine::event::{Event, EventPayload};
use apb_engine::state::{NodeStatus, RunState, RunStatus};

fn ev(seq: u64, payload: EventPayload) -> Event {
    Event {
        seq,
        ts: 0,
        payload,
    }
}

#[test]
fn folds_finished_run() {
    let events = vec![
        ev(
            0,
            EventPayload::RunStarted {
                playbook: "w".into(),
                version: "1.0.0".into(),
            },
        ),
        ev(
            1,
            EventPayload::NodeFinished {
                node: "start".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: String::new(),
            },
        ),
        ev(
            2,
            EventPayload::NodeFinished {
                node: "ping".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: "pong".into(),
            },
        ),
        ev(
            3,
            EventPayload::RunFinished {
                outcome: "succeeded".into(),
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.run_status, RunStatus::Succeeded);
    assert_eq!(s.nodes.get("ping"), Some(&NodeStatus::Succeeded));
    assert_eq!(s.outputs.get("ping").map(String::as_str), Some("pong"));
    assert_eq!(s.last_node.as_deref(), Some("ping"));
}

#[test]
fn open_attempt_marks_interrupted() {
    // attempt_started without attempt_finished => node and run interrupted
    let events = vec![
        ev(
            0,
            EventPayload::RunStarted {
                playbook: "w".into(),
                version: "1.0.0".into(),
            },
        ),
        ev(
            1,
            EventPayload::AttemptStarted {
                node: "ping".into(),
                attempt: 1,
                agent: "claude-code".into(),
                soul_delivery: None,
                skills_mode: None,
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.nodes.get("ping"), Some(&NodeStatus::Interrupted));
    assert_eq!(s.run_status, RunStatus::Interrupted);
}
