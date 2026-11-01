//! `apb version list|use|history`: the versions with the current one
//! marked, a person's move with its expectation and reason, the history in
//! text and JSON, the global scope, the refusal inside a run step, a
//! hand-edited record shown sanitized, and refusals under `--json`.

use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::apb;

const YAML: &str = "schema: 2\nid: demo\nname: Demo\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

/// `demo` with versions 1.0.0 and 1.1.0 under the definition parent
/// `parent`, `current` at 1.0.0.
fn seed(parent: &Path) -> PathBuf {
    let dir = parent.join("playbooks/demo");
    for v in ["1.0.0", "1.1.0"] {
        fs::create_dir_all(dir.join(v)).unwrap();
        fs::write(dir.join(v).join("playbook.yaml"), YAML).unwrap();
    }
    fs::write(dir.join("current"), "1.0.0").unwrap();
    dir
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(t.path()).unwrap();
    let dir = seed(&t.path().join(".apb"));
    (t, dir)
}

#[test]
fn a_person_lists_moves_and_reads_the_history() {
    let (t, dir) = project();
    apb()
        .args(["version", "list", "demo"])
        .current_dir(t.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("1.0.0  (current)"));
    apb()
        .args(["version", "use", "demo", "1.1.0", "--expect", "1.0.0"])
        .args(["--reason", "the retry loops"])
        .current_dir(t.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "demo: current moved from 1.0.0 to 1.1.0",
        ));
    assert_eq!(fs::read_to_string(dir.join("current")).unwrap(), "1.1.0");

    // A move that expects what is no longer there is refused.
    apb()
        .args(["version", "use", "demo", "1.0.0", "--expect", "1.0.0"])
        .current_dir(t.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "the current version changed meanwhile: it is now 1.1.0",
        ));

    apb()
        .args(["version", "history", "demo"])
        .current_dir(t.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1.0.0 -> 1.1.0  by a person on the CLI  (the retry loops)",
        ))
        .stdout(predicate::str::contains("incomplete").not());
    let out = apb()
        .args(["version", "history", "demo", "--json"])
        .current_dir(t.path())
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["history"]["entries"][0]["to"], "1.1.0");
    assert_eq!(json["history"]["incomplete"], false);
}

#[test]
fn a_move_by_hand_outside_apb_reads_incomplete() {
    let (t, dir) = project();
    apb()
        .args(["version", "use", "demo", "1.1.0"])
        .current_dir(t.path())
        .assert()
        .success();
    fs::write(dir.join("current"), "1.0.0").unwrap();
    apb()
        .args(["version", "history", "demo"])
        .current_dir(t.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("incomplete: the record has a gap"));
}

#[test]
fn the_global_scope_moves_a_global_playbook() {
    let cfg = tempfile::tempdir().unwrap();
    let dir = seed(cfg.path());
    let t = tempfile::tempdir().unwrap();
    apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["version", "use", "demo", "1.1.0", "--scope", "global"])
        .current_dir(t.path())
        .assert()
        .success();
    assert_eq!(fs::read_to_string(dir.join("current")).unwrap(), "1.1.0");
    // Not a project playbook here.
    apb()
        .args(["version", "history", "demo"])
        .current_dir(t.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("playbook `demo` not found"));
}

#[test]
fn a_run_step_may_not_move_current() {
    let (t, dir) = project();
    apb()
        .env("APB_RUN_ID", "r1")
        .args(["version", "use", "demo", "1.1.0"])
        .current_dir(t.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("APB_RUN_ID is set"));
    assert_eq!(fs::read_to_string(dir.join("current")).unwrap(), "1.0.0");
    assert!(!dir.join("current-history.jsonl").exists());
}

#[test]
fn a_reason_reaches_the_history_in_text_and_json() {
    let (t, _dir) = project();
    apb()
        .args([
            "version",
            "use",
            "demo",
            "1.1.0",
            "--reason",
            "after the leak",
        ])
        .current_dir(t.path())
        .assert()
        .success();
    for args in [
        &["version", "history", "demo"][..],
        &["version", "history", "demo", "--json"][..],
    ] {
        apb()
            .args(args)
            .current_dir(t.path())
            .assert()
            .success()
            .stdout(predicate::str::contains("after the leak"));
    }
}

#[test]
fn a_hand_edited_record_never_reaches_the_terminal_raw() {
    let (t, dir) = project();
    apb()
        .args(["version", "use", "demo", "1.1.0"])
        .current_dir(t.path())
        .assert()
        .success();
    let path = dir.join("current-history.jsonl");
    let mut line: serde_json::Value =
        serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
    line["to"] = "1.1.0\u{1b}[2J\nfake line".into();
    line["superseded_candidate"] = "x\u{1b}]0;title\u{7}".into();
    fs::write(&path, format!("{line}\n")).unwrap();
    let out = apb()
        .args(["version", "history", "demo"])
        .current_dir(t.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{7}'),
        "{text:?}"
    );
    assert!(!text.contains("\nfake line"), "{text:?}");
    // A tampered pointer is no current version.
    fs::write(dir.join("current"), "../x").unwrap();
    apb()
        .args(["version", "list", "demo", "--json"])
        .current_dir(t.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"current\": null"));
}

#[test]
fn json_refusals_are_json_on_stdout() {
    let (t, _dir) = project();
    for args in [
        &["version", "list", "nope", "--json"][..],
        &["version", "history", "nope", "--json"][..],
        &["version", "use", "nope", "1.0.0", "--json"][..],
    ] {
        let out = apb().args(args).current_dir(t.path()).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(body["error"], "not_found", "{args:?}");
    }
}
