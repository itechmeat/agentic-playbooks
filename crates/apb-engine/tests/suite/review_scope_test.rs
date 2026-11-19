//! The scope of a review request (people-in-the-loop D2): the pending
//! review shows what the decision covers since the gate's previous
//! decision - commits, changed files and lines, counts only - and a
//! withheld option is refused through the channel and fails an unattended
//! run instead of approving.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::decision_channel::DecisionChannel;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::review::{ReviewCommand, post_decision_via};
use apb_engine::run_view::RunView;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const POLL_DEADLINE: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(10);

/// Commits one file of 3 lines.
const COMMIT_ONE: &str = r#"printf 'a\nb\nc\n' > out.txt
git add out.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m work
"#;

/// Commits one file of 3 lines and leaves another file modified
/// uncommitted.
const COMMIT_ONE_DIRTY: &str = r#"printf 'a\nb\nc\n' > out.txt
git add out.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m work
printf 'changed' > loose.txt
"#;

/// Commits the file a person would read: the journal must not name it.
const COMMIT_SECRET_NAME: &str = r#"printf 'a\nb\nc\n' > secret-name.txt
git add secret-name.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m work
"#;

/// Commits two files, so a `max_scope: { files: 1 }` gate withholds
/// `approve`.
const COMMIT_TWO: &str = r#"printf 'a\n' > one.txt
printf 'b\n' > two.txt
git add one.txt two.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m work
"#;

/// A `max_scope` no test run reaches: the gate uses a form feature, so its
/// request journals a `review_detail` with the scope
/// (`NodeKind::needs_review_detail`), and withholds nothing.
const WIDE: &str = ", max_scope: { files: 100 }";

/// Commits one file of 3 lines, then marks every file binary in the work
/// tree's attributes without committing it.
const COMMIT_ONE_THEN_BINARY_ATTRS: &str = r#"printf 'a\nb\nc\n' > out.txt
git add out.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m work
printf '* binary\n' > .gitattributes
"#;

/// The scope line of one commit of one 3-line file: its lines where the
/// installed git keeps the work tree's attributes out of the diff
/// (`apb_core::git::no_tree_attributes`), else `lines not read`.
fn one_file_line(root: &Path) -> &'static str {
    if apb_core::git::no_tree_attributes(root).is_some() {
        "1 commit, 1 file, +3 -0 lines"
    } else {
        "1 commit, 1 file, lines not read"
    }
}

/// `start -> commit -> gate`, the gate routing on `review_status`.
fn scope_playbook(gate_extra: &str) -> String {
    format!(
        "schema: 2\nid: rsc\nname: RScope\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: commit, type: script, script: \"scripts/commit.sh\", runner: sh }}\n  - {{ id: gate, type: human_review, options: [approve, reject]{gate_extra} }}\n  - {{ id: ok, type: finish, outcome: success }}\n  - {{ id: no, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: commit }}\n  - {{ from: commit, to: gate }}\n  - {{ from: gate, to: ok, condition: {{ type: review_status, equals: approve }} }}\n  - {{ from: gate, to: no, condition: {{ type: review_status, equals: reject }} }}\n"
    )
}

/// `start -> commit -> gate`; `more` loops back to the gate with one more
/// commit before the deciding edges take over.
const LOOP_PLAYBOOK: &str = r#"schema: 2
id: rsc
name: RScope
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: commit, type: script, script: "scripts/commit.sh", runner: sh }
  - { id: more, type: script, script: "scripts/more.sh", runner: sh }
  - { id: gate, type: human_review, options: [approve, more, reject], max_scope: { files: 100 } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: commit }
  - { from: commit, to: gate }
  - { from: gate, to: more, condition: { type: review_status, equals: more } }
  - { from: more, to: gate, max_traversals: 1 }
  - { from: gate, to: ok, condition: { type: review_status, equals: approve } }
  - { from: gate, to: no, condition: { type: review_status, equals: reject } }
"#;

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

fn seed(root: &Path, yaml: &str, script: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/rsc/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(dir.join("scripts/commit.sh"), script).unwrap();
    if yaml.contains("more.sh") {
        fs::write(
            dir.join("scripts/more.sh"),
            "printf 'x\\ny\\nz\\n' > extra.txt\ngit add extra.txt\ngit -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m more\n",
        )
        .unwrap();
    }
    fs::write(root.join(".apb/playbooks/rsc/current"), "1.0.0").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["add", ".gitignore"]);
    git(root, &["commit", "-q", "-m", "base"]);
}

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn run_in_background(root: &Path) -> mpsc::Receiver<RunResult> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let res = run(&root, "rsc", None, RunOptions::default()).unwrap();
        let _ = tx.send(res);
    });
    rx
}

fn latest_run_dir(root: &Path) -> PathBuf {
    poll_until("run dir to appear", || {
        let runs = root.join(".apb/runs");
        let entry = fs::read_dir(&runs)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())?;
        Some(entry.path())
    })
}

fn wait_result(rx: &mpsc::Receiver<RunResult>) -> RunResult {
    rx.recv_timeout(POLL_DEADLINE)
        .unwrap_or_else(|_| panic!("run did not finish within {POLL_DEADLINE:?}"))
}

fn run_id_of(run_dir: &Path) -> String {
    run_dir.file_name().unwrap().to_string_lossy().into_owned()
}

/// The run surface's pending review of `gate`, as soon as it is pending.
/// A gate that uses a form feature journals its request and its
/// `review_detail` in one write, so the first look that sees the request
/// sees its scope too.
fn pending_of(run_dir: &Path, gate: &str) -> apb_engine::progress::PendingReview {
    let pending = pending_any(run_dir, gate);
    assert!(
        pending.scope_line.is_some(),
        "a pending request without its detail: {pending:?}"
    );
    pending
}

/// The pending review of `gate`, with or without a detail.
fn pending_any(run_dir: &Path, gate: &str) -> apb_engine::progress::PendingReview {
    poll_until("pending review", || {
        let view = RunView::load(run_dir, &run_id_of(run_dir)).ok()?;
        view.progress?.pending_review.filter(|p| p.node == gate)
    })
}

/// How many `review_requested` the journal holds for `gate`.
fn requests_of(run_dir: &Path, gate: &str) -> usize {
    read_all(run_dir).map_or(0, |events| {
        events
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == gate))
            .count()
    })
}

/// The scope of the latest `review_detail` event.
fn latest_detail_scope(run_dir: &Path) -> apb_engine::review::scope::ReviewScope {
    read_all(run_dir)
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewDetail {
                node,
                scope: Some(s),
                ..
            } if node == "gate" => Some(s.clone()),
            _ => None,
        })
        .expect("a review_detail with a scope")
}

#[test]
fn a_gate_shows_the_scope_of_its_commits() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), &scope_playbook(WIDE), COMMIT_ONE);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    assert_eq!(
        pending.scope_line.as_deref(),
        Some(one_file_line(tmp.path())),
        "scope_line {:?}",
        pending.scope_line
    );
    let scope = pending.scope.expect("the scope itself");
    assert_eq!(scope.commits, 1);
    assert_eq!(scope.files, Some(1));
    let pinned = apb_core::git::no_tree_attributes(tmp.path()).is_some();
    assert_eq!(scope.lines_added, pinned.then_some(3));
    assert_eq!(scope.lines_removed, pinned.then_some(0));
    // The request's open-since timestamp is the request's ts.
    assert!(pending.requested_at.is_some());
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    // An unchanged scope is consumed as it was asked: the measure before
    // the consume journals nothing.
    assert_eq!(requests_of(&run_dir, "gate"), 1);
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewWithdrawn { .. })),
        "nothing withdrawn on an unchanged scope"
    );
}

/// `start` forks into `flaky`, an agent step that fails once and, on its
/// deferred retry, commits one more file, and `commit -> gate`, one node at
/// a time so every commit is recorded on the sequential arm. The gate's
/// `max_scope: { files: 1 }` allows `approve` on the first request only.
const RESCOPE_PLAYBOOK: &str = r#"schema: 2
id: rsc
name: RScope
version: 1.0.0
defaults: { profile: main, max_parallel: 1 }
nodes:
  - { id: start, type: start }
  - { id: commit, type: script, script: "scripts/commit.sh", runner: sh }
  - { id: flaky, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: { initial: 2s } }
  - { id: gate, type: human_review, options: [approve, reject], max_scope: { files: 1 } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: flaky }
  - { from: start, to: commit }
  - { from: commit, to: gate }
  - { from: flaky, to: ok }
  - { from: gate, to: ok, condition: { type: review_status, equals: approve } }
  - { from: gate, to: no, condition: { type: review_status, equals: reject } }
"#;

/// The stub agent: `FLAKY` fails its first call; its retry commits one
/// more file in `root`.
#[cfg(unix)]
fn flaky_agent(root: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let d = root.display();
    let path = root.join(".apb/flaky-agent.sh");
    crate::common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *FLAKY*) if [ -f '{d}/.apb/flaky-mark' ]; then printf 'late\\n' > '{d}/late.txt'; git -C '{d}' add late.txt; git -C '{d}' -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m late; echo ok; exit 0; fi; : > '{d}/.apb/flaky-mark'; echo 'the agent made a mistake' 1>&2; exit 1;;\n  *) echo ok;;\nesac\n"
        ),
    );
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().into_owned()
}

/// Work that lands while a form gate's request waits (a due retry the
/// drive lets run) changes what the decision covers: the decision queued
/// on the old scope is not consumed, the request is withdrawn once with
/// the reason, and the gate asks again on the larger scope, where
/// `max_scope` withholds `approve`.
#[cfg(unix)]
#[test]
fn work_landing_while_the_request_waits_asks_again_on_the_new_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let _lock = crate::common::env_lock();
    seed(tmp.path(), RESCOPE_PLAYBOOK, COMMIT_ONE);
    crate::common::seed_main(tmp.path());
    let prog = flaky_agent(tmp.path());
    let _cmd = crate::common::EnvVar::set("APB_AGENT_CMD", &prog);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let first = pending_of(&run_dir, "gate");
    assert_eq!(first.scope.as_ref().map(|s| s.files), Some(Some(1)));
    assert!(first.withheld.is_empty(), "{:?}", first.withheld);
    // The retry lands while the request waits.
    poll_until("the retry to commit", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| {
                matches!(&e.payload, EventPayload::NodeFinished { node, status, .. }
                if node == "flaky" && status == "succeeded")
            })
            .then_some(())
    });
    // A decision on the request the person saw: accepted by the channel,
    // which checks the open request's detail.
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    // The drive measures again before consuming it and asks anew.
    poll_until("the second request", || {
        (requests_of(&run_dir, "gate") >= 2).then_some(())
    });
    let second = pending_of(&run_dir, "gate");
    assert_eq!(second.scope.as_ref().map(|s| s.files), Some(Some(2)));
    assert!(
        second
            .instruction
            .contains(apb_engine::progress::ASKED_AGAIN_LINE),
        "the request says why it was asked again: {}",
        second.instruction
    );
    assert!(
        !first
            .instruction
            .contains(apb_engine::progress::ASKED_AGAIN_LINE)
    );
    assert_eq!(second.withheld, vec!["approve"]);
    let events = read_all(&run_dir).unwrap();
    let withdrawn: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewWithdrawn { node, reason } if node == "gate" => {
                Some(reason.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(withdrawn.len(), 1, "{withdrawn:?}");
    assert!(
        apb_engine::event::is_scope_changed(withdrawn[0]),
        "{}",
        withdrawn[0]
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDecided { .. })),
        "the decision on the old scope was not consumed"
    );
    // On the new scope the channel refuses the withheld option.
    let refused = post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap_err();
    assert!(refused.to_string().contains("withheld"), "{refused}");
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "reject".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(requests_of(&run_dir, "gate"), 2, "no further re-ask");
    let decided: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided { decision, .. } => Some(decision.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(decided, vec!["reject"]);
    // One gate visit, not a withdrawn one and a second.
    let r = apb_engine::run_stats::interventions::of_run(&events);
    assert_eq!(r.reviews.visits, 1);
    assert_eq!(r.reviews.withdrawn, 0);
}

#[test]
fn an_uncommitted_change_is_not_in_the_scope() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), &scope_playbook(WIDE), COMMIT_ONE_DIRTY);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    assert_eq!(
        pending.scope_line.as_deref(),
        Some(one_file_line(tmp.path())),
        "the uncommitted file is not counted"
    );
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let _ = wait_result(&rx);
}

/// An attributes file the work tree gained after the commits does not
/// change their line counts: the diff reads no attributes from the work
/// tree, so the committed text file still counts its lines.
#[test]
fn work_tree_attributes_do_not_change_the_line_counts() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(WIDE),
        COMMIT_ONE_THEN_BINARY_ATTRS,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    assert!(tmp.path().join(".gitattributes").exists());
    // A git without `attr.tree` cannot keep them out: the lines are
    // unknown there, never the attributes' zero.
    assert_eq!(
        pending.scope_line.as_deref(),
        Some(one_file_line(tmp.path()))
    );
    let scope = pending.scope.expect("the scope itself");
    let pinned = apb_core::git::no_tree_attributes(tmp.path()).is_some();
    assert_eq!(scope.lines_added, pinned.then_some(3));
    assert_eq!(scope.lines_removed, pinned.then_some(0));
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let _ = wait_result(&rx);
}

#[test]
fn the_detail_holds_counts_only() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), &scope_playbook(WIDE), COMMIT_SECRET_NAME);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let raw = fs::read_to_string(run_dir.join("events.jsonl")).expect("the journal on disk");
    assert!(
        !raw.contains("secret-name"),
        "no journal line may name the committed file"
    );
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let _ = wait_result(&rx);
}

#[test]
fn a_second_visit_counts_since_the_first_decision() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), LOOP_PLAYBOOK, COMMIT_ONE);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    // First visit: ask for one more commit.
    let _ = pending_of(&run_dir, "gate");
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "more".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    // Second visit: one more commit is in the window.
    poll_until("the second request", || {
        (requests_of(&run_dir, "gate") >= 2).then_some(())
    });
    let scope = latest_detail_scope(&run_dir);
    assert_eq!(scope.commits, 1, "only the commit since the first decision");
    assert_eq!(scope.files, Some(1));
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
}

#[test]
fn a_withheld_option_is_refused_through_the_channel() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(", max_scope: { files: 1 }"),
        COMMIT_TWO,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    assert_eq!(pending.withheld, vec!["approve"]);
    assert!(
        pending
            .withheld_reason
            .as_deref()
            .unwrap_or_default()
            .contains("max_scope"),
        "the reason names the limit: {:?}",
        pending.withheld_reason
    );
    // The channel refuses the withheld option and accepts the other.
    let refused = post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap_err();
    assert!(refused.to_string().contains("withheld"), "{refused}");
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "reject".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
}

#[test]
fn an_unattended_withheld_decision_fails_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(", max_scope: { files: 1 }, unattended: { decide: approve, note: ci }"),
        COMMIT_TWO,
    );
    let res = run(
        tmp.path(),
        "rsc",
        None,
        RunOptions {
            unattended: true,
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&tmp.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let (_, reason) = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunError { node, reason } => Some((node.clone(), reason.clone())),
            _ => None,
        })
        .expect("a run_error");
    assert!(reason.contains("withheld"), "{reason}");
    // Nobody approved: no ReviewDecided of the withheld option.
    assert!(!events.iter().any(|e| matches!(&e.payload,
            EventPayload::ReviewDecided { decision, .. } if decision == "approve")));
}

#[cfg(unix)]
#[test]
fn a_publish_consent_journals_its_request_alone() {
    use crate::common::bare_remote::Fixture;
    let _env = crate::common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    // The publish playbook: work commits one file, then the consent gate.
    init_project(&f.root).unwrap();
    let dir = f.root.join(".apb/playbooks/pub/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(
        dir.join("playbook.yaml"),
        "schema: 2\nid: pub\nname: Pub\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: \"scripts/work.sh\", runner: sh }\n  - { id: publish, type: publish, from: work, remote: origin, branch: \"apb/out\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: publish }\n  - { from: publish, to: done }\n",
    )
    .unwrap();
    fs::write(dir.join("scripts/work.sh"), COMMIT_ONE).unwrap();
    fs::write(f.root.join(".apb/playbooks/pub/current"), "1.0.0").unwrap();
    let (tx, rx) = mpsc::channel();
    let root = f.root.clone();
    std::thread::spawn(move || {
        let res = run(
            &root,
            "pub",
            None,
            RunOptions {
                consent: Some(apb_engine::consent::RunConsent::irreversible("cli")),
                ..RunOptions::default()
            },
        )
        .unwrap();
        let _ = tx.send(res);
    });
    let run_dir = latest_run_dir(&f.root);
    // A publish consent uses no form feature: its request is journaled
    // alone, the journal an older apb writes, so it shows no scope.
    let pending_publish = pending_any(&run_dir, "publish");
    assert_eq!(pending_publish.scope_line, None);
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDetail { .. }))
    );
    let nonce = pending_publish.publish.expect("the plan").nonce;
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "publish".into(),
            decision: "publish".into(),
            note: String::new(),
        },
        Some(nonce),
        None,
        DecisionChannel::Terminal,
    )
    .unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
}

/// A person-only gate with `max_scope` and a field: the form's checks run
/// at post time like on any gate, so a withheld option and a value of the
/// wrong type are refused where they are posted and the gate keeps
/// waiting for an allowed decision.
#[test]
fn a_person_only_gate_refuses_a_withheld_option_and_a_bad_value_when_posted() {
    let _env = crate::common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = crate::common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(
            ", person_only: true, max_scope: { files: 1 }, fields: [{ name: amount, type: number }]",
        ),
        COMMIT_TWO,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    assert!(pending.person_only);
    assert_eq!(pending.withheld, vec!["approve"]);
    let post = |decision: &str, amount: serde_json::Value| {
        apb_engine::review::post(
            &run_dir,
            apb_engine::review::DecisionPost {
                cmd: ReviewCommand {
                    node: "gate".into(),
                    decision: decision.into(),
                    note: String::new(),
                },
                nonce: None,
                request: open_seq(&run_dir),
                channel: DecisionChannel::Terminal,
                fields: [("amount".to_string(), amount)].into(),
                passkey: None,
            },
        )
    };
    let withheld = post("approve", serde_json::json!(5)).unwrap_err();
    assert!(withheld.to_string().contains("is withheld"), "{withheld}");
    let bad = post("reject", serde_json::json!("ten")).unwrap_err();
    assert!(bad.to_string().contains("is not a number"), "{bad}");
    post("reject", serde_json::json!(5)).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Failed);
}

/// The crash point between a request and its detail: a journal whose open
/// request of a `max_scope` gate has no `review_detail` (an older apb, a
/// torn write). While the detail is missing the channel refuses every
/// option `max_scope` may withhold; the resumed drive journals the detail
/// before it waits, and the gate then withholds by its scope as usual.
#[test]
fn an_open_request_without_its_detail_fails_closed_until_the_resume_writes_it() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(", max_scope: { files: 1 }"),
        COMMIT_TWO,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let run_id = run_id_of(&run_dir);
    let _ = pending_of(&run_dir, "gate");
    request_pause(
        tmp.path(),
        &run_id,
        &PauseRequest {
            reason: Some("crash point".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    // The detail line is gone, as if the drive died right after the
    // request.
    let journal = run_dir.join("events.jsonl");
    let kept: String = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("\"type\":\"review_detail\""))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&journal, kept).unwrap();
    let detail_count = || {
        read_all(&run_dir)
            .unwrap()
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::ReviewDetail { .. }))
            .count()
    };
    assert_eq!(detail_count(), 0);
    let decide = |decision: &str| {
        post_decision_via(
            &run_dir,
            ReviewCommand {
                node: "gate".into(),
                decision: decision.into(),
                note: String::new(),
            },
            None,
            open_seq(&run_dir),
            DecisionChannel::Terminal,
        )
    };
    // Fail closed: the option `max_scope` withholds is refused.
    let refused = decide("approve").unwrap_err();
    assert!(
        refused.to_string().contains("not recorded yet"),
        "{refused}"
    );
    // The resume journals the detail for the open request before it waits.
    let (tx, resumed) = mpsc::channel();
    let root = tmp.path().to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(apb_engine::scheduler::resume(&root, &id, None).unwrap());
    });
    let pending = poll_until("the detail the resume journals", || {
        let view = RunView::load(&run_dir, &run_id).ok()?;
        view.progress?
            .pending_review
            .filter(|p| p.node == "gate" && p.scope_line.is_some())
    });
    assert_eq!(pending.withheld, vec!["approve"]);
    assert_eq!(detail_count(), 1, "one detail for the one open request");
    let refused = decide("approve").unwrap_err();
    assert!(refused.to_string().contains("is withheld"), "{refused}");
    decide("reject").unwrap();
    assert_eq!(wait_result(&resumed).outcome, RunStatus::Failed);
}

/// A scenario's scripted decision on an ordinary gate (no `person_only`,
/// no `approves`) is journaled with its channel, so the interventions fold
/// keeps it apart from a person's: never decided without review, however
/// fast it came.
#[test]
fn a_scripted_decision_on_an_ordinary_gate_keeps_its_channel() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), &scope_playbook(WIDE), COMMIT_ONE);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Scenario,
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let via = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewDecided { node, via, .. } if node == "gate" => Some(*via),
            _ => None,
        })
        .expect("a review_decided");
    assert_eq!(via, Some(DecisionChannel::Scenario));
    let writer = apb_engine::run_stats::interventions::writer_of(&events);
    let counts = apb_engine::run_stats::interventions::interventions(
        &events,
        &apb_engine::identity::index(&events),
        writer.as_deref(),
    );
    assert_eq!(counts.reviews.fast, 0, "{:?}", counts.reviews);
}

/// A withheld option is never offered as a command: the next action of a
/// gate above its `max_scope` offers only the options the gate takes now
/// and names the withheld one with its reason.
#[test]
fn the_next_action_leaves_a_withheld_option_out() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(", max_scope: { files: 1 }"),
        COMMIT_TWO,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let obs = apb_engine::next_action::Observation::load(&run_dir, &run_id_of(&run_dir)).unwrap();
    let actions = apb_engine::next_action::next_actions(&obs);
    let action = actions
        .actions
        .iter()
        .find(|a| a.code == apb_engine::next_action::ActionCode::DecideReview)
        .expect("a decide_review action");
    let decision = action
        .args
        .iter()
        .find(|a| a.name == "decision")
        .expect("a decision argument");
    match &decision.value {
        apb_engine::next_action::ArgValue::Choice { options, .. } => {
            assert_eq!(options, &vec!["reject".to_string()]);
        }
        other => panic!("not a choice: {other:?}"),
    }
    assert!(
        action.reason.contains("withheld: approve") && action.reason.contains("max_scope"),
        "{}",
        action.reason
    );
    let line = apb_engine::next_action::command::render(action, &Default::default())
        .expect("a command")
        .line();
    assert!(!line.contains("approve"), "{line}");
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "reject".into(),
            note: String::new(),
        },
        None,
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Failed);
}

/// The seq of the latest `review_requested` of `gate`.
fn latest_request_seq(run_dir: &Path, gate: &str) -> u64 {
    read_all(run_dir)
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested { node, .. } if node == gate => Some(e.seq),
            _ => None,
        })
        .expect("a review_requested")
}

/// The review channel entries of the run, whole.
fn entries_of(run_dir: &Path) -> Vec<apb_engine::review::ReviewEntry> {
    apb_engine::review::read_reviews_after(run_dir, None).unwrap()
}

/// The seq of the open request of `gate`, as every decision surface names
/// it for a form gate.
fn open_seq(run_dir: &Path) -> Option<u64> {
    read_all(run_dir)
        .ok()
        .and_then(|events| apb_engine::review::open_request(&events, "gate").map(|(seq, _)| seq))
}

/// A late decision never answers the request the gate asked again with
/// (carried-over decisions 2 and 5): a scope change withdrew request A and
/// the gate asked again with B; a decision that names no request and one
/// that names A are each refused `decide_again`, one naming B is accepted,
/// and the run's single `review_decided` follows B.
#[cfg(unix)]
#[test]
fn a_late_decision_does_not_answer_the_request_asked_again() {
    let tmp = tempfile::tempdir().unwrap();
    let _lock = crate::common::env_lock();
    seed(
        tmp.path(),
        &RESCOPE_PLAYBOOK.replace("max_scope: { files: 1 }", "max_scope: { files: 5 }"),
        COMMIT_ONE,
    );
    crate::common::seed_main(tmp.path());
    let prog = flaky_agent(tmp.path());
    let _cmd = crate::common::EnvVar::set("APB_AGENT_CMD", &prog);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    // The retry lands while request A waits. No decision is posted, so A
    // stays open until the post below names it.
    poll_until("the retry to commit", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| {
                matches!(&e.payload, EventPayload::NodeFinished { node, status, .. }
                if node == "flaky" && status == "succeeded")
            })
            .then_some(())
    });
    let seq_a = latest_request_seq(&run_dir, "gate");
    // A decision naming A: accepted while A is still open and stamped with
    // it; the drive measures the scope again before consuming and asks
    // again with B. The entry stamped A never answers B.
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        Some(seq_a),
        DecisionChannel::Terminal,
    )
    .unwrap();
    poll_until("the second request", || {
        (requests_of(&run_dir, "gate") >= 2).then_some(())
    });
    let seq_b = latest_request_seq(&run_dir, "gate");
    assert_ne!(seq_a, seq_b);
    let post = |decision: &str, request: Option<u64>| {
        post_decision_via(
            &run_dir,
            ReviewCommand {
                node: "gate".into(),
                decision: decision.into(),
                note: String::new(),
            },
            None,
            request,
            DecisionChannel::Terminal,
        )
    };
    let none_named = post("reject", None).unwrap_err();
    assert!(
        none_named.to_string().contains("decide_again"),
        "{none_named}"
    );
    assert!(
        none_named.to_string().contains(&format!("seq {seq_b}")),
        "the refusal names the open request: {none_named}"
    );
    let naming_a = post("approve", Some(seq_a)).unwrap_err();
    assert!(naming_a.to_string().contains("decide_again"), "{naming_a}");
    post("approve", Some(seq_b)).unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let decided: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided { decision, .. } => Some(decision.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        decided,
        vec!["approve"],
        "one decision, the second request's"
    );
    assert_eq!(requests_of(&run_dir, "gate"), 2);
}

/// A form gate's decision names the request it answers: a post that names
/// none is refused `decide_again`, naming the open request, and nothing is
/// queued.
#[test]
fn a_form_gate_decision_without_a_request_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(", show: [{ label: Work, text: \"the commits so far\" }]"),
        COMMIT_ONE,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let open = latest_request_seq(&run_dir, "gate");
    let refused = post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        None,
        DecisionChannel::Terminal,
    )
    .unwrap_err();
    let text = refused.to_string();
    assert!(text.starts_with("conflict: decide_again:"), "{text}");
    assert!(text.contains(&format!("seq {open}")), "{text}");
    assert!(
        entries_of(&run_dir).is_empty(),
        "a refused decision is not queued"
    );
    // The named post is accepted and the run goes on.
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        Some(open),
        DecisionChannel::Terminal,
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// The passkey variant: with a passkey enrolled on this machine, a plain
/// (not person-only, unbound) form gate still takes a terminal post that
/// names its request, and the drive consumes it.
#[test]
fn a_form_gate_decision_naming_its_request_is_consumed_with_a_passkey() {
    let _guard = crate::common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    crate::review_form_test::enroll_real_passkey(&cfg.path().join("passkeys.yaml"));
    let _var = crate::common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    apb_core::run_origin::ensure_key().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(", show: [{ label: Work, text: \"the commits so far\" }]"),
        COMMIT_ONE,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let open = latest_request_seq(&run_dir, "gate");
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        Some(open),
        DecisionChannel::Terminal,
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// An entry no poster stamped (a hand-written line) never decides a form
/// gate: the run keeps waiting until a stamped post lands, and `apb
/// doctor --run` reports the entry under `reviews` as unstamped.
#[test]
fn an_unstamped_entry_never_decides_a_form_gate() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(", show: [{ label: Work, text: \"the commits so far\" }]"),
        COMMIT_ONE,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let open = latest_request_seq(&run_dir, "gate");
    // A line without a request stamp, appended by hand while the gate
    // waits.
    let hand = r#"{"seq":0,"node":"gate","decision":"approve","note":""}"#;
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("reviews.jsonl"))
        .unwrap()
        .write_all(format!("{hand}\n").as_bytes())
        .unwrap();
    poll_until("the hand-written line to be ignored", || {
        (!entries_of(&run_dir).is_empty()
            && read_all(&run_dir)
                .unwrap()
                .iter()
                .all(|e| !matches!(e.payload, EventPayload::ReviewDecided { .. })))
        .then_some(())
    });
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDecided { .. })),
        "the unstamped entry is never consumed"
    );
    let report = apb_engine::run_doctor::diagnose_run(tmp.path(), &run_id_of(&run_dir)).unwrap();
    let reviews = report
        .iter()
        .find(|c| c.subject == "reviews")
        .expect("a reviews row");
    assert!(
        reviews.detail.contains("no request stamp"),
        "{}",
        reviews.detail
    );
    // A stamped post is consumed.
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        Some(open),
        DecisionChannel::Terminal,
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// A plain gate keeps today's behaviour: a decision that names no request
/// is accepted, its entry carries no request stamp, and the journal holds
/// the request alone with no `review_detail`.
#[test]
fn a_plain_gate_takes_a_decision_without_a_request() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), &scope_playbook(""), COMMIT_ONE);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    poll_until("pending review", || {
        let view = RunView::load(&run_dir, &run_id_of(&run_dir)).ok()?;
        view.progress?.pending_review.filter(|p| p.node == "gate")
    });
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        None,
        DecisionChannel::Terminal,
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
    let entries = entries_of(&run_dir);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].request, None, "no request stamp on a plain gate");
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDetail { .. })),
        "a plain gate journals no detail"
    );
}

/// The engine's own decision on a form gate (an unattended run's declared
/// outcome) names the request and its entry carries it with `by:
/// unattended`.
#[test]
fn an_unattended_decision_on_a_form_gate_is_stamped() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(
            ", show: [{ label: Work, text: \"the commits so far\" }], unattended: { decide: approve, note: ci }",
        ),
        COMMIT_ONE,
    );
    let res = run(
        tmp.path(),
        "rsc",
        None,
        RunOptions {
            unattended: true,
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = tmp.path().join(".apb/runs").join(&res.run_id);
    let entries = entries_of(&run_dir);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let open = latest_request_seq(&run_dir, "gate");
    assert_eq!(
        entries[0].request,
        Some(open),
        "the entry names the request"
    );
    assert!(
        matches!(
            entries[0].by,
            Some(apb_engine::event::DecidedBy::Unattended)
        ),
        "{:?}",
        entries[0].by
    );
}

/// While the run's definition snapshot does not read, a run whose run
/// guard names a review form feature refuses a decision the same way
/// (`decide_again`), instead of reading the gate as a plain one; with the
/// snapshot back and the detail the resume journals, a post naming the
/// request is consumed.
#[test]
fn an_unreadable_definition_under_a_form_guard_refuses() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &scope_playbook(", max_scope: { files: 1 }"),
        COMMIT_TWO,
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let run_id = run_id_of(&run_dir);
    let _ = pending_of(&run_dir, "gate");
    request_pause(
        tmp.path(),
        &run_id,
        &PauseRequest {
            reason: Some("crash point".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let journal = run_dir.join("events.jsonl");
    let kept: String = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("\"type\":\"review_detail\""))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&journal, kept).unwrap();
    let snapshot = run_dir.join("playbook.yaml");
    let away = run_dir.join("playbook.yaml.away");
    fs::rename(&snapshot, &away).unwrap();
    let open = latest_request_seq(&run_dir, "gate");
    let post = |decision: &str, request: Option<u64>| {
        post_decision_via(
            &run_dir,
            ReviewCommand {
                node: "gate".into(),
                decision: decision.into(),
                note: String::new(),
            },
            None,
            request,
            DecisionChannel::Terminal,
        )
    };
    let refused = post("reject", None).unwrap_err();
    assert!(refused.to_string().contains("decide_again"), "{refused}");
    let named = post("reject", Some(open)).unwrap_err();
    assert!(named.to_string().contains("decide_again"), "{named}");
    assert!(
        entries_of(&run_dir).is_empty(),
        "nothing is queued while the snapshot does not read"
    );
    // The snapshot reads again; the resumed drive journals the detail and
    // a post naming the request is consumed.
    fs::rename(&away, &snapshot).unwrap();
    let (tx, resumed) = mpsc::channel();
    let root = tmp.path().to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(apb_engine::scheduler::resume(&root, &id, None).unwrap());
    });
    poll_until("the detail the resume journals", || {
        let view = RunView::load(&run_dir, &run_id).ok()?;
        view.progress?
            .pending_review
            .filter(|p| p.node == "gate" && p.scope_line.is_some())
    });
    post("reject", Some(open)).unwrap();
    assert_eq!(wait_result(&resumed).outcome, RunStatus::Failed);
}

/// A run whose run guard names no review form feature keeps today's
/// acceptance while its definition snapshot does not read: a decision on
/// its plain gate is accepted.
#[test]
fn an_unreadable_definition_without_a_form_guard_accepts() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), &scope_playbook(""), COMMIT_ONE);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let run_id = run_id_of(&run_dir);
    poll_until("pending review", || {
        let view = RunView::load(&run_dir, &run_id).ok()?;
        view.progress?.pending_review.filter(|p| p.node == "gate")
    });
    request_pause(
        tmp.path(),
        &run_id,
        &PauseRequest {
            reason: Some("crash point".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let snapshot = run_dir.join("playbook.yaml");
    let away = run_dir.join("playbook.yaml.away");
    fs::rename(&snapshot, &away).unwrap();
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        None,
        DecisionChannel::Terminal,
    )
    .unwrap();
    fs::rename(&away, &snapshot).unwrap();
    let (tx, resumed) = mpsc::channel();
    let root = tmp.path().to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(apb_engine::scheduler::resume(&root, &id, None).unwrap());
    });
    assert_eq!(wait_result(&resumed).outcome, RunStatus::Succeeded);
}

/// A quick scenario's scripted decision on a form gate goes through the
/// drive's own scripted post (`scheduler::scripted_review`), which names
/// the open request: the answer is used and the run ends succeeded.
#[test]
fn a_scripted_decision_on_a_form_gate_names_its_request() {
    use apb_core::scenario::{Scenario, ScenarioLevel};
    use apb_engine::scenario::session::ScenarioPrep;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    apb_core::registry::init_project(root).unwrap();
    let yaml = "schema: 2\nid: rsc\nname: RScope\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject], show: [{ label: Work, text: \"the work so far\" }], fields: [{ name: note, type: text }] }\n  - { id: ok, type: finish, outcome: success }\n  - { id: no, type: finish, outcome: failure }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: ok, condition: { type: review_status, equals: approve } }\n  - { from: gate, to: no, condition: { type: review_status, equals: reject } }\n";
    let vdir = root.join(".apb/playbooks/rsc/1.0.0");
    fs::create_dir_all(vdir.join("evals/scenarios")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/rsc/current"), "1.0.0").unwrap();
    let scenario = "schema: 1\nid: rsc\nanswers:\n  gate:\n    - visit: 1\n      review: { decision: approve }\n";
    fs::write(vdir.join("evals/scenarios/form.yaml"), scenario).unwrap();
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(scenario).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:form".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let quick = apb_engine::scheduler::run_scenario(root, "rsc", None, &prep).unwrap();
    assert_eq!(quick.outcome, RunStatus::Succeeded, "{:?}", quick.halted);
    assert!(
        quick
            .answers
            .used
            .iter()
            .any(|a| a.node.as_deref() == Some("gate") && a.part.contains("review")),
        "{:?}",
        quick.answers.used
    );
    let ev = apb_engine::event::read_all(&quick.run_dir).unwrap();
    let requested = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested { node, .. } if node == "gate" => Some(e.seq),
            _ => None,
        })
        .expect("the form gate asked");
    let decided = ev.iter().find_map(|e| match &e.payload {
        EventPayload::ReviewDecided { node, decision, .. } if node == "gate" => {
            Some(decision.clone())
        }
        _ => None,
    });
    assert_eq!(decided.as_deref(), Some("approve"));
    // The scripted entry named the request it answered.
    let entries = apb_engine::read_reviews_after(&quick.run_dir, None).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.cmd.node == "gate" && e.request == Some(requested)),
        "{entries:?}"
    );
}
