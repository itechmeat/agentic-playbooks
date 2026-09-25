use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

const VALID: &str = include_str!("../../../apb-core/tests/fixtures/valid.yaml");

fn playbook() -> Command {
    crate::common::apb()
}

fn seeded_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/implement-task/current"),
        "1.0.0",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".apb/profiles/architect")).unwrap();
    dir
}

#[test]
fn init_creates_structure() {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(".apb"));
    assert!(dir.path().join(".apb/playbooks").is_dir());
}

#[test]
fn list_shows_playbook() {
    let dir = seeded_dir();
    playbook()
        .arg("list")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("implement-task"))
        .stdout(predicate::str::contains("1.0.0"));
}

#[test]
fn validate_ok_playbook() {
    let dir = seeded_dir();
    playbook()
        .args(["validate", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("OK"));
}

#[test]
fn validate_broken_playbook_fails_with_code() {
    let dir = seeded_dir();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    let bad = VALID.replace("{{params.task}}", "{{params.ghost}}");
    fs::write(vdir.join("playbook.yaml"), bad).unwrap();
    playbook()
        .args(["validate", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("V13"));
}

/// Whole-project validation must not skip a playbook that fails to load (the
/// listing drops it, so it used to vanish and the run exited 0): a playbook
/// with unparseable YAML is named with its load error and the exit code is 1,
/// while the healthy playbook next to it is still reported OK.
#[test]
fn validate_all_reports_a_playbook_that_fails_to_load() {
    let dir = seeded_dir();
    let broken = dir.path().join(".apb/playbooks/broken-one");
    fs::create_dir_all(broken.join("1.0.0")).unwrap();
    fs::write(broken.join("1.0.0/playbook.yaml"), "nodes: [unclosed").unwrap();
    fs::write(broken.join("current"), "1.0.0").unwrap();
    playbook()
        .arg("validate")
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("broken-one: error"))
        .stdout(predicate::str::contains("implement-task: OK"));
}

/// `apb validate` checks the connector rules run start checks: a playbook
/// binding an installed connector whose manifest no longer loads is a V42
/// error here, not only a code-less refusal when the run starts.
#[test]
fn validate_reports_v42_for_an_installed_broken_connector() {
    let dir = seeded_dir();
    let cfg = tempfile::tempdir().unwrap();
    let conn = cfg.path().join("connectors/brokenhook");
    fs::create_dir_all(&conn).unwrap();
    fs::write(
        conn.join("connector.yaml"),
        "name: brokenhook\nversion: 0.1.0\nfunctions:\n  - name: inbox_read\n    description: read pending\n    read_only: true\n    response_pick: [events]\n    inbox:\n      op: read\n",
    )
    .unwrap();
    let pdir = dir.path().join(".apb/playbooks/hooked");
    fs::create_dir_all(pdir.join("1.0.0")).unwrap();
    fs::write(
        pdir.join("1.0.0/playbook.yaml"),
        "schema: 2\nid: hooked\nname: hooked\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - id: a\n    type: agent_task\n    prompt: hi\n    profile: architect\n    connectors: [{ name: brokenhook, functions: [inbox_read] }]\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
    )
    .unwrap();
    fs::write(pdir.join("current"), "1.0.0").unwrap();
    playbook()
        .arg("validate")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("hooked: error V42"));
}

/// Whole-project validation checks zcode profile models against apb's
/// allowlist: the bare id and the legacy `zai-individual/` spelling pass, any
/// other model is an error naming the allowlist.
#[test]
fn validate_refuses_a_zcode_profile_model_off_the_allowlist() {
    let dir = seeded_dir();
    let profile =
        |model: &str| format!("name: architect\nexecutor:\n  agent: zcode\n  model: {model}\n");
    let path = dir.path().join(".apb/profiles/architect/profile.yaml");
    for ok in ["GLM-5.3-Flash@high", "zai-individual/GLM-5.3"] {
        fs::write(&path, profile(ok)).unwrap();
        playbook()
            .arg("validate")
            .current_dir(dir.path())
            .assert()
            .success();
    }
    fs::write(&path, profile("GLM-5-Turbo")).unwrap();
    playbook()
        .arg("validate")
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "profile architect: error zcode_model_not_allowed",
        ))
        .stdout(predicate::str::contains("GLM-5.3-Flash"));
}

#[test]
fn list_without_apb_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("list")
        .current_dir(dir.path())
        .assert()
        .code(2);
}

#[test]
fn dashboard_is_visible_and_serve_alias_parses() {
    // Primary name is visible; the hidden `serve` alias is not listed.
    playbook()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("dashboard"))
        .stdout(predicate::str::contains("Start the web dashboard"))
        // Trailing space distinguishes the alias from the unrelated `server`
        // command, which also starts with the substring "serve".
        .stdout(predicate::str::contains("  serve ").not());

    // Hidden alias still resolves to the same command (help works).
    playbook()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Start the web dashboard"));

    playbook()
        .args(["dashboard", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Start the web dashboard"));
}

/// `apb trash`: the listing shows a deletion, a restore by id brings the
/// playbook back, and a restore whose id is taken again is refused with exit
/// code 1 and a message saying why (scripts tell it apart from not found, 2).
#[test]
fn trash_list_restore_and_conflict() {
    let dir = seeded_dir();
    apb_core::versioning::delete_playbook(dir.path(), "implement-task", 1_700_000_000_000).unwrap();

    let out = playbook()
        .args(["trash", "list", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed[0]["name"], "implement-task-1700000000000");
    playbook()
        .args(["trash", "list"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("2023-11-14T22:13:20Z"));

    playbook()
        .args(["trash", "restore", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("restored implement-task"));
    playbook()
        .args(["validate", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .success();

    apb_core::versioning::delete_playbook(dir.path(), "implement-task", 1).unwrap();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    playbook()
        .args(["trash", "restore", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("exists again"));
    playbook()
        .args(["trash", "restore", "no-such"])
        .current_dir(dir.path())
        .assert()
        .code(2);
}
