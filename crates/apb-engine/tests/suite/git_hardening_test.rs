//! apb's own git commands never run a program the working tree's
//! configuration, attributes or hooks name (decision record 0009): a
//! repository whose `core.fsmonitor`, `core.hooksPath`, hooks,
//! `diff.external`, a diff driver's `textconv` and `command` and a filter
//! driver's `clean`, `smudge` and `process` all point at a marker script
//! goes through a run (the tree lock's `git_path`, provenance), the
//! fingerprints the cache and the baseline read (with a dirty tracked file,
//! so the diff hashes worktree content), and the trial's worktree, status
//! and diff, and the marker never appears. A planted `commondir` that names
//! a config of the attacker's choosing is refused. The controls at the end
//! show each trap is live for a plain git.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use apb_core::git::{Repo, RepoError};
use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const PLAYBOOK: &str = r#"
schema: 2
id: hard
name: Hardened
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: commit, type: script, script: "scripts/commit.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: commit }
  - { from: commit, to: done }
"#;

/// The node's own commit: the script is not apb, so it switches the trap
/// off itself.
const SCRIPT: &str = r#"printf x > a.txt
g() { git -c core.fsmonitor=false -c core.hooksPath=/dev/null -c user.name=t \
  -c user.email=t@example.invalid -c commit.gpgsign=false \
  -c filter.evil.clean= -c filter.evil.smudge= -c filter.evil.process= \
  -c filter.evil.required=false "$@"; }
g add a.txt
g commit -q -m "node commit"
"#;

/// A plain git with an identity: only the setup and the controls use it.
fn plain(dir: &Path, args: &[&str]) -> std::process::ExitStatus {
    Command::new("git")
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
}

/// The marker script: records that it ran, then exits.
fn evil_script(dir: &Path, marker: &Path) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let evil = dir.join("evil.sh");
    fs::write(
        &evil,
        format!("#!/bin/sh\necho \"$0 $*\" >> '{}'\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&evil, fs::Permissions::from_mode(0o755)).unwrap();
    evil
}

/// A repository with `.gitattributes` naming the `evil` diff and filter
/// drivers for every path, a tracked file, and `.apb`/`.evil` ignored.
fn repo(root: &Path) {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join(".gitignore"), ".apb/\n.evil/\npwn/\n").unwrap();
    fs::write(root.join(".gitattributes"), "* diff=evil filter=evil\n").unwrap();
    fs::write(root.join("tracked.txt"), "one\n").unwrap();
    assert!(plain(root, &["init", "-q"]).success());
    assert!(plain(root, &["add", "."]).success());
    assert!(plain(root, &["commit", "-q", "-m", "base"]).success());
}

/// Arms the trap: every program the tree may name writes `marker`.
fn arm(root: &Path, marker: &Path) {
    let evil_dir = root.join(".evil");
    let evil = evil_script(&evil_dir, marker);
    let hooks = [
        "pre-commit",
        "post-commit",
        "post-checkout",
        "reference-transaction",
        "post-index-change",
        "fsmonitor-watchman",
    ];
    for dir in [root.join(".git/hooks"), evil_dir.join("hooks")] {
        fs::create_dir_all(&dir).unwrap();
        for h in hooks {
            fs::copy(&evil, dir.join(h)).unwrap();
        }
    }
    let ev = evil.to_string_lossy();
    let hooks_dir = evil_dir.join("hooks");
    for (k, v) in [
        ("core.fsmonitor", ev.as_ref()),
        ("core.hooksPath", hooks_dir.to_str().unwrap()),
        ("gpg.program", ev.as_ref()),
        ("log.showSignature", "true"),
        ("core.pager", ev.as_ref()),
        ("credential.helper", ev.as_ref()),
        ("core.askPass", ev.as_ref()),
        ("core.sshCommand", ev.as_ref()),
        ("diff.external", ev.as_ref()),
        ("diff.evil.textconv", ev.as_ref()),
        ("diff.evil.command", ev.as_ref()),
        ("filter.evil.clean", ev.as_ref()),
        ("filter.evil.smudge", ev.as_ref()),
        ("filter.evil.process", ev.as_ref()),
        ("filter.evil.required", "true"),
    ] {
        assert!(plain(root, &["config", k, v]).success());
    }
}

fn assert_never_ran(marker: &Path, step: &str) {
    assert!(
        !marker.exists(),
        "{step}: apb's git ran a program the tree names: {}",
        fs::read_to_string(marker).unwrap_or_default()
    );
}

#[test]
fn apb_git_never_runs_a_program_the_tree_names() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    init_project(&root).unwrap();
    let dir = root.join(".apb/playbooks/hard/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(dir.join("scripts/commit.sh"), SCRIPT).unwrap();
    fs::write(root.join(".apb/playbooks/hard/current"), "1.0.0").unwrap();
    repo(&root);
    let marker = tmp.path().join("ran");
    arm(&root, &marker);

    // A run: the tree lock and provenance read the tree.
    let res = run(&root, "hard", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ArtifactsCommitted { node, .. } if node == "commit")),
        "provenance read the tree"
    );
    assert_never_ran(&marker, "the run");

    // The cache and baseline fingerprint, with a dirty tracked file: the
    // diff hashes worktree content, where a clean filter and an external
    // diff would run.
    fs::write(root.join("tracked.txt"), "two\n").unwrap();
    assert!(apb_core::fingerprint::git_fingerprint(&root, &[]).is_some());
    let _ = apb_core::fingerprint::ignored_paths(&root);
    let _ = apb_core::workspace::fingerprint(&root);
    assert_never_ran(&marker, "the fingerprint");

    // The trial's path: a worktree checked out (smudge), changed, then
    // its status (clean) and diff (external diff, textconv) read.
    let scratch = tmp.path().join("trial");
    let added = apb_core::git::command(&root)
        .unwrap()
        .args(["worktree", "add", "--detach"])
        .arg(&scratch)
        .arg("HEAD")
        .output()
        .unwrap();
    assert!(added.status.success(), "worktree add failed");
    assert_never_ran(&marker, "the worktree checkout");
    fs::write(scratch.join("tracked.txt"), "three\n").unwrap();
    let status = apb_core::git::read(&scratch, &["status", "--porcelain"]).unwrap();
    assert!(status.contains("tracked.txt"));
    let diff = Repo::locate(&scratch)
        .unwrap()
        .diff(&scratch)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&diff.stdout).contains("three"));
    assert_never_ran(&marker, "the trial status and diff");

    // Controls: a plain git runs the tree's external diff and its hook.
    plain(&root, &["diff", "HEAD"]);
    assert!(marker.exists(), "the diff trap is live for a plain git");
    fs::remove_file(&marker).unwrap();
    plain(&root, &["commit", "-q", "--allow-empty", "-m", "control"]);
    assert!(marker.exists(), "the hook trap is live for a plain git");
}

/// The attacker's repository directory inside the tree: a config naming
/// the marker as external diff and clean filter, sharing the real objects
/// and refs.
fn planted_common(root: &Path, marker: &Path) -> std::path::PathBuf {
    let evil = evil_script(&root.join(".evil"), marker);
    let pwn = root.join("pwn");
    fs::create_dir_all(&pwn).unwrap();
    fs::write(
        pwn.join("config"),
        format!(
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\
             [diff]\n\texternal = {e}\n[filter \"evil\"]\n\tclean = {e}\n",
            e = evil.display()
        ),
    )
    .unwrap();
    for d in ["objects", "refs"] {
        std::os::unix::fs::symlink(root.join(".git").join(d), pwn.join(d)).unwrap();
    }
    pwn
}

#[test]
fn a_planted_commondir_never_hands_apb_another_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    repo(&root);
    let marker = tmp.path().join("ran");
    let pwn = planted_common(&root, &marker);
    fs::write(
        root.join(".git/commondir"),
        pwn.to_string_lossy().as_bytes(),
    )
    .unwrap();
    fs::write(root.join("tracked.txt"), "two\n").unwrap();

    assert!(matches!(
        Repo::locate(&root),
        Err(RepoError::Redirected { .. })
    ));
    assert_eq!(apb_core::fingerprint::git_fingerprint(&root, &[]), None);
    assert_eq!(
        apb_core::git::read(&root, &["rev-parse", "--git-common-dir"]),
        None
    );
    assert_never_ran(&marker, "a redirected repository");

    // Control: a plain git follows the planted file into the attacker's
    // config.
    plain(&root, &["diff", "HEAD"]);
    assert!(
        marker.exists(),
        "the planted commondir is live for a plain git"
    );

    // Twin: without the planted file the same repository is read.
    fs::remove_file(root.join(".git/commondir")).unwrap();
    fs::remove_file(&marker).unwrap();
    assert!(apb_core::fingerprint::git_fingerprint(&root, &[]).is_some());
    assert_never_ran(&marker, "the restored repository");
}

/// A filter only the operator's own global config defines (git-lfs) keeps
/// working: the neutralization covers the repository's own config only.
#[test]
fn a_filter_of_the_operators_global_config_still_applies() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".gitattributes"), "*.txt filter=up\n").unwrap();
    fs::write(root.join("a.txt"), "lower\n").unwrap();
    let global = tmp.path().join("gitconfig");
    fs::write(
        &global,
        "[filter \"up\"]\n\tsmudge = tr a-z A-Z\n\tclean = cat\n",
    )
    .unwrap();
    assert!(plain(&root, &["init", "-q"]).success());
    assert!(plain(&root, &["add", "."]).success());
    assert!(plain(&root, &["commit", "-q", "-m", "base"]).success());
    let scratch = tmp.path().join("wt");
    let added = apb_core::git::command(&root)
        .unwrap()
        .env("GIT_CONFIG_GLOBAL", &global)
        .args(["worktree", "add", "--detach"])
        .arg(&scratch)
        .arg("HEAD")
        .output()
        .unwrap();
    assert!(added.status.success());
    assert_eq!(
        fs::read_to_string(scratch.join("a.txt")).unwrap(),
        "LOWER\n"
    );
}

/// A submodule planted in the tree (a gitlink in the parent's index, its
/// own `.git/config` with a clean filter its `.gitattributes` names) never
/// runs that filter through apb's diff or status: git never starts inside a
/// submodule for apb. The filter is defined only in the submodule's config,
/// so the parent's neutralization cannot reach it. The control shows a
/// plain git runs it.
#[test]
fn a_planted_submodule_never_runs_its_own_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    repo(&root);
    let marker = tmp.path().join("ran");
    let evil = evil_script(&tmp.path().join("bin"), &marker);
    let sub = root.join("sub");
    fs::create_dir_all(&sub).unwrap();
    assert!(plain(&sub, &["init", "-q"]).success());
    fs::write(sub.join(".gitattributes"), "* filter=x\n").unwrap();
    fs::write(sub.join("f.txt"), "one\n").unwrap();
    assert!(plain(&sub, &["add", "."]).success());
    assert!(plain(&sub, &["commit", "-q", "-m", "sub"]).success());
    assert!(plain(&sub, &["config", "filter.x.clean", &evil.to_string_lossy()]).success());
    assert!(plain(&root, &["add", "sub"]).success());
    assert!(!marker.exists(), "the setup ran nothing");
    // The committed file no longer matches what git recorded: git re-reads
    // its content, through the filter, when it checks the submodule.
    fs::write(sub.join("f.txt"), "two\n").unwrap();

    assert!(apb_core::fingerprint::git_fingerprint(&root, &[]).is_some());
    let repo = Repo::locate(&root).unwrap();
    // `GIT_TRACE` lists every process git starts: apb's diff and status
    // start none (no git inside the submodule).
    let trace = tmp.path().join("trace");
    let status = repo
        .status(&root)
        .arg("--porcelain")
        .env("GIT_TRACE", &trace)
        .output()
        .unwrap();
    assert!(status.status.success());
    let diff = repo
        .diff(&root)
        .arg("HEAD")
        .env("GIT_TRACE", &trace)
        .output()
        .unwrap();
    assert!(diff.status.success());
    let traced = fs::read_to_string(&trace).unwrap();
    assert!(traced.contains("git diff"), "the trace was written");
    assert!(
        !traced.contains("run_command"),
        "apb's git started another: {traced}"
    );
    assert_never_ran(&marker, "apb's diff and status over a submodule");

    // Control: a plain git checks the submodule and runs its filter.
    plain(&root, &["status", "--porcelain"]);
    assert!(
        marker.exists(),
        "the submodule's filter is live for a plain git"
    );
}

/// A submodule bumped to another commit (the gitlink staged in the index)
/// changes the cache fingerprint, so a cached node never replays a result
/// computed against the old commit; the trial diff shows the bump too.
#[test]
fn a_submodule_bump_changes_the_fingerprint() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    repo(&root);
    let sub = root.join("sub");
    fs::create_dir_all(&sub).unwrap();
    assert!(plain(&sub, &["init", "-q"]).success());
    assert!(plain(&sub, &["commit", "-q", "--allow-empty", "-m", "one"]).success());
    assert!(plain(&root, &["add", "sub"]).success());
    assert!(plain(&root, &["commit", "-q", "-m", "sub"]).success());
    let before = apb_core::fingerprint::git_fingerprint(&root, &[]).unwrap();
    assert!(plain(&sub, &["commit", "-q", "--allow-empty", "-m", "two"]).success());
    let moved = apb_core::fingerprint::git_fingerprint(&root, &[]).unwrap();
    assert_ne!(before, moved, "a submodule's new commit in its work tree");
    assert!(plain(&root, &["add", "sub"]).success());
    let staged = apb_core::fingerprint::git_fingerprint(&root, &[]).unwrap();
    assert_ne!(before, staged, "a staged submodule bump");
    let diff = Repo::locate(&root)
        .unwrap()
        .diff(&root)
        .arg("HEAD")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&diff.stdout).contains("Subproject commit"));
}
