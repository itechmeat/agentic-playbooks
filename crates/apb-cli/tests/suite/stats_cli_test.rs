//! `apb stats` (C3): cross-run metrics from the journals of this project.

use predicates::prelude::*;
use std::fs;

use crate::common::{apb, sandbox_config_dir};

fn journal(version: &str, retry: bool, outcome: &str) -> String {
    let retry = if retry {
        r#"{"seq":2,"ts":1790000001000,"type":"retry_started","node":"w","attempt":2}
"#
    } else {
        ""
    };
    format!(
        r#"{{"seq":0,"ts":1790000000000,"type":"run_started","playbook":"demo","version":"{version}"}}
{{"seq":1,"ts":1790000000000,"type":"node_started","node":"w","attempt":1}}
{retry}{{"seq":3,"ts":1790000004000,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[]}}
{{"seq":4,"ts":1790000004000,"type":"goal_checked","index":0,"description":"tests pass","check":"script","status":"{status}"}}
{{"seq":5,"ts":1790000005000,"type":"run_finished","outcome":"{outcome}"}}
"#,
        status = if outcome == "succeeded" {
            "passed"
        } else {
            "failed"
        }
    )
}

/// A project with four runs of `demo`: two of 1.0.0 (one with a retry), two
/// of 1.1.0 (one failed), each stamped as created by the sandboxed install.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let cfg = sandbox_config_dir();
    let key = cfg.join("run-origin.key");
    if !key.exists() {
        fs::write(&key, "k".repeat(64)).unwrap();
    }
    for (id, version, retry, outcome) in [
        ("demo-1", "1.0.0", false, "succeeded"),
        ("demo-2", "1.0.0", true, "succeeded"),
        ("demo-3", "1.1.0", false, "succeeded"),
        ("demo-4", "1.1.0", false, "failed"),
    ] {
        let run = dir.path().join(".apb/runs").join(id);
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("events.jsonl"), journal(version, retry, outcome)).unwrap();
        apb_core::run_origin::stamp_in(cfg, &run, id).unwrap();
    }
    dir
}

#[test]
fn stats_prints_counts_next_to_rates_per_version() {
    let dir = project();
    apb()
        .args(["stats", "--playbook", "demo"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("4 runs\n"))
        .stdout(predicate::str::contains(
            "demo 1.0.0: 2 runs\n  note: 2 runs: fewer than 10, the rates are indicative only\n  outcome: 2/2 (100%) succeeded",
        ))
        .stdout(predicate::str::contains("  first pass: 1/2 (50%)\n"))
        .stdout(predicate::str::contains(
            "  goal 1: tests pass (script): passed 1/2 (50%)\n",
        ));
}

#[test]
fn stats_compares_two_versions_and_prints_json() {
    let dir = project();
    let out = apb()
        .args([
            "stats",
            "--playbook",
            "demo",
            "--compare",
            "1.0.0",
            "--json",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["runs"], 4);
    assert_eq!(v["compare"]["against"], "1.1.0");
    assert_eq!(v["compare"]["success_delta"], -0.5);
    assert_eq!(v["compare"]["first_pass_delta"], 0.0);
    // Nothing matches: a note, still a success.
    apb()
        .args(["stats", "--playbook", "none"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout("no runs recorded\n");
}

#[test]
fn stats_refuses_compare_without_a_playbook_and_a_bad_since() {
    let dir = project();
    apb()
        .args(["stats", "--compare", "1.0.0"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--compare needs --playbook"));
    apb()
        .args(["stats", "--since", "soon"])
        .current_dir(dir.path())
        .assert()
        .code(2);
}

/// A run that fails at start (here: a profile agent with no invocation form,
/// refused while the manifest is built) still gets `run_started` and the
/// origin stamp, so `apb stats` counts it as a failed run of its playbook
/// instead of skipping it as a directory from elsewhere.
#[test]
fn stats_counts_a_run_that_failed_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb().arg("init").current_dir(root).assert().success();
    let v = root.join(".apb/playbooks/early/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(
        v.join("playbook.yaml"),
        "schema: 2\nid: early\nname: Early\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: \"Work\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/early/current"), "1.0.0").unwrap();
    let p = root.join(".apb/profiles/main");
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("profile.yaml"),
        "name: main\ndescription: d\nexecutor:\n  agent: pi\n  model: any\n",
    )
    .unwrap();
    fs::write(p.join("SOUL.md"), "Work.").unwrap();
    apb()
        .args(["run", "early"])
        .current_dir(root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("no invocation for agent `pi`"));
    apb()
        .args(["stats", "--playbook", "early"])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains("early 1.0.0: 1 runs"))
        .stdout(predicate::str::contains("outcome: 0/1 (0%) succeeded"))
        .stdout(predicate::str::contains("skipped").not());
}
