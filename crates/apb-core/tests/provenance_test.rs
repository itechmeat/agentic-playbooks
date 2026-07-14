use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::versioning::{
    VersionProvenance, VersioningError, create_version, read_provenance, set_promoted,
    write_provenance,
};

const VALID: &str = include_str!("fixtures/valid.yaml");

fn seed(root: &Path) {
    init_project(root).unwrap();
    let version_dir = root.join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(version_dir.join("scripts")).unwrap();
    fs::write(version_dir.join("playbook.yaml"), VALID).unwrap();
    fs::write(root.join(".apb/playbooks/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join(".apb/profiles/architect")).unwrap();
}

#[test]
fn provenance_round_trip_and_promotion_update() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    assert_eq!(
        read_provenance(dir.path(), "implement-task", "1.0.0").unwrap(),
        None
    );

    let provenance = VersionProvenance {
        created_by: "supervisor".into(),
        run_id: Some("run-42".into()),
        classification: Some("improvement".into()),
        promoted: false,
    };
    write_provenance(dir.path(), "implement-task", "1.0.0", &provenance).unwrap();
    assert_eq!(
        read_provenance(dir.path(), "implement-task", "1.0.0").unwrap(),
        Some(provenance)
    );

    set_promoted(dir.path(), "implement-task", "1.0.0", true).unwrap();
    assert!(
        read_provenance(dir.path(), "implement-task", "1.0.0")
            .unwrap()
            .unwrap()
            .promoted
    );
}

#[test]
fn provenance_rejects_unsafe_segments() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let provenance = VersionProvenance {
        created_by: "user".into(),
        run_id: None,
        classification: None,
        promoted: true,
    };

    let err = write_provenance(dir.path(), "../evil", "1.0.0", &provenance).unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));

    let err = read_provenance(dir.path(), "implement-task", "../evil").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
}

#[test]
fn create_version_records_user_provenance() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let version = create_version(
        dir.path(),
        "implement-task",
        &VALID.replace("name: Implement Task", "name: Updated Task"),
        None,
        true,
    )
    .unwrap();

    assert_eq!(
        read_provenance(dir.path(), "implement-task", &version).unwrap(),
        Some(VersionProvenance {
            created_by: "user".into(),
            run_id: None,
            classification: None,
            promoted: true,
        })
    );
}
