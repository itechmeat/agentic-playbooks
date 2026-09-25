//! A failed node whose only way on is an unconditional edge (issue #106).
//!
//! An unconditional edge carries the run forward whatever the node's status,
//! so a failure that nothing inspects used to reach a `finish outcome:
//! success` node and be reported as a succeeded run. The run's verdict must
//! stay honest: a failure no route handled makes the run failed, naming the
//! node. A failure some route DID look at - a condition on the failed node's
//! status, as in the common `lint -> check -> fix` loop - is handled and the
//! declared outcome stands.

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::path::Path;

fn write_pb(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Runs `id` and returns its status and the run errors it journaled.
fn run_it(root: &Path, id: &str) -> (RunStatus, Vec<(Option<String>, String)>) {
    let res = run(root, id, None, RunOptions::default()).expect("the run completes");
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    let errors = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunError { node, reason } => Some((node.clone(), reason.clone())),
            _ => None,
        })
        .collect();
    (res.outcome, errors)
}

const SCRIPT_FAILS: &str = r#"schema: 2
id: scriptfail
name: s
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: build, type: script, script: "scripts/build.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: build }
  - { from: build, to: done }
"#;

const PARENT: &str = r#"schema: 2
id: parent
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: delegate, type: playbook, playbook: child }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: delegate }
  - { from: delegate, to: done }
"#;

const CHILD_FAILS: &str = r#"schema: 2
id: child
name: c
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: end, type: finish, outcome: failure }
edges:
  - { from: start, to: end }
"#;

/// The failure goes on unconditionally, but the next node routes on it.
const CHECKED: &str = r#"schema: 2
id: checked
name: k
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: build, type: script, script: "scripts/build.sh", runner: sh }
  - { id: check, type: condition }
  - { id: report, type: script, script: "scripts/report.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: build }
  - { from: build, to: check }
  - { from: check, to: done, condition: { type: node_status, node: build, equals: success } }
  - { from: check, to: report, condition: { type: node_status, node: build, equals: failure } }
  - { from: report, to: done }
"#;

#[test]
fn a_failed_script_carried_on_by_an_unconditional_edge_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(
        dir.path(),
        "scriptfail",
        SCRIPT_FAILS,
        &[("build.sh", "exit 1\n")],
    );
    let (status, errors) = run_it(dir.path(), "scriptfail");
    assert_eq!(status, RunStatus::Failed, "errors: {errors:?}");
    assert!(
        errors.iter().any(|(n, _)| n.as_deref() == Some("build")),
        "the run error names the failed node: {errors:?}"
    );
}

#[test]
fn a_failed_child_playbook_carried_on_by_an_unconditional_edge_fails_the_parent() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT, &[]);
    write_pb(dir.path(), "child", CHILD_FAILS, &[]);
    let (status, errors) = run_it(dir.path(), "parent");
    assert_eq!(status, RunStatus::Failed, "errors: {errors:?}");
    assert!(
        errors.iter().any(|(n, _)| n.as_deref() == Some("delegate")),
        "the run error names the failed node: {errors:?}"
    );
}

#[test]
fn a_failure_a_later_condition_routes_on_is_handled() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(
        dir.path(),
        "checked",
        CHECKED,
        &[("build.sh", "exit 1\n"), ("report.sh", "echo reported\n")],
    );
    let (status, errors) = run_it(dir.path(), "checked");
    assert_eq!(status, RunStatus::Succeeded, "errors: {errors:?}");
    assert!(errors.is_empty(), "{errors:?}");
}
