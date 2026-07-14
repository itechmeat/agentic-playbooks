use apb_mcp::tools::{playbook_get, playbook_list, playbook_validate};
use std::fs;
use std::path::Path;

const VALID: &str = include_str!("../../apb-core/tests/fixtures/valid.yaml");

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
    let v = playbook_get(dir.path(), "implement-task", None).unwrap();
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
fn get_unknown_is_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = playbook_get(dir.path(), "ghost", None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}
