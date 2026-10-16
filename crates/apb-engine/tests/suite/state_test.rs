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
                failure_kind: None,
                usage: None,
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
                spawn_ms: None,
                model: None,
                workdir: None,
                transcript: None,
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
                by: None,
                explicit: false,
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
                by: None,
                mode: None,
                until: None,
                keep_lock: false,
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.run_status, RunStatus::Paused);
}

/// Journal compatibility: an older journal whose resume wrote the legacy
/// `run_paused { reason: "resume from X" }` marker and whose driver then died
/// mid-attempt reads interrupted, as it always did, not paused.
#[test]
fn a_legacy_resume_marker_followed_by_a_crash_reads_interrupted() {
    let events = vec![
        ev(0, run_started("w")),
        ev(
            1,
            EventPayload::RunPaused {
                reason: "resume from `a`".into(),
                by: None,
                mode: None,
                until: None,
                keep_lock: false,
            },
        ),
        ev(
            2,
            EventPayload::NodeStarted {
                node: "a".into(),
                attempt: 1,
            },
        ),
        ev(
            3,
            EventPayload::AttemptStarted {
                node: "a".into(),
                attempt: 1,
                agent: "claude".into(),
                soul_delivery: None,
                skills_mode: None,
                pid: None,
                spawn_ms: None,
                model: None,
                workdir: None,
                transcript: None,
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.run_status, RunStatus::Interrupted);
    assert_eq!(s.nodes.get("a"), Some(&NodeStatus::Interrupted));

    // A real pause over a dead attempt stays paused.
    let mut paused = events;
    paused.push(ev(
        4,
        EventPayload::RunPaused {
            reason: "driverless".into(),
            by: None,
            mode: None,
            until: None,
            keep_lock: false,
        },
    ));
    assert_eq!(RunState::fold(&paused).run_status, RunStatus::Paused);
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
                spawn_ms: None,
                model: None,
                workdir: None,
                transcript: None,
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
                failure_kind: None,
                usage: None,
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
                spawn_ms: None,
                model: None,
                workdir: None,
                transcript: None,
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
                uncounted: false,
            },
        ),
        ev(
            2,
            EventPayload::EdgeTraversed {
                from: "work".into(),
                to: "handler".into(),
                via_policy: true,
                uncounted: false,
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
        s.journaled_hops
            .contains(&("work".to_string(), "handler".to_string())),
        "the policy route is recorded as one"
    );
}

/// The whole accounting contract in one table. Only a record that is neither a
/// policy route nor uncounted spends a bounded edge's max_traversals budget;
/// every record, whatever its flags, is a hop.
#[test]
fn edge_traversed_flags_decide_counting_and_every_record_is_a_hop() {
    let events = vec![
        ev(
            0,
            EventPayload::EdgeTraversed {
                from: "a".into(),
                to: "b".into(),
                via_policy: false,
                uncounted: false,
            },
        ),
        ev(
            1,
            EventPayload::EdgeTraversed {
                from: "c".into(),
                to: "d".into(),
                via_policy: false,
                uncounted: true,
            },
        ),
        ev(
            2,
            EventPayload::EdgeTraversed {
                from: "e".into(),
                to: "f".into(),
                via_policy: true,
                uncounted: false,
            },
        ),
        ev(
            3,
            EventPayload::EdgeTraversed {
                from: "g".into(),
                to: "h".into(),
                via_policy: true,
                uncounted: true,
            },
        ),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.edge_counts.get(&("a".into(), "b".into())), Some(&1));
    assert!(!s.edge_counts.contains_key(&("c".into(), "d".into())));
    assert!(!s.edge_counts.contains_key(&("e".into(), "f".into())));
    assert!(!s.edge_counts.contains_key(&("g".into(), "h".into())));
    for pair in [("a", "b"), ("c", "d"), ("e", "f"), ("g", "h")] {
        assert!(
            s.journaled_hops.contains(&(pair.0.into(), pair.1.into())),
            "every journaled record is a hop, missing {pair:?}"
        );
    }
}

/// A journal line written before the field existed carries neither flag on the
/// wire and must still COUNT, unchanged. Written as raw JSON on purpose, so the
/// test exercises serde defaulting rather than a Rust literal.
#[test]
fn a_legacy_edge_traversed_line_still_counts() {
    let line = r#"{"seq":0,"ts":1,"type":"edge_traversed","from":"a","to":"b"}"#;
    let e: Event = serde_json::from_str(line).unwrap();
    let s = RunState::fold(&[e]);
    assert_eq!(s.edge_counts.get(&("a".into(), "b".into())), Some(&1));
    assert!(s.journaled_hops.contains(&("a".into(), "b".into())));
}

/// Issue #203: the fold-level revival. A journal of a first pass (`c`
/// delivered through `c2` into `j`, which ran into `done`; `c` and `b` fork
/// marks; `j` refused once) ending right after `nodes_revived` for `c`.
fn revival_journal() -> Vec<EventPayload> {
    let finished = |node: &str, status: &str| {
        [
            EventPayload::NodeStarted {
                node: node.into(),
                attempt: 1,
            },
            EventPayload::NodeFinished {
                node: node.into(),
                status: status.into(),
                attempt: 1,
                output: format!("{node} out"),
                artifacts: Vec::new(),
            },
        ]
    };
    let hop = |from: &str, to: &str, uncounted: bool| EventPayload::EdgeTraversed {
        from: from.into(),
        to: to.into(),
        via_policy: false,
        uncounted,
    };
    let mut e = vec![run_started("p")];
    e.extend(finished("b", "succeeded"));
    e.push(hop("b", "j", true));
    e.extend(finished("c", "succeeded"));
    e.push(hop("c", "c2", false));
    e.extend(finished("c2", "succeeded"));
    e.push(hop("c2", "j", true));
    e.push(EventPayload::NodeStarted {
        node: "j".into(),
        attempt: 1,
    });
    e.push(EventPayload::JoinRefused {
        node: "j".into(),
        sources: vec!["c2".into()],
        reason: "test".into(),
    });
    e.push(EventPayload::NodeFinished {
        node: "j".into(),
        status: "succeeded".into(),
        attempt: 1,
        output: "j out".into(),
        artifacts: Vec::new(),
    });
    e.push(hop("j", "done", true));
    for n in ["b", "c"] {
        e.push(EventPayload::BranchFailed {
            fork: "start".into(),
            node: n.into(),
            policy: "fail_fast".into(),
            target: None,
        });
    }
    e.push(EventPayload::NodesRevived {
        from_node: "c".into(),
        nodes: ["c", "c2", "j", "done"].map(String::from).to_vec(),
    });
    e
}

fn fold_of(payloads: &[EventPayload]) -> RunState {
    let events: Vec<Event> = payloads
        .iter()
        .enumerate()
        .map(|(i, p)| ev(i as u64, p.clone()))
        .collect();
    RunState::fold(&events)
}

#[test]
fn nodes_revived_keeps_history_and_changes_only_routing() {
    let all = revival_journal();
    let without: Vec<EventPayload> = all[..all.len() - 1].to_vec();
    let s = fold_of(&all);
    let old = fold_of(&without);
    // The history every surface reports is untouched.
    assert_eq!(s.nodes, old.nodes);
    assert_eq!(s.outputs, old.outputs);
    assert_eq!(s.edge_counts, old.edge_counts);
    assert_eq!(s.finished_counts, old.finished_counts);
    // Routing reads the revived nodes as not yet run.
    assert_eq!(s.nodes.get("j"), Some(&NodeStatus::Succeeded));
    assert_eq!(s.current_status("j"), NodeStatus::Pending);
    assert_eq!(s.current_status("c"), NodeStatus::Pending);
    assert_eq!(s.current_status("b"), NodeStatus::Succeeded);
    // Hops and marks are dropped for revived nodes only.
    for hop in [("c", "c2"), ("c2", "j"), ("j", "done")] {
        assert!(old.journaled_hops.contains(&(hop.0.into(), hop.1.into())));
        assert!(!s.journaled_hops.contains(&(hop.0.into(), hop.1.into())));
    }
    assert!(s.journaled_hops.contains(&("b".into(), "j".into())));
    assert!(old.route_suppressed.contains("c") && !s.route_suppressed.contains("c"));
    assert!(s.route_suppressed.contains("b"));
    assert!(old.refused_joins.contains("j") && !s.refused_joins.contains("j"));
    assert_eq!(
        s.revival_starts.iter().collect::<Vec<_>>(),
        [&"c".to_string()]
    );
}

#[test]
fn a_revived_node_leaves_the_revival_when_it_starts_or_finishes_again() {
    let mut all = revival_journal();
    all.push(EventPayload::NodeStarted {
        node: "c".into(),
        attempt: 1,
    });
    let s = fold_of(&all);
    assert!(!s.revived.contains("c") && s.revival_starts.is_empty());
    assert_eq!(s.current_status("c"), NodeStatus::Running);
    assert!(s.revived.contains("j"));
    // A finish node without a prompt journals only its finish.
    all.push(EventPayload::NodeFinished {
        node: "done".into(),
        status: "succeeded".into(),
        attempt: 1,
        output: String::new(),
        artifacts: Vec::new(),
    });
    assert!(!fold_of(&all).revived.contains("done"));
}

/// A later revival covering an earlier owed re-run drops it from the owed
/// set; one that does not cover it keeps it (round 4 item 3).
#[test]
fn a_covering_revival_drops_the_earlier_owed_rerun() {
    let mut all = revival_journal();
    all.push(EventPayload::NodesRevived {
        from_node: "x".into(),
        nodes: ["x", "y"].map(String::from).to_vec(),
    });
    let s = fold_of(&all);
    assert_eq!(s.revival_starts.len(), 2, "{:?}", s.revival_starts);
    all.push(EventPayload::NodesRevived {
        from_node: "b".into(),
        nodes: ["b", "c", "x", "j"].map(String::from).to_vec(),
    });
    let s = fold_of(&all);
    assert_eq!(
        s.revival_starts.iter().collect::<Vec<_>>(),
        [&"b".to_string()]
    );
}
