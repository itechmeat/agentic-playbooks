use apb_core::registry::init_project;
use apb_engine::control::{Control, read_control_after};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run, run_cancel};
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
// Child with a defaulted param echoed by a prompt node (review I6/R1-I2): the
// parent starts it with an empty params map, so the default must be filled.
const CHILD_PARAM: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nparams:\n  - { name: greeting, type: text, default: \"hello-default\" }\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{params.greeting}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n";
// Child whose finish outcome is failure -> maps the parent node to Failed.
const CHILD_FAIL: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: failure }\nedges:\n  - { from: s, to: f }\n";

/// Writes a run directory's `events.jsonl` from raw NDJSON lines (the on-disk
/// event format), and optionally a `playbook.yaml` snapshot. Mirrors how the
/// progress tests construct run dirs by hand, so a resume/abort path can be
/// exercised deterministically without driving an agent.
fn seed_run_dir(root: &Path, run_id: &str, playbook_yaml: Option<&str>, events_ndjson: &str) {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("events.jsonl"), events_ndjson).unwrap();
    if let Some(pb) = playbook_yaml {
        fs::write(run_dir.join("playbook.yaml"), pb).unwrap();
    }
}

fn child_run_id_for(events: &[apb_engine::event::Event], node: &str) -> Option<String> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::ChildRunStarted { node_id, run_id } if node_id == node => {
            Some(run_id.clone())
        }
        _ => None,
    })
}

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

#[test]
fn child_param_default_is_filled_for_child_run() {
    // Review I6/R1-I2: a sub-playbook child starts with an empty params map, so
    // a declared param's `default` must be filled during prepare. The child's
    // prompt node echoes the param, so both its persisted RunConfig and the
    // rendered output must carry the default.
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_PARAM);

    let res = run(dir.path(), "parent", None, RunOptions::default()).expect("parent runs");
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let child_id = child_run_id_for(&events, "c").expect("child run started");

    let child_dir = dir.path().join(".apb/runs").join(&child_id);
    let child_cfg = apb_engine::run_config::read_run_config(&child_dir).unwrap();
    assert_eq!(
        child_cfg.params.get("greeting").map(|s| s.as_str()),
        Some("hello-default"),
        "the child's persisted params carry the schema default"
    );

    // The child's prompt node `n` rendered the default (not an empty string).
    let child_events = read_all(&child_dir).unwrap();
    let n_out = child_events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "n" => Some(output.clone()),
            _ => None,
        })
        .expect("child node n finished");
    assert_eq!(
        n_out, "hello-default",
        "prompt rendered the defaulted param"
    );
}

#[test]
fn child_failure_maps_parent_node_to_failed() {
    // Review I10: a child whose finish outcome is `failure` maps the parent
    // playbook node to Failed, with a diagnostic naming the child run id.
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_FAIL);

    let res = run(dir.path(), "parent", None, RunOptions::default()).expect("parent runs");
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let child_id = child_run_id_for(&events, "c").expect("child run started");

    let (status, output) = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node,
                status,
                output,
                ..
            } if node == "c" => Some((status.clone(), output.clone())),
            _ => None,
        })
        .expect("node c finished");
    assert_eq!(status, "failed", "a failed child maps node c to failed");
    assert!(
        output.contains(&child_id),
        "diagnostic names the child run id: {output}"
    );
}

#[test]
fn resume_reattaches_to_the_same_nonterminal_child() {
    // Review I10: a parent interrupted while its child is non-terminal, on
    // resume, CONTINUES the same child run id - it does not start a second
    // child. Built by hand: the parent recorded ChildRunStarted for a child run
    // dir that is still Running (RunStarted + NodeStarted, no terminal event).
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    // Parent: Running, blocked at node c with a child already started.
    let parent_events = concat!(
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"parent\",\"version\":\"1.0.0\"}\n",
        "{\"seq\":1,\"ts\":0,\"type\":\"node_started\",\"node\":\"c\",\"attempt\":1}\n",
        "{\"seq\":2,\"ts\":0,\"type\":\"child_run_started\",\"node_id\":\"c\",\"run_id\":\"child-1\"}\n",
    );
    seed_run_dir(dir.path(), "parent-1", Some(PARENT), parent_events);

    // Child: non-terminal (Running), with a finished start node so `last_node`
    // is set (fold sets last_node only on NodeFinished), letting a resume
    // continue it to its finish node without a terminal event.
    let child_events = concat!(
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"child\",\"version\":\"1.0.0\"}\n",
        "{\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"s\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n",
    );
    seed_run_dir(dir.path(), "child-1", Some(CHILD), child_events);

    let res = resume(dir.path(), "parent-1", Some("c")).expect("parent resumes");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let events = read_all(&dir.path().join(".apb/runs").join("parent-1")).unwrap();
    let started: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { node_id, run_id } if node_id == "c" => {
                Some(run_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        started,
        vec!["child-1".to_string()],
        "resume reattaches to child-1, no second ChildRunStarted with a new id"
    );
}

#[test]
fn resume_after_terminal_child_starts_a_new_child() {
    // Review I10 (retry-new-child): after a child has reached a terminal state,
    // re-executing the parent's playbook node (here via `resume`, the path the
    // engine supports) does NOT reattach - the `run_is_terminal` guard is
    // satisfied, so a fresh child run with a DIFFERENT run id is started.
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD);

    // Parent: Running, blocked at node c; the recorded child already FAILED.
    let parent_events = concat!(
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"parent\",\"version\":\"1.0.0\"}\n",
        "{\"seq\":1,\"ts\":0,\"type\":\"node_started\",\"node\":\"c\",\"attempt\":1}\n",
        "{\"seq\":2,\"ts\":0,\"type\":\"child_run_started\",\"node_id\":\"c\",\"run_id\":\"oldchild-1\"}\n",
    );
    seed_run_dir(dir.path(), "parent-1", Some(PARENT), parent_events);

    // The old child is terminal (Failed), so it must not be reattached.
    let old_child_events = concat!(
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"child\",\"version\":\"1.0.0\"}\n",
        "{\"seq\":1,\"ts\":0,\"type\":\"run_finished\",\"outcome\":\"failed\"}\n",
    );
    seed_run_dir(dir.path(), "oldchild-1", Some(CHILD), old_child_events);

    resume(dir.path(), "parent-1", Some("c")).expect("parent resumes");

    let events = read_all(&dir.path().join(".apb/runs").join("parent-1")).unwrap();
    let started: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { node_id, run_id } if node_id == "c" => {
                Some(run_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        started.len(),
        2,
        "the terminal child is not reattached; a new child starts"
    );
    assert_eq!(started[0], "oldchild-1");
    assert_ne!(
        started[1], "oldchild-1",
        "the retry starts a child with a fresh run id"
    );

    let old_cfg =
        apb_engine::run_config::read_run_config(&dir.path().join(".apb/runs").join("oldchild-1"))
            .unwrap();
    let new_cfg =
        apb_engine::run_config::read_run_config(&dir.path().join(".apb/runs").join(&started[1]))
            .unwrap();
    assert_eq!(old_cfg.superseded_by.as_deref(), Some(started[1].as_str()));
    assert_eq!(new_cfg.continued_from.as_deref(), Some("oldchild-1"));
}

#[test]
fn abort_propagates_to_a_running_child() {
    // Review I10 (abort propagation): a parent cancel posts Abort into a
    // non-terminal child's control.jsonl. Driven deterministically via the
    // public `run_cancel` (which wraps `abort_children`) on a hand-built
    // parent-with-running-child state, then the child control log is inspected.
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    let parent_events = concat!(
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"parent\",\"version\":\"1.0.0\"}\n",
        "{\"seq\":1,\"ts\":0,\"type\":\"node_started\",\"node\":\"c\",\"attempt\":1}\n",
        "{\"seq\":2,\"ts\":0,\"type\":\"child_run_started\",\"node_id\":\"c\",\"run_id\":\"child-1\"}\n",
    );
    seed_run_dir(dir.path(), "parent-1", None, parent_events);

    // A non-terminal (Running) child that the abort must reach.
    let child_events = "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"child\",\"version\":\"1.0.0\"}\n";
    seed_run_dir(dir.path(), "child-1", None, child_events);

    run_cancel(dir.path(), "parent-1").expect("cancel posts aborts");

    let child_dir = dir.path().join(".apb/runs").join("child-1");
    let entries = read_control_after(&child_dir, None).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| matches!(&e.cmd, Control::Abort { .. })),
        "the running child received an Abort control entry"
    );

    // And the parent itself received its own Abort (run_cancel's primary effect).
    let parent_dir = dir.path().join(".apb/runs").join("parent-1");
    let parent_entries = read_control_after(&parent_dir, None).unwrap();
    assert!(
        parent_entries
            .iter()
            .any(|e| matches!(&e.cmd, Control::Abort { .. })),
        "the parent received an Abort control entry"
    );
}
