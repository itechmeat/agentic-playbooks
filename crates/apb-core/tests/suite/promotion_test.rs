use std::fs;
use std::path::Path;

use apb_core::candidate::policy::unreadable;
use apb_core::quality::compare::{CompareMode, QualityGate};
use apb_core::registry::init_project;
use apb_core::schema::Playbook;
use apb_core::versioning::{
    PromotePolicy, VersioningError, create_patch_version, promote_policy, promote_version,
    read_provenance,
};

const VALID: &str = include_str!("../fixtures/valid.yaml");

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
}

#[test]
fn promote_version_keeps_current_when_the_version_has_no_playbook() {
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

/// An ordinary saved version carries no provenance sidecar (only supervisor
/// patches do), and it still has to be selectable as the current one.
#[test]
fn promote_version_switches_to_a_version_without_provenance() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let version_dir = dir.path().join(".apb/playbooks/implement-task/1.1.0");
    fs::create_dir_all(version_dir.join("scripts")).unwrap();
    fs::write(version_dir.join("playbook.yaml"), VALID).unwrap();

    promote_version(dir.path(), "implement-task", "1.1.0").unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/implement-task/current"))
            .unwrap()
            .trim(),
        "1.1.0"
    );
    assert!(
        read_provenance(dir.path(), "implement-task", "1.1.0")
            .unwrap()
            .is_none()
    );

    // And back down to the older one: promotion is not one-directional.
    promote_version(dir.path(), "implement-task", "1.0.0").unwrap();
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
    let cases = [
        (PromotePolicy::Always, "workaround", true, true, 10, false),
        (PromotePolicy::Manual, "improvement", true, true, 0, false),
        (PromotePolicy::Always, "improvement", false, false, 0, true),
        (PromotePolicy::OnSuccess, "improvement", true, true, 0, true),
        (
            PromotePolicy::OnSuccess,
            "improvement",
            true,
            false,
            0,
            false,
        ),
        (
            PromotePolicy::AfterNSuccesses(2),
            "improvement",
            true,
            true,
            0,
            false,
        ),
        (
            PromotePolicy::AfterNSuccesses(2),
            "improvement",
            true,
            true,
            1,
            true,
        ),
        (
            PromotePolicy::AfterNSuccesses(2),
            "improvement",
            false,
            true,
            1,
            false,
        ),
        // One run is no comparison: an in-run patch under a quality policy
        // never becomes current on its own.
        (
            PromotePolicy::Quality(QualityGate::default()),
            "improvement",
            true,
            true,
            50,
            false,
        ),
    ];
    for (policy, class, run_ok, nodes_ok, prior, want) in cases {
        assert_eq!(
            policy.should_promote(class, run_ok, nodes_ok, prior),
            want,
            "{policy:?} {class} {run_ok} {nodes_ok} {prior}"
        );
    }
}

#[test]
fn the_success_count_each_policy_needs() {
    assert_eq!(PromotePolicy::OnSuccess.required_successes(), Some(1));
    assert_eq!(PromotePolicy::Always.required_successes(), Some(1));
    assert_eq!(
        PromotePolicy::AfterNSuccesses(0).required_successes(),
        Some(1)
    );
    assert_eq!(
        PromotePolicy::AfterNSuccesses(3).required_successes(),
        Some(3)
    );
    assert_eq!(PromotePolicy::Manual.required_successes(), None);
    assert_eq!(
        PromotePolicy::Quality(QualityGate::default()).required_successes(),
        None
    );
}

#[test]
fn a_quality_policy_reads_its_gate_with_defaults() {
    assert_eq!(
        promote_policy(&playbook_with_policy("{ when: quality }")),
        PromotePolicy::Quality(QualityGate::default())
    );
    assert_eq!(
        promote_policy(&playbook_with_policy(
            "{ when: quality, compare: better, min_runs: 3, max_trials: 4, window: 8, margin: 0.2 }"
        )),
        PromotePolicy::Quality(QualityGate {
            compare: CompareMode::Better,
            min_runs: 3,
            max_trials: 4,
            window: 8,
            margin: 0.2,
        })
    );
}

#[test]
fn an_unreadable_policy_falls_back_and_says_so() {
    // A quality gate that does not read falls back to a person, never to
    // promoting on successes.
    for (bad, fallback) in [
        ("sometimes", PromotePolicy::OnSuccess),
        ("{ after_n_successes: two }", PromotePolicy::OnSuccess),
        ("{ after_n_successes: 0 }", PromotePolicy::OnSuccess),
        ("{ when: quality, min_runs: 2 }", PromotePolicy::Manual),
        ("{ when: quality, compare: worse }", PromotePolicy::Manual),
        ("{ when: quality, min_run: 8 }", PromotePolicy::Manual),
        (
            "{ when: quality, min_runs: 8, max_trials: 4 }",
            PromotePolicy::Manual,
        ),
        (
            "{ when: quality, min_runs: 8, window: 5 }",
            PromotePolicy::Manual,
        ),
        ("{ when: quality, margin: 2 }", PromotePolicy::Manual),
        ("{ when: vibes }", PromotePolicy::Manual),
        (
            "{ after_n_successes: 2, when: quality }",
            PromotePolicy::Manual,
        ),
    ] {
        let p = playbook_with_policy(bad);
        assert_eq!(promote_policy(&p), fallback, "{bad}");
        let why = unreadable(&p);
        assert_eq!(why.len(), 1, "{bad}");
        assert_eq!(why[0].0, "promote_supervisor_patches");
    }
    assert!(unreadable(&playbook_with_policy("{ after_n_successes: 2 }")).is_empty());
}
