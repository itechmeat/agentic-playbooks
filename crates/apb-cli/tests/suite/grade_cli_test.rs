//! `apb grade` and `apb candidate`: grading an ended run by rules, its
//! stored grades in text and JSON, the grade line of `apb runs <id>`, a
//! backfill that skips what is graded, and the candidate command without a
//! candidate on trial.

use predicates::prelude::*;
use std::fs;
use std::path::Path;

use crate::common::{apb, sandbox_config_dir};

const SNAPSHOT: &str = "schema: 2\nid: demo\nname: Demo\nversion: 1.0.0\nquality: {}\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: script, script: run.sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";

fn journal(outcome: &str) -> String {
    let status = if outcome == "succeeded" {
        "succeeded"
    } else {
        "failed"
    };
    format!(
        r#"{{"seq":0,"ts":1790000000000,"type":"run_started","playbook":"demo","version":"1.0.0"}}
{{"seq":1,"ts":1790000000000,"type":"node_started","node":"w","attempt":1}}
{{"seq":2,"ts":1790000004000,"type":"node_finished","node":"w","status":"{status}","attempt":1,"output":"ok","artifacts":[]}}
{{"seq":3,"ts":1790000005000,"type":"run_finished","outcome":"{outcome}"}}
"#
    )
}

/// A project with two ended runs of `demo` (one failed), each stamped as
/// created by the sandboxed install, and none graded.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let cfg = sandbox_config_dir();
    let key = cfg.join("run-origin.key");
    if !key.exists() {
        fs::write(&key, "k".repeat(64)).unwrap();
    }
    for (id, outcome) in [("demo-1", "succeeded"), ("demo-2", "failed")] {
        let run = dir.path().join(".apb/runs").join(id);
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("events.jsonl"), journal(outcome)).unwrap();
        fs::write(run.join("playbook.yaml"), SNAPSHOT).unwrap();
        apb_core::run_origin::stamp_in(cfg, &run, id).unwrap();
    }
    dir
}

fn grades_of(root: &Path, run: &str) -> serde_json::Value {
    let out = apb()
        .args(["grade", run, "--show", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success());
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn grade_grades_an_ended_run_by_rules_and_runs_shows_the_line() {
    let dir = project();
    apb()
        .args(["grade", "demo-2", "--show"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("demo-2: not graded"));
    apb()
        .args(["grade", "demo-2"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("rules/1"))
        .stdout(predicate::str::contains("outcome"))
        .stdout(predicate::str::contains("poor"));
    let shown = grades_of(dir.path(), "demo-2");
    assert_eq!(shown["grades"].as_array().unwrap().len(), 1);
    assert_eq!(shown["grades"][0]["criteria"][0]["id"], "outcome");
    apb()
        .args(["runs", "demo-2"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("  grade rules/1: "))
        .stdout(predicate::str::contains("outcome poor"));
}

#[test]
fn backfill_grades_only_the_runs_without_a_rules_grade() {
    let dir = project();
    apb()
        .args(["grade", "demo-1"])
        .current_dir(dir.path())
        .assert()
        .success();
    let out = apb()
        .args(["grade", "--backfill", "demo", "--last", "5", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let runs = report["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2, "{report}");
    let skipped: Vec<&str> = runs.iter().filter_map(|r| r["skipped"].as_str()).collect();
    assert_eq!(skipped, ["graded"]);
    assert_eq!(
        grades_of(dir.path(), "demo-2")["grades"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Nothing left to grade.
    apb()
        .args(["grade", "--backfill", "demo"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "demo: graded 0 of 2 runs by rules",
        ));
}

#[test]
fn grade_refuses_an_unknown_run_and_flags_out_of_place() {
    let dir = project();
    apb()
        .args(["grade", "nope"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("grade failed"));
    // --version only goes with --backfill.
    apb()
        .args(["grade", "demo-1", "--version", "1.0.0"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("go with --backfill"));
}

#[test]
fn candidate_without_a_candidate_says_so_and_refuses_a_decision() {
    let dir = project();
    apb()
        .args(["candidate", "demo"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("demo: no candidate on trial"));
    let out = apb()
        .args(["candidate", "demo", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v, serde_json::json!({ "id": "demo", "candidate": null }));
    apb()
        .args(["candidate", "demo", "--keep-current", "--reason", "not now"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no candidate on trial"));
    // --reason belongs to --keep-current.
    apb()
        .args(["candidate", "demo", "--promote", "--reason", "x"])
        .current_dir(dir.path())
        .assert()
        .failure();
}

#[test]
fn a_run_step_may_look_at_grades_but_not_grade_or_decide() {
    let dir = project();
    apb()
        .args(["grade", "demo-2"])
        .env("APB_RUN_ID", "r-step")
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run step"));
    apb()
        .args(["grade", "--backfill", "demo"])
        .env("APB_HOST_TASK", "t-1")
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run step"));
    apb()
        .args(["candidate", "demo", "--promote"])
        .env("APB_RUN_ID", "r-step")
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run step"));
    // Looking is allowed, and nothing was graded.
    apb()
        .args(["grade", "demo-2", "--show"])
        .env("APB_RUN_ID", "r-step")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("demo-2: not graded"));
}

#[test]
fn reopen_needs_a_host_grade_to_reopen() {
    let dir = project();
    apb()
        .args(["grade", "demo-1", "--reopen"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no host grade"));
    apb()
        .args(["grade", "--backfill", "demo", "--reopen"])
        .current_dir(dir.path())
        .assert()
        .code(2);
}
