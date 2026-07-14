use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

const VALID: &str = include_str!("../../apb-core/tests/fixtures/valid.yaml");

fn playbook() -> Command {
    Command::cargo_bin("apb").unwrap()
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

#[test]
fn list_without_apb_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("list")
        .current_dir(dir.path())
        .assert()
        .code(2);
}
