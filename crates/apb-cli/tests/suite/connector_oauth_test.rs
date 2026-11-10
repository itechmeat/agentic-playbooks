//! `apb connector oauth` refusals (D1): the command is a person's terminal
//! surface. Inside a run step, or without a terminal, it refuses before it
//! contacts anything, with exit code 2. The happy path needs a browser and
//! is not tested here.

use std::fs;
use std::path::Path;

fn playbook_env(dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = crate::common::apb_std();
    cmd.args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env_remove("APB_RUN_ID")
        .env_remove("APB_HOST_TASK")
        .env_remove("APB_RUN_DIR")
        .env_remove("APB_NODE_ID");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn setup(dir: &Path) {
    fs::create_dir_all(dir.join("cfg")).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
}

#[test]
fn refused_inside_a_run() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let out = playbook_env(
        dir.path(),
        &["connector", "oauth", "some-connector"],
        &[("APB_RUN_ID", "run-123")],
    );
    assert_eq!(out.status.code(), Some(2), "{:?}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("only a person grants access"), "{stderr}");
    assert!(stderr.contains("run-123"), "{stderr}");
    assert!(out.stdout.is_empty(), "nothing is printed to stdout");
}

#[test]
fn refused_inside_a_host_task() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let out = playbook_env(
        dir.path(),
        &["connector", "oauth", "some-connector"],
        &[("APB_HOST_TASK", "task-7")],
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("only a person grants access"), "{stderr}");
    assert!(stderr.contains("task-7"), "{stderr}");
}

/// The test harness has no terminal: the command must refuse with the
/// terminal message before it loads anything.
#[test]
fn refused_without_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let out = playbook_env(dir.path(), &["connector", "oauth", "some-connector"], &[]);
    assert_eq!(out.status.code(), Some(2), "{:?}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("needs a terminal"), "{stderr}");
    assert!(out.stdout.is_empty());
}
