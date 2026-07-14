use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::schema::Playbook;
use apb_core::versioning::{
    PromotePolicy, VersioningError, create_patch_version, promote_policy, promote_version,
    read_provenance, should_promote,
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

fn playbook_with_policy(policy: &str) -> Playbook {
    let yaml =
        format!("{VALID}\nsupervisor:\n  policy:\n    promote_supervisor_patches: {policy}\n");
    Playbook::from_yaml(&yaml).unwrap()
}

#[test]
fn promote_version_moves_current_and_marks_provenance() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let version = create_patch_version(
        dir.path(),
        "implement-task",
        "1.0.0",
        VALID,
        "run-42",
        "improvement",
    )
    .unwrap();

    promote_version(dir.path(), "implement-task", &version).unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/implement-task/current"))
            .unwrap()
            .trim(),
        version
    );
    assert!(
        read_provenance(dir.path(), "implement-task", &version)
            .unwrap()
            .unwrap()
            .promoted
    );
}

#[test]
fn promote_version_keeps_current_when_provenance_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    fs::create_dir_all(dir.path().join(".apb/playbooks/implement-task/1.0.1")).unwrap();

    let err = promote_version(dir.path(), "implement-task", "1.0.1").unwrap_err();
    assert!(matches!(err, VersioningError::NotFound(_)));
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/implement-task/current"))
            .unwrap()
            .trim(),
        "1.0.0"
    );
}

#[test]
fn promote_policy_parses_all_supported_forms_and_defaults() {
    assert_eq!(
        promote_policy(&Playbook::from_yaml(VALID).unwrap()),
        PromotePolicy::OnSuccess
    );
    assert_eq!(
        promote_policy(&playbook_with_policy("on_success")),
        PromotePolicy::OnSuccess
    );
    assert_eq!(
        promote_policy(&playbook_with_policy("manual")),
        PromotePolicy::Manual
    );
    assert_eq!(
        promote_policy(&playbook_with_policy("always")),
        PromotePolicy::Always
    );
    assert_eq!(
        promote_policy(&playbook_with_policy("{ after_n_successes: 3 }")),
        PromotePolicy::AfterNSuccesses(3)
    );
}

#[test]
fn should_promote_applies_classification_and_policy() {
    assert!(!should_promote(
        PromotePolicy::Always,
        "workaround",
        true,
        true,
        10,
    ));
    assert!(!should_promote(
        PromotePolicy::Manual,
        "improvement",
        true,
        true,
        0,
    ));
    assert!(should_promote(
        PromotePolicy::Always,
        "improvement",
        false,
        false,
        0,
    ));
    assert!(should_promote(
        PromotePolicy::OnSuccess,
        "improvement",
        true,
        true,
        0,
    ));
    assert!(!should_promote(
        PromotePolicy::OnSuccess,
        "improvement",
        true,
        false,
        0,
    ));
    assert!(!should_promote(
        PromotePolicy::AfterNSuccesses(2),
        "improvement",
        true,
        true,
        0,
    ));
    assert!(should_promote(
        PromotePolicy::AfterNSuccesses(2),
        "improvement",
        true,
        true,
        1,
    ));
    assert!(!should_promote(
        PromotePolicy::AfterNSuccesses(2),
        "improvement",
        false,
        true,
        1,
    ));
}
