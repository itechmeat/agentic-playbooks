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

/// Stamps `demo-1` as created by the installation whose config dir is `cfg`
/// (the key an `apb` process creates at its entry point).
fn stamp_demo(cfg: &std::path::Path, project: &std::path::Path) {
    let key = cfg.join("run-origin.key");
    if !key.exists() {
        fs::write(&key, "k".repeat(64)).unwrap();
    }
    apb_core::run_origin::stamp_in(cfg, &project.join(".apb/runs/demo-1"), "demo-1").unwrap();
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
        // No decisions line: only the run's own lines, its route (this
        // synthetic journal names no definition) and its outcome.
        .stdout(concat!(
            "demo-1\tdemo 1.0.0\tsucceeded\n  execution: cli\n  w\tsucceeded\n",
            "  route:\n    definitions: 1 segment(s); not established: segment 0 from seq 0 (definition_not_journaled)\n",
            "  outcome: Run succeeded; nothing checked by the engine\n",
            "    Done:\n      succeeded: w\n",
        ));
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
    // A journal apb did not write here (no stamp, as a repository could ship
    // one) is not read: nothing is recorded, and the skip is named.
    apb()
        .args(["decisions", "report"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout("no decisions recorded\n")
        .stderr(predicate::str::contains(
            "skipped 1 run directory not created by apb on this machine",
        ));
    // The same run stamped with this installation's key is read.
    stamp_demo(cfg.path(), dir.path());
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
fn thresholds_are_set_replaced_range_checked_and_listed() {
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
    stamp_demo(cfg.path(), dir.path());
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

/// Card A8 through the binary: `apb decisions thresholds set --use
/// retry_advice` writes the v2 file under rule `exec/1` and says so, the
/// legacy file stays byte for byte, a stale `--rule` is refused, `list`
/// shows both files, and inside a run step (`APB_RUN_ID` or
/// `APB_HOST_TASK` set) the command exits 3 and writes nothing.
#[test]
fn retry_advice_thresholds_go_to_the_v2_file_and_a_run_step_is_refused() {
    use apb_core::decision_thresholds::{THRESHOLDS_FILE, THRESHOLDS_V2_FILE};
    let dir = project(false);
    let cfg = tempfile::tempdir().unwrap();
    let legacy = "version: 1\nthresholds:\n- use: retry_advice\n  provider: main\n  model: jev-1.13.0\n  threshold: 0.6\n";
    fs::write(cfg.path().join(THRESHOLDS_FILE), legacy).unwrap();
    let set = |use_site: &str, extra: &[&str], env: Option<(&str, &str)>| {
        let mut c = apb();
        c.args([
            "decisions",
            "thresholds",
            "set",
            "--use",
            use_site,
            "--provider",
            "main",
            "--model",
            "jev-1.13.0",
            "--threshold",
            "0.7",
        ])
        .args(extra)
        .env("APB_CONFIG_DIR", cfg.path())
        .env_remove("APB_RUN_ID")
        .env_remove("APB_HOST_TASK")
        .current_dir(dir.path());
        if let Some((k, v)) = env {
            c.env(k, v);
        }
        c.assert()
    };
    for step in [("APB_RUN_ID", "r-1"), ("APB_HOST_TASK", "w-1")] {
        for use_site in ["retry_advice", "completion_check"] {
            set(use_site, &[], Some(step))
                .code(3)
                .stderr(predicate::str::contains("refused inside"));
        }
    }
    assert!(!cfg.path().join(THRESHOLDS_V2_FILE).exists());
    assert_eq!(
        fs::read_to_string(cfg.path().join(THRESHOLDS_FILE)).unwrap(),
        legacy
    );
    set("retry_advice", &["--rule", "exec/0"], None)
        .code(2)
        .stderr(predicate::str::contains(
            "labels are measured under rule exec/1 now",
        ));
    set("retry_advice", &[], None)
        .success()
        .stdout(predicate::str::contains(format!(
            "under rule exec/1 in {THRESHOLDS_V2_FILE}"
        )));
    assert_eq!(
        fs::read_to_string(cfg.path().join(THRESHOLDS_FILE)).unwrap(),
        legacy,
        "the legacy file is byte-identical"
    );
    let v2 = fs::read_to_string(cfg.path().join(THRESHOLDS_V2_FILE)).unwrap();
    assert!(v2.contains("rule: exec/1"), "{v2}");
    apb()
        .args(["decisions", "thresholds", "list"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "retry_advice\tmain/jev-1.13.0\t0.6\tlegacy, not enforced: re-measure under rule exec/1\n",
        ))
        .stdout(predicate::str::contains(
            "retry_advice\tmain/jev-1.13.0\t0.7\trule exec/1\n",
        ));
    let out = apb()
        .args(["decisions", "thresholds", "list", "--json"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["enforced"], false, "the legacy entry: {rows:?}");
    assert_eq!(rows[0]["legacy"], true);
    assert!(rows[1].get("enforced").is_none(), "a v2 entry is unchanged");
}
