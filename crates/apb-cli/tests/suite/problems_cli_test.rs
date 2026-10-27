//! `apb problems`: the list in text and JSON, a scan of earlier runs, a
//! person's confirm, dismiss and reopen, a recheck that shows the new
//! passages of a changed source, a close that quotes one, `--holds`, and a
//! run step that may look but not write.

use predicates::prelude::*;
use std::fs;
use std::path::Path;

use crate::common::{apb, sandbox_config_dir};

const EVENTS: &str = include_str!("../../../apb-engine/tests/fixtures/known_problems/events.jsonl");
const YAML: &str = "schema: 2\nid: fix-build\nname: Fix build\nversion: 1.0.0\nknown_problems: { min_runs: 2 }\nnodes:\n  - { id: start, type: start }\n  - { id: fix, type: agent_task, prompt: \"Fix the build\", profile: main }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: fix }\n  - { from: fix, to: done }\n";
const FIXED: &str = "cc -Wno-error=deprecated -c vendor/zlib.c";

/// A project with `fix-build`, its build script and two ended runs that
/// reported the same problems, stamped as created by the sandboxed install.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let cfg = sandbox_config_dir();
    let key = cfg.join("run-origin.key");
    if !key.exists() {
        fs::write(&key, "k".repeat(64)).unwrap();
    }
    let v = root.join(".apb/playbooks/fix-build/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), YAML).unwrap();
    fs::write(root.join(".apb/playbooks/fix-build/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::write(
        root.join("scripts/build.sh"),
        "#!/bin/sh\nset -e\ncc -Werror=deprecated -c vendor/zlib.c\n",
    )
    .unwrap();
    for id in ["fix-build-1", "fix-build-2"] {
        let run = root.join(".apb/runs").join(id);
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("events.jsonl"), EVENTS).unwrap();
        fs::write(run.join("playbook.yaml"), YAML).unwrap();
        apb_core::run_origin::stamp_in(cfg, &run, id).unwrap();
    }
    dir
}

fn view(root: &Path) -> serde_json::Value {
    let out = apb()
        .args(["problems", "fix-build", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The build-script problem's id.
fn build_problem(root: &Path) -> String {
    view(root)["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["scope"]["kind"] == "path")
        .expect("the build-script problem")["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn ok(root: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    apb().args(args).current_dir(root).assert().success()
}

#[test]
fn problems_are_scanned_listed_decided_and_closed_with_a_quote() {
    let dir = project();
    let root = dir.path();
    ok(root, &["problems", "fix-build"]).stdout(predicate::str::contains(
        "fix-build (project): no known problems (apb problems fix-build scan collects earlier runs)",
    ));
    ok(root, &["problems", "fix-build", "scan"]).stdout(predicate::str::contains(
        "fix-build: looked at 2 ended runs",
    ));
    ok(root, &["problems", "fix-build"])
        .stdout(predicate::str::contains("known problems,"))
        .stdout(predicate::str::contains("path:scripts/build.sh"))
        .stdout(predicate::str::contains("unchanged"));
    let id = build_problem(root);

    ok(
        root,
        &[
            "problems",
            "fix-build",
            "confirm",
            &id,
            "--reason",
            "seen it",
        ],
    )
    .stdout(predicate::str::contains(format!("{id}: open, confirmed")));
    ok(root, &["problems", "fix-build", "dismiss", &id])
        .stdout(predicate::str::contains(format!("{id}: dismissed")));
    ok(root, &["problems", "fix-build", "reopen", &id])
        .stdout(predicate::str::contains(format!("{id}: open")));
    // Nothing changed yet.
    ok(root, &["problems", "fix-build", "recheck"]).stdout(predicate::str::contains(
        "fix-build: no open problem's source changed",
    ));

    fs::write(
        root.join("scripts/build.sh"),
        format!("#!/bin/sh\nset -e\n{FIXED}\n"),
    )
    .unwrap();
    ok(root, &["problems", "fix-build", "recheck"])
        .stdout(predicate::str::contains("source changed"))
        .stdout(predicate::str::contains(format!(
            "passage 1 (lines 3-3):\n{FIXED}\n"
        )));
    // A close needs --quote, and a quote of old text is refused.
    apb()
        .args(["problems", "fix-build", "close", &id])
        .current_dir(root)
        .assert()
        .failure();
    apb()
        .args([
            "problems",
            "fix-build",
            "close",
            &id,
            "--quote",
            "#!/bin/sh\nset -e",
        ])
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("quote_not_new"));
    ok(
        root,
        &["problems", "fix-build", "close", &id, "--quote", FIXED],
    )
    .stdout(predicate::str::contains(format!("{id}: closed")));
    let closed = view(root)["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id.as_str())
        .cloned()
        .unwrap();
    assert_eq!(closed["status"], "closed");
    assert_eq!(closed["check"]["quote"]["text"], FIXED);
}

#[test]
fn holds_is_recorded_and_a_model_recheck_needs_the_use_on() {
    let dir = project();
    let root = dir.path();
    ok(root, &["problems", "fix-build", "scan", "--last", "1"])
        .stdout(predicate::str::contains("looked at 1 ended runs"));
    let id = build_problem(root);
    ok(root, &["problems", "fix-build", "recheck", &id, "--holds"]).stdout(
        predicate::str::contains(format!("{id}: holds (person, holds)")),
    );
    apb()
        .args(["problems", "fix-build", "recheck", &id, "--model"])
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no decision model is on"));
    // --holds names one problem.
    apb()
        .args(["problems", "fix-build", "recheck", "--holds"])
        .current_dir(root)
        .assert()
        .failure();
}

#[test]
fn a_run_step_may_look_at_problems_but_not_write_them() {
    let dir = project();
    let root = dir.path();
    for args in [
        vec!["problems", "fix-build", "scan"],
        vec!["problems", "fix-build", "confirm", "kp-0000000000"],
        vec![
            "problems",
            "fix-build",
            "close",
            "kp-0000000000",
            "--quote",
            "a fix landed here",
        ],
        vec!["problems", "fix-build", "recheck", "--model"],
        vec![
            "problems",
            "fix-build",
            "recheck",
            "kp-0000000000",
            "--holds",
        ],
    ] {
        apb()
            .args(&args)
            .env("APB_RUN_ID", "r-step")
            .current_dir(root)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("run step"));
    }
    apb()
        .args(["problems", "fix-build"])
        .env("APB_HOST_TASK", "t-1")
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains("no known problems"));
    apb()
        .args(["problems", "../x"])
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not found"));
}
