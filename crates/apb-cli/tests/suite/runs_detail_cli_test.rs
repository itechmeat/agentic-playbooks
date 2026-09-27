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
