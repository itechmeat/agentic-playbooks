use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};
use std::fs;
use std::path::Path;

fn write_pb(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

const PARENT: &str = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child, instruction: \"child input\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";
// Child: prompt renders the run instruction, finish WITHOUT a prompt -> empty answer.
const CHILD: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{run.instruction}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n";

#[test]
fn parent_runs_child_and_records_child_run_started() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD);

    let res = run(dir.path(), "parent", None, RunOptions::default()).expect("parent runs");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let started: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { node_id, run_id } if node_id == "c" => {
                Some(run_id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 1, "one child run started for node c");

    // The child run persisted with parent_run set, and it reached a terminal state.
    let child_dir = dir.path().join(".apb/runs").join(started[0]);
    let child_cfg = apb_engine::run_config::read_run_config(&child_dir).unwrap();
    assert_eq!(child_cfg.parent_run.as_deref(), Some(res.run_id.as_str()));
    assert_eq!(child_cfg.instruction.as_deref(), Some("child input"));
    let child_state = RunState::fold(&read_all(&child_dir).unwrap());
    assert_eq!(child_state.run_status, RunStatus::Succeeded);

    // The parent node `c` succeeded (empty answer: child finish has no prompt).
    let parent_state = RunState::fold(&events);
    assert_eq!(parent_state.outputs.get("c").map(|s| s.as_str()), Some(""));
}

#[test]
fn gated_run_with_no_pin_for_node_fails_closed() {
    // Fail-closed pins (review I4): a gated run (expected_children is Some) that
    // carries NO pin for a playbook node must FAIL that node, not silently
    // live-resolve unverified content. Some(empty map) proves the case.
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD);

    let opts = RunOptions {
        expected_children: Some(std::collections::BTreeMap::new()),
        ..RunOptions::default()
    };
    let res = run(dir.path(), "parent", None, opts).expect("run completes");

    // The security property: node `c` FAILED with the no-pin diagnostic and NO
    // child run was ever started (the engine refused to live-resolve unverified
    // content under a gated run). Autonomous mode still routes past the failed
    // node to the success finish, so the run outcome itself is not the signal.
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let diag = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node,
                status,
                output,
                ..
            } if node == "c" && status == "failed" => Some(output.clone()),
            _ => None,
        })
        .expect("node c finished failed");
    assert!(
        diag.contains("no pin") && diag.contains('c'),
        "diagnostic names the node and the missing pin: {diag}"
    );
    let child_started = events.iter().any(
        |e| matches!(&e.payload, EventPayload::ChildRunStarted { node_id, .. } if node_id == "c"),
    );
    assert!(
        !child_started,
        "no child run may start when the pin is missing"
    );
}
