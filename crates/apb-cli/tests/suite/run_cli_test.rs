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

// Task 4: `apb note <run_id> <text>` posts a supervisor note by appending a
// ContextAppend entry to the run's control.jsonl (dispatches to
// `apb_engine::scheduler::post_supervisor_command`).
#[test]
fn note_command_appends_context_append_to_control_jsonl() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success();

    let runs_dir = dir.path().join(".apb/runs");
    let run_id = fs::read_dir(&runs_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();

    playbook()
        .args(["note", &run_id, "hello"])
        .current_dir(dir.path())
        .assert()
        .success();

    let control = fs::read_to_string(runs_dir.join(&run_id).join("control.jsonl")).unwrap();
    assert!(
        control.contains("\"cmd\":\"context_append\"") && control.contains("\"note\":\"hello\""),
        "expected control.jsonl to contain a ContextAppend note, got:\n{control}"
    );
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

/// Task 8 smoke: `apb stop <run_id>` against a run whose driver is gone
/// finalizes it, and says so.
#[test]
fn stop_finalizes_a_run_whose_driver_is_gone() {
    let dir = seeded();
    let run_dir = dir.path().join(".apb/runs/noagent-dead");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        run_dir.join("events.jsonl"),
        concat!(
            r#"{"seq":0,"ts":1,"type":"run_started","playbook":"noagent","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":2,"type":"node_started","node":"note","attempt":1}"#,
            "\n"
        ),
    )
    .unwrap();

    playbook()
        .args(["stop", "noagent-dead"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("noagent-dead"));

    let journal = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(
        journal.contains("run_aborted"),
        "apb stop must have finalized the abandoned run, journal: {journal}"
    );
}

/// An unknown run id fails loudly rather than pretending to stop something.
#[test]
fn stop_of_an_unknown_run_fails() {
    let dir = seeded();
    playbook()
        .args(["stop", "nope-1"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

/// `--continued-from` threads into RunOptions and establishes run lineage.
#[test]
fn run_continued_from_establishes_lineage() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success();

    let runs_dir = dir.path().join(".apb/runs");
    let first_id = fs::read_dir(&runs_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();

    playbook()
        .args([
            "run",
            "noagent",
            "--param",
            "who=world",
            "--continued-from",
            &first_id,
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    let second_id = fs::read_dir(&runs_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .find(|id| id != &first_id)
        .expect("successor run dir");

    let pred_cfg = apb_engine::run_config::read_run_config(&runs_dir.join(&first_id)).unwrap();
    let succ_cfg = apb_engine::run_config::read_run_config(&runs_dir.join(&second_id)).unwrap();
    assert_eq!(pred_cfg.superseded_by.as_deref(), Some(second_id.as_str()));
    assert_eq!(succ_cfg.continued_from.as_deref(), Some(first_id.as_str()));
}

#[test]
fn run_continued_from_rejects_unknown_predecessor() {
    let dir = seeded();
    playbook()
        .args([
            "run",
            "noagent",
            "--param",
            "who=world",
            "--continued-from",
            "ghost-1",
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("run `ghost-1`"));
}

const OTHER: &str = r#"
schema: 1
id: other
name: Other
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

#[test]
fn run_continued_from_rejects_cross_playbook() {
    let dir = seeded();
    let vdir = dir.path().join(".apb/playbooks/other/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), OTHER).unwrap();
    fs::write(dir.path().join(".apb/playbooks/other/current"), "1.0.0").unwrap();

    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success();

    let runs_dir = dir.path().join(".apb/runs");
    let first_id = fs::read_dir(&runs_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();

    playbook()
        .args([
            "run",
            "other",
            "--param",
            "who=world",
            "--continued-from",
            &first_id,
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("noagent"))
        .stderr(predicate::str::contains("other"));
}
