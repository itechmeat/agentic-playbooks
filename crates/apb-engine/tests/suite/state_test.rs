use apb_engine::event::{Event, EventPayload};
use apb_engine::state::{NodeStatus, RunState, RunStatus};

fn ev(seq: u64, payload: EventPayload) -> Event {
    Event {
        seq,
        ts: 0,
        payload,
    }
}

fn run_started(playbook: &str) -> EventPayload {
    EventPayload::RunStarted {
        playbook: playbook.into(),
        version: "1.0.0".into(),
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
                artifacts: Vec::new(),
            },
        ),
        ev(
            2,
            EventPayload::NodeFinished {
                node: "ping".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: "pong".into(),
                artifacts: Vec::new(),
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
    // Crash-shape simulation (Task 2 fold test): a real mid-attempt crash now
    // leaves `attempt_started` in the journal (written at spawn time, carrying
    // the agent pid) with NO matching `attempt_finished`. This hand-built
    // journal reproduces exactly that shape - a spawn-journaled attempt that
    // never returned - and asserts the fold at state.rs:184-192 maps the open
    // attempt to interrupted (node and run). Before spawn-time journaling this
    // shape could never occur, because both events were written back-to-back at
    // node return, so a dead node read as `running` forever.
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
                pid: Some(4242),
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.nodes.get("ping"), Some(&NodeStatus::Interrupted));
    assert_eq!(s.run_status, RunStatus::Interrupted);
}

#[test]
fn run_resumed_folds_to_running() {
    // Task 3: a resume journals `run_resumed` (not the old `RunPaused` marker),
    // which folds the run back to running - so a resumed run is never stuck on
    // paused for the rest of its life. An interrupted node ahead of the marker
    // is still Running, and the marker sets the run status to Running.
    let events = vec![
        ev(0, run_started("w")),
        ev(
            1,
            EventPayload::NodeStarted {
                node: "a".into(),
                attempt: 1,
            },
        ),
        ev(
            2,
            EventPayload::RunResumed {
                from_node: "a".into(),
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.run_status, RunStatus::Running);
}

#[test]
fn legacy_run_paused_marker_still_folds_to_paused() {
    // Old journals that carry the legacy `RunPaused { reason: "resume from X" }`
    // marker must keep folding to paused, unchanged by the Task 3 rework.
    let events = vec![
        ev(0, run_started("w")),
        ev(
            1,
            EventPayload::RunPaused {
                reason: "resume from `a`".into(),
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.run_status, RunStatus::Paused);
}

#[test]
fn multi_attempt_open_after_finished_marks_interrupted() {
    // Crash-shape simulation with a retry: attempt 1 finished (failed), then a
    // retry spawned attempt 2 which never returned (the crash window). The last
    // event for `ping` is an open attempt_started, so the fold at
    // state.rs:184-192 must still map the node (and run) to interrupted - the
    // earlier finished attempt does not close the later open one.
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
                pid: Some(1001),
            },
        ),
        ev(
            2,
            EventPayload::AttemptFinished {
                node: "ping".into(),
                attempt: 1,
                status: "failed".into(),
                duration_ms: Some(1200),
                session: None,
            },
        ),
        ev(
            3,
            EventPayload::RetryStarted {
                node: "ping".into(),
                attempt: 2,
            },
        ),
        ev(
            4,
            EventPayload::AttemptStarted {
                node: "ping".into(),
                attempt: 2,
                agent: "claude-code".into(),
                soul_delivery: None,
                skills_mode: None,
                pid: Some(1002),
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.nodes.get("ping"), Some(&NodeStatus::Interrupted));
    assert_eq!(s.run_status, RunStatus::Interrupted);
    // The open attempt is attempt 2 (the crash window), recorded as the latest.
    assert_eq!(s.attempts.get("ping"), Some(&2));
}
