use apb_core::registry::init_project;
use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};

// A pipeline with an infinite loop: start -> tick -> check(max_loops: 2) -> tick -> ...
// The only edge out of check is unconditional (no condition and no fallback), so the
// finish node in this loop is unreachable at all - that is only a V08 warning, not an
// error, so validation must pass. We check that the engine itself breaks the loop via
// max_loops, without waiting for the hard max_steps limit.
const LOOPWF: &str = r#"
schema: 1
id: loopwf
name: Loop Playbook
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: tick, type: prompt, prompt: "tick" }
  - { id: check, type: condition, max_loops: 2 }
edges:
  - { from: start, to: tick }
  - { from: tick, to: check }
  - { from: check, to: tick }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/loopwf/1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), LOOPWF).unwrap();
    std::fs::write(root.join(".apb/playbooks/loopwf/current"), "1.0.0").unwrap();
}

// A condition node with max_loops:2 that ALSO declares a fallback edge: the
// unconditional `check -> tick` edge is what routes every ordinary pass (it
// wins over the fallback in `selected_edges`), and the fallback `check ->
// done` edge is what the budget-exhausted branch in the scheduler jumps to
// directly, bypassing `advance_frontier` entirely.
const LOOPWF_FALLBACK: &str = r#"
schema: 1
id: loopwf_fb
name: Loop Playbook With Fallback
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: tick, type: prompt, prompt: "tick" }
  - { id: check, type: condition, max_loops: 2 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: tick }
  - { from: tick, to: check }
  - { from: check, to: tick }
  - { from: check, to: done, fallback: true }
"#;

fn seed_fallback(root: &std::path::Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/loopwf_fb/1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), LOOPWF_FALLBACK).unwrap();
    std::fs::write(root.join(".apb/playbooks/loopwf_fb/current"), "1.0.0").unwrap();
}

#[test]
fn playbook_with_unreachable_finish_is_only_a_warning() {
    let playbook = Playbook::from_yaml(LOOPWF).unwrap();
    let ctx = ValidationContext {
        profiles: vec![],
        ..Default::default()
    };
    let report = validate(&playbook, &ctx);
    assert!(
        !report.issues.iter().any(|i| i.severity == Severity::Error),
        "expected no errors, got: {:?}",
        report.issues.iter().map(|i| &i.message).collect::<Vec<_>>()
    );
    assert!(
        report.issues.iter().any(|i| i.code == "V08"),
        "expected V08 warning about unreachable finish"
    );
}

#[test]
fn max_loops_exhausted_without_fallback_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "loopwf", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    // There must be no step-limit error - run() returned Ok(Failed), not Err.
    // events.jsonl does not contain any text about exhausting max_steps at all,
    // but for extra safety we also confirm this at the log level.
    assert!(
        !events
            .iter()
            .any(|e| format!("{:?}", e.payload).contains("exceeded"))
    );

    // max_loops: 2 -> executions 1 and 2 are allowed, the third exceeds the budget and breaks the loop.
    let starts = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "check"))
        .count();
    let finishes = events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "check"),
        )
        .count();
    assert_eq!(
        starts, 3,
        "check should start exactly 3 times (2 allowed + 1 that trips the limit)"
    );
    assert_eq!(
        finishes, 3,
        "check should finish exactly 3 times before the run fails"
    );

    assert!(events.iter().any(
        |e| matches!(&e.payload, EventPayload::RunFinished { outcome } if outcome == "failed")
    ));
}

/// (f) A condition node WITH a fallback edge, budget exhausted: the fallback
/// hop the scheduler takes directly (never through `advance_frontier`) must
/// still be journaled, and it must not spend a bounded edge's traversal
/// budget - it crossed no bounded edge at all.
#[test]
fn max_loops_fallback_hop_is_journaled_uncounted() {
    let dir = tempfile::tempdir().unwrap();
    seed_fallback(dir.path());
    let res = run(dir.path(), "loopwf_fb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    let fallback_hop = events.iter().find(|e| {
        matches!(
            &e.payload,
            EventPayload::EdgeTraversed { from, to, uncounted, .. }
                if from == "check" && to == "done" && *uncounted
        )
    });
    assert!(
        fallback_hop.is_some(),
        "the max_loops fallback hop must be journaled as uncounted, got: {events:?}"
    );

    let s = RunState::fold(&events);
    assert!(
        !s.edge_counts
            .contains_key(&("check".to_string(), "done".to_string())),
        "the fallback hop must not spend a bounded edge's traversal budget"
    );
}
