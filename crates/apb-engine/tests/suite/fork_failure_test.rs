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

/// Seeds `yaml` with extra scripts: `slowfail.sh` fails after a second, and
/// `hold.sh` sleeps five seconds before leaving the marker.
fn seed_with(root: &Path, yaml: &str) {
    seed(root, yaml, 5);
    let scripts = root.join(".apb/playbooks/fk/1.0.0/scripts");
    fs::write(
        scripts.join("slowfail.sh"),
        "sleep 1\necho broken\nexit 1\n",
    )
    .unwrap();
}

/// The reviewer's rework shape: `review` can send `design` back, which made
/// `design` look shared between both branches.
const REWORK: &str = r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: split, type: script, script: "scripts/ok.sh", runner: sh, fork: { on_branch_failure: fail_fast, on_failure: rejected } }
  - { id: design, type: script, script: "scripts/fail.sh", runner: sh }
  - { id: content, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: assemble, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: review, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: rejected, type: script, script: "scripts/count.sh", runner: sh }
  - { id: lost, type: finish, outcome: failure }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: split }
  - { from: split, to: design }
  - { from: split, to: content }
  - { from: design, to: assemble, join: all }
  - { from: content, to: assemble, join: all }
  - { from: assemble, to: review }
  - { from: review, to: design, max_traversals: 2, condition: { type: output_match, node: review, pattern: rejected } }
  - { from: review, to: done, fallback: true }
  - { from: rejected, to: lost }
"#;

#[test]
fn fail_fast_holds_with_a_rework_loop_into_a_head() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_with(root, REWORK);
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(!root.join(MARKER).exists(), "content was not interrupted");
    assert_eq!(
        cancelled_by_fork(&events),
        [("content".into(), "design".into())]
    );
    assert_eq!(count_runs(root), 1);
    assert!(!started(&events, "assemble") && !started(&events, "done"));
}

/// `content -> notify -> done` never reaches the join: that dead end belongs to
/// `content`'s branch and is cancelled with it.
const DEAD_END: &str = r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - { id: start, type: start, fork: { on_branch_failure: fail_fast, on_failure: rejected } }
  - { id: design, type: script, script: "scripts/slowfail.sh", runner: sh }
  - { id: content, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: notify, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: assemble, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: rejected, type: script, script: "scripts/count.sh", runner: sh }
  - { id: lost, type: finish, outcome: failure }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: design }
  - { from: start, to: content }
  - { from: design, to: assemble }
  - { from: content, to: notify }
  - { from: notify, to: done }
  - { from: assemble, to: done }
  - { from: rejected, to: lost }
"#;

#[test]
fn fail_fast_cancels_a_dead_end_only_the_sibling_reaches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_with(root, DEAD_END);
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(finished(&events, "content"), ["succeeded"]);
    assert!(!root.join(MARKER).exists(), "notify ran");
    assert_eq!(finished(&events, "notify"), ["cancelled"]);
    assert!(!started(&events, "done"));
    assert_eq!(count_runs(root), 1);
}

/// An outer `fail_fast` fork around an inner `cancel_siblings` fork. `x1`'s
/// edges out are spliced in as `{x1_out}`.
fn nested(x1_out: &str) -> String {
    format!(
        r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - {{ id: start, type: start, fork: {{ on_branch_failure: fail_fast, on_failure: rejected }} }}
  - {{ id: x, type: script, script: "scripts/ok.sh", runner: sh, fork: {{ on_branch_failure: cancel_siblings }} }}
  - {{ id: x1, type: script, script: "scripts/fail.sh", runner: sh }}
  - {{ id: x2, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: x_fix, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: xj, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: y, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: y2, type: script, script: "scripts/slow.sh", runner: sh }}
  - {{ id: oj, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: rejected, type: script, script: "scripts/count.sh", runner: sh }}
  - {{ id: lost, type: finish, outcome: failure }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: x }}
  - {{ from: start, to: y }}
  - {{ from: x, to: x1 }}
  - {{ from: x, to: x2 }}
{x1_out}  - {{ from: x_fix, to: xj }}
  - {{ from: x2, to: xj, condition: {{ type: node_status, node: x2, equals: success }} }}
  - {{ from: xj, to: oj }}
  - {{ from: y, to: y2 }}
  - {{ from: y2, to: oj }}
  - {{ from: oj, to: done }}
  - {{ from: rejected, to: lost }}
"#
    )
}

#[test]
fn an_inner_failure_with_no_route_escalates_to_the_outer_fail_fast() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // x1 reaches xj only on success; x_fix hangs off x2 so it stays reachable.
    let yaml = nested(
        "  - { from: x1, to: xj, condition: { type: node_status, node: x1, equals: success } }\n  - { from: x2, to: x_fix, condition: { type: node_status, node: x2, equals: failure } }\n",
    );
    seed_with(root, &yaml);
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(
        !root.join(MARKER).exists(),
        "the outer sibling y was not interrupted"
    );
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::BranchFailed { fork, node, policy, .. }
            if fork == "start" && node == "x1" && policy == "fail_fast"
    )));
    assert_eq!(count_runs(root), 1);
    assert!(!started(&events, "oj"));
}

#[test]
fn an_inner_failure_handled_inside_leaves_the_outer_siblings_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // x1 fails into x_fix, a node of its own branch: handled inside.
    let yaml = nested(
        "  - { from: x1, to: xj, condition: { type: node_status, node: x1, equals: success } }\n  - { from: x1, to: x_fix, condition: { type: node_status, node: x1, equals: failure } }\n",
    );
    let yaml = yaml.replace("scripts/slow.sh", "scripts/hold1.sh");
    seed_with(root, &yaml);
    fs::write(
        root.join(".apb/playbooks/fk/1.0.0/scripts/hold1.sh"),
        format!("sleep 1\n: > '{}'\necho ok\n", root.join(MARKER).display()),
    )
    .unwrap();
    let (outcome, _, events) = run_events(root);

    assert_eq!(
        outcome,
        RunStatus::Succeeded,
        "{:?}",
        events.iter().map(|e| &e.payload).collect::<Vec<_>>()
    );
    assert!(root.join(MARKER).exists(), "the outer sibling must finish");
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::BranchFailed { fork, node, .. } if fork == "x" && node == "x1"
    )));
    assert_eq!(count_runs(root), 0);
    assert_eq!(finished(&events, "oj"), ["succeeded"]);
}

/// Keeps the journal up to and including the first line `keep` accepts, then
/// appends `extra` lines.
fn cut_journal(run_dir: &Path, keep: impl Fn(&str) -> bool, extra: &[&str]) {
    let path = run_dir.join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        out.push(line.to_string());
        if keep(line) {
            break;
        }
    }
    let base = out.len();
    for (i, x) in extra.iter().enumerate() {
        out.push(x.replace("SEQ", &(base + i).to_string()));
    }
    fs::write(&path, out.join("\n") + "\n").unwrap();
    let _ = fs::remove_file(run_dir.join("driver.pid"));
}

#[test]
fn a_resume_applies_the_policy_a_dead_driver_never_journaled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = fork_playbook(", fork: { on_branch_failure: fail_fast, on_failure: rejected }");
    seed(root, &yaml, 5);
    let (_, run_id, _) = run_events(root);
    let run_dir = root.join(".apb/runs").join(&run_id);
    // The driver died right after `a` finished failed, while `b` was still
    // running: `b`'s attempt is left open, and the resume restarts it.
    cut_journal(
        &run_dir,
        |l| l.contains(r#""type":"node_finished""#) && l.contains(r#""node":"a""#),
        &[],
    );
    let _ = fs::remove_file(root.join(COUNT));

    let res = resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::BranchFailed { node, .. } if node == "a"
    )));
    assert_eq!(finished(&events, "b"), ["cancelled"]);
    assert!(!root.join(MARKER).exists());
    assert_eq!(count_runs(root), 1);
    assert!(!started(&events, "j"));
}

#[test]
fn a_resume_finishes_a_cancellation_a_paused_batch_left_undone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // `b` succeeds into the join at once; `a` fails a second later.
    let yaml = fork_playbook(", fork: { on_branch_failure: fail_fast, on_failure: rejected }")
        .replace("scripts/fail.sh", "scripts/slowfail.sh")
        .replace("scripts/slow.sh", "scripts/ok.sh")
        .replace(
            "  - { from: b, to: b2 }\n  - { from: b2, to: j }\n",
            "  - { from: b, to: j }\n  - { from: b2, to: j }\n",
        )
        .replace(
            "  - { from: start, to: b }\n",
            "  - { from: start, to: b }\n  - { from: start, to: b2 }\n",
        );
    seed_with(root, &yaml);
    let (_, run_id, _) = run_events(root);
    let run_dir = root.join(".apb/runs").join(&run_id);
    // Paused right after the policy was journaled, before any cancellation.
    cut_journal(
        &run_dir,
        |l| l.contains(r#""type":"edge_traversed""#) && l.contains(r#""to":"rejected""#),
        &[r#"{"seq":SEQ,"ts":1,"type":"run_paused","reason":"test"}"#],
    );
    let _ = fs::remove_file(root.join(COUNT));

    let res = resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    // `j` had `b`'s delivery and `a` dead: without the recovery it would run.
    assert!(!finished(&events, "j").iter().any(|s| s != "cancelled"));
    assert_eq!(count_runs(root), 1);
}

/// A `require` join inside a `fail_fast` fork's branches refuses: the fork's
/// policy routes the refusal to `on_failure` instead of stopping the run.
const REFUSED_IN_FORK: &str = r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - { id: start, type: start, fork: { on_branch_failure: fail_fast, on_failure: rejected } }
  - { id: x, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: x1, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: x2, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: side, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: xj, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: y, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: oj, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: rejected, type: script, script: "scripts/count.sh", runner: sh }
  - { id: lost, type: finish, outcome: failure }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: x }
  - { from: start, to: y }
  - { from: x, to: x1 }
  - { from: x, to: x2 }
  - { from: x1, to: xj, condition: { type: output_match, node: x1, pattern: go } }
  - { from: x1, to: side, fallback: true }
  - { from: x2, to: xj, require: all_succeeded }
  - { from: side, to: oj }
  - { from: xj, to: oj }
  - { from: y, to: oj }
  - { from: oj, to: done }
  - { from: rejected, to: lost }
"#;

#[test]
fn a_refused_join_inside_a_fail_fast_fork_goes_to_the_failure_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, REFUSED_IN_FORK, 1);
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::JoinRefused { node, .. } if node == "xj"
    )));
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::BranchFailed { node, policy, .. } if node == "xj" && policy == "fail_fast"
    )));
    assert_eq!(count_runs(root), 1, "the failure target must run once");
    assert_eq!(finished(&events, "lost"), ["succeeded"]);
    assert!(!started(&events, "done"));
}

// --- pipelined batches (issue #195) ---

/// The agency-site shape: `design1 -> design2` against one long `content`
/// step. `{policy}` is the fork option; `{d2_out}` the edges out of `design2`
/// and `{c_out}` those out of `content`.
fn agency(fork: &str, d2_out: &str, c_out: &str) -> String {
    format!(
        r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - {{ id: start, type: start{fork} }}
  - {{ id: design1, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: design2, type: script, script: "scripts/fail.sh", runner: sh }}
  - {{ id: content, type: script, script: "scripts/slow.sh", runner: sh }}
  - {{ id: assemble, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: rejected, type: script, script: "scripts/count.sh", runner: sh }}
  - {{ id: lost, type: finish, outcome: failure }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: design1 }}
  - {{ from: start, to: content }}
  - {{ from: design1, to: design2 }}
{d2_out}{c_out}  - {{ from: assemble, to: done }}
  - {{ from: rejected, to: lost }}
"#
    )
}

fn position(events: &[Event], pred: impl Fn(&EventPayload) -> bool) -> usize {
    events
        .iter()
        .position(|e| pred(&e.payload))
        .expect("event present")
}

#[test]
fn fail_fast_kills_a_long_sibling_when_a_later_branch_step_fails() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &agency(
            ", fork: { on_branch_failure: fail_fast, on_failure: rejected }",
            "  - { from: design2, to: assemble }\n",
            "  - { from: content, to: assemble }\n",
        ),
        5,
    );
    let clock = std::time::Instant::now();
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(!root.join(MARKER).exists(), "content was waited out");
    assert!(clock.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(finished(&events, "design2"), ["failed"]);
    assert_eq!(finished(&events, "content"), ["cancelled"]);
    assert_eq!(
        cancelled_by_fork(&events),
        [("content".into(), "design2".into())]
    );
    assert_eq!(count_runs(root), 1);
    assert!(!started(&events, "assemble"));
}

#[test]
fn cancel_siblings_kills_a_long_sibling_when_a_later_branch_step_fails() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &agency(
            ", fork: { on_branch_failure: cancel_siblings }",
            "  - { from: design2, to: assemble, condition: { type: node_status, node: design2, equals: success } }\n  - { from: design2, to: rejected, condition: { type: node_status, node: design2, equals: failure } }\n",
            "  - { from: content, to: assemble, condition: { type: node_status, node: content, equals: success } }\n  - { from: content, to: rejected, condition: { type: node_status, node: content, equals: failure } }\n",
        ),
        5,
    );
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(!root.join(MARKER).exists(), "content was waited out");
    assert_eq!(
        cancelled_by_fork(&events),
        [("content".into(), "design2".into())]
    );
    assert_eq!(count_runs(root), 1);
    assert!(!started(&events, "assemble"));
}

#[test]
fn a_fast_branch_takes_its_next_step_while_the_slow_branch_runs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = agency("", "  - { from: design2, to: assemble }\n", "  - { from: content, to: assemble }\n")
        .replace("scripts/fail.sh", "scripts/ok.sh")
        .replace("  - { id: rejected, type: script, script: \"scripts/count.sh\", runner: sh }\n  - { id: lost, type: finish, outcome: failure }\n", "")
        .replace("  - { from: rejected, to: lost }\n", "");
    seed(root, &yaml, 1);
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Succeeded);
    let d2_start = position(
        &events,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "design2"),
    );
    let content_end = position(
        &events,
        |p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "content"),
    );
    assert!(d2_start < content_end, "design2 waited for content");
    // The join still waited for both branches.
    let assemble_start = position(
        &events,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "assemble"),
    );
    assert!(content_end < assemble_start);
}

#[test]
fn pipelining_respects_max_parallel() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Three branches, two of them two steps long, and two slots.
    let yaml = r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
defaults:
  max_parallel: 2
nodes:
  - { id: start, type: start }
  - { id: a1, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: a2, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: b1, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: b2, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: c, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a1 }
  - { from: start, to: b1 }
  - { from: start, to: c }
  - { from: a1, to: a2 }
  - { from: b1, to: b2 }
  - { from: a2, to: j }
  - { from: b2, to: j }
  - { from: c, to: j }
  - { from: j, to: done }
"#;
    seed(root, yaml, 1);
    fs::write(
        root.join(".apb/playbooks/fk/1.0.0/scripts/nap.sh"),
        "sleep 0.3\necho ok\n",
    )
    .unwrap();
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Succeeded);
    let mut running = 0i32;
    let mut peak = 0i32;
    for e in &events {
        match &e.payload {
            EventPayload::NodeStarted { node, .. } if node != "start" && node != "done" => {
                running += 1
            }
            EventPayload::NodeFinished { node, .. } if node != "start" && node != "done" => {
                running -= 1
            }
            _ => {}
        }
        peak = peak.max(running);
    }
    assert_eq!(peak, 2, "never more than two members at once");
    assert_eq!(finished(&events, "j"), ["succeeded"]);
}

#[test]
fn a_resume_mid_pipeline_completes_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = agency("", "  - { from: design2, to: assemble }\n", "  - { from: content, to: assemble }\n")
        .replace("scripts/fail.sh", "scripts/ok.sh")
        .replace("  - { id: rejected, type: script, script: \"scripts/count.sh\", runner: sh }\n  - { id: lost, type: finish, outcome: failure }\n", "")
        .replace("  - { from: rejected, to: lost }\n", "");
    seed(root, &yaml, 1);
    let (_, run_id, _) = run_events(root);
    let run_dir = root.join(".apb/runs").join(&run_id);
    // The driver died after design2 was admitted mid-batch, with content
    // still running.
    cut_journal(
        &run_dir,
        |l| l.contains(r#""type":"node_started""#) && l.contains(r#""node":"design2""#),
        &[],
    );
    let _ = fs::remove_file(root.join(MARKER));

    let res = resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(finished(&events, "assemble"), ["succeeded"]);
    assert_eq!(finished(&events, "done"), ["succeeded"]);
}

/// `b` reaches the `require` join while `a` still runs (the join is not ready
/// then); `a` fails a second later. The join is re-offered once the batch
/// ends and refuses, instead of being forgotten.
#[test]
fn a_join_a_sibling_reached_early_is_reoffered_when_the_late_branch_dies() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_with(
        root,
        &require_playbook(
            "slowfail.sh",
            "  - { from: j, to: done, condition: { type: node_status, node: j, equals: success } }\n  - { from: j, to: handled, condition: { type: node_status, node: j, equals: failure } }\n",
        ),
    );
    let (outcome, _, events) = run_events(root);

    assert_eq!(outcome, RunStatus::Failed);
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::JoinRefused { node, .. } if node == "j"
    )));
    assert_eq!(finished(&events, "handled"), ["succeeded"]);
}

/// Issue #195: the chain after a fork's join leaves the batch, so the control
/// scan runs between its nodes again: a context note posted while `j` runs is
/// applied before `k` starts.
#[test]
fn the_control_scan_runs_between_the_nodes_after_a_join() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: b, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: j, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: k, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: k }
  - { from: k, to: done }
"#;
    seed(root, yaml, 1);
    fs::write(
        root.join(".apb/playbooks/fk/1.0.0/scripts/nap.sh"),
        "sleep 1\necho ok\n",
    )
    .unwrap();
    let runs = root.join(".apb/runs");
    let poster = {
        let runs = runs.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while std::time::Instant::now() < deadline {
                if let Some(run_dir) = fs::read_dir(&runs)
                    .ok()
                    .and_then(|mut d| d.next())
                    .and_then(|e| e.ok())
                    .map(|e| e.path())
                    && let Ok(events) = read_all(&run_dir)
                    && started(&events, "j")
                {
                    apb_engine::control::post_control(
                        &run_dir,
                        apb_engine::control::Control::ContextAppend {
                            note: "for k".into(),
                        },
                    )
                    .unwrap();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
    };
    let (outcome, _, events) = run_events(root);
    poster.join().unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);
    let applied = position(
        &events,
        |p| matches!(p, EventPayload::SupervisorAction { action, .. } if action == "context_append"),
    );
    let k_start = position(
        &events,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "k"),
    );
    assert!(
        applied < k_start,
        "the note reached k only after it started"
    );
}

/// Issue #195: a member that is a cache hit and wins a `join: any` cancels
/// the running sibling exactly as an executed member does: the sibling is
/// killed and finishes once, `cancelled`, never journaled twice.
#[test]
fn a_cache_hit_that_wins_a_join_any_cancels_the_running_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: fast, type: script, script: "scripts/fast.sh", runner: sh, cache: auto }
  - { id: slow, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: j, type: script, script: "scripts/fast.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: fast }
  - { from: start, to: slow }
  - { from: fast, to: j, join: any }
  - { from: slow, to: j, join: any }
  - { from: j, to: done }
"#;
    init_project(root).unwrap();
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    fs::write(root.join("work.txt"), "hello\n").unwrap();
    let vdir = root.join(".apb/playbooks/fk/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/fk/current"), "1.0.0").unwrap();
    fs::write(vdir.join("scripts/fast.sh"), "echo fast\n").unwrap();
    let marker = root.join(".apb").join(MARKER);
    fs::write(
        vdir.join("scripts/slow.sh"),
        format!("sleep 3\n: > '{}'\necho slow\n", marker.display()),
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "c1",
        ],
    ] {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(&args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }
    // The first run stores `fast`; the second hits it.
    let (first, _, _) = run_events(root);
    assert_eq!(first, RunStatus::Succeeded);
    let (outcome, _, events) = run_events(root);
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(
        events.iter().any(
            |e| matches!(&e.payload, EventPayload::NodeCacheHit { node, .. } if node == "fast")
        ),
        "the second run must hit the cache"
    );
    assert_eq!(finished(&events, "slow"), ["cancelled"]);
    assert!(!marker.exists(), "the sibling ran to completion");
    assert_eq!(finished(&events, "j"), ["succeeded"]);
}

/// A fan-in inside its own cycle (`f -> {a, b} -> c -> f`, `c` first-arrival)
/// runs `c` once per pass whichever branch ends first, and the next pass never
/// starts beside the old one (issue #195, pipelined batches).
fn cycle_fan_in(a_script: &str, b_script: &str) -> String {
    format!(
        r#"
schema: 2
id: fk
name: Fork
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: f, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: a, type: script, script: "scripts/{a_script}", runner: sh }}
  - {{ id: b, type: script, script: "scripts/{b_script}", runner: sh }}
  - {{ id: c, type: script, script: "scripts/count.sh", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: f }}
  - {{ from: f, to: a }}
  - {{ from: f, to: b }}
  - {{ from: a, to: c }}
  - {{ from: b, to: c }}
  - {{ from: c, to: f, max_traversals: 1, condition: {{ type: node_status, node: c, equals: success }} }}
  - {{ from: c, to: done, fallback: true }}
"#
    )
}

#[test]
fn a_fan_in_inside_a_cycle_runs_once_per_pass_in_either_order() {
    for (a, b) in [("ok.sh", "nap.sh"), ("nap.sh", "ok.sh")] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed(root, &cycle_fan_in(a, b), 1);
        fs::write(
            root.join(".apb/playbooks/fk/1.0.0/scripts/nap.sh"),
            "sleep 0.5\necho ok\n",
        )
        .unwrap();
        let (outcome, _, events) = run_events(root);
        assert_eq!(outcome, RunStatus::Succeeded, "{a}/{b}");
        assert_eq!(count_runs(root), 2, "c once per pass ({a}/{b})");
        assert_eq!(finished(&events, "a").len(), 2, "{a}/{b}");
        assert_eq!(finished(&events, "b").len(), 2, "{a}/{b}");
        // The second pass starts only after both branches of the first ended.
        let second_f = events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "f"))
            .nth(1)
            .map(|(i, _)| i)
            .unwrap();
        let first_ends: Vec<usize> = ["a", "b"]
            .iter()
            .map(|n| {
                position(
                    &events,
                    |p| matches!(p, EventPayload::NodeFinished { node, .. } if node == *n),
                )
            })
            .collect();
        assert!(first_ends.iter().all(|e| *e < second_f), "{a}/{b}");
    }
}
