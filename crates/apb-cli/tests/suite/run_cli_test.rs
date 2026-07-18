use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

// A playbook without agent_task: start -> prompt -> finish. No real agent is needed.
// Note: the `params:` entry describing `who` was added beyond the literal brief text -
// V13 (validate.rs) requires that `{{params.X}}` reference a declared playbook
// parameter; without it `run()` rejects the playbook as invalid (see also
// crates/apb-engine/tests/scheduler_test.rs).
const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn playbook() -> Command {
    Command::cargo_bin("apb").unwrap()
}

fn seeded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    let vdir = dir.path().join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(dir.path().join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
    dir
}

#[test]
fn run_succeeds_and_writes_events() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("succeeded"));
    // a run has appeared
    let runs_dir = dir.path().join(".apb/runs");
    let count = fs::read_dir(&runs_dir).unwrap().count();
    assert_eq!(count, 1);
}

#[test]
fn runs_command_lists_the_run() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent"])
        .current_dir(dir.path())
        .assert()
        .success();
    playbook()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("noagent"))
        .stdout(predicate::str::contains("succeeded"));
}

#[test]
fn run_without_project_fails_env() {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .args(["run", "ghost"])
        .current_dir(dir.path())
        .assert()
        .code(2);
}
