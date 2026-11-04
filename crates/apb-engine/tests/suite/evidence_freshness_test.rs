//! Evidence freshness: a check's result read after a later node changed
//! what it checked.
//!
//! Every test drives a real run: an agent step writes `src/lib.txt`, its
//! `success_check` script passes on that content (candidate A), then a
//! script node rewrites the file (candidate B) before the outcome is read.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_engine::event::EventPayload;
use apb_engine::run_outcome::OutcomeSummary;
use apb_engine::run_outcome::summary::{EvidenceBasis, EvidenceSource, FreshnessState};
use apb_engine::run_view::RunView;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

/// `implement` writes `A`; its check greps for `A`; `edit` runs `edit.sh`.
/// `goal` adds goal criteria (YAML) when given.
fn playbook(goal: &str) -> String {
    playbook_with(goal, "scripts/check.sh")
}

/// [`playbook`] with the `success_check` given as YAML.
fn playbook_with(goal: &str, check: &str) -> String {
    format!(
        r#"
schema: 2
id: fr
name: fr
version: 1.0.0
defaults: {{ profile: main }}
{goal}
nodes:
  - {{ id: start, type: start }}
  - {{ id: implement, type: agent_task, prompt: "write it", success_check: {check} }}
  - {{ id: edit, type: script, script: scripts/edit.sh, runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: implement }}
  - {{ from: implement, to: edit }}
  - {{ from: edit, to: done }}
"#
    )
}

fn seed(root: &Path, yaml: &str, edit: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/fr/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/fr/current"), "1.0.0").unwrap();
    common::seed_main(root);
    common::write_sync(
        &vdir.join("scripts/check.sh"),
        "grep -q '^A$' src/lib.txt\n",
    );
    common::write_sync(&vdir.join("scripts/edit.sh"), edit);
    fs::create_dir_all(root.join("src")).unwrap();
}

/// The agent writes candidate A and reports success.
const AGENT_A: &str = "#!/bin/sh\nprintf 'A\\n' > src/lib.txt\necho done\n";

/// Runs the playbook to its end and reads the outcome every surface shows.
fn run_and_read(root: &Path) -> (RunStatus, OutcomeSummary, PathBuf) {
    run_with_agent(root, &root.join("agent.sh"), AGENT_A)
}

/// [`run_and_read`] with the agent program `body` written at `prog`.
fn run_with_agent(root: &Path, prog: &Path, body: &str) -> (RunStatus, OutcomeSummary, PathBuf) {
    run_with_agent_opts(root, prog, body, RunOptions::default())
}

fn run_with_agent_opts(
    root: &Path,
    prog: &Path,
    body: &str,
    opts: RunOptions,
) -> (RunStatus, OutcomeSummary, PathBuf) {
    fs::write(prog, body).unwrap();
    let mut p = fs::metadata(prog).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(prog, p).unwrap();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", prog);
    }
    let res = run(root, "fr", None, opts);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    let res = res.unwrap();
    let rd = root.join(".apb/runs").join(&res.run_id);
    let view = RunView::load(&rd, &res.run_id).unwrap();
    (res.outcome, OutcomeSummary::build(&view, &rd), rd)
}

fn success_check(o: &OutcomeSummary) -> &apb_engine::run_outcome::summary::Evidence {
    o.verified
        .iter()
        .find(|e| e.source == EvidenceSource::SuccessCheck && e.subject == "implement")
        .expect("the success check is evidence")
}

/// The reproduction: the check passed on A, `edit` made B, and nothing at
/// the finish looks at it again (no goal, no baseline). Outside a git
/// repository and with no declared scope there is no fingerprint, so the
/// pass is not known to be current once `edit` ran: never a plain pass.
#[test]
fn a_later_edit_leaves_the_success_check_not_known_to_be_current() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(""),
        "printf 'B\\n' > src/lib.txt\necho ok\n",
    );
    let (status, outcome, _) = run_and_read(root);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(fs::read_to_string(root.join("src/lib.txt")).unwrap(), "B\n");
    let e = success_check(&outcome);
    assert_eq!(e.result, "passed");
    assert_eq!(e.basis, EvidenceBasis::Executed);
    let f = e.freshness.as_ref().expect("the check is bound");
    assert_eq!(f.state, FreshnessState::Unknown, "{f:?}");
    assert!(
        f.reason
            .as_deref()
            .unwrap()
            .contains("node edit ran after the check"),
        "{f:?}"
    );
    assert!(!e.passed(), "a pass not known to be current is not a pass");
    assert!(
        outcome
            .headline
            .contains("0 of 1 check passed, 1 not current"),
        "{}",
        outcome.headline
    );
    let line = outcome
        .lines()
        .into_iter()
        .find(|l| l.contains("success check: implement"))
        .unwrap();
    assert!(
        line.contains("[executed] [not known to be current: node edit ran"),
        "{line}"
    );
}

/// A git repository with the project committed, so the whole-tree
/// fingerprint can be taken.
fn git_init(root: &Path) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    fs::write(root.join("src/lib.txt"), "base\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
}

/// A declared scope: an edit inside it makes the pass stale.
#[test]
fn an_edit_in_the_declared_scope_makes_the_pass_stale() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let check = r#"{ script: scripts/check.sh, scope: ["src/**"] }"#;
    seed(
        root,
        &playbook_with("", check),
        "printf 'B\\n' > src/lib.txt\necho ok\n",
    );
    let (_, outcome, rd) = run_and_read(root);
    let e = success_check(&outcome);
    let f = e.freshness.as_ref().unwrap();
    assert_eq!(f.state, FreshnessState::Stale, "{f:?}");
    assert!(f.reason.as_deref().unwrap().contains("src/**"), "{f:?}");
    assert!(!e.passed());
    // The binding and the finish's fingerprint are both journaled.
    let events = apb_engine::event::read_all(&rd).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::EvidenceBound { node, scope, fingerprint: Some(_), .. } if node == "implement" && scope == &["src/**".to_string()])));
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::EvidenceObserved { observations } if observations.len() == 1)));
}

/// A declared scope: an edit outside it leaves the pass current.
#[test]
fn an_unrelated_edit_leaves_a_scoped_pass_current() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let check = r#"{ script: scripts/check.sh, scope: ["src/**"] }"#;
    seed(
        root,
        &playbook_with("", check),
        "mkdir -p docs\nprintf 'notes\\n' > docs/notes.txt\necho ok\n",
    );
    let (_, outcome, _) = run_and_read(root);
    let e = success_check(&outcome);
    assert_eq!(e.freshness.as_ref().unwrap().state, FreshnessState::Current);
    assert!(e.passed());
    assert!(
        outcome.headline.contains("1 of 1 check passed"),
        "{}",
        outcome.headline
    );
    assert!(
        !outcome.headline.contains("not current"),
        "{}",
        outcome.headline
    );
}

/// No scope in a git repository: the tree changed, and nothing says the
/// check reads what changed, so it is unknown, not stale.
#[test]
fn a_change_anywhere_leaves_an_unscoped_pass_unknown() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(""),
        "printf 'notes\\n' > notes.txt\necho ok\n",
    );
    git_init(root);
    let (_, outcome, _) = run_and_read(root);
    let f = success_check(&outcome).freshness.clone().unwrap();
    assert_eq!(f.state, FreshnessState::Unknown, "{f:?}");
    assert!(
        f.reason.as_deref().unwrap().contains("declares no scope"),
        "{f:?}"
    );
}

/// No scope in a git repository, and the later node changes nothing git
/// sees: equal git fingerprints say nothing about ignored files or paths
/// above the check's directory, so a node that can act still leaves an
/// unscoped pass unknown.
#[test]
fn a_later_node_leaves_an_unscoped_pass_unknown_even_when_git_sees_no_change() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, &playbook(""), "echo ok\n");
    git_init(root);
    let (_, outcome, rd) = run_and_read(root);
    let e = success_check(&outcome);
    let f = e.freshness.as_ref().unwrap();
    assert_eq!(f.state, FreshnessState::Unknown, "{e:?}");
    assert!(
        f.reason.as_deref().unwrap().contains("declares no scope"),
        "{f:?}"
    );
    assert!(!e.passed());
    // No fingerprint is taken for an unscoped check, at the bind or the
    // finish.
    let events = apb_engine::event::read_all(&rd).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::EvidenceBound { node, fingerprint: None, .. } if node == "implement")));
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::EvidenceObserved { .. }))
    );
}

/// Finding 2 of the review: the check reads a gitignored build output that
/// a later node rebuilds. The git fingerprint cannot see it, so the pass
/// must not read as current.
#[test]
fn a_rebuilt_ignored_file_leaves_an_unscoped_pass_unknown() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(""),
        "printf 'B\\n' > dist/out.txt\necho ok\n",
    );
    let vdir = root.join(".apb/playbooks/fr/1.0.0");
    common::write_sync(
        &vdir.join("scripts/check.sh"),
        "grep -q '^A$' dist/out.txt\n",
    );
    let prog = root.join("agent.sh");
    git_init(root);
    fs::write(root.join(".gitignore"), ".apb/\ndist/\nagent.sh\n").unwrap();
    fs::create_dir_all(root.join("dist")).unwrap();
    let (status, outcome, _) = run_with_agent(
        root,
        &prog,
        "#!/bin/sh\nprintf 'A\\n' > dist/out.txt\necho done\n",
    );
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(root.join("dist/out.txt")).unwrap(),
        "B\n"
    );
    let e = success_check(&outcome);
    assert_eq!(
        e.freshness.as_ref().unwrap().state,
        FreshnessState::Unknown,
        "{e:?}"
    );
    assert!(!e.passed());
}

/// Finding 1 of the review: a pass restored from the node cache carries no
/// binding of this run. A later node that rewrites what it checked must
/// leave it not known to be current, and the line must say it came from
/// the cache.
#[test]
fn a_pass_restored_from_the_cache_is_not_current_after_a_later_edit() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = playbook("").replace(
        r#"success_check: scripts/check.sh }"#,
        r#"success_check: scripts/check.sh, cache: auto }"#,
    );
    assert!(yaml.contains("cache: auto"));
    // `edit` writes only an ignored file, so the workspace fingerprint (and
    // the cache key) is the same on the second run.
    seed(
        root,
        &yaml,
        "mkdir -p build\nprintf 'B\\n' > build/out.txt\necho ok\n",
    );
    let vdir = root.join(".apb/playbooks/fr/1.0.0");
    common::write_sync(
        &vdir.join("scripts/check.sh"),
        "grep -q '^base$' src/lib.txt\n",
    );
    git_init(root);
    fs::write(root.join(".gitignore"), ".apb/\nbuild/\nagent.sh\n").unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
    };
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "ignore"]);
    let prog = root.join("agent.sh");
    let body = "#!/bin/sh\necho done\n";
    let opts = || RunOptions {
        cache: apb_engine::run_config::CacheRunMode::Auto,
        ..RunOptions::default()
    };
    let (first, _, rd1) = run_with_agent_opts(root, &prog, body, opts());
    assert_eq!(first, RunStatus::Succeeded);
    let (second, outcome, rd2) = run_with_agent_opts(root, &prog, body, opts());
    assert_eq!(second, RunStatus::Succeeded);
    let events = apb_engine::event::read_all(&rd2).unwrap();
    assert!(
        events.iter().any(
            |e| matches!(&e.payload, EventPayload::NodeCacheHit { node, .. } if node == "implement")
        ),
        "the second run restores implement from the cache"
    );
    let e = success_check(&outcome);
    let f = e
        .freshness
        .as_ref()
        .expect("a restored pass has a freshness");
    assert_eq!(f.state, FreshnessState::Unknown, "{e:?}");
    let source = rd1.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        e.detail
            .as_deref()
            .unwrap()
            .contains(&format!("from the cache of run {source}")),
        "{e:?}"
    );
    assert!(!e.passed());
}

/// What already closes it: a goal script criterion runs at the finish node,
/// after every node, so it sees B and fails. The stale success check is
/// listed next to it as not known to be current.
#[test]
fn a_goal_script_at_the_finish_sees_the_later_edit() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let goal = "goal:\n  statement: lib holds A\n  criteria:\n    - { description: lib holds A, check: { type: script, path: scripts/check.sh } }";
    seed(
        root,
        &playbook(goal),
        "printf 'B\\n' > src/lib.txt\necho ok\n",
    );
    let (_, outcome, _) = run_and_read(root);
    let goal = outcome
        .verified
        .iter()
        .find(|e| e.source == EvidenceSource::Goal)
        .expect("the goal criterion is evidence");
    assert_eq!(goal.result, "failed", "the goal check sees candidate B");
    assert_eq!(goal.basis, EvidenceBasis::Executed);
    assert!(
        goal.freshness.is_none(),
        "nothing runs after a goal criterion"
    );
    let e = success_check(&outcome);
    assert_eq!(e.result, "passed");
    assert!(
        !e.is_current(),
        "the success check is no longer shown as a plain pass"
    );
}
