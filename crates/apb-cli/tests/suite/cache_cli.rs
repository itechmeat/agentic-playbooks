//! `apb cache` CLI tests (Task 8, node-cache design): thin dispatch over the
//! already-tested `apb_core::cache::CacheStore`, so these exercise only the
//! CLI wiring - project-root resolution, plain-text output, and honest error
//! paths for a bad `--older-than`/`--max-size` value - plus the `apb run
//! --no-cache --refresh-cache` clap conflict.

use assert_cmd::Command;
use predicates::prelude::*;

fn apb() -> Command {
    Command::cargo_bin("apb").unwrap()
}

fn seeded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb().arg("init").current_dir(dir.path()).assert().success();
    dir
}

#[test]
fn cache_status_on_empty_store_reports_zero() {
    let dir = seeded();
    apb()
        .args(["cache", "status"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("records: 0"))
        .stdout(predicate::str::contains("objects: 0"));
}

#[test]
fn cache_clear_on_empty_store_confirms_and_exits_success() {
    let dir = seeded();
    apb()
        .args(["cache", "clear"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("cache cleared"));
}

#[test]
fn cache_inspect_missing_key_is_a_clean_error() {
    let dir = seeded();
    apb()
        .args(["cache", "inspect", "sha256:doesnotexist"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("sha256:doesnotexist"));
}

#[test]
fn cache_prune_with_bad_older_than_names_the_value_and_fails() {
    let dir = seeded();
    apb()
        .args(["cache", "prune", "--older-than", "not-a-duration"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not-a-duration"));
}

#[test]
fn cache_prune_with_bad_max_size_names_the_value_and_fails() {
    let dir = seeded();
    apb()
        .args(["cache", "prune", "--max-size", "not-a-size"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not-a-size"));
}

#[test]
fn cache_prune_with_no_flags_on_empty_store_reports_zero_removed() {
    let dir = seeded();
    apb()
        .args(["cache", "prune"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("removed records: 0"))
        .stdout(predicate::str::contains("removed objects: 0"));
}

#[test]
fn cache_command_without_project_fails_env() {
    let dir = tempfile::tempdir().unwrap();
    apb()
        .args(["cache", "status"])
        .current_dir(dir.path())
        .assert()
        .code(2);
}

#[test]
fn run_rejects_no_cache_and_refresh_cache_together() {
    // Clap's `conflicts_with` must reject this combination before the run
    // ever starts (no playbook needs to exist for this check to fire).
    let dir = seeded();
    apb()
        .args(["run", "ghost", "--no-cache", "--refresh-cache"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}
