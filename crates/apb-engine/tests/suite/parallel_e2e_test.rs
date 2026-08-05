use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;

// Diamond: start forks into a and b, they converge in join:all -> j -> finish.
const DIAMOND_ALL: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

// join:any - the first branch continues the flow, the second is cancelled.
const DIAMOND_ANY: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: any }
  - { from: b, to: j, join: any }
  - { from: j, to: done }
"#;

// A barrier-less diamond with branches of different length: `a` reaches the
// merge in one hop, `b` needs two. Without an implicit join `j` would run as
// soon as `a` arrived, while `b2` was still pending.
const IMPLICIT_DIAMOND: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: b2, type: prompt, prompt: "b2" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j }
  - { from: b, to: b2 }
  - { from: b2, to: j }
  - { from: j, to: done }
"#;

// An either-or conditional fork whose branches merge again: exactly one of
// `a`/`b` is ever selected, so the merge must not wait for the branch that was
// not taken.
const EITHER_OR: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: pick, type: condition }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: m, type: prompt, prompt: "m" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: pick }
  - { from: pick, to: a, condition: { type: node_status, node: pick, equals: success } }
  - { from: pick, to: b, condition: { type: node_status, node: pick, equals: failure } }
  - { from: a, to: m }
  - { from: b, to: m }
  - { from: m, to: done }
"#;

// A linear chain where every step routes its own failure into ONE shared
// failure finish node. The most common failure topology in real playbooks (this
// repo's own `apb-task-*` playbooks all have it): `aborted` has an incoming
// edge per upstream step, so it is a barrier-less fan-in whose inputs are, by
// construction, mutually exclusive.
const SHARED_FAILURE_SINK: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w1, type: script, script: "scripts/fail.sh", runner: sh }
  - { id: w2, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: start, to: w1 }
  - { from: w1, to: w2, condition: { type: node_status, node: w1, equals: success } }
  - { from: w1, to: aborted, condition: { type: node_status, node: w1, equals: failure } }
  - { from: w2, to: done, condition: { type: node_status, node: w2, equals: success } }
  - { from: w2, to: aborted, condition: { type: node_status, node: w2, equals: failure } }
"#;

// A fork whose two branches each declare a success route and a failure route
// into the SAME pair of nodes: a shared error handler and a shared success
// finish. `a` fails, `b` succeeds, so the handler must run (a failure edge
// exists precisely to deliver a failure into a node that handles it).
const SHARED_HANDLER: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/fail.sh", runner: sh }
  - { id: b, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: handler, type: prompt, prompt: "recover" }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: done, condition: { type: node_status, node: a, equals: success } }
  - { from: a, to: handler, condition: { type: node_status, node: a, equals: failure } }
  - { from: b, to: done, condition: { type: node_status, node: b, equals: success } }
  - { from: b, to: handler, condition: { type: node_status, node: b, equals: failure } }
  - { from: handler, to: aborted }
"#;

// The `join: all` diamond plus a SIDE BRANCH out of `a`, so advancing past `a`
// always has something runnable (`k`) even while the join waits for `b`. A
// resume therefore passes the pointless-resume guard and has to carry the
// outstanding branch heads into the live frontier, not just into one liveness
// query.
const DIAMOND_WITH_SIDE_BRANCH: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: k, type: prompt, prompt: "k" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: a, to: k }
  - { from: k, to: done }
  - { from: j, to: done }
"#;

// A fork out of `x` where one leg is a BOUNDED edge. Once `x -> y` has been
// traversed its cap is reached, so `y` stops showing up in `x`'s routing even
// though the journal records that the edge was taken. A resume must read the
// journaled traversal, otherwise `y` disappears from both the rebuilt heads and
// from liveness and the merge fires without it.
const CAPPED_FORK: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: x, type: prompt, prompt: "x" }
  - { id: y, type: prompt, prompt: "y" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: x }
  - { from: x, to: y, max_traversals: 1 }
  - { from: x, to: j }
  - { from: y, to: j }
  - { from: j, to: done }
"#;

// Two independent merges: `m` is a barrier-less (implicit) join of p1/p2, `w` is
// an explicit `join: any` of fa/fb. The edge order makes `m` reach the frontier
// BEFORE the `any` race is won, so the win has to leave the pending join alone.
const ANY_WIN_WITH_PENDING_JOIN: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "p1" }
  - { id: p2, type: prompt, prompt: "p2" }
  - { id: m, type: prompt, prompt: "m" }
  - { id: fa, type: prompt, prompt: "fa" }
  - { id: fb, type: prompt, prompt: "fb" }
  - { id: w, type: prompt, prompt: "w" }
  - { id: mdone, type: finish, outcome: success }
  - { id: wdone, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: start, to: p2 }
  - { from: start, to: fa }
  - { from: start, to: fb }
  - { from: p1, to: m }
  - { from: p2, to: m }
  - { from: fa, to: w, join: any }
  - { from: fb, to: w, join: any }
  - { from: m, to: mdone }
  - { from: w, to: wdone }
"#;

// A fork where one branch reaches finish - the run completes, the other does not execute.
const FORK_FINISH: &str = r#"
schema: 1
id: par
name: Par
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: finish, outcome: success }
  - { id: b, type: prompt, prompt: "b" }
  - { id: bdone, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: b, to: bdone }
"#;

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/par/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/par/current"), "1.0.0").unwrap();
}

/// Seeds a playbook that has `script` nodes, plus the two scripts the failure
/// fixtures reference. A script is the cheapest node that can actually FAIL
/// (a `prompt` always succeeds and an `agent_task` would need a stub agent and
/// the process-global `APB_AGENT_CMD`).
fn seed_with_scripts(root: &Path, yaml: &str) {
    seed(root, yaml);
    let scripts = root.join(".apb/playbooks/par/1.0.0/scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("fail.sh"), "echo boom 1>&2\nexit 1\n").unwrap();
    fs::write(scripts.join("ok.sh"), "echo fine\n").unwrap();
}

fn started(events: &[apb_engine::event::Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .count()
}

/// Position of a node's `node_started` in the journal, for ordering assertions.
fn position_of_start(events: &[apb_engine::event::Event], node: &str) -> usize {
    events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .unwrap_or_else(|| panic!("node `{node}` never started"))
}

/// Synthesizes a driver that died at a chosen point: keeps only the journal
/// lines before the first event `cut_at` accepts, and returns how many were
/// kept. One event is one line (`EventLog::append` uses a single `writeln!`), so
/// the parsed index and the line index agree.
fn truncate_journal_before<F>(run_dir: &Path, cut_at: F) -> usize
where
    F: Fn(&EventPayload) -> bool,
{
    let events = read_all(run_dir).unwrap();
    let cut = events
        .iter()
        .position(|e| cut_at(&e.payload))
        .expect("no journal event matched the cut point");
    let journal = run_dir.join("events.jsonl");
    let kept: Vec<String> = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .take(cut)
        .map(str::to_string)
        .collect();
    fs::write(&journal, format!("{}\n", kept.join("\n"))).unwrap();
    cut
}

/// [`finish_status`] read straight off a run dir.
fn finish_status_of(run_dir: &Path, node: &str) -> String {
    finish_status(&read_all(run_dir).unwrap(), node)
}

/// The status on a node's `node_finished`, for "did it really execute" checks.
fn finish_status(events: &[apb_engine::event::Event], node: &str) -> String {
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n, status, ..
            } if n == node => Some(status.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("node `{node}` never finished"))
}

/// Position of a node's `node_finished` in the journal.
fn position_of_finish(events: &[apb_engine::event::Event], node: &str) -> usize {
    events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
        .unwrap_or_else(|| panic!("node `{node}` never finished"))
}

#[test]
fn join_all_runs_both_branches_then_join() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), DIAMOND_ALL);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "a"), 1, "branch a runs once");
    assert_eq!(started(&events, "b"), 1, "branch b runs once");
    assert_eq!(
        started(&events, "j"),
        1,
        "join runs once, after both branches"
    );
    // j starts after both branches have finished.
    let j_start = position_of_start(&events, "j");
    assert!(
        j_start > position_of_finish(&events, "a") && j_start > position_of_finish(&events, "b"),
        "join must start after both branches finish"
    );
}

#[test]
fn implicit_join_waits_for_the_slower_branch() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), IMPLICIT_DIAMOND);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "j"), 1, "the merge runs exactly once");
    let j_start = position_of_start(&events, "j");
    for branch in ["a", "b2"] {
        assert!(
            j_start > position_of_finish(&events, branch),
            "a barrier-less merge must wait for branch `{branch}`"
        );
    }
}

#[test]
fn either_or_merge_reaches_finish_without_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), EITHER_OR);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "a"), 1, "the selected branch runs");
    assert_eq!(started(&events, "b"), 0, "the other branch is never taken");
    assert_eq!(
        started(&events, "m"),
        1,
        "the merge runs instead of waiting for a branch that can never arrive"
    );
}

/// A shared failure finish node is a barrier-less fan-in whose inputs are
/// mutually exclusive: the step that failed routed into it, and every other step
/// is on the success path that was abandoned. The barrier must let it through.
#[test]
fn shared_failure_sink_receives_the_failure_instead_of_deadlocking() {
    let dir = tempfile::tempdir().unwrap();
    seed_with_scripts(dir.path(), SHARED_FAILURE_SINK);
    let res = run(dir.path(), "par", None, RunOptions::default())
        .expect("a failure route into a shared sink must not dead-end the drive loop");
    assert_eq!(res.outcome, RunStatus::Failed, "the run ends at `aborted`");
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    // A finish node journals no `node_started`, so reaching it is proved by its
    // `node_finished` plus the absence of a dead-end `run_error`.
    assert_eq!(
        finish_status(&events, "aborted"),
        "succeeded",
        "the failure sink executes"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RunError { .. })),
        "the drive loop must not dead-end on the barrier"
    );
    assert_eq!(
        started(&events, "w2"),
        0,
        "the abandoned success path never runs"
    );
}

/// A shared error handler reached over a declared failure edge must EXECUTE.
/// Failing it from the barrier skipped the whole recovery path while the run
/// still reported success.
#[test]
fn shared_error_handler_executes_instead_of_being_failed_by_the_barrier() {
    let dir = tempfile::tempdir().unwrap();
    seed_with_scripts(dir.path(), SHARED_HANDLER);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "handler"), 1, "the handler executes once");
    assert_eq!(
        finish_status(&events, "handler"),
        "succeeded",
        "the handler must not be failed by the barrier it is behind"
    );
    assert!(
        !events.iter().any(|e| matches!(&e.payload,
            EventPayload::NodeFinished { node, output, .. }
                if node == "handler" && output.contains("upstream branch failed"))),
        "an implicit barrier must never fail the node it guards"
    );
}

/// A resume rebuilds the frontier from the journal. A sibling branch that never
/// started must still count as active, otherwise the join fires without it and
/// the run silently completes with a branch missing.
#[test]
fn resume_after_a_finished_branch_still_waits_for_the_unstarted_sibling() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), DIAMOND_ALL);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);

    // Synthesize a driver that died right after branch `a` finished, so `b` is
    // Pending (never started, so not `interrupted` either) and `a` is the last
    // finished node.
    let cut = truncate_journal_before(
        &run_dir,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "b"),
    );
    assert!(
        read_all(&run_dir).unwrap().len() == cut && finish_status_of(&run_dir, "a") == "succeeded",
        "branch a must have finished before the synthesized death"
    );

    let err = resume(dir.path(), &res.run_id, None)
        .expect_err("resuming past `a` must not fire the join without branch `b`");
    assert!(
        err.to_string().contains("no pending successor"),
        "expected the pointless-resume refusal, got: {err}"
    );
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "j"), 0, "the join must not have executed");
}

/// A resume whose start node has another runnable successor passes the
/// pointless-resume guard, so the guard is no safety net: the outstanding branch
/// heads have to be installed into the live frontier, or the next advance sees
/// only the side branch, flips the unstarted sibling to dead, and fires the
/// joins without it.
#[test]
fn resume_installs_the_lost_branch_heads_into_the_frontier() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), DIAMOND_WITH_SIDE_BRANCH);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    truncate_journal_before(
        &run_dir,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "b"),
    );

    let res = resume(dir.path(), &res.run_id, None)
        .expect("the side branch `k` is runnable, so the resume must proceed");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    for node in ["k", "b", "j"] {
        assert_eq!(
            started(&events, node),
            1,
            "`{node}` must run on the resumed drive"
        );
    }
}

/// The same reconstruction is needed on the `Rerun` path: the interrupted node is
/// re-executed, and the advance past it must still see the sibling branch that
/// never started.
#[test]
fn resume_of_an_interrupted_node_still_waits_for_the_unstarted_sibling() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), DIAMOND_ALL);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    // Died mid-`a`: its `node_started` stays, its `node_finished` is gone, so `a`
    // folds to Running and `plan_resume` restarts it (`StartMode::Rerun`).
    truncate_journal_before(
        &run_dir,
        |p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "a"),
    );

    let res = resume(dir.path(), &res.run_id, None).expect("an interrupted node is re-runnable");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "a"), 2, "`a` runs again after the death");
    assert_eq!(
        started(&events, "b"),
        1,
        "the sibling branch must still run"
    );
    assert_eq!(started(&events, "j"), 1, "the join runs once, after both");
}

/// A branch reached over a bounded edge that has hit its cap is still a branch:
/// the journaled traversal is the only remaining evidence that it was routed to,
/// and dropping that evidence turns a safe refusal into a silent branch skip.
#[test]
fn resume_reads_journaled_traversals_when_rebuilding_branch_heads() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), CAPPED_FORK);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    // Died after the bounded edge was traversed but before `y` started.
    truncate_journal_before(
        &run_dir,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "y"),
    );
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::EdgeTraversed { from, to, .. } if from == "x" && to == "y")),
        "the traversal that proves `y` was routed to must survive the cut"
    );

    let err = resume(dir.path(), &res.run_id, None)
        .expect_err("the merge must not fire without the branch behind the capped edge");
    assert!(
        err.to_string().contains("no pending successor"),
        "expected the pointless-resume refusal, got: {err}"
    );
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "y"), 0, "`y` never started");
    assert_eq!(started(&events, "j"), 0, "the merge must not have executed");
}

/// A `join: any` win cancels the losing branches, but a pending join elsewhere in
/// the graph is not part of that race and must survive it.
#[test]
fn an_any_win_does_not_drop_a_pending_join_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), ANY_WIN_WITH_PENDING_JOIN);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(started(&events, "w"), 1, "the any-join wins and runs");
    assert_eq!(
        started(&events, "m"),
        1,
        "a join waiting in the frontier must not be dropped by an any-win"
    );
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::NodeFinished { node, status, .. }
                if node == "fb" && status == "cancelled")),
        "the losing branch of the race is still cancelled"
    );
}

#[test]
fn join_any_cancels_sibling() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), DIAMOND_ANY);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    // One branch ran, the join passed; the other branch is cancelled,
    // rather than executed as a normal node.
    assert_eq!(started(&events, "j"), 1, "join runs once");
    let cancelled = events.iter().any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, status, .. } if (node == "a" || node == "b") && status == "cancelled"));
    assert!(cancelled, "sibling branch must be cancelled on join:any");
}

#[test]
fn finish_in_one_branch_cancels_the_other() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), FORK_FINISH);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    // Branch a (finish) is processed first (the frontier order is deterministic) and
    // completes the run; branch b does not execute.
    assert_eq!(
        started(&events, "b"),
        0,
        "sibling branch must not run once a branch finishes"
    );
    assert!(events.iter().any(
        |e| matches!(&e.payload, EventPayload::RunFinished { outcome } if outcome == "succeeded")
    ));
}

/// A join that proceeds because one of its inputs can never arrive must SAY so
/// (Task 4, Task 1 handover note 1). Before this, the branch was written off in
/// a pure function with no journal handle, so a run report showed the skipped
/// node pending forever while the merge reported success - and the implicit
/// barrier widened how many nodes that can happen to.
#[test]
fn a_dead_join_input_is_journaled_as_a_write_off() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), EITHER_OR);
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();

    let write_offs: Vec<(Option<String>, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SupervisorAction {
                action,
                node,
                detail,
            } if action == "join_input_dead" => Some((node.clone(), detail.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        write_offs.len(),
        1,
        "the merge writes off exactly one input, got: {write_offs:?}"
    );
    let (node, detail) = &write_offs[0];
    assert_eq!(
        node.as_deref(),
        Some("m"),
        "the event names the join that proceeded"
    );
    assert!(
        detail.contains("`b`"),
        "the event names the dead source, got: {detail}"
    );
}
