//! Joins inside a loop and implicit joins at a finish, driven end to end
//! with script nodes.
//!
//! A loop round runs the join's branches again, but the nodes of the round
//! before still carry their finished status and their hops: the join must
//! wait for a branch that runs again this round instead of counting its
//! delivery of the round before. A finish node with two incoming edges must
//! end the run when one of them delivers and the other can no longer run.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

/// A review panel round as a fork: each gate either sends its reviewer
/// (`open`) or goes straight to the collector (`clean`), the collector is an
/// explicit `join: all`, and a bounded back edge starts the next round.
/// Reviewer `b` takes two steps, so when `a` is clean the collector is
/// weighed while `b`'s first step is still to run.
const PANEL: &str = r#"
schema: 2
id: jl
name: Join loop
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: fan, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: gate_a, type: script, script: "scripts/gate_a.sh", runner: sh }
  - { id: gate_b, type: script, script: "scripts/gate_b.sh", runner: sh }
  - { id: rev_a, type: script, script: "scripts/rev_a.sh", runner: sh }
  - { id: prep_b, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: rev_b, type: script, script: "scripts/rev_b.sh", runner: sh }
  - { id: collect, type: script, script: "scripts/collect.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: fan }
  - { from: fan, to: gate_a }
  - { from: fan, to: gate_b }
  - { from: gate_a, to: rev_a, condition: { type: output_match, node: gate_a, pattern: open } }
  - { from: gate_a, to: collect, join: all, condition: { type: output_match, node: gate_a, pattern: clean } }
  - { from: gate_b, to: prep_b, condition: { type: output_match, node: gate_b, pattern: open } }
  - { from: prep_b, to: rev_b }
  - { from: gate_b, to: collect, join: all, condition: { type: output_match, node: gate_b, pattern: clean } }
  - { from: rev_a, to: collect, join: all }
  - { from: rev_b, to: collect, join: all }
  - { from: collect, to: done, condition: { type: output_match, node: collect, pattern: finished } }
  - { from: collect, to: fan, condition: { type: output_match, node: collect, pattern: again }, max_traversals: 2 }
"#;

/// A finish fed by two edges: `collect` ends the run when it is clean,
/// otherwise a bounded fix loop runs and, once spent, a person's node ends
/// it through the same finish.
const SHARED_FINISH: &str = r#"
schema: 2
id: jl
name: Join loop
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: collect, type: script, script: "scripts/collect.sh", runner: sh }
  - { id: fix, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: handoff, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: collect }
  - { from: collect, to: done, condition: { type: output_match, node: collect, pattern: finished } }
  - { from: collect, to: fix, fallback: true }
  - { from: fix, to: collect, max_traversals: 2 }
  - { from: fix, to: handoff, fallback: true }
  - { from: handoff, to: done }
"#;

/// A script that counts its own runs in `<root>/n-<name>` and prints the
/// line of `answers` for this run (the last line once they run out).
fn counted(root: &Path, name: &str, answers: &[&str], before: &str) -> String {
    let cases: String = answers
        .iter()
        .enumerate()
        .map(|(i, a)| format!("  {}) echo {a} ;;\n", i + 1))
        .collect();
    format!(
        "f='{f}'\nn=$(cat \"$f\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$f\"\n{before}case $n in\n{cases}  *) echo {last} ;;\nesac\n",
        f = root.join(format!("n-{name}")).display(),
        last = answers.last().copied().unwrap_or("ok"),
    )
}

fn seed(root: &Path, yaml: &str, scripts: &[(&str, String)]) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/jl/1.0.0");
    let dir = vdir.join("scripts");
    fs::create_dir_all(&dir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/jl/current"), "1.0.0").unwrap();
    fs::write(dir.join("ok.sh"), "echo ok\n").unwrap();
    for (name, body) in scripts {
        fs::write(dir.join(name), body).unwrap();
    }
}

/// The positions of `kind` events (`start` or `finish`) of `node`, in
/// journal order.
fn positions(events: &[Event], node: &str, finished: bool) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| match &e.payload {
            EventPayload::NodeStarted { node: n, .. } => !finished && n == node,
            EventPayload::NodeFinished { node: n, .. } => finished && n == node,
            _ => false,
        })
        .map(|(i, _)| i)
        .collect()
}

/// Round 1 both reviewers run; from round 2 `a` is clean and only `b` runs
/// again, slowly. Every round the collector starts only after `b`'s review
/// of that round finished, so it never reads the round before's review.
#[test]
fn a_join_in_a_loop_waits_for_the_branch_that_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let slow = "sleep 1\n";
    seed(
        root,
        PANEL,
        &[
            ("gate_a.sh", counted(root, "gate_a", &["open", "clean"], "")),
            ("gate_b.sh", counted(root, "gate_b", &["open"], "")),
            ("rev_a.sh", counted(root, "rev_a", &["ok"], "")),
            ("rev_b.sh", counted(root, "rev_b", &["ok", "ok"], slow)),
            (
                "collect.sh",
                counted(root, "collect", &["again", "again", "finished"], ""),
            ),
        ],
    );
    let res = run(root, "jl", None, RunOptions::default()).unwrap();
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let rev_b_done = positions(&ev, "rev_b", true);
    let collect_started = positions(&ev, "collect", false);
    assert_eq!((rev_b_done.len(), collect_started.len()), (3, 3));
    for (round, (done, start)) in rev_b_done.iter().zip(&collect_started).enumerate() {
        assert!(
            done < start,
            "round {}: the collector started before rev_b finished",
            round + 1
        );
    }
    assert_eq!(
        positions(&ev, "rev_a", false).len(),
        1,
        "a is clean after round 1"
    );
}

/// `collect` is clean at once: the run ends at `done`, although `handoff`,
/// the finish's other input, never ran.
#[test]
fn a_finish_with_two_inputs_ends_the_run_on_the_one_that_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        SHARED_FINISH,
        &[("collect.sh", counted(root, "collect", &["finished"], ""))],
    );
    let res = run(root, "jl", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(positions(&ev, "handoff", false).is_empty());
}

/// `collect` is clean in the last round the fix loop allows: the run ends
/// at `done`. The fix's bounded edge is spent by then, which must not read
/// as if the fix had taken its fallback into `handoff`.
#[test]
fn a_finish_with_two_inputs_ends_the_run_after_the_loop_spent_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        SHARED_FINISH,
        &[(
            "collect.sh",
            counted(root, "collect", &["open", "open", "finished"], ""),
        )],
    );
    let res = run(root, "jl", None, RunOptions::default()).unwrap();
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    let errors: Vec<&str> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunError { reason, .. } => Some(reason.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(errors, Vec::<&str>::new());
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(positions(&ev, "collect", false).len(), 3);
    assert!(positions(&ev, "handoff", false).is_empty());
}

/// The same finish reached through the other input once the fix loop is
/// spent.
#[test]
fn a_finish_with_two_inputs_ends_the_run_through_the_other_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        SHARED_FINISH,
        &[("collect.sh", counted(root, "collect", &["open"], ""))],
    );
    let res = run(root, "jl", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(positions(&ev, "collect", false).len(), 3);
    assert_eq!(positions(&ev, "handoff", false).len(), 1);
}

/// Every loop round journals its own routing decision: `collect -> fix` is
/// recorded once per round it was taken, so routing, which reads a node's
/// latest execution only, finds it even when a patch has removed the edge.
#[test]
fn every_loop_round_journals_its_own_hop() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        SHARED_FINISH,
        &[(
            "collect.sh",
            counted(root, "collect", &["open", "open", "finished"], ""),
        )],
    );
    let res = run(root, "jl", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    let decisions = ev
        .iter()
        .filter(|e| {
            matches!(
                &e.payload,
                EventPayload::EdgeTraversed { from, to, uncounted: true, .. }
                    if from == "collect" && to == "fix"
            )
        })
        .count();
    assert_eq!(decisions, 2);
}
