//! End-to-end profile scenario (spec 2026-07-12, Task 14): profile_write through
//! the MCP logic -> a playbook referencing the profile -> a run on a stub agent ->
//! Succeeded. Then editing the skill on disk -> the gate (check_run) refuses with
//! `untrusted_profile_requires_acknowledge` until the user confirms.
//!
//! Unix-only: the former `#![cfg(unix)]` inner attribute now lives as an
//! outer `#[cfg(unix)]` on this module's `mod` declaration in `../main.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::scope::{Origin, PlaybookRef};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use apb_mcp::profile_tools::{self, ExecutorInput};

use crate::common::env_lock as lock;

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
        }
    }
}

fn make_stub(dir: &Path) -> String {
    let path = dir.join("stub.sh");
    fs::write(&path, "#!/bin/sh\necho done\n").unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(".agents/skills").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), body).unwrap();
}

fn seed_playbook(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let src = "schema: 1\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\", profile: arch }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";
    let dir = root.join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
}

#[test]
fn write_profile_then_run_then_skill_edit_untrusts() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", make_stub(bin.path()));
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let root = proj.path();
    seed_skill(root, "cs", "v1");
    seed_playbook(root);

    // profile_write: creates the profile and auto-approves the bundle.
    let written = profile_tools::profile_write(
        root,
        profile_tools::ProfileWrite {
            name: "arch".into(),
            scope: "project".into(),
            description: "architect".into(),
            soul_md: "You are the architect.".into(),
            skills: profile_tools::skill_refs(&["cs".to_string()]),
            executor: ExecutorInput {
                agent: "claude".into(),
                model: "haiku".into(),
                fallbacks: vec![],
            },
            ..Default::default()
        },
    )
    .expect("profile_write ok");
    assert_eq!(written["trust_write_failed"], serde_json::json!(false));

    // Activate the playbook (lifecycle active + digest trusted), otherwise the gate
    // would refuse at the playbook level before checking the profile.
    apb_mcp::tools::playbook_approve(root, "p", None, "project").expect("approve ok");

    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "p".into(),
        version: None,
    };
    // The gate lets it through: the playbook is active/trusted, the profile bundle is auto-approved.
    assert!(
        apb_mcp::policy::check_run(root, &wref, false, false).is_ok(),
        "freshly written profile must be trusted"
    );

    // The run on the stub finishes successfully, the profile is snapshotted.
    let res = run(root, "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert!(
        root.join(".apb/runs")
            .join(&res.run_id)
            .join("profiles/project/arch/profile.yaml")
            .is_file()
    );

    // Editing the skill on disk changes the bundle -> the gate refuses without acknowledge.
    fs::write(root.join(".agents/skills/cs/SKILL.md"), "v2 changed").unwrap();
    let refusal = apb_mcp::policy::check_run(root, &wref, false, false)
        .expect_err("edited skill must untrust the bundle");
    assert_eq!(
        refusal["policy"],
        serde_json::json!("untrusted_profile_requires_acknowledge")
    );
    // With acknowledge - passes.
    assert!(
        apb_mcp::policy::check_run(root, &wref, true, false).is_ok(),
        "acknowledge must cover it"
    );
}

#[test]
fn permit_bundle_map_is_enforced_against_skill_drift() {
    // Anti-TOCTOU (spec 5.1): the bundle map from the permit (captured by the gate)
    // must be checked by the engine against the snapshot. If the skill changes AFTER the gate,
    // the engine recomputes the snapshot (B) and rejects the mismatch with the permit (A).
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", make_stub(bin.path()));
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let root = proj.path();
    seed_skill(root, "cs", "v1");
    seed_playbook(root);
    profile_tools::profile_write(
        root,
        profile_tools::ProfileWrite {
            name: "arch".into(),
            scope: "project".into(),
            description: "architect".into(),
            soul_md: "role".into(),
            skills: profile_tools::skill_refs(&["cs".to_string()]),
            executor: ExecutorInput {
                agent: "claude".into(),
                model: "haiku".into(),
                fallbacks: vec![],
            },
            ..Default::default()
        },
    )
    .expect("write");
    apb_mcp::tools::playbook_approve(root, "p", None, "project").expect("approve");

    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "p".into(),
        version: None,
    };
    // The permit captures bundle A for skill v1.
    let permit = apb_mcp::policy::check_run(root, &wref, false, false).expect("gate ok");

    // The skill drifts to v2 AFTER the permit is captured.
    fs::write(root.join(".agents/skills/cs/SKILL.md"), "v2 drift").unwrap();

    // Running with the map from the permit (A) - the engine recomputes the snapshot (B) and refuses.
    let opts = RunOptions {
        expected_digest: Some(permit.playbook_digest),
        expected_profile_bundles: Some(permit.profile_bundles),
        ..RunOptions::default()
    };
    let err = run(root, "p", None, opts)
        .expect_err("drifted skill must be rejected against the permit map");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("bundle") || msg.contains("changed"),
        "expected bundle-mismatch rejection, got: {msg}"
    );
}
