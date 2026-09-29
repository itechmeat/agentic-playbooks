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

/// The `guard` playbook with its own `protect` list, retry budget and fake
/// agent body (a shell script run in the project root).
fn seed_with(root: &Path, protect: &str, retries: u32, body: &str) -> String {
    let agent = seed(root);
    let pb = root.join(".apb/playbooks/guard/1.0.0/playbook.yaml");
    fs::write(
        &pb,
        PLAYBOOK.replace(
            "max_retries: 1, protect: [\"tests/**\"]",
            &format!("max_retries: {retries}, protect: {protect}"),
        ),
    )
    .unwrap();
    common::write_sync(Path::new(&agent), &format!("#!/bin/sh\n{body}\n"));
    agent
}

fn restore_failed(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .flat_map(|e| match &e.payload {
            EventPayload::ProtectedPathsModified { restore_failed, .. } => restore_failed.clone(),
            _ => Vec::new(),
        })
        .collect()
}

/// A file git ignored before the attempt was never protected: an attempt
/// that stops ignoring it (edits `.gitignore`) does not get it removed as
/// "added".
#[test]
fn a_pre_existing_ignored_file_is_never_removed_when_the_ignore_rules_change() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let agent = seed_with(root, "[\"*.cfg\"]", 0, ": > .gitignore\necho ok");
    fs::write(root.join("main.cfg"), "tracked\n").unwrap();
    fs::write(root.join("local.cfg"), "my local settings\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\nlocal.cfg\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    let (outcome, events) = run_guard(root, &agent);
    assert_eq!(
        fs::read_to_string(root.join("local.cfg")).unwrap(),
        "my local settings\n"
    );
    assert!(modified(&events).is_empty(), "{:?}", modified(&events));
    assert_eq!(outcome, RunStatus::Succeeded);
}

/// A protected directory the attempt replaced with a symlink to a directory
/// outside the tree: the restore removes the link and recreates the real
/// directory, writing nothing into the outside one.
#[test]
fn a_symlinked_protected_dir_is_restored_without_writing_outside() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = outside.path().display();
    let agent = seed_with(
        root,
        "[\"tests/**\"]",
        0,
        &format!("rm -rf tests\nln -s '{out}' tests\necho ok"),
    );
    let (outcome, events) = run_guard(root, &agent);
    assert_eq!(outcome, RunStatus::Failed);
    let written: Vec<_> = fs::read_dir(outside.path()).unwrap().collect();
    assert!(written.is_empty(), "{written:?}");
    assert!(
        fs::symlink_metadata(root.join("tests"))
            .unwrap()
            .file_type()
            .is_dir()
    );
    assert_eq!(
        fs::read_to_string(root.join("tests/spec.txt")).unwrap(),
        "expect 42\n"
    );
    assert!(restore_failed(&events).is_empty());
}

/// A tracked protected path missing before the attempt, that resolves
/// through a planted directory symlink afterwards, is never read as added
/// and never deleted outside the tree.
#[test]
fn a_file_behind_a_symlinked_dir_is_never_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = outside.path().display();
    let agent = seed_with(
        root,
        "[\"tests/**\"]",
        0,
        &format!("rm -rf tests\nln -s '{out}' tests\necho ok"),
    );
    fs::write(root.join("tests/gone.txt"), "tracked\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    fs::remove_file(root.join("tests/gone.txt")).unwrap();
    fs::write(outside.path().join("gone.txt"), "precious\n").unwrap();
    let (_outcome, events) = run_guard(root, &agent);
    assert_eq!(
        fs::read_to_string(outside.path().join("gone.txt")).unwrap(),
        "precious\n"
    );
    assert!(
        !modified(&events)
            .iter()
            .any(|(_, c)| c.iter().any(|(p, _)| p == "tests/gone.txt")),
        "{:?}",
        modified(&events)
    );
}

/// A protected file the attempt replaced with a hardlink to a file outside
/// the tree: the restore replaces the link, the outside file keeps its
/// content.
#[test]
fn a_hardlinked_protected_file_is_restored_without_touching_the_other_file() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "outside content\n").unwrap();
    let agent = seed_with(
        root,
        "[\"tests/**\"]",
        0,
        &format!("ln -f '{}' tests/spec.txt\necho ok", secret.display()),
    );
    let (outcome, _) = run_guard(root, &agent);
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(fs::read_to_string(&secret).unwrap(), "outside content\n");
    assert_eq!(
        fs::read_to_string(root.join("tests/spec.txt")).unwrap(),
        "expect 42\n"
    );
}

/// An attempt that edits a protected file and the engine's stored copy of
/// it alike fails the node: the tampered copy is never written back and no
/// retry runs from the edited tree.
#[test]
fn a_tampered_snapshot_copy_fails_the_node() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let agent = seed_with(
        root,
        "[\"tests/**\"]",
        1,
        "n=$(cat .count 2>/dev/null || echo 0); n=$((n+1)); echo $n > .count\n\
         if [ \"$n\" -eq 1 ]; then\n\
           echo weak > tests/spec.txt\n\
           cp tests/spec.txt \"$APB_RUN_DIR/protect/fix/1/tests/spec.txt\"\n\
         fi\necho done $n",
    );
    let (outcome, events) = run_guard(root, &agent);
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(fs::read_to_string(root.join(".count")).unwrap(), "1\n");
    assert_eq!(restore_failed(&events), ["tests/spec.txt"]);
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
        output.contains("protected path snapshot was tampered with: tests/spec.txt"),
        "{output}"
    );
}

/// A path that could not be restored keeps the snapshot copies, and the
/// event names where they are.
#[test]
fn the_snapshot_copies_stay_when_a_restore_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let agent = seed_with(root, "[\"cfg/**\"]", 0, "rm -rf cfg\necho x > cfg\necho ok");
    fs::create_dir_all(root.join("cfg")).unwrap();
    fs::write(root.join("cfg/a.cfg"), "setting\n").unwrap();
    let (outcome, events, run_id) = run_guard_id(root, &agent);
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(restore_failed(&events), ["cfg/a.cfg"]);
    let kept = root
        .join(".apb/runs")
        .join(&run_id)
        .join("protect/fix/1/cfg/a.cfg");
    assert_eq!(fs::read_to_string(&kept).unwrap(), "setting\n");
    let named = events.iter().find_map(|e| match &e.payload {
        EventPayload::ProtectedPathsModified { kept_copies, .. } => kept_copies.clone(),
        _ => None,
    });
    assert!(named.is_some_and(|k| k.ends_with("protect/fix/1")));
}

/// The protected-path listing never runs a `core.fsmonitor` hook named in
/// the repository's own config.
#[test]
fn the_protect_listing_never_runs_the_repository_fsmonitor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let agent = seed_with(root, "[\"tests/**\"]", 0, "echo ok");
    fs::write(root.join(".gitignore"), ".apb/\nmarker\nhook.sh\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    let hook = root.join("hook.sh");
    common::write_sync(
        &hook,
        &format!("#!/bin/sh\ntouch '{}'\n", root.join("marker").display()),
    );
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(root, &["config", "core.fsmonitor", &hook.to_string_lossy()]);
    let (outcome, _) = run_guard(root, &agent);
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(!root.join("marker").exists());
}
