//! Protected paths on an agent_task (C6): an attempt that changes a
//! protected file is rejected, the files are restored before the retry, and
//! the change is journaled.

use std::fs;
use std::path::Path;
use std::process::Command;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

const PLAYBOOK: &str = r#"
schema: 2
id: guard
name: Guard
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: fix, type: agent_task, prompt: "make the tests pass", max_retries: 1, protect: ["tests/**"] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: fix }
  - { from: fix, to: done }
"#;

/// Counts its invocations; records what `tests/spec.txt` held when it
/// started; on the first invocation weakens the spec, deletes one protected
/// file and adds another, plus a file nobody protects.
const AGENT: &str = r#"#!/bin/sh
n=$(cat .count 2>/dev/null || echo 0); n=$((n+1)); echo $n > .count
printf '%s|' "$(cat tests/spec.txt)" >> seen.txt
if [ "$n" -eq 1 ]; then
  echo "expect anything" > tests/spec.txt
  rm -f tests/old.txt
  echo new > tests/new.txt
  echo work > src.txt
fi
echo "done $n"
"#;

fn seed(root: &Path) -> String {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/guard/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/guard/current"), "1.0.0").unwrap();
    common::seed_main(root);
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(root.join("tests/spec.txt"), "expect 42\n").unwrap();
    fs::write(root.join("tests/old.txt"), "old\n").unwrap();
    let agent = root.join("agent.sh");
    common::write_sync(&agent, AGENT);
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    agent.to_string_lossy().into_owned()
}

fn run_guard(root: &Path, agent: &str) -> (RunStatus, Vec<Event>) {
    let (outcome, events, _) = run_guard_id(root, agent);
    (outcome, events)
}

fn run_guard_id(root: &Path, agent: &str) -> (RunStatus, Vec<Event>, String) {
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", agent);
    }
    let res = run(root, "guard", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    (res.outcome, events, res.run_id)
}

fn modified(events: &[Event]) -> Vec<(u32, Vec<(String, String)>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ProtectedPathsModified {
                attempt, changes, ..
            } => Some((
                *attempt,
                changes
                    .iter()
                    .map(|c| (c.path.clone(), c.change.clone()))
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn an_attempt_that_changes_a_protected_file_is_rejected_and_the_retry_starts_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let agent = seed(root);
    let (outcome, events, run_id) = run_guard_id(root, &agent);

    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(
        modified(&events),
        [(
            1,
            vec![
                ("tests/new.txt".into(), "added".into()),
                ("tests/old.txt".into(), "deleted".into()),
                ("tests/spec.txt".into(), "modified".into()),
            ]
        )]
    );
    // The first attempt failed with its report kept as rejected_output.
    let first = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                attempt: 1,
                status,
                rejected_output,
                ..
            } => Some((status.clone(), rejected_output.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(first.0, "failed");
    assert!(first.1.unwrap().contains("done 1"));
    // The second attempt saw the original spec: restored before the retry.
    assert_eq!(
        fs::read_to_string(root.join("seen.txt")).unwrap(),
        "expect 42|expect 42|"
    );
    assert_eq!(
        fs::read_to_string(root.join("tests/spec.txt")).unwrap(),
        "expect 42\n"
    );
    assert!(root.join("tests/old.txt").exists());
    assert!(!root.join("tests/new.txt").exists());
    // Unprotected work stays.
    assert!(root.join("src.txt").exists());
    // The snapshot copies are removed after each check.
    let store = root.join(".apb/runs").join(&run_id).join("protect/fix");
    let left: Vec<_> = fs::read_dir(&store)
        .map(|d| d.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn the_rejection_names_the_path_when_no_retry_is_left() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let agent = seed(root);
    let pb = root.join(".apb/playbooks/guard/1.0.0/playbook.yaml");
    fs::write(&pb, PLAYBOOK.replace("max_retries: 1", "max_retries: 0")).unwrap();
    let (outcome, events) = run_guard(root, &agent);
    assert_eq!(outcome, RunStatus::Failed);
    let output = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "fix" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(
        output.starts_with("protected path modified: tests/new.txt (and 2 more)"),
        "{output}"
    );
    // Restored even though nothing retries.
    assert_eq!(
        fs::read_to_string(root.join("tests/spec.txt")).unwrap(),
        "expect 42\n"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// On a git tree a path git ignores is never protected: build output under
/// a protected directory does not fail the attempt.
#[test]
fn a_git_ignored_path_is_not_protected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed(root);
    let agent = root.join("agent.sh");
    common::write_sync(
        &agent,
        "#!/bin/sh\nmkdir -p tests/cache && echo x > tests/cache/blob && echo ok\n",
    );
    fs::write(root.join(".gitignore"), ".apb/\ntests/cache/\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    let (outcome, events) = run_guard(root, &agent.to_string_lossy());
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(modified(&events).is_empty());
    assert!(root.join("tests/cache/blob").exists());
}

/// Without `protect` nothing is snapshotted and no event is journaled.
#[test]
fn a_node_without_protect_is_not_checked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let agent = seed(root);
    let pb = root.join(".apb/playbooks/guard/1.0.0/playbook.yaml");
    fs::write(&pb, PLAYBOOK.replace(", protect: [\"tests/**\"]", "")).unwrap();
    let (outcome, events) = run_guard(root, &agent);
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(modified(&events).is_empty());
    assert_eq!(
        fs::read_to_string(root.join("tests/spec.txt")).unwrap(),
        "expect anything\n"
    );
}
