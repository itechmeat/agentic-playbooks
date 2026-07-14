use apb_mcp::tools::{playbook_run, run_events, run_status, runs_list};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
}

#[test]
fn run_then_inspect() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run = playbook_run(dir.path(), "noagent", None, params, None, None, None).unwrap();
    assert_eq!(run["outcome"], "succeeded");
    let run_id = run["run_id"].as_str().unwrap().to_string();

    let listed = runs_list(dir.path()).unwrap();
    assert_eq!(listed[0]["run_id"], run_id.as_str());

    let status = run_status(dir.path(), &run_id).unwrap();
    assert_eq!(status["run_status"], "succeeded");
    assert_eq!(status["nodes"]["note"], "succeeded");

    let ev = run_events(dir.path(), &run_id, None).unwrap();
    assert!(ev["events"].as_array().unwrap().len() >= 3);
    // pagination: from_seq cuts off earlier events
    let ev2 = run_events(dir.path(), &run_id, Some(2)).unwrap();
    let first_seq = ev2["events"][0]["seq"].as_u64().unwrap();
    assert!(first_seq >= 2);
}

#[test]
fn status_unknown_run_is_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    assert!(run_status(dir.path(), "ghost-1").is_err());
}

#[test]
fn resume_unknown_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = apb_mcp::tools::run_resume(dir.path(), "ghost-1", None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn status_traversal_run_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    for bad in ["../../etc", "/etc", "..", "a/b"] {
        let err = run_status(dir.path(), bad).unwrap_err();
        assert!(
            matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
            "id {bad:?}: expected NotFound, got {err:?}"
        );
    }
}

#[test]
fn events_traversal_run_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = run_events(dir.path(), "../../etc", None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}
