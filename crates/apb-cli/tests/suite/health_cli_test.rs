//! `apb health` (routine health D2): the state, a check that exits 3 on
//! hold, a person's clear and mute, and the surfaces that show it.

use predicates::prelude::*;
use std::fs;
use std::path::Path;

use crate::common::{apb, sandbox_config_dir};

const DAY: u64 = 86_400_000;

/// A project with playbook `r` carrying `health`, and `failed` failed
/// runs of it stamped as created by the sandboxed install.
fn project(health: &str, failed: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/r/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        format!(
            "schema: 2\nid: r\nname: r\nversion: 1.0.0\n{health}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: note, type: prompt, prompt: \"hi\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: note }}\n  - {{ from: note, to: done }}\n"
        ),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/r/current"), "1.0.0").unwrap();
    let now = apb_core::clock::now_ms_u64();
    for i in 0..failed as u64 {
        seed_run(
            root,
            &format!("r-{}", now - DAY + i),
            now - DAY + i * 1000,
            "failed",
        );
    }
    dir
}

fn seed_run(root: &Path, id: &str, ts: u64, outcome: &str) {
    let run = root.join(".apb/runs").join(id);
    fs::create_dir_all(&run).unwrap();
    fs::write(
        run.join("events.jsonl"),
        format!(
            "{{\"seq\":1,\"ts\":{ts},\"type\":\"run_started\",\"playbook\":\"r\",\"version\":\"1.0.0\"}}\n{{\"seq\":2,\"ts\":{ts},\"type\":\"run_provenance\",\"origin\":\"project\"}}\n{{\"seq\":3,\"ts\":{},\"type\":\"run_finished\",\"outcome\":\"{outcome}\"}}\n",
            ts + 10
        ),
    )
    .unwrap();
    apb_core::run_origin::stamp_in(sandbox_config_dir(), &run, id).unwrap();
}

const HOLD: &str = "health:\n  consecutive_failures: 2\n  on_trip: hold\n";

#[test]
fn a_held_playbook_is_refused_by_apb_run() {
    let dir = project(HOLD, 2);
    apb()
        .args(["health", "check", "r"])
        .current_dir(dir.path())
        .assert()
        .code(3);
    apb()
        .args(["run", "r"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("playbook_on_hold"));
}

#[test]
fn check_exits_3_on_hold() {
    let dir = project(HOLD, 2);
    apb()
        .args(["health", "check", "r"])
        .current_dir(dir.path())
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "health: r: consecutive_failures tripped",
        ));
    apb()
        .args(["health", "r"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("r  held"));
}

#[test]
fn check_exits_0_when_tripped_but_not_held() {
    let dir = project("health:\n  consecutive_failures: 2\n", 2);
    apb()
        .args(["health", "check"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("consecutive_failures tripped"));
}

#[test]
fn clear_in_a_run_step_is_refused() {
    let dir = project(HOLD, 2);
    apb()
        .args(["health", "check", "r"])
        .current_dir(dir.path())
        .assert()
        .code(3);
    apb()
        .args(["health", "clear", "r"])
        .env("APB_RUN_ID", "x")
        .current_dir(dir.path())
        .assert()
        .code(2);
    apb()
        .args(["health", "r"])
        .current_dir(dir.path())
        .assert()
        .stdout(predicate::str::contains("r  held"));
    apb()
        .args(["health", "clear", "r"])
        .current_dir(dir.path())
        .assert()
        .success();
    apb()
        .args(["health", "check", "r"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn mute_until_silences_notices() {
    let dir = project("health:\n  consecutive_failures: 2\n", 2);
    apb()
        .args(["health", "mute", "r", "--until", "2h"])
        .current_dir(dir.path())
        .assert()
        .success();
    let out = apb()
        .args(["health", "r", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v[0]["muted_until"].is_string(), "{v}");
    apb()
        .args(["health", "unmute", "r"])
        .current_dir(dir.path())
        .assert()
        .success();
    let out = apb()
        .args(["health", "r", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v[0].get("muted_until").is_none(), "{v}");
}

#[test]
fn doctor_names_tripped_playbooks() {
    let dir = project(HOLD, 2);
    apb()
        .args(["health", "check", "r"])
        .current_dir(dir.path())
        .assert()
        .code(3);
    apb()
        .args(["doctor"])
        .current_dir(dir.path())
        .assert()
        .stdout(predicate::str::contains("1 tripped (r)"))
        .stdout(predicate::str::contains("1 on hold (r)"));
}

#[test]
fn health_without_a_block_says_unwatched() {
    let dir = project("", 0);
    apb()
        .args(["health", "r"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("r  unwatched"));
}

#[test]
fn stats_shows_health_without_runs() {
    let dir = project(HOLD, 2);
    apb()
        .args(["health", "check", "r"])
        .current_dir(dir.path())
        .assert()
        .code(3);
    // A window with no run: the report says so, and still names the held
    // routine first.
    apb()
        .args(["stats", "--since", "2099-01-01"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "health: r held (consecutive_failures)",
        ))
        .stdout(predicate::str::contains("no runs recorded"));
}
