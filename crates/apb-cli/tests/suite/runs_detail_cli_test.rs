//! `apb runs` over a journal with token usage and an event a newer apb
//! wrote (issue #167): the table flags the skipped event, `apb runs <id>`
//! shows the run's totals and the same note.

use predicates::prelude::*;
use std::fs;

use crate::common::apb;

const JOURNAL: &str = concat!(
    r#"{"seq":0,"ts":1790000000000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
    "\n",
    r#"{"seq":1,"ts":1790000000001,"type":"node_started","node":"w","attempt":1}"#,
    "\n",
    r#"{"seq":2,"ts":1790000000002,"type":"attempt_finished","node":"w","attempt":1,"status":"succeeded","duration_ms":10,"session":null,"summary":"done","usage":{"input_tokens":1200,"output_tokens":300,"cache_read_tokens":5000,"cache_write_tokens":0,"cost_usd":0.0123,"source":"reported"}}"#,
    "\n",
    r#"{"seq":3,"ts":1790000000003,"type":"future_event","use":"completion_check","node":"w"}"#,
    "\n",
    r#"{"seq":4,"ts":1790000000004,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[]}"#,
    "\n",
    r#"{"seq":5,"ts":1790000000005,"type":"run_finished","outcome":"succeeded"}"#,
    "\n",
);

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run = dir.path().join(".apb/runs/demo-1");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("events.jsonl"), JOURNAL).unwrap();
    dir
}

#[test]
fn the_runs_table_flags_events_a_newer_apb_wrote() {
    let dir = project();
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "demo-1\tdemo\tsucceeded\t1 unknown event (newer apb?)",
        ));
}

#[test]
fn runs_with_an_id_shows_the_usage_totals_and_the_note() {
    let dir = project();
    apb()
        .args(["runs", "demo-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("demo-1\tdemo 1.0.0\tsucceeded"))
        .stdout(predicate::str::contains("  w\tsucceeded"))
        .stdout(predicate::str::contains(
            "  usage: 1200 input, 300 output, 5000 cache read, 0 cache write tokens over 1 attempt, $0.0123 reported",
        ))
        .stdout(predicate::str::contains("  1 unknown event (newer apb?)"));
}

#[test]
fn runs_with_an_unknown_id_fails() {
    let dir = project();
    apb()
        .args(["runs", "nope"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run `nope` not found"));
}

// --- 0.24.0: the execution mode in `apb runs` ---

/// A run whose manifest carries an execution block, next to the plain one.
fn project_with_host_run(execution: &str, extra_events: &str) -> tempfile::TempDir {
    let dir = project();
    let run = dir.path().join(".apb/runs/host-1");
    fs::create_dir_all(&run).unwrap();
    let fallback = if extra_events.is_empty() {
        String::new()
    } else {
        format!("{extra_events}\n")
    };
    let journal = format!(
        concat!(
            r#"{{"seq":0,"ts":1790000001000,"type":"run_started","playbook":"demo","version":"1.0.0"}}"#,
            "\n",
            r#"{{"seq":1,"ts":1790000001001,"type":"node_started","node":"w","attempt":1}}"#,
            "\n{}",
            r#"{{"seq":4,"ts":1790000001004,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[]}}"#,
            "\n",
            r#"{{"seq":5,"ts":1790000001005,"type":"run_finished","outcome":"succeeded"}}"#,
            "\n",
        ),
        fallback
    );
    fs::write(run.join("events.jsonl"), journal).unwrap();
    fs::write(
        run.join("manifest.yaml"),
        format!("profiles: []\nnode_bindings: {{}}\nexecution:\n{execution}"),
    )
    .unwrap();
    dir
}

#[test]
fn runs_with_an_id_names_the_host_execution_mode() {
    let dir = project_with_host_run(
        "  mode: host\n  source: argument\n  client: claude-code\n",
        "",
    );
    apb()
        .args(["runs", "host-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  execution: host (argument, client claude-code)\n",
        ));
    apb()
        .args(["runs", "demo-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("  execution: cli\n"));
}

#[test]
fn runs_with_an_id_names_the_nodes_that_fell_back_to_the_host() {
    let dir = project_with_host_run(
        "  mode: cli\n  source: default\n  client: claude-code\n  fallback_to_host: true\n",
        r#"{"seq":3,"ts":1790000001003,"type":"execution_fallback","node":"w","attempt":1,"reason":"spawn failed"}"#,
    );
    apb()
        .args(["runs", "host-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  execution: cli (default, client claude-code, host fallback allowed); host fallback: w\n",
        ));
}

#[test]
fn the_runs_table_gets_a_mode_column_only_when_a_run_has_an_execution_block() {
    let plain = project();
    apb()
        .arg("runs")
        .current_dir(plain.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "demo-1\tdemo\tsucceeded\t1 unknown event (newer apb?)\n",
        ));
    let dir = project_with_host_run("  mode: host\n  source: argument\n", "");
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("host-1\tdemo\tsucceeded\thost\n"))
        .stdout(predicate::str::contains("demo-1\tdemo\tsucceeded\tcli\t"));
}

const FALLBACK_EXECUTION: &str =
    "  mode: cli\n  source: default\n  client: claude-code\n  fallback_to_host: true\n";
const FALLBACK_EVENT: &str = r#"{"seq":3,"ts":1790000001003,"type":"execution_fallback","node":"w","attempt":1,"reason":"spawn failed"}"#;

/// A `cli` run with the host fallback shows as `cli+host-fallback` in the
/// table (R2).
#[test]
fn the_runs_table_marks_a_run_with_the_host_fallback() {
    let dir = project_with_host_run(FALLBACK_EXECUTION, FALLBACK_EVENT);
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "host-1\tdemo\tsucceeded\tcli+host-fallback\n",
        ));
}

/// `--json` of a fallback run carries the nodes that fell back, the list
/// the text line names (R1).
#[test]
fn runs_json_names_the_nodes_that_fell_back() {
    let dir = project_with_host_run(FALLBACK_EXECUTION, FALLBACK_EVENT);
    let out = apb()
        .args(["runs", "host-1", "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["execution"]["fallback_to_host"], true);
    assert_eq!(v["execution"]["fell_back"], serde_json::json!(["w"]));
}

/// `--json` needs a run id (R3).
#[test]
fn runs_json_without_an_id_is_a_usage_error() {
    let dir = project();
    apb()
        .args(["runs", "--json"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("<RUN_ID>"));
}

#[test]
fn runs_json_is_the_run_status_object() {
    let dir = project_with_host_run("  mode: host\n  source: argument\n", "");
    let out = apb()
        .args(["runs", "host-1", "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let expected = apb_mcp::tools::run::run_status(dir.path(), "host-1").unwrap();
    assert!(
        v["execution"].get("fell_back").is_none(),
        "nothing fell back"
    );
    // `driver_alive` reads the process table at request time; the rest is
    // the same fold of the same journal.
    let strip = |mut v: serde_json::Value| {
        v.as_object_mut().unwrap().remove("driver_alive");
        v
    };
    assert_eq!(strip(v.clone()), strip(expected));
    assert_eq!(v["execution"]["mode"], "host");
    assert_eq!(v["run_status"], "succeeded");
}

/// Issue #195: `apb runs <id>` lists what a fork's branch-failure policy did
/// and which joins refused.
#[test]
fn runs_with_an_id_lists_branch_failures() {
    let dir = project();
    let run = dir.path().join(".apb/runs/fork-1");
    fs::create_dir_all(&run).unwrap();
    let journal = [
        r#"{"seq":0,"ts":1790000002000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
        r#"{"seq":1,"ts":1790000002001,"type":"node_finished","node":"a","status":"failed","attempt":1,"output":"boom","artifacts":[]}"#,
        r#"{"seq":2,"ts":1790000002002,"type":"branch_failed","fork":"start","node":"a","policy":"fail_fast","target":"rejected"}"#,
        r#"{"seq":3,"ts":1790000002003,"type":"node_finished","node":"b","status":"cancelled","attempt":1,"output":"cancelled","artifacts":[]}"#,
        r#"{"seq":4,"ts":1790000002004,"type":"branch_cancelled","fork":"start","node":"b","failed_node":"a"}"#,
        r#"{"seq":5,"ts":1790000002005,"type":"run_finished","outcome":"failed"}"#,
    ];
    fs::write(run.join("events.jsonl"), journal.join("\n") + "\n").unwrap();
    apb()
        .args(["runs", "fork-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  branch_failed a: fork `start` fail_fast: failed, the run goes to `rejected`\n",
        ))
        .stdout(predicate::str::contains(
            "  branch_cancelled b: fork `start`: cancelled after `a` failed\n",
        ));
}
