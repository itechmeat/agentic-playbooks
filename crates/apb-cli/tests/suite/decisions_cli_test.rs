//! `apb runs <id>`, `apb wait` and `apb decisions` over journals with
//! decisions (issue #165 Parts 4 and 13). A journal without decisions prints
//! exactly what it did before.

use predicates::prelude::*;
use std::fs;

use crate::common::apb;

const RUN_START: &str = concat!(
    r#"{"seq":0,"ts":1790000000000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
    "\n",
    r#"{"seq":1,"ts":1790000000001,"type":"node_started","node":"w","attempt":1}"#,
    "\n",
);

const DECISIONS: &str = concat!(
    r#"{"seq":2,"ts":1790000000002,"type":"decision_made","use_site":"completion_check","node":"w","attempt":1,"provider":"main","model":"jev-1.13.0","calibrated":true,"mode":"shadow","questions_digest":"sha256:q","state_digest":"sha256:s","state_bytes":10,"output_chars":40,"answers":{"final_result":{"p":0.1}},"applied":false,"would_change":true,"latency_ms":190,"cost_usd":0.0004,"cached":false,"error":null}"#,
    "\n",
    r#"{"seq":3,"ts":1790000000003,"type":"decision_made","use_site":"completion_check","node":"w","attempt":1,"provider":"main","model":"jev-1.13.0","calibrated":true,"mode":"shadow","questions_digest":"sha256:q","state_digest":"sha256:s","state_bytes":10,"answers":{"final_result":{"p":0.1}},"applied":false,"would_change":true,"latency_ms":0,"cached":true,"error":null}"#,
    "\n",
    r#"{"seq":4,"ts":1790000000004,"type":"decision_made","use_site":"completion_check","node":"w","attempt":1,"provider":"main","calibrated":false,"mode":"shadow","questions_digest":"sha256:q","state_digest":"sha256:s","state_bytes":10,"answers":{},"applied":false,"latency_ms":3000,"cached":false,"error":"timeout"}"#,
    "\n",
);

const RUN_END: &str = concat!(
    r#"{"seq":5,"ts":1790000000005,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[]}"#,
    "\n",
    r#"{"seq":6,"ts":1790000000006,"type":"run_finished","outcome":"succeeded"}"#,
    "\n",
);

fn project(with_decisions: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run = dir.path().join(".apb/runs/demo-1");
    fs::create_dir_all(&run).unwrap();
    let decisions = if with_decisions { DECISIONS } else { "" };
    fs::write(
        run.join("events.jsonl"),
        format!("{RUN_START}{decisions}{RUN_END}"),
    )
    .unwrap();
    dir
}

const LINE: &str =
    "  decisions: 3 (1 replayed, 1 error), $0.0004, p50 190 ms; shadow would change: 2";

#[test]
fn runs_and_wait_print_one_decisions_line() {
    let dir = project(true);
    apb()
        .args(["runs", "demo-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{LINE}\n")));
    apb()
        .args(["wait", "demo-1", "--timeout", "5"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(format!("run demo-1 finished: succeeded\n{LINE}\n"));
}

#[test]
fn a_run_without_decisions_prints_as_before() {
    let dir = project(false);
    apb()
        .args(["runs", "demo-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout("demo-1\tdemo 1.0.0\tsucceeded\n  w\tsucceeded\n");
    apb()
        .args(["wait", "demo-1", "--timeout", "5"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout("run demo-1 finished: succeeded\n");
}

#[test]
fn the_report_says_when_nothing_was_recorded_and_reads_journals() {
    let empty = project(false);
    let cfg = tempfile::tempdir().unwrap();
    apb()
        .args(["decisions", "report"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(empty.path())
        .assert()
        .success()
        .stdout("no decisions recorded\n");
    let dir = project(true);
    let journal = dir.path().join(".apb/runs/demo-1/events.jsonl");
    let before = fs::read(&journal).unwrap();
    let out = apb()
        .args(["decisions", "report", "--json", "--use", "completion_check"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["decisions"], 3);
    assert_eq!(v["groups"][0]["use"], "completion_check");
    assert_eq!(v["groups"][0]["errors"], 1);
    // The only node is the last one and the run succeeded: labelled keep.
    assert_eq!(v["groups"][0]["keep_labels"], 2);
    assert_eq!(v["groups"][0]["eligible"], false);
    assert_eq!(
        fs::read(&journal).unwrap(),
        before,
        "the report writes nothing"
    );
    apb()
        .args(["decisions", "report", "--since", "tomorrow"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .code(2);
}

#[test]
fn thresholds_round_trip_for_the_exact_model_only() {
    let dir = project(false);
    let cfg = tempfile::tempdir().unwrap();
    let set = |model: &str, t: &str| {
        apb()
            .args([
                "decisions",
                "thresholds",
                "set",
                "--use",
                "completion_check",
                "--provider",
                "main",
                "--model",
                model,
                "--threshold",
                t,
            ])
            .env("APB_CONFIG_DIR", cfg.path())
            .current_dir(dir.path())
            .assert()
    };
    set("jev-1.13.0", "0.15").success();
    set("jev-1.13.0", "0.12").success();
    set("jev-1.13.0", "2").code(2);
    apb()
        .args(["decisions", "thresholds", "list"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout("completion_check\tmain/jev-1.13.0\t0.12\n");
    assert_eq!(
        apb_core::decision_thresholds::stored_threshold_in(
            cfg.path(),
            "completion_check",
            "main",
            "jev-1.14.0"
        ),
        None
    );
}

#[test]
fn replay_refuses_without_a_provider() {
    let dir = project(true);
    let cfg = tempfile::tempdir().unwrap();
    apb()
        .args(["decisions", "replay"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("replay needs --provider <id>"));
}

/// A run of `project(true)` with the debug state of its first decision, and
/// a machine `decisions.yaml` whose only provider is a hosted fake.
fn replayable(project_config: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = project(true);
    let debug = dir.path().join(".apb/runs/demo-1/decisions");
    fs::create_dir_all(&debug).unwrap();
    fs::write(
        debug.join("2.json"),
        r#"{"seq":2,"state":{"result":"done","meta":{}},"state_order":["result","meta"],"questions":{"final_result":{"type":"noul","instructions":"done?"}},"answers":null}"#,
    )
    .unwrap();
    fs::write(dir.path().join(".apb/config.yaml"), project_config).unwrap();
    let cfg = tempfile::tempdir().unwrap();
    fs::write(
        cfg.path().join("decisions.yaml"),
        "mode: shadow\nproviders:\n  - id: hosted\n    kind: fake\n    answers:\n      final_result: { type: noul, noul: 0.9 }\nuses:\n  completion_check: { mode: shadow }\n",
    )
    .unwrap();
    (dir, cfg)
}

#[test]
fn replay_honours_the_projects_narrowing_and_the_kill_switch() {
    let run = |project_config: &str, kill: bool| {
        let (dir, cfg) = replayable(project_config);
        let mut cmd = apb();
        cmd.args(["decisions", "replay", "--provider", "hosted"])
            .env("APB_CONFIG_DIR", cfg.path())
            .env("APB_DECISIONS_ALLOW_FAKE", "1")
            .current_dir(dir.path());
        if kill {
            cmd.env("APB_DECISIONS", "off");
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (code, stdout, _) = run("", false);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("replayed 1 of"), "{stdout}");
    for project in [
        "decisions:\n  data_class: local\n",
        "decisions:\n  enabled: false\n",
        "decisions:\n  send: [prompts]\n",
        "decisions:\n  uses: { completion_check: { mode: off } }\n",
    ] {
        let (code, stdout, stderr) = run(project, false);
        assert_eq!(code, Some(0), "{project}: {stderr}");
        assert!(stdout.contains("replayed 0 of"), "{project}: {stdout}");
        assert!(stdout.contains("skipped"), "{project}: {stdout}");
    }
    let (code, _, stderr) = run("", true);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("APB_DECISIONS=off"), "{stderr}");
}
