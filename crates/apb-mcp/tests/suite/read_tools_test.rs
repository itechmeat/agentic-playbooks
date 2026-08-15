use apb_mcp::tools::{DetailMode, playbook_get, playbook_list, playbook_validate};
use std::fs;
use std::path::Path;

const VALID: &str = include_str!("../../../apb-core/tests/fixtures/valid.yaml");

fn seed(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    fs::write(root.join(".apb/playbooks/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join(".apb/profiles/architect")).unwrap();
}

#[test]
fn list_returns_playbook() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = playbook_list(dir.path()).unwrap();
    assert_eq!(v[0]["id"], "implement-task");
    assert_eq!(v[0]["current"], "1.0.0");
}

#[test]
fn get_returns_yaml_and_model() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = playbook_get(dir.path(), "implement-task", None, DetailMode::Full).unwrap();
    assert_eq!(v["version"], "1.0.0");
    assert_eq!(v["playbook"]["nodes"][0]["type"], "start");
    assert!(v["yaml"].as_str().unwrap().contains("implement-task"));
}

#[test]
fn validate_reports_ok() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = playbook_validate(dir.path(), "implement-task").unwrap();
    assert_eq!(v["valid"], true);
    assert!(v["issues"].as_array().unwrap().is_empty());
}

#[test]
fn get_summary_includes_goal_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    let yaml = format!(
        "goal:\n  statement: the task is implemented and verified\n  criteria:\n    - description: tests pass\n      check: {{ type: marker, marker: DONE }}\n{VALID}"
    );
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join(".apb/profiles/architect")).unwrap();

    let v = playbook_get(root, "implement-task", None, DetailMode::Summary).unwrap();
    assert_eq!(
        v["goal"]["statement"].as_str(),
        Some("the task is implemented and verified")
    );
    let criteria = v["goal"]["criteria"].as_array().unwrap();
    assert_eq!(criteria[0]["description"].as_str(), Some("tests pass"));
    assert_eq!(criteria[0]["check"]["type"].as_str(), Some("marker"));
    assert_eq!(criteria[0]["check"]["marker"].as_str(), Some("DONE"));
}

#[test]
fn get_summary_omits_goal_key_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = playbook_get(dir.path(), "implement-task", None, DetailMode::Summary).unwrap();
    assert!(
        v.get("goal").is_none(),
        "goal-less playbook must omit the goal key from the summary: {v}"
    );
}

#[test]
fn get_unknown_is_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = playbook_get(dir.path(), "ghost", None, DetailMode::Full).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}
