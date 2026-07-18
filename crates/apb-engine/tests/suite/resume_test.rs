use apb_core::registry::init_project;
use apb_engine::error::EngineError;
use apb_engine::scheduler::{RunOptions, list_runs, resume, run};
use apb_engine::state::RunStatus;
use std::fs;

const PLAYBOOK: &str = r#"
schema: 1
id: lin
name: Lin
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "x" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/lin/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/lin/current"), "1.0.0").unwrap();
}

#[test]
fn lists_runs_after_a_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    let runs = list_runs(dir.path()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, res.run_id);
    assert_eq!(runs[0].playbook, "lin");
    assert_eq!(runs[0].status, "succeeded");
}

#[test]
fn resume_from_node_reaches_finish() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    // a repeat pass from node `a` finishes with success (the version snapshot lives inside the run)
    let again = resume(dir.path(), &res.run_id, Some("a")).unwrap();
    assert_eq!(again.run_id, res.run_id);
    assert_eq!(again.outcome, RunStatus::Succeeded);
}

#[test]
fn resume_traversal_run_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    // The path-traversal check must fire before the check for the run
    // directory's existence, so a valid run is not required here.
    let err = resume(dir.path(), "../../etc", None).unwrap_err();
    assert!(
        matches!(err, EngineError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}
