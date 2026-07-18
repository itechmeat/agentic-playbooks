use std::fs;
use std::path::Path;

use apb_core::doctor::{CheckStatus, diagnose};
use apb_core::registry::init_project;

use crate::common::env_lock;

fn seed_playbook(root: &Path, id: &str, body: &str) {
    let dir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), body).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
}

const VALID_AGENT: &str = r#"
schema: 2
id: va
name: Valid Agent
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", expected_duration: 5m }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

const VALID_SCRIPT: &str = r#"
schema: 1
id: vs
name: Valid Script
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: "s.sh", runner: sh, expected_duration: 5m }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: done }
"#;

// References a nonexistent project profile `ghost` - parses fine, but
// the validator raises an error (V14) for the explicit scope: project.
const INVALID: &str = r#"
schema: 2
id: bad
name: Bad
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", profile: { name: ghost, scope: project } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

// Seeds a profile onto the project's disk.
fn seed_profile(root: &Path, name: &str, agent: &str) {
    let dir = root.join(".apb/profiles").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        format!("name: {name}\ndescription: t\nexecutor:\n  agent: {agent}\n  model: sonnet\n"),
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), "").unwrap();
}

#[test]
fn doctor_reports_clean_then_flags_invalid_playbook() {
    let _l = env_lock();
    // Empty config directory - deterministic defaults (the no-config path).
    let cfg_dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg_dir.path());
    }

    // 1. Clean project: a valid agent-based + a valid script-based playbook.
    let good = tempfile::tempdir().unwrap();
    init_project(good.path()).unwrap();
    seed_playbook(good.path(), "va", VALID_AGENT);
    seed_playbook(good.path(), "vs", VALID_SCRIPT);
    seed_profile(good.path(), "main", "claude-code");

    let report = diagnose(good.path());
    assert!(
        !report.has_failure(),
        "clean project must not fail: {:?}",
        report.checks
    );
    // Basic checks are present.
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "global config" && c.status == CheckStatus::Ok)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "runner sh" && c.status == CheckStatus::Ok)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "playbook va" && c.status == CheckStatus::Ok)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "playbook vs" && c.status == CheckStatus::Ok)
    );
    // Agent claude-code is mentioned (Ok or Warn - depends on whether `claude` is present).
    assert!(report.checks.iter().any(|c| c.name == "agent claude-code"));

    // 2. Invalid playbook -> has_failure.
    let bad = tempfile::tempdir().unwrap();
    init_project(bad.path()).unwrap();
    seed_playbook(bad.path(), "bad", INVALID);
    let report = diagnose(bad.path());
    assert!(report.has_failure(), "invalid playbook must fail");
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "playbook bad" && c.status == CheckStatus::Fail),
        "expected playbook bad to be flagged: {:?}",
        report.checks
    );

    // 3. An unparseable playbook.yaml must not silently disappear: we load
    //    playbooks independently, surfacing the broken one as Fail with its id,
    //    while the valid one next to it stays Ok.
    let broken = tempfile::tempdir().unwrap();
    init_project(broken.path()).unwrap();
    seed_playbook(broken.path(), "va", VALID_AGENT);
    seed_playbook(broken.path(), "broke", "this: is: not: valid: yaml: ][");
    let report = diagnose(broken.path());
    assert!(report.has_failure(), "broken playbook must fail");
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "playbook broke" && c.status == CheckStatus::Fail),
        "broken playbook must surface as Fail: {:?}",
        report.checks
    );
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "playbook va" && c.status == CheckStatus::Ok),
        "valid playbook must still load alongside a broken one: {:?}",
        report.checks
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[cfg(unix)]
fn write_stub(dir: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    fs::write(&p, "#!/bin/sh\necho '1.0.0'\n").unwrap();
    fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn claude_code_agent_normalizes_to_claude_probe_no_false_not_found() {
    let _l = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    // Stub `claude` on PATH: the claude detection probe must find it for the
    // `claude-code` agent (normalization), rather than looking for a
    // nonexistent `claude-code`.
    write_stub(bin.path(), "claude");
    let saved_path = std::env::var("PATH").ok();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
        std::env::set_var("PATH", bin.path());
    }

    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "va", VALID_AGENT);
    seed_profile(proj.path(), "main", "claude-code");
    let report = diagnose(proj.path());

    let check = report
        .checks
        .iter()
        .find(|c| c.name == "agent claude-code")
        .expect("agent claude-code checked");
    assert_eq!(
        check.status,
        CheckStatus::Ok,
        "claude-code must resolve via the claude probe, not a false not-found: {}",
        check.detail
    );
    assert!(
        check.detail.contains("authority"),
        "doctor must print detection authority for the agent's models: {}",
        check.detail
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
        std::env::remove_var("HOME");
        match saved_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }
}

#[test]
fn global_scope_profile_ref_is_resolved() {
    let _l = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    // A global profile in config_dir; the project playbook references it with
    // scope: global. Doctor must resolve it (there is no project profile with the same name).
    let gdir = cfg.path().join("profiles/shared");
    fs::create_dir_all(&gdir).unwrap();
    fs::write(
        gdir.join("profile.yaml"),
        "name: shared\ndescription: t\nexecutor:\n  agent: codex\n  model: o1\n",
    )
    .unwrap();
    fs::write(gdir.join("SOUL.md"), "").unwrap();

    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    let playbook = "schema: 2\nid: g\nname: G\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: \"do\", profile: { name: shared, scope: global } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";
    seed_playbook(proj.path(), "g", playbook);

    let report = diagnose(proj.path());
    // The profile resolves: no Fail "profile shared", and agent codex is mentioned.
    assert!(
        !report
            .checks
            .iter()
            .any(|c| c.name == "profile shared" && c.status == CheckStatus::Fail),
        "global-scope profile must resolve: {:?}",
        report.checks
    );
    assert!(
        report.checks.iter().any(|c| c.name == "agent codex"),
        "codex from the global profile chain must be checked: {:?}",
        report.checks
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}
