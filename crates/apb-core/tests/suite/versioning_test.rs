use std::fs;
use std::path::Path;

use apb_core::registry::{Registry, init_project};
use apb_core::versioning::{
    VersioningError, create_patch_version, create_version, delete_playbook, list_trash,
    next_minor_version, next_patch_version, read_provenance, restore_playbook, save_layout,
    version_diff,
};

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
fn next_minor_version_computes_and_skips_collisions() {
    assert_eq!(
        next_minor_version("1.3.42", &["1.3.42".into(), "1.2.0".into()]),
        "1.4.0"
    );
    assert_eq!(
        next_minor_version("1.0.0", &["1.0.0".into(), "1.1.0".into()]),
        "1.2.0"
    );
    assert_eq!(next_minor_version("not-semver", &[]), "1.0.0");
}

#[test]
fn next_patch_version_computes_and_skips_collisions() {
    assert_eq!(next_patch_version("1.2.0", &["1.2.0".into()]), "1.2.1");
    assert_eq!(
        next_patch_version("1.2.0", &["1.2.0".into(), "1.2.1".into(), "1.2.2".into()]),
        "1.2.3"
    );
}

#[test]
fn create_patch_version_keeps_current_and_records_supervisor_provenance() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let base_scripts = dir
        .path()
        .join(".apb/playbooks/implement-task/1.0.0/scripts/retry.sh");
    fs::write(&base_scripts, "#!/bin/sh\necho retry\n").unwrap();

    let patched = VALID.replace("name: Implement Task", "name: Patched Task");
    let version = create_patch_version(
        dir.path(),
        "implement-task",
        "1.0.0",
        &patched,
        "run-42",
        "improvement",
    )
    .unwrap();

    assert_eq!(version, "1.0.1");
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/implement-task/current"))
            .unwrap()
            .trim(),
        "1.0.0"
    );
    assert!(
        dir.path()
            .join(".apb/playbooks/implement-task/1.0.1/playbook.yaml")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(
            dir.path()
                .join(".apb/playbooks/implement-task/1.0.1/scripts/retry.sh"),
        )
        .unwrap(),
        "#!/bin/sh\necho retry\n"
    );
    assert!(
        dir.path()
            .join(".apb/playbooks/implement-task/layouts/1.0.1.yaml")
            .is_file()
    );

    let provenance = read_provenance(dir.path(), "implement-task", &version)
        .unwrap()
        .unwrap();
    assert_eq!(provenance.created_by, "supervisor");
    assert_eq!(provenance.run_id.as_deref(), Some("run-42"));
    assert_eq!(provenance.classification.as_deref(), Some("improvement"));
    assert!(!provenance.promoted);
}

#[test]
fn create_patch_version_rejects_invalid_input() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err = create_patch_version(
        dir.path(),
        "implement-task",
        "1.0.0",
        VALID,
        "run-42",
        "other",
    )
    .unwrap_err();
    assert!(matches!(err, VersioningError::Validation(_)));

    let invalid = VALID.replace(
        "  - id: plan",
        "  - id: start2\n    type: start\n    title: Second start\n  - id: plan",
    );
    let err = create_patch_version(
        dir.path(),
        "implement-task",
        "1.0.0",
        &invalid,
        "run-42",
        "improvement",
    )
    .unwrap_err();
    assert!(matches!(err, VersioningError::Validation(_)));

    let err = create_patch_version(
        dir.path(),
        "implement-task",
        "../evil",
        VALID,
        "run-42",
        "improvement",
    )
    .unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
}

#[test]
fn create_version_for_existing_playbook() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let base_yaml = fs::read_to_string(
        dir.path()
            .join(".apb/playbooks/implement-task/1.0.0/playbook.yaml"),
    )
    .unwrap();

    let modified = VALID
        .replace("name: Implement Task", "name: Implement Task v2")
        .replace(
            "Write a plan: {{params.task}}",
            "Write a detailed plan: {{params.task}}",
        );

    let version = create_version(dir.path(), "implement-task", &modified, None, true).unwrap();
    assert_eq!(version, "1.1.0");

    let new_yaml_path = dir
        .path()
        .join(".apb/playbooks/implement-task/1.1.0/playbook.yaml");
    assert!(new_yaml_path.is_file());
    let new_yaml = fs::read_to_string(&new_yaml_path).unwrap();
    assert!(new_yaml.contains("version: 1.1.0"));
    assert!(new_yaml.contains("name: Implement Task v2"));

    let current =
        fs::read_to_string(dir.path().join(".apb/playbooks/implement-task/current")).unwrap();
    assert_eq!(current.trim(), "1.1.0");

    let layout = fs::read_to_string(
        dir.path()
            .join(".apb/playbooks/implement-task/layouts/1.1.0.yaml"),
    )
    .unwrap();
    assert!(layout.contains("id: plan"));

    let base_after = fs::read_to_string(
        dir.path()
            .join(".apb/playbooks/implement-task/1.0.0/playbook.yaml"),
    )
    .unwrap();
    assert_eq!(
        base_after, base_yaml,
        "base version folder must stay immutable"
    );
}

#[test]
fn create_version_rejects_invalid_playbook() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let invalid = VALID.replace(
        "  - id: plan",
        "  - id: start2\n    type: start\n    title: Second start\n  - id: plan",
    );

    let err = create_version(dir.path(), "implement-task", &invalid, None, true).unwrap_err();
    match &err {
        VersioningError::Validation(codes) => assert!(codes.contains(&"V03".to_string())),
        other => panic!("expected Validation, got {other:?}"),
    }

    assert!(
        !dir.path()
            .join(".apb/playbooks/implement-task/1.1.0")
            .exists()
    );

    let current =
        fs::read_to_string(dir.path().join(".apb/playbooks/implement-task/current")).unwrap();
    assert_eq!(current.trim(), "1.0.0");
}

#[test]
fn create_version_for_new_playbook() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    fs::create_dir_all(dir.path().join(".apb/profiles/architect")).unwrap();

    let new_yaml = VALID.replace("id: implement-task", "id: brand-new");
    let version = create_version(dir.path(), "brand-new", &new_yaml, None, true).unwrap();
    assert_eq!(version, "1.0.0");

    let current = fs::read_to_string(dir.path().join(".apb/playbooks/brand-new/current")).unwrap();
    assert_eq!(current.trim(), "1.0.0");

    let reg = Registry::open(dir.path()).unwrap();
    let loaded = reg.load("brand-new", None).unwrap();
    assert_eq!(loaded.version, "1.0.0");
    assert_eq!(loaded.playbook.id, "brand-new");
}

#[test]
fn create_version_rejects_unsafe_id() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err = create_version(dir.path(), "../evil", VALID, None, true).unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
}

#[test]
fn next_minor_version_handles_minor_overflow() {
    let max = u32::MAX;
    let base = format!("1.{max}.0");
    let existing = vec![format!("1.{max}.0")];
    assert_eq!(next_minor_version(&base, &existing), "2.0.0");
}

#[test]
fn create_version_rejects_unsafe_base_version() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err =
        create_version(dir.path(), "implement-task", VALID, Some("../../etc"), true).unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
}

#[test]
fn create_version_copies_scripts_from_base() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let script_path = dir
        .path()
        .join(".apb/playbooks/implement-task/1.0.0/scripts/x.sh");
    fs::write(&script_path, "#!/bin/sh\necho ok\n").unwrap();

    let modified = VALID.replace("name: Implement Task", "name: With Scripts");
    let version = create_version(dir.path(), "implement-task", &modified, None, true).unwrap();
    assert_eq!(version, "1.1.0");

    let copied = dir.path().join(format!(
        ".apb/playbooks/implement-task/{version}/scripts/x.sh"
    ));
    assert!(copied.is_file());
    let content = fs::read_to_string(&copied).unwrap();
    assert_eq!(content, "#!/bin/sh\necho ok\n");
}

#[test]
fn delete_playbook_moves_to_trash_and_restore_brings_back() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let id = "implement-task";
    let ts: u128 = 1_700_000_000_000;
    let trash_name = format!("{id}-{ts}");

    let trashed = delete_playbook(dir.path(), id, ts).unwrap();
    assert_eq!(trashed, dir.path().join(".apb/trash").join(&trash_name));
    assert!(trashed.is_dir());
    assert!(!dir.path().join(".apb/playbooks").join(id).exists());

    let listed = list_trash(dir.path()).unwrap();
    assert!(listed.contains(&trash_name));

    let restored_id = restore_playbook(dir.path(), &trash_name).unwrap();
    assert_eq!(restored_id, id);
    assert!(dir.path().join(".apb/playbooks").join(id).is_dir());
    assert!(!trashed.exists());

    let current =
        fs::read_to_string(dir.path().join(".apb/playbooks").join(id).join("current")).unwrap();
    assert_eq!(current.trim(), "1.0.0");

    // a repeat restore when the id already exists -> Conflict
    fs::create_dir_all(dir.path().join(".apb/trash").join(&trash_name)).unwrap();
    fs::write(
        dir.path()
            .join(".apb/trash")
            .join(&trash_name)
            .join("marker"),
        "x",
    )
    .unwrap();
    let err = restore_playbook(dir.path(), &trash_name).unwrap_err();
    assert!(matches!(err, VersioningError::Conflict(_)));
}

#[test]
fn delete_playbook_missing_and_unsafe_id() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err = delete_playbook(dir.path(), "no-such-apb", 1).unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));

    let err = delete_playbook(dir.path(), "../evil", 1).unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
}

#[test]
fn restore_playbook_rejects_unsafe_trash_name() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    let err = restore_playbook(dir.path(), "../evil-1").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
}

#[test]
fn list_trash_empty_when_no_trash_dir() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    let listed = list_trash(dir.path()).unwrap();
    assert!(listed.is_empty());
}

#[test]
fn save_layout_writes_and_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let layout1 = "nodes:\n  - { id: plan, x: 100, y: 200 }\n";
    save_layout(dir.path(), "implement-task", "1.0.0", layout1).unwrap();

    let path = dir
        .path()
        .join(".apb/playbooks/implement-task/layouts/1.0.0.yaml");
    assert_eq!(fs::read_to_string(&path).unwrap(), layout1);

    let layout2 = "nodes:\n  - { id: plan, x: 50, y: 75 }\n";
    save_layout(dir.path(), "implement-task", "1.0.0", layout2).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), layout2);

    let reg = Registry::open(dir.path()).unwrap();
    let loaded = reg.load("implement-task", Some("1.0.0")).unwrap();
    let layout = loaded.layout.expect("layout should be present");
    assert_eq!(layout["nodes"][0]["x"], 50);
    assert_eq!(layout["nodes"][0]["y"], 75);
}

#[test]
fn save_layout_rejects_invalid_yaml_and_unsafe_segments() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err = save_layout(dir.path(), "implement-task", "1.0.0", ":\n  - bad").unwrap_err();
    assert!(matches!(err, VersioningError::Schema(_)));

    let err = save_layout(dir.path(), "../evil", "1.0.0", "nodes: []\n").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));

    let err = save_layout(dir.path(), "implement-task", "../evil", "nodes: []\n").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));

    let err = save_layout(dir.path(), "no-such", "1.0.0", "nodes: []\n").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
}

#[test]
fn version_diff_detects_node_and_edge_changes() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    // Second version: the plan prompt changed + edge fix->lint replaced with fix->check
    // (the graph stays valid: lint is still reachable via plan).
    let modified = VALID
        .replace(
            "Write a plan: {{params.task}}",
            "Write a detailed plan: {{params.task}}",
        )
        .replace(
            "  - { from: fix, to: lint }",
            "  - { from: fix, to: check }",
        );
    let v2 = create_version(dir.path(), "implement-task", &modified, None, true).unwrap();
    assert_eq!(v2, "1.1.0");

    let diff = version_diff(dir.path(), "implement-task", "1.0.0", &v2).unwrap();
    assert!(
        diff.nodes_changed.contains(&"plan".to_string()),
        "plan prompt changed: {diff:?}"
    );
    assert!(diff.nodes_added.is_empty(), "{diff:?}");
    assert!(diff.nodes_removed.is_empty(), "{diff:?}");
    assert!(
        diff.edges_removed.contains(&"fix->lint".to_string()),
        "{diff:?}"
    );
    assert!(
        diff.edges_added.contains(&"fix->check".to_string()),
        "{diff:?}"
    );
    assert!(!diff.yaml_diff.is_empty());
    assert!(
        diff.yaml_diff.contains("detailed") || diff.yaml_diff.contains("plan"),
        "yaml_diff should mention the changed content: {}",
        diff.yaml_diff
    );
}

#[test]
fn version_diff_rejects_missing_and_unsafe() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let err = version_diff(dir.path(), "implement-task", "1.0.0", "9.9.9").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));

    let err = version_diff(dir.path(), "../evil", "1.0.0", "1.0.0").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));

    let err = version_diff(dir.path(), "implement-task", "../evil", "1.0.0").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
}
