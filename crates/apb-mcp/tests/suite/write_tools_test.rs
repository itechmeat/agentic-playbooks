use std::fs;
use std::path::Path;

use apb_core::registry::Registry;
use apb_mcp::tools::{
    DetailMode, ToolError, playbook_create, playbook_delete, playbook_get, playbook_trash_list,
    playbook_trash_restore, playbook_update,
};

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
    let _cfg = crate::common::config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    fs::create_dir_all(dir.path().join(".apb/profiles/architect")).unwrap();

    let yaml = VALID.replace("id: implement-task", "id: brand-new");
    let v = playbook_create(dir.path(), "brand-new", &yaml).unwrap();
    assert_eq!(v["id"], "brand-new");
    assert_eq!(v["version"], "1.0.0");

    let loaded = playbook_get(dir.path(), "brand-new", None, DetailMode::Full).unwrap();
    assert_eq!(loaded["version"], "1.0.0");
    assert_eq!(loaded["playbook"]["id"], "brand-new");
}

#[test]
fn playbook_update_creates_minor_version() {
    let _cfg = crate::common::config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let modified = VALID.replace("name: Implement Task", "name: Implement Task v2");
    let v = playbook_update(dir.path(), "implement-task", &modified).unwrap();
    assert_eq!(v["id"], "implement-task");
    assert_eq!(v["version"], "1.1.0");

    let loaded = playbook_get(dir.path(), "implement-task", None, DetailMode::Full).unwrap();
    assert_eq!(loaded["version"], "1.1.0");
    assert_eq!(loaded["playbook"]["name"], "Implement Task v2");
}

#[test]
fn playbook_update_missing_is_not_found() {
    let _cfg = crate::common::config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err = playbook_update(dir.path(), "ghost", VALID).unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)), "got {err:?}");
}

/// Delete, the trash listing and restore over the MCP tool layer, including
/// the conflict a restore meets when the id was taken again: it must reach
/// the caller as a conflict, not as a generic engine failure.
#[test]
fn playbook_delete_trash_list_and_restore() {
    let _cfg = crate::common::config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let v = playbook_delete(dir.path(), "implement-task").unwrap();
    let trashed = v["trashed"].as_str().expect("trashed path");
    assert!(Path::new(trashed).is_dir());
    let err = playbook_get(dir.path(), "implement-task", None, DetailMode::Full).unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)), "got {err:?}");

    let listed = playbook_trash_list(dir.path()).unwrap();
    assert_eq!(listed[0]["id"], "implement-task");
    assert_eq!(listed[0]["conflict"], false);

    let restored = playbook_trash_restore(dir.path(), "implement-task").unwrap();
    assert_eq!(restored["id"], "implement-task");
    assert!(playbook_get(dir.path(), "implement-task", None, DetailMode::Full).is_ok());
    assert_eq!(
        playbook_trash_list(dir.path()).unwrap(),
        serde_json::json!([])
    );

    playbook_delete(dir.path(), "implement-task").unwrap();
    playbook_create(dir.path(), "implement-task", VALID).unwrap();
    let err = playbook_trash_restore(dir.path(), "implement-task").unwrap_err();
    assert!(matches!(err, ToolError::Conflict(_)), "got {err:?}");
}

#[test]
fn playbook_update_invalid_playbook_renders_structured_validation_message() {
    let _cfg = crate::common::config_sandbox();
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
    let _cfg = crate::common::config_sandbox();
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

/// MCP `trust_list` / `trust_revoke`: the listing shows the approvals, a
/// revoke by id removes every approval of that id and returns them, and the
/// listing no longer shows them.
#[test]
fn trust_list_and_revoke_by_id() {
    use apb_core::trust::{Kind, OriginKind, TrustStore};
    let _cfg = crate::common::config_sandbox();
    let mut store = TrustStore::load();
    for (digest, id) in [
        ("sha256:d1", "demo"),
        ("sha256:d2", "demo"),
        ("sha256:k1", "keep"),
    ] {
        store
            .approve_kind(digest, id, Kind::Playbook, OriginKind::LocallyApproved)
            .unwrap();
    }

    let listed = apb_mcp::tools::trust_list(Some("playbook")).unwrap();
    assert_eq!(listed["approvals"].as_array().unwrap().len(), 3, "{listed}");
    let revoked = apb_mcp::tools::trust_revoke("demo", None).unwrap();
    assert_eq!(revoked["revoked"].as_array().unwrap().len(), 2, "{revoked}");
    let listed = apb_mcp::tools::trust_list(None).unwrap();
    let ids: Vec<&str> = listed["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_str())
        .collect();
    assert_eq!(ids, ["keep"]);
    assert!(apb_mcp::tools::trust_list(Some("nonsense")).is_err());
}
