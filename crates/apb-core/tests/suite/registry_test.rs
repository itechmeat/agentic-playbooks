use apb_core::registry::{Registry, RegistryError, init_project};
use std::fs;
use std::path::Path;

const VALID: &str = include_str!("../fixtures/valid.yaml");

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    fs::write(root.join(".apb/playbooks/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join(".apb/playbooks/implement-task/layouts")).unwrap();
    fs::write(
        root.join(".apb/playbooks/implement-task/layouts/1.0.0.yaml"),
        "nodes:\n  - { id: plan, x: 10, y: 20 }\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".apb/profiles/architect")).unwrap();
}

#[test]
fn lists_playbooks_with_versions() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    let list = reg.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "implement-task");
    assert_eq!(list[0].current, "1.0.0");
    assert_eq!(list[0].versions, vec!["1.0.0"]);
    assert_eq!(list[0].name, "Implement Task");
}

#[test]
fn loads_current_version_with_layout() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    let loaded = reg.load("implement-task", None).unwrap();
    assert_eq!(loaded.version, "1.0.0");
    assert_eq!(loaded.playbook.id, "implement-task");
    let layout = loaded.layout.expect("layout must load");
    assert_eq!(layout["nodes"][0]["id"], "plan");
    assert_eq!(reg.profiles(), vec!["architect".to_string()]);
}

#[test]
fn version_mismatch_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    let patched = VALID.replace("version: 1.0.0", "version: 9.9.9");
    fs::write(vdir.join("playbook.yaml"), patched).unwrap();
    let reg = Registry::open(dir.path()).unwrap();
    match reg.load("implement-task", None) {
        Err(RegistryError::VersionMismatch { file, dir }) => {
            assert_eq!(file, "9.9.9");
            assert_eq!(dir, "1.0.0");
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

#[test]
fn unknown_playbook_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    assert!(matches!(
        reg.load("ghost", None),
        Err(RegistryError::NotFound(_))
    ));
}

#[test]
fn relative_traversal_version_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    let result = reg.load("implement-task", Some("../../etc"));
    assert!(
        matches!(result, Err(RegistryError::NotFound(_))),
        "expected NotFound, got {result:?}"
    );
}

#[test]
fn absolute_path_version_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    let result = reg.load("implement-task", Some("/etc"));
    assert!(
        matches!(result, Err(RegistryError::NotFound(_))),
        "expected NotFound, got {result:?}"
    );
}

#[test]
fn legitimate_version_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    let loaded = reg.load("implement-task", Some("1.0.0")).unwrap();
    assert_eq!(loaded.version, "1.0.0");
}
