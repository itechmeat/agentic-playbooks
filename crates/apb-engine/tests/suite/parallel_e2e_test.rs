use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

// Diamond: start forks into a and b, they converge in join:all -> j -> finish.
const DIAMOND_ALL: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

// join:any - the first branch continues the flow, the second is cancelled.
const DIAMOND_ANY: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: any }
  - { from: b, to: j, join: any }
  - { from: j, to: done }
"#;

// A barrier-less diamond with branches of different length: `a` reaches the
// merge in one hop, `b` needs two. Without an implicit join `j` would run as
// soon as `a` arrived, while `b2` was still pending.
const IMPLICIT_DIAMOND: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: b2, type: prompt, prompt: "b2" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j }
  - { from: b, to: b2 }
  - { from: b2, to: j }
  - { from: j, to: done }
"#;

// An either-or conditional fork whose branches merge again: exactly one of
// `a`/`b` is ever selected, so the merge must not wait for the branch that was
// not taken.
const EITHER_OR: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: pick, type: condition }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: m, type: prompt, prompt: "m" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: pick }
  - { from: pick, to: a, condition: { type: node_status, node: pick, equals: success } }
  - { from: pick, to: b, condition: { type: node_status, node: pick, equals: failure } }
  - { from: a, to: m }
  - { from: b, to: m }
  - { from: m, to: done }
"#;

// A fork where one branch reaches finish - the run completes, the other does not execute.
const FORK_FINISH: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: finish, outcome: success }
  - { id: b, type: prompt, prompt: "b" }
  - { id: bdone, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: b, to: bdone }
"#;

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/par/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/par/current"), "1.0.0").unwrap();
}

fn started(events: &[apb_engine::event::Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .count()
}

/// Position of a node's `node_started` in the journal, for ordering assertions.
fn position_of_start(events: &[apb_engine::event::Event], node: &str) -> usize {
    events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .unwrap_or_else(|| panic!("node `{node}` never started"))
}

/// Position of a node's `node_finished` in the journal.
fn position_of_finish(events: &[apb_engine::event::Event], node: &str) -> usize {
    events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
        .unwrap_or_else(|| panic!("node `{node}` never finished"))
}

#[test]
fn join_all_runs_both_branches_then_join() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), DIAMOND_ALL);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "a"), 1, "branch a runs once");
    assert_eq!(started(&events, "b"), 1, "branch b runs once");
    assert_eq!(
        started(&events, "j"),
        1,
        "join runs once, after both branches"
    );
    // j starts after both branches have finished.
    let j_start = position_of_start(&events, "j");
    assert!(
        j_start > position_of_finish(&events, "a") && j_start > position_of_finish(&events, "b"),
        "join must start after both branches finish"
    );
}

#[test]
fn implicit_join_waits_for_the_slower_branch() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), IMPLICIT_DIAMOND);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "j"), 1, "the merge runs exactly once");
    let j_start = position_of_start(&events, "j");
    for branch in ["a", "b2"] {
        assert!(
            j_start > position_of_finish(&events, branch),
            "a barrier-less merge must wait for branch `{branch}`"
        );
    }
}

#[test]
fn either_or_merge_reaches_finish_without_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), EITHER_OR);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "a"), 1, "the selected branch runs");
    assert_eq!(started(&events, "b"), 0, "the other branch is never taken");
    assert_eq!(
        started(&events, "m"),
        1,
        "the merge runs instead of waiting for a branch that can never arrive"
    );
}

#[test]
fn join_any_cancels_sibling() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), DIAMOND_ANY);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    // One branch ran, the join passed; the other branch is cancelled,
    // rather than executed as a normal node.
    assert_eq!(started(&events, "j"), 1, "join runs once");
    let cancelled = events.iter().any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, status, .. } if (node == "a" || node == "b") && status == "cancelled"));
    assert!(cancelled, "sibling branch must be cancelled on join:any");
}

#[test]
fn finish_in_one_branch_cancels_the_other() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), FORK_FINISH);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    // Branch a (finish) is processed first (the frontier order is deterministic) and
    // completes the run; branch b does not execute.
    assert_eq!(
        started(&events, "b"),
        0,
        "sibling branch must not run once a branch finishes"
    );
    assert!(events.iter().any(
        |e| matches!(&e.payload, EventPayload::RunFinished { outcome } if outcome == "succeeded")
    ));
}
