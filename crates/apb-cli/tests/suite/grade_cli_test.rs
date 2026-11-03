//! `apb grade` and `apb candidate`: grading an ended run by rules (and
//! the model grader naming why it stored nothing), its
//! stored grades in text and JSON, the grade line of `apb runs <id>`, a
//! backfill that skips what is graded, the candidate command without a
//! candidate on trial, and its summary whose printed decision promotes
//! exactly what it showed.

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
    assert_eq!(
        v,
        serde_json::json!({ "id": "demo", "candidate": null, "summary": null })
    );
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
    // A promotion names the version and digest it was read under; an older
    // script without them is refused before anything is looked up.
    apb()
        .args(["candidate", "demo", "--promote"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--expect"))
        .stderr(predicate::str::contains("no candidate on trial").not());
    apb()
        .args(["candidate", "demo", "--promote", "--expect", "1.0.0"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("<version>@<digest>"));
}

/// `demo` 1.0.0 current and 1.0.1 its candidate (a forward patch on
/// trial), with one scenario that never ran.
fn project_with_candidate() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let pb = dir.path().join(".apb/playbooks/demo");
    apb_core::registry::init_project(dir.path()).unwrap();
    let plain = "schema: 2\nid: demo\nname: Demo\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
    for v in ["1.0.0", "1.0.1"] {
        fs::create_dir_all(pb.join(v)).unwrap();
        fs::write(pb.join(v).join("playbook.yaml"), plain.replace("1.0.0", v)).unwrap();
    }
    fs::create_dir_all(pb.join("meta")).unwrap();
    fs::write(
        pb.join("meta/1.0.1.yaml"),
        "created_by: supervisor\nrun_id: null\nclassification: improvement\nscope: next_runs\nbase_version: 1.0.0\n",
    )
    .unwrap();
    fs::write(pb.join("current"), "1.0.0").unwrap();
    fs::write(pb.join("candidate"), "1.0.1").unwrap();
    fs::create_dir_all(pb.join("evals/scenarios")).unwrap();
    fs::write(
        pb.join("evals/scenarios/walk.yaml"),
        "schema: 1\nid: walk\n",
    )
    .unwrap();
    dir
}

#[test]
fn candidate_prints_its_summary_and_the_decision_that_promotes_it() {
    let dir = project_with_candidate();
    let out = apb()
        .args(["candidate", "demo"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "{text}");
    assert!(text.contains("scenario_not_run"), "{text}");
    // The decision line names the version, digest and summary id shown.
    let line = text
        .lines()
        .find(|l| l.contains("--promote --expect"))
        .unwrap_or_else(|| panic!("{text}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    let after = |flag: &str| words[words.iter().position(|w| *w == flag).unwrap() + 1];
    let (expect, ack) = (after("--expect"), after("--ack"));
    assert!(expect.starts_with("1.0.1@sha256:"), "{line}");

    apb()
        .args(["candidate", "demo", "--promote", "--expect", expect])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("needs_acknowledgement"))
        .stderr(predicate::str::contains(ack));
    apb()
        .args([
            "candidate",
            "demo",
            "--promote",
            "--expect",
            expect,
            "--ack",
            ack,
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/demo/current"))
            .unwrap()
            .trim(),
        "1.0.1"
    );
}

#[test]
fn grade_model_names_why_it_stored_nothing() {
    let dir = project();
    // No decision model is configured: nothing is asked or stored.
    apb()
        .args(["grade", "demo-1", "--model"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "demo-1: not graded by model: the project's decision model is off for grading",
        ));
    let out = apb()
        .args(["grade", "demo-1", "--model", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (v["grader"].as_str(), v["skipped"].as_str()),
        (Some("model"), Some("off"))
    );
    assert_eq!(
        grades_of(dir.path(), "demo-1")["grades"],
        serde_json::json!([])
    );
    apb()
        .args(["grade", "--backfill", "demo", "--model"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "demo: graded 0 of 1 runs by model",
        ))
        .stdout(predicate::str::contains(
            "skipped: the project's decision model is off for grading",
        ));
    // --model does not go with --show.
    apb()
        .args(["grade", "demo-1", "--model", "--show"])
        .current_dir(dir.path())
        .assert()
        .failure();
}

#[test]
fn a_run_step_may_look_at_grades_but_not_grade_or_decide() {
    let dir = project();
    apb()
        .args(["grade", "demo-2", "--model"])
        .env("APB_RUN_ID", "r-step")
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run step"));
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
        .args([
            "candidate",
            "demo",
            "--promote",
            "--expect",
            "1.0.0@sha256:00",
        ])
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

// --- apb judge -------------------------------------------------------------

const JUDGE: &str = "schema: 2\nid: doc\nname: Doc\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - id: triage\n    type: judge\n    state: { task: \"{{run.instruction}}\" }\n    questions:\n      verdict: { type: choice, instructions: \"Which?\", criteria: { clean: a, needs_fix: b } }\n    on_unavailable: { default: { verdict: needs_fix } }\n  - { id: fix, type: finish, outcome: success }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: triage }\n  - { id: to-fix, from: triage, to: fix, condition: { type: output_field, node: triage, field: verdict, equals: needs_fix } }\n  - { from: triage, to: done, fallback: true }\n";

/// A project with `doc` current and run `r1` of it (one judge execution),
/// plus an inputs and an answers file.
fn judge_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let v = root.join(".apb/playbooks/doc/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), JUDGE).unwrap();
    fs::write(root.join(".apb/playbooks/doc/current"), "1.0.0").unwrap();
    let run = root.join(".apb/runs/r1");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("playbook.yaml"), JUDGE).unwrap();
    fs::write(run.join("run.yaml"), "instruction: Triage it\n").unwrap();
    fs::write(
        run.join("events.jsonl"),
        "{\"seq\":0,\"ts\":1,\"type\":\"run_started\",\"playbook\":\"doc\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":1,\"type\":\"node_started\",\"node\":\"triage\",\"attempt\":1}\n\
         {\"seq\":2,\"ts\":2,\"type\":\"node_finished\",\"node\":\"triage\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"{}\",\"artifacts\":[]}\n",
    )
    .unwrap();
    fs::write(root.join("inputs.yaml"), "instruction: Fix the parser\n").unwrap();
    fs::write(
        root.join("answers.yaml"),
        "verdict: { value: clean, p: 0.9, confidence: 0.9 }\n",
    )
    .unwrap();
    dir
}

/// `apb judge eval` with given answers prints the tool layer's evaluation
/// (here, with no decision settings, the fallback on this machine and the
/// answer at enforce, each routed); inside a run step it still evaluates
/// given answers but refuses a live ask and a save, and writes nothing.
#[test]
fn judge_eval_shows_three_answers_and_a_run_step_never_spends() {
    let dir = judge_project();
    let root = dir.path();
    let out = apb()
        .current_dir(root)
        .args([
            "judge",
            "eval",
            "doc",
            "triage",
            "--inputs",
            "inputs.yaml",
            "--answers",
            "answers.yaml",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["evaluation"]["at_enforce"]["output"]["verdict"], "clean");
    assert_eq!(v["evaluation"]["as_configured"]["kind"], "fallback");
    assert_eq!(v["evaluation"]["as_configured"]["reason"], "not_configured");
    assert_eq!(
        v["route_at_enforce"],
        serde_json::json!(["e:triage>done#1"])
    );
    assert_eq!(v["route_as_configured"], serde_json::json!(["id:to-fix"]));
    assert_eq!(v["evaluation"]["requests"], 0);

    let in_run = || {
        let mut cmd = apb();
        cmd.current_dir(root).env("APB_RUN_ID", "other-run");
        cmd
    };
    in_run()
        .args([
            "judge",
            "eval",
            "doc",
            "triage",
            "--answers",
            "answers.yaml",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("at enforce: output"));
    in_run()
        .args(["judge", "eval", "doc", "triage"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("inside run `other-run`"));
    in_run()
        .args([
            "judge",
            "save-example",
            "r1",
            "triage",
            "--expect",
            "verdict=clean",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("inside run `other-run`"));
    assert!(!root.join(".apb/playbooks/doc/evals").exists());

    // A person saves it, and the examples list shows it.
    apb()
        .current_dir(root)
        .args([
            "judge",
            "save-example",
            "r1",
            "triage",
            "--expect",
            "verdict=clean",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("saved ex-0001"));
    apb()
        .current_dir(root)
        .args(["judge", "examples", "doc", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("triage: 1 examples"));
}
