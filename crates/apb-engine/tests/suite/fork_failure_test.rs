//! Fork branch-failure policies and `require: all_succeeded` joins (issue
//! #195), driven end to end with script nodes.
//!
//! The slow sibling's script leaves a marker on the line past its sleep, so the
//! marker's ABSENCE proves the branch was interrupted rather than waited out
//! (see `parallel_cancel_test` for why this is not a wall-clock bound).

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;

/// `start` forks into a failing `a` and a slow `b -> b2`, which merge in `j`.
/// `{fork}` is spliced into the start node.
fn fork_playbook(fork: &str) -> String {
    format!(
        r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - {{ id: start, type: start{fork} }}
  - {{ id: a, type: script, script: "scripts/fail.sh", runner: sh }}
  - {{ id: b, type: script, script: "scripts/slow.sh", runner: sh }}
  - {{ id: b2, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: j, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: rejected, type: script, script: "scripts/count.sh", runner: sh }}
  - {{ id: lost, type: finish, outcome: failure }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: a }}
  - {{ from: start, to: b }}
  - {{ from: a, to: j }}
  - {{ from: b, to: b2 }}
  - {{ from: b2, to: j }}
  - {{ from: j, to: done }}
  - {{ from: rejected, to: lost }}
"#
    )
}

/// The agency-site shape: both branches fail into one shared sink
/// `rejected`, which is therefore itself an implicit join.
const SHARED_SINK: &str = r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - { id: start, type: start, fork: { on_branch_failure: cancel_siblings } }
  - { id: a, type: script, script: "scripts/fail.sh", runner: sh }
  - { id: b, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: rejected, type: script, script: "scripts/count.sh", runner: sh }
  - { id: lost, type: finish, outcome: failure }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, condition: { type: node_status, node: a, equals: success } }
  - { from: a, to: rejected, condition: { type: node_status, node: a, equals: failure } }
  - { from: b, to: j, condition: { type: node_status, node: b, equals: success } }
  - { from: b, to: rejected, condition: { type: node_status, node: b, equals: failure } }
  - { from: j, to: done }
  - { from: rejected, to: lost }
"#;

/// A join that requires both branches to succeed. `a` reaches `j` only on
/// success, so a failed `a` is a dead arrival. `{a}` is `a`'s script and
/// `{j_out}` the edges out of `j`; the `handled -> lost` failure path exists
/// only when `j_out` leads into it.
fn require_playbook(a: &str, j_out: &str) -> String {
    let yaml = format!(
        r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: a, type: script, script: "scripts/{a}", runner: sh }}
  - {{ id: b, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: j, type: script, script: "scripts/count.sh", runner: sh }}
  - {{ id: handled, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: lost, type: finish, outcome: failure }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: a }}
  - {{ from: start, to: b }}
  - {{ from: a, to: j, condition: {{ type: node_status, node: a, equals: success }} }}
  - {{ from: b, to: j, require: all_succeeded }}
{j_out}  - {{ from: handled, to: lost }}
"#
    );
    match j_out.contains("handled") {
        true => yaml,
        false => yaml
            .lines()
            .filter(|l| !l.contains("handled") && !l.contains("id: lost"))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

const MARKER: &str = "slow-branch-ran-to-completion";
const COUNT: &str = "count-runs";

fn seed(root: &Path, yaml: &str, slow_seconds: u32) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/fk/1.0.0");
    let scripts = vdir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/fk/current"), "1.0.0").unwrap();
    fs::write(scripts.join("fail.sh"), "echo broken\nexit 1\n").unwrap();
    fs::write(scripts.join("ok.sh"), "echo ok\n").unwrap();
    fs::write(
        scripts.join("slow.sh"),
        format!(
            "sleep {slow_seconds}\n: > '{}'\necho ok\n",
            root.join(MARKER).display()
        ),
    )
    .unwrap();
    fs::write(
        scripts.join("count.sh"),
        format!(
            "printf x >> '{}'\necho counted\n",
            root.join(COUNT).display()
        ),
    )
    .unwrap();
}

fn run_events(root: &Path) -> (RunStatus, String, Vec<Event>) {
    let res = run(root, "fk", None, RunOptions::default()).unwrap();
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    (res.outcome, res.run_id, events)
}

fn finished(events: &[Event], node: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n, status, ..
            } if n == node => Some(status.clone()),
            _ => None,
        })
        .collect()
}

fn started(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
}

fn cancelled_by_fork(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BranchCancelled {
                node, failed_node, ..
            } => Some((node.clone(), failed_node.clone())),
            _ => None,
        })
        .collect()
}

fn count_runs(root: &Path) -> usize {
    fs::read_to_string(root.join(COUNT))
        .map(|s| s.len())
        .unwrap_or(0)
}

#[test]
fn fail_fast_interrupts_the_sibling_and_runs_the_failure_target_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &fork_playbook(", fork: { on_branch_failure: fail_fast, on_failure: rejected }"),
        5,
    );
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(
        !root.join(MARKER).exists(),
        "the slow sibling was waited out instead of interrupted"
    );
    assert_eq!(finished(&events, "b"), ["cancelled"]);
    assert_eq!(cancelled_by_fork(&events), [("b".into(), "a".into())]);
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::BranchFailed { fork, node, policy, target }
            if fork == "start" && node == "a" && policy == "fail_fast"
                && target.as_deref() == Some("rejected")
    )));
    // The failure target runs exactly once; the join and the rest of the
    // cancelled branch never run.
    assert_eq!(count_runs(root), 1);
    assert_eq!(finished(&events, "rejected"), ["succeeded"]);
    assert!(!started(&events, "j") && !started(&events, "b2"));
    assert_eq!(finished(&events, "lost"), ["succeeded"]);
}

#[test]
fn a_resume_after_fail_fast_does_not_bring_cancelled_branches_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &fork_playbook(", fork: { on_branch_failure: fail_fast, on_failure: rejected }"),
        5,
    );
    let (_, run_id, _) = run_events(root);

    // Nothing is left to run: the cancelled branch's `b2` and the join are
    // not pending heads, so the resume is refused as pointless.
    let resumed = resume(root, &run_id, None);
    assert!(resumed.is_err(), "resume found work: {resumed:?}");
    let events = read_all(&root.join(".apb/runs").join(&run_id)).unwrap();
    assert!(!started(&events, "j") && !started(&events, "b2"));
    assert_eq!(count_runs(root), 1);
}

#[test]
fn cancel_siblings_follows_the_failure_edge_into_a_shared_sink() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, SHARED_SINK, 5);
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(
        !root.join(MARKER).exists(),
        "the sibling was not interrupted"
    );
    assert_eq!(cancelled_by_fork(&events), [("b".into(), "a".into())]);
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::BranchFailed { node, policy, target: None, .. }
            if node == "a" && policy == "cancel_siblings"
    )));
    // `a`'s own failure edge was taken, and the shared sink did not wait for
    // the cancelled sibling.
    assert_eq!(count_runs(root), 1);
    assert!(!started(&events, "j"));
    assert_eq!(finished(&events, "lost"), ["succeeded"]);
}

#[test]
fn the_default_wait_lets_the_sibling_finish() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // `rejected` stays reachable through the failure policy, which never
    // fires here: `a` moves on along its unconditional edge.
    let yaml = fork_playbook("").replace("nodes:", "defaults:\n  on_failure: rejected\nnodes:");
    seed(root, &yaml, 1);
    let (_, _, events) = run_events(root);

    assert!(
        root.join(MARKER).exists(),
        "the sibling must run to completion"
    );
    assert_eq!(finished(&events, "b"), ["succeeded"]);
    assert_eq!(finished(&events, "j"), ["succeeded"]);
    assert!(cancelled_by_fork(&events).is_empty());
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::BranchFailed { .. }))
    );
}

#[test]
fn require_all_succeeded_refuses_a_dead_arrival_and_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &require_playbook("fail.sh", "  - { from: j, to: done }\n"),
        1,
    );
    let (outcome, run_id, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(count_runs(root), 0, "a refused join must not run");
    assert_eq!(finished(&events, "j"), ["failed"]);
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::JoinRefused { node, sources, .. } if node == "j" && sources == &["a".to_string()]
    )));
    assert!(
        !started(&events, "done"),
        "a refused join takes no unconditional edge"
    );
    let view = apb_engine::run_view::RunView::load(&root.join(".apb/runs").join(&run_id), &run_id)
        .unwrap();
    let reason = view.failure_reason().unwrap_or_default();
    assert!(reason.contains("refused"), "{reason}");
}

#[test]
fn a_refused_join_takes_its_failure_route() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &require_playbook(
            "fail.sh",
            "  - { from: j, to: done, condition: { type: node_status, node: j, equals: success } }\n  - { from: j, to: handled, condition: { type: node_status, node: j, equals: failure } }\n",
        ),
        1,
    );
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(count_runs(root), 0);
    assert_eq!(finished(&events, "handled"), ["succeeded"]);
    assert_eq!(finished(&events, "lost"), ["succeeded"]);
}

#[test]
fn require_all_succeeded_runs_the_join_when_every_branch_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &require_playbook("ok.sh", "  - { from: j, to: done }\n"),
        1,
    );
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(count_runs(root), 1);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::JoinRefused { .. }))
    );
}

#[test]
fn fail_fast_on_the_sequential_path_cancels_the_unstarted_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // One slot: `a` runs alone first and `b` waits in the frontier.
    let yaml = fork_playbook(", fork: { on_branch_failure: fail_fast, on_failure: rejected }")
        .replace("nodes:", "defaults:\n  max_parallel: 1\nnodes:");
    seed(root, &yaml, 5);
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(!root.join(MARKER).exists(), "the sibling must never start");
    assert_eq!(finished(&events, "b"), ["cancelled"]);
    assert_eq!(cancelled_by_fork(&events), [("b".into(), "a".into())]);
    assert_eq!(count_runs(root), 1);
    assert!(!started(&events, "j") && !started(&events, "b2"));
}
