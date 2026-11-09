//! End to end: a param value that does not match its pattern fails the
//! start before a run directory exists, on the paths that do not pass
//! the gate (a detached background start), and a scenario run starts
//! with a secret param's placeholder.

use std::fs;

use apb_core::registry::init_project;
use apb_core::store::ResolvedPlaybook;
use apb_engine::scheduler::{RunOptions, run_background_resolved};

use crate::common;

const PATTERNED: &str = "schema: 2\nid: patt\nname: Patt\nversion: 1.0.0\nparams:\n  - { name: repo, type: text, pattern: '[A-Za-z0-9._-]{1,100}' }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

const SECRET_PATTERNED: &str = "schema: 2\nid: secp\nname: Secp\nversion: 1.0.0\nparams:\n  - { name: token, type: text, secret: true, pattern: '[a-z]{8,}' }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

fn seed(root: &std::path::Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
}

fn resolved_of(root: &std::path::Path, id: &str) -> ResolvedPlaybook {
    ResolvedPlaybook {
        definition_parent: root.join(".apb"),
        execution_root: root.to_path_buf(),
        id: id.into(),
        version: "1.0.0".into(),
        digest: crate_digest(root, id),
        origin_label: "project",
    }
}

fn crate_digest(root: &std::path::Path, id: &str) -> String {
    let yaml =
        fs::read_to_string(root.join(format!(".apb/playbooks/{id}/1.0.0/playbook.yaml"))).unwrap();
    format!("sha256:{}", {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(yaml.as_bytes());
        hex_lower(&h.finalize())
    })
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A background start with a mismatching value fails before a run
/// directory exists: the preparation backstop covers the paths that do
/// not pass the launch gate.
#[test]
fn a_mismatching_value_fails_before_a_run_directory_exists() {
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path(), "patt", PATTERNED);
    let resolved = resolved_of(proj.path(), "patt");
    let mut opts = RunOptions::default();
    opts.params.insert("repo".into(), "a b".into());
    let err = run_background_resolved(&resolved, opts).unwrap_err();
    assert!(
        err.to_string().contains("does not match its pattern"),
        "{err}"
    );
    let runs = proj.path().join(".apb/runs");
    assert!(
        !runs.exists() || fs::read_dir(&runs).unwrap().next().is_none(),
        "a refused start left a run directory"
    );

    // A matching value starts.
    let mut opts = RunOptions::default();
    opts.params.insert("repo".into(), "acme.site".into());
    let run_id = run_background_resolved(&resolved, opts).unwrap();
    assert!(proj.path().join(".apb/runs").join(&run_id).exists());
}

/// A scenario run of a playbook with a secret param that has a pattern
/// starts: the placeholder apb fills in is never pattern-checked.
#[test]
fn a_scenario_run_starts_with_a_secret_placeholder() {
    let _lock = crate::common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    // SAFETY: the env lock holds for the whole test.
    let _env = [common::EnvVar::set("APB_CONFIG_DIR", cfg.path())];
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path(), "secp", SECRET_PATTERNED);
    let scenarios = proj.path().join(".apb/playbooks/secp/evals/scenarios");
    fs::create_dir_all(&scenarios).unwrap();
    fs::write(
        scenarios.join("green.yaml"),
        "schema: 1\nid: green\nexpect: { status: { done: succeeded } }\n",
    )
    .unwrap();
    let prep = apb_engine::scenario::session::ScenarioPrep {
        scenario: yaml_serde::from_str::<apb_core::scenario::Scenario>(
            "schema: 1\nid: green\nexpect: { status: { done: succeeded } }\n",
        )
        .unwrap(),
        level: apb_core::scenario::ScenarioLevel::Quick,
        scenario_digest: "sha256:green".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let res = apb_engine::scheduler::run_scenario(proj.path(), "secp", None, &prep).unwrap();
    assert_eq!(res.outcome, apb_engine::state::RunStatus::Succeeded);
}
