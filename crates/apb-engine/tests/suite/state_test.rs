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

// A success report a success_check rejected is folded into `rejected_outputs`
// so a downstream node can read the discarded text via
// `nodes.<id>.rejected_output` (spec field-report-robustness).
#[test]
fn attempt_finished_rejected_output_populates_rejected_outputs() {
    let events = vec![
        ev(0, run_started("w")),
        ev(
            1,
            EventPayload::AttemptFinished {
                node: "a".into(),
                attempt: 1,
                status: "failed".into(),
                duration_ms: None,
                session: None,
                summary: None,
                rejected_output: Some("interim progress only".into()),
                partial_output: None,
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(
        s.rejected_outputs.get("a").map(String::as_str),
        Some("interim progress only"),
        "a rejected attempt's discarded text must fold into rejected_outputs"
    );
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
                summary: None,
                rejected_output: None,
                partial_output: None,
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

// Issue #42 finding 3: RunState::fold must carry the last RunError's reason
// (and node, when known) forward as `failure_reason`, for run_status/doctor
// to surface without reading events.jsonl directly.
#[test]
fn folds_run_error_into_failure_reason() {
    let events = vec![
        ev(0, run_started("w")),
        ev(
            1,
            EventPayload::NodeFinished {
                node: "work".into(),
                status: "failed".into(),
                attempt: 1,
                output: "boom".into(),
                artifacts: Vec::new(),
            },
        ),
        ev(
            2,
            EventPayload::RunError {
                node: Some("work".into()),
                reason: "node `work` has no outgoing edge and is not finish".into(),
            },
        ),
        ev(
            3,
            EventPayload::RunFinished {
                outcome: "failed".into(),
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.run_status, RunStatus::Failed);
    let reason = s.failure_reason.expect("failure_reason must be set");
    assert_eq!(reason.node.as_deref(), Some("work"));
    assert!(reason.reason.contains("no outgoing edge"));
    assert_eq!(
        reason.display(),
        "node `work`: node `work` has no outgoing edge and is not finish"
    );
}

// A run with no RunError at all (every run before this fix, and every run
// that never fails) carries no failure_reason.
#[test]
fn no_run_error_means_no_failure_reason() {
    let events = vec![
        ev(0, run_started("w")),
        ev(
            1,
            EventPayload::RunFinished {
                outcome: "succeeded".into(),
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert!(s.failure_reason.is_none());
}

/// A `defaults.on_failure` route is journaled as a traversal so the journal
/// records where the run actually went (Task 4). It is NOT a declared edge, so
/// it must not spend a bounded edge's `max_traversals` budget: it folds into its
/// own set instead of `edge_counts`.
#[test]
fn a_policy_route_folds_separately_from_a_declared_edge_traversal() {
    let events = vec![
        ev(0, run_started("w")),
        ev(
            1,
            EventPayload::EdgeTraversed {
                from: "check".into(),
                to: "tick".into(),
                via_policy: false,
            },
        ),
        ev(
            2,
            EventPayload::EdgeTraversed {
                from: "work".into(),
                to: "handler".into(),
                via_policy: true,
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(
        s.edge_counts
            .get(&("check".to_string(), "tick".to_string())),
        Some(&1),
        "a declared edge traversal still counts"
    );
    assert!(
        !s.edge_counts
            .contains_key(&("work".to_string(), "handler".to_string())),
        "a policy route must not consume an edge budget"
    );
    assert!(
        s.policy_routes
            .contains(&("work".to_string(), "handler".to_string())),
        "the policy route is recorded as one"
    );
}
