//! `apb runs prune` (routine health D3): a dry run by default, a person's
//! command with `--yes`, and `apb runs <id>` unchanged beside it.

use predicates::prelude::*;
use std::fs;
use std::path::Path;

use crate::common::{apb, sandbox_config_dir};

/// A project with 21 old successful runs of `demo`: with the defaults
/// (the newest 20 and the latest success stay) exactly the oldest goes.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let cfg = sandbox_config_dir();
    let key = cfg.join("run-origin.key");
    if !key.exists() {
        fs::write(&key, "k".repeat(64)).unwrap();
    }
    for i in 0..21u64 {
        let id = format!("demo-{}", 1_790_000_000_000u64 + i);
        let ts = 1_790_000_000_000u64 + i * 1000;
        let run = dir.path().join(".apb/runs").join(&id);
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("events.jsonl"),
            format!(
                "{{\"seq\":0,\"ts\":{ts},\"type\":\"run_started\",\"playbook\":\"demo\",\"version\":\"1.0.0\"}}\n{{\"seq\":1,\"ts\":{ts},\"type\":\"run_provenance\",\"origin\":\"project\"}}\n{{\"seq\":2,\"ts\":{},\"type\":\"run_finished\",\"outcome\":\"succeeded\"}}\n",
                ts + 500
            ),
        )
        .unwrap();
        apb_core::run_origin::stamp_in(cfg, &run, &id).unwrap();
    }
    dir
}

const OLDEST: &str = "demo-1790000000000";

fn tree(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p.display().to_string());
        }
    }
    out.sort();
    out
}

#[test]
fn prune_without_yes_writes_nothing() {
    let dir = project();
    let before = tree(&dir.path().join(".apb/runs"));
    apb()
        .args(["runs", "prune"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("remove  {OLDEST}")))
        .stdout(predicate::str::contains("1 runs would be removed"))
        .stdout(predicate::str::contains("run again with --yes"));
    assert_eq!(tree(&dir.path().join(".apb/runs")), before);
    assert!(!dir.path().join(".apb/runs-archive").exists());
}

#[test]
fn prune_yes_in_a_run_step_is_refused() {
    let dir = project();
    apb()
        .args(["runs", "prune", "--yes"])
        .env("APB_RUN_ID", "x")
        .current_dir(dir.path())
        .assert()
        .code(2);
    assert!(dir.path().join(".apb/runs").join(OLDEST).is_dir());
}

#[test]
fn prune_yes_removes_and_reports() {
    let dir = project();
    apb()
        .args(["runs", "prune", "--yes"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1 runs"))
        .stdout(predicate::str::contains("kept 20"));
    assert!(!dir.path().join(".apb/runs").join(OLDEST).exists());
    assert!(
        dir.path()
            .join(".apb/runs-archive")
            .join(format!("{OLDEST}.json"))
            .is_file()
    );
}

#[test]
fn prune_json_prints_the_rows() {
    let dir = project();
    let out = apb()
        .args(["runs", "prune", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 21);
    let oldest = rows.iter().find(|r| r["run_id"] == OLDEST).unwrap();
    assert_eq!(oldest["action"]["action"], "remove");
}

#[test]
fn apb_runs_with_a_run_id_still_works() {
    let dir = project();
    apb()
        .args(["runs", OLDEST])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(OLDEST));
}
