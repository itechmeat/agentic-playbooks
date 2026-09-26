//! The run's working tree (issue #67 item 8).
//!
//! A run can be given a working tree at start (by the caller, or by the
//! playbook's `worktree` template over params) or have one resolved by a node
//! (`worktree: "{{nodes.probe.output.working_tree}}"`). Agent and script nodes
//! without their own `workdir` then run in it, `worktree_resolved` records
//! it, and the run's busy lock covers that tree instead of the execution
//! root, so runs over different trees never wait on each other.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use apb_core::registry::init_project;
use apb_engine::error::EngineError;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};
use apb_engine::workdir::{acquire, acquire_tree};

use crate::common;
use crate::token_economy_test::{invocations, recording_stub, seed_playbook};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

/// Every node records the directory it ran in (`pwd-<node>` next to the
/// stub, which lives in the project root) and which busy locks were held
/// while it ran (`locks-<node>`); the `probe` node publishes `working_tree`.
fn stub_part(tree: &str) -> String {
    format!(
        "R=\"$(dirname \"$0\")\"; pwd > \"$R/pwd-$NODE\"; \
         {{ [ -f \"$R/.apb/workdir.lock\" ] && echo root; ls \"$R/.apb/locks\" 2>/dev/null; }} > \"$R/locks-$NODE\"; \
         if [ \"$NODE\" = probe ]; then printf '{{\"status\":\"success\",\"outputs\":{{\"working_tree\":\"{tree}\"}}}}' > \"$APB_STATUS_FILE\"; fi; \
         {OK}"
    )
}

/// `probe -> work -> gate(script) -> done`, with `worktree_line` added at
/// the top level of the playbook.
fn playbook(worktree_line: &str) -> String {
    format!(
        r#"schema: 2
id: tree
name: Tree
version: 1.0.0
params:
  - {{ name: tree, type: text }}
{worktree_line}
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: probe, type: agent_task, prompt: "Find the tree.", outputs: {{ fields: [working_tree] }} }}
  - {{ id: work, type: agent_task, prompt: "Work in the tree." }}
  - {{ id: gate, type: script, script: scripts/pwd.sh, runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: probe }}
  - {{ from: probe, to: work }}
  - {{ from: work, to: gate }}
  - {{ from: gate, to: done }}
"#
    )
}

fn seed(root: &Path, worktree_line: &str) {
    init_project(root).unwrap();
    seed_playbook(root, "tree", &playbook(worktree_line));
    let scripts = root.join(".apb/playbooks/tree/1.0.0/scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("pwd.sh"), "pwd\n").unwrap();
    common::seed_main(root);
}

/// Runs git in `root` with commit signing off.
fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

/// Makes the project a git repository and adds a linked worktree for each
/// name, inside the project (the usual `git worktree add <dir>` layout).
fn worktrees(root: &Path, names: &[&str]) {
    fs::write(
        root.join(".gitignore"),
        ".apb/\ninv/\npwd-*\nlocks-*\nstub.sh\n",
    )
    .unwrap();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);
    for (i, name) in names.iter().enumerate() {
        git(
            root,
            &["worktree", "add", "-q", "-b", &format!("b{i}"), name],
        );
    }
}

/// Starts `tree` under the recording stub with the given start options.
fn start(root: &Path, stub: &str, opts: RunOptions) -> Result<(String, RunStatus), EngineError> {
    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", stub) };
    let res = run(root, "tree", None, opts);
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    res.map(|r| (r.run_id, r.outcome))
}

fn with_tree(tree: &str) -> RunOptions {
    RunOptions {
        worktree: Some(tree.to_string()),
        ..Default::default()
    }
}

fn pwd_of(root: &Path, node: &str) -> PathBuf {
    let raw = fs::read_to_string(root.join(format!("pwd-{node}"))).unwrap();
    Path::new(raw.trim()).canonicalize().unwrap()
}

fn events_of(root: &Path, run_id: &str) -> Vec<apb_engine::event::Event> {
    read_all(&root.join(".apb/runs").join(run_id)).unwrap()
}

fn resolved(events: &[apb_engine::event::Event]) -> Vec<(String, String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::WorktreeResolved { path, source, node } => {
                Some((path.clone(), source.clone(), node.clone()))
            }
            _ => None,
        })
        .collect()
}

fn gate_output(events: &[apb_engine::event::Event]) -> PathBuf {
    let out = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "gate" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    Path::new(out.trim()).canonicalize().unwrap()
}

/// A tree passed at start: every agent and script node runs there, the run
/// status reports it, and the busy lock taken is the tree's, not the root's.
#[test]
fn a_tree_passed_at_start_is_where_every_node_runs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "");
    worktrees(root, &["wt"]);
    let stub = recording_stub(root, &stub_part("unused"));
    let (run_id, outcome) = start(root, &stub, with_tree("wt")).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);

    let tree = root.join("wt").canonicalize().unwrap();
    assert_eq!(pwd_of(root, "probe"), tree);
    assert_eq!(pwd_of(root, "work"), tree);
    let events = events_of(root, &run_id);
    assert_eq!(gate_output(&events), tree, "the script ran in the tree");
    assert_eq!(
        resolved(&events),
        vec![(tree.display().to_string(), "caller".into(), None)]
    );
    assert_eq!(
        RunState::fold(&events).worktree.as_deref(),
        Some(tree.to_str().unwrap())
    );
    let locks = fs::read_to_string(root.join("locks-work")).unwrap();
    assert!(!locks.contains("root"), "the root was not locked: {locks}");
    assert!(locks.contains("tree-"), "the tree was locked: {locks}");
}

/// A `worktree` template over params resolves at start; the caller's tree
/// wins over it.
#[test]
fn a_playbook_worktree_over_params_resolves_at_start_and_the_caller_wins() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "worktree: \"{{params.tree}}\"");
    worktrees(root, &["wt-a", "wt-b"]);
    let stub = recording_stub(root, &stub_part("unused"));

    let mut opts = RunOptions::default();
    opts.params.insert("tree".into(), "wt-a".into());
    let (run_id, outcome) = start(root, &stub, opts).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);
    let a = root.join("wt-a").canonicalize().unwrap();
    assert_eq!(pwd_of(root, "probe"), a);
    assert_eq!(resolved(&events_of(root, &run_id))[0].1, "playbook");

    let mut opts = with_tree("wt-b");
    opts.params.insert("tree".into(), "wt-a".into());
    let (run_id, _) = start(root, &stub, opts).unwrap();
    assert_eq!(
        pwd_of(root, "work"),
        root.join("wt-b").canonicalize().unwrap()
    );
    assert_eq!(resolved(&events_of(root, &run_id))[0].1, "caller");
}

/// A template over a node's output resolves when that node succeeds: the
/// node itself ran in the execution root under the root lock, every node
/// after it runs in the tree, and by then the lock has moved onto the tree.
#[test]
fn a_tree_published_by_a_node_takes_over_from_that_node_on() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "worktree: \"{{nodes.probe.output.working_tree}}\"");
    worktrees(root, &["wt"]);
    let stub = recording_stub(root, &stub_part("wt"));
    let (run_id, outcome) = start(root, &stub, RunOptions::default()).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);

    let tree = root.join("wt").canonicalize().unwrap();
    assert_eq!(pwd_of(root, "probe"), root.canonicalize().unwrap());
    assert_eq!(pwd_of(root, "work"), tree);
    let events = events_of(root, &run_id);
    assert_eq!(gate_output(&events), tree);
    assert_eq!(
        resolved(&events),
        vec![(
            tree.display().to_string(),
            "node".into(),
            Some("probe".into())
        )]
    );
    let before = fs::read_to_string(root.join("locks-probe")).unwrap();
    assert!(before.contains("root"), "the probe ran under the root lock");
    let after = fs::read_to_string(root.join("locks-work")).unwrap();
    assert!(
        !after.contains("root") && after.contains("tree-"),
        "the lock moved onto the tree: {after}"
    );
    assert!(
        !root.join(".apb/workdir.lock").exists()
            && fs::read_dir(root.join(".apb/locks"))
                .unwrap()
                .next()
                .is_none(),
        "every lock is released when the run ends"
    );
}

/// A node-published tree that is empty or missing fails the run before any
/// later node runs, instead of letting them run in the execution root.
#[test]
fn an_unresolvable_published_tree_fails_the_run() {
    for published in ["", "does/not/exist"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed(root, "worktree: \"{{nodes.probe.output.working_tree}}\"");
        let stub = recording_stub(root, &stub_part(published));
        let (run_id, outcome) = start(root, &stub, RunOptions::default()).unwrap();
        assert_eq!(outcome, RunStatus::Failed, "{published:?}");
        assert!(
            !invocations(root).iter().any(|i| i.node == "work"),
            "{published:?}: no node runs after the probe"
        );
        let reason = RunState::fold(&events_of(root, &run_id))
            .failure_reason
            .map(|r| r.reason)
            .unwrap_or_default();
        assert!(reason.contains("worktree"), "{published:?}: {reason}");
    }
}

/// Runs over different trees do not contend; two runs over one tree still
/// do, and a run in the execution root is not blocked by a run in a tree.
#[test]
fn runs_over_different_trees_do_not_contend() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "");
    worktrees(root, &["wt-a", "wt-b"]);
    let stub = recording_stub(root, &stub_part("unused"));

    // Another write-run holds tree A for the whole test.
    let _held = acquire_tree(root, Some(&root.join("wt-a")), false)
        .unwrap()
        .unwrap();
    let (_, outcome) = start(root, &stub, with_tree("wt-b")).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded, "tree B is free");
    let (_, outcome) = start(root, &stub, RunOptions::default()).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded, "the root is free");
    match start(root, &stub, with_tree("wt-a")) {
        Err(EngineError::WorkdirBusy(_)) => {}
        other => panic!("a second run over tree A must be refused, got {other:?}"),
    }

    // And the other way round: a run holding the root does not block a tree.
    let _root_held = acquire(root, false).unwrap().unwrap();
    let (_, outcome) = start(root, &stub, with_tree("wt-b")).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);
}

/// A tree must belong to the project: a directory inside it, or a git
/// worktree of its repository. Anything else refuses the start before a run
/// directory exists.
#[test]
fn a_tree_outside_the_project_is_refused_but_a_git_worktree_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    seed(&root, "");
    let stub = recording_stub(&root, &stub_part("unused"));

    let elsewhere = tempfile::tempdir().unwrap();
    match start(&root, &stub, with_tree(elsewhere.path().to_str().unwrap())) {
        Err(EngineError::Invalid(msg)) => {
            assert!(msg.contains("neither inside the project"), "{msg}")
        }
        other => panic!("an unrelated directory must be refused, got {other:?}"),
    }
    assert!(
        fs::read_dir(root.join(".apb/runs"))
            .map(|mut d| d.next().is_none())
            .unwrap_or(true),
        "a refused start leaves no run behind"
    );

    worktrees(&root, &[]);
    let linked = dir.path().join("linked");
    git(&root, &["worktree", "add", "-q", linked.to_str().unwrap()]);
    let (_, outcome) = start(&root, &stub, with_tree(linked.to_str().unwrap())).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(pwd_of(&root, "work"), linked.canonicalize().unwrap());
}

/// Trees that overlap the execution root's own checkout (a plain directory
/// in it) share the root's lock: their files are the root's files, so a run
/// there and a run in the root must still take turns.
#[test]
fn a_directory_of_the_root_checkout_shares_the_root_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "");
    fs::create_dir_all(root.join("sub")).unwrap();
    worktrees(root, &[]);
    let stub = recording_stub(root, &stub_part("unused"));
    let _root_held = acquire(root, false).unwrap().unwrap();
    match start(root, &stub, with_tree("sub")) {
        Err(EngineError::WorkdirBusy(_)) => {}
        other => panic!("a directory of the root checkout must share its lock, got {other:?}"),
    }
}

/// Runs that no longer wait on each other can start in the same millisecond.
/// A start whose `<id>-<ms>` directory is already taken moves on to a free
/// one and never writes into the other run's journal (the loser's start-up
/// failure used to land there).
#[test]
fn a_start_never_writes_into_a_run_directory_that_is_already_taken() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "");
    let stub = recording_stub(root, &stub_part("unused"));
    let now = apb_core::clock::now_ms();
    let taken: Vec<PathBuf> = (now..now + 500)
        .map(|ms| root.join(format!(".apb/runs/tree-{ms}")))
        .collect();
    for d in &taken {
        fs::create_dir_all(d).unwrap();
    }
    let (run_id, outcome) = start(root, &stub, RunOptions::default()).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(!taken.contains(&root.join(".apb/runs").join(&run_id)));
    assert!(
        taken
            .iter()
            .all(|d| fs::read_dir(d).unwrap().next().is_none()),
        "no taken run directory was written to"
    );
}
