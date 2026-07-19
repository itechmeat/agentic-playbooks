use std::fs;
use std::path::Path;

use apb_core::registry::Registry;
use apb_mcp::tools::{ToolError, playbook_create, playbook_delete, playbook_get, playbook_update};

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
fn playbook_create_new_then_load() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    fs::create_dir_all(dir.path().join(".apb/profiles/architect")).unwrap();

    let yaml = VALID.replace("id: implement-task", "id: brand-new");
    let v = playbook_create(dir.path(), "brand-new", &yaml).unwrap();
    assert_eq!(v["id"], "brand-new");
    assert_eq!(v["version"], "1.0.0");

    let loaded = playbook_get(dir.path(), "brand-new", None).unwrap();
    assert_eq!(loaded["version"], "1.0.0");
    assert_eq!(loaded["playbook"]["id"], "brand-new");
}

#[test]
fn playbook_update_creates_minor_version() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let modified = VALID.replace("name: Implement Task", "name: Implement Task v2");
    let v = playbook_update(dir.path(), "implement-task", &modified).unwrap();
    assert_eq!(v["id"], "implement-task");
    assert_eq!(v["version"], "1.1.0");

    let loaded = playbook_get(dir.path(), "implement-task", None).unwrap();
    assert_eq!(loaded["version"], "1.1.0");
    assert_eq!(loaded["playbook"]["name"], "Implement Task v2");
}

#[test]
fn playbook_update_missing_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err = playbook_update(dir.path(), "ghost", VALID).unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)), "got {err:?}");
}

#[test]
fn playbook_delete_moves_to_trash() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let v = playbook_delete(dir.path(), "implement-task").unwrap();
    let trashed = v["trashed"].as_str().expect("trashed path");
    assert!(trashed.contains(".apb/trash/implement-task-"));
    assert!(Path::new(trashed).is_dir());

    let err = playbook_get(dir.path(), "implement-task", None).unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)), "got {err:?}");
}

#[test]
fn playbook_update_invalid_playbook_renders_structured_validation_message() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let invalid = VALID.replace("{{params.task}}", "{{outputs.plan}}");
    let err = playbook_update(dir.path(), "implement-task", &invalid).unwrap_err();
    match err {
        ToolError::Engine(msg) => {
            assert!(
                msg.starts_with("validation failed:"),
                "expected `validation failed:` prefix, got: {msg}"
            );
            assert!(
                msg.lines()
                    .any(|l| l.starts_with("- V13 error (node `plan`):")),
                "expected a `- V13 error (node `plan`):` line, got: {msg}"
            );
        }
        other => panic!("expected Engine, got {other:?}"),
    }
}

#[test]
fn playbook_create_invalid_yaml_is_engine_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let invalid = VALID.replace(
        "  - id: plan",
        "  - id: start2\n    type: start\n    title: Second start\n  - id: plan",
    );
    let err = playbook_create(dir.path(), "implement-task", &invalid).unwrap_err();
    match err {
        ToolError::Engine(msg) => {
            assert!(msg.contains("V03") || msg.to_lowercase().contains("valid"))
        }
        other => panic!("expected Engine, got {other:?}"),
    }

    let reg = Registry::open(dir.path()).unwrap();
    let loaded = reg.load("implement-task", None).unwrap();
    assert_eq!(loaded.version, "1.0.0");
}
