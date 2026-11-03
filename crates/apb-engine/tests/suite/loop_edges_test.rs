//! Bounded loop edge engine tests (spec 2026-07-20-run-reliability, Task 5).
//!
//! A `max_traversals` edge lets a cycle repeat a bounded number of times: once
//! the edge has been traversed its cap times, edge selection treats it as
//! non-matching, so an alternative edge or the existing no-edge behavior
//! applies. These tests drive real runs of a review/fix loop with a stub agent
//! and assert exact execution counts, the `edge_traversed` journal, resume
//! preservation, and the cache bypass on re-execution.

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, list_runs, resume, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::common;

/// A review/fix loop: `review` is an agent_task that the stub agent drives;
/// `fix` is a plain prompt node so a single stub can make `review` fail without
/// also failing `fix`. The back edge `review -> fix` is bounded to two
/// traversals; on review success the run proceeds to `done`.
const LOOP: &str = r#"schema: 2
id: loopedge
name: Loop Edge
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: review, type: agent_task, prompt: "review" }
  - { id: fix, type: prompt, prompt: "fix" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: review }
  - { from: review, to: fix, condition: { type: node_status, node: review, equals: failure }, max_traversals: 2 }
  - { from: fix, to: review }
  - { from: review, to: done, condition: { type: node_status, node: review, equals: success } }
"#;

fn seed_loop(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/loopedge/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), LOOP).unwrap();
    fs::write(root.join(".apb/playbooks/loopedge/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

/// Writes an executable stub agent script and returns its path.
fn write_agent(root: &Path, name: &str, body: &str) -> String {
    let path = root.join(name);
    common::write_sync(&path, body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// A stub agent that always fails (non-zero exit -> node Failed).
const ALWAYS_FAIL: &str = "#!/bin/sh\necho failing 1>&2\nexit 1\n";
/// A stub agent that always succeeds.
const ALWAYS_OK: &str = "#!/bin/sh\necho ok\n";

/// A flaky stub: fails on the first invocation, succeeds afterwards (via a
/// marker file). Used to exercise review succeeding on the second pass.
fn flaky_body(marker: &Path) -> String {
    format!(
        "#!/bin/sh\nif [ -f '{m}' ]; then echo ok; exit 0; else touch '{m}'; echo firstfail 1>&2; exit 1; fi\n",
        m = marker.display()
    )
}

fn count_starts(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .count()
}

/// Counted traversals only (#82): `advance_frontier` now also journals an
/// `uncounted` routing-decision record for every hop selected, including
/// unbounded ones, so an unfiltered match here would start counting those too.
/// Only a record that is neither a policy route nor `uncounted` spent a
/// bounded edge's `max_traversals` budget, so that is what this helper counts.
fn count_traversals(events: &[Event], from_id: &str, to_id: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::EdgeTraversed { from, to, via_policy, uncounted, .. } if from == from_id && to == to_id && !via_policy && !uncounted)
        })
        .count()
}

fn count_attempt_starts(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node))
        .count()
}

fn count_finishes(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
        .count()
}

/// The id of the single run under `root` (used when `run` returns Err on a
/// deliberate dead-end and so cannot hand back a run id itself).
fn only_run_id(root: &Path) -> String {
    let runs = list_runs(root).unwrap();
    assert_eq!(runs.len(), 1, "expected exactly one run");
    runs[0].run_id.clone()
}

#[test]
fn bounded_loop_exhausts_then_dead_ends_at_review() {
    let dir = tempfile::tempdir().unwrap();
    seed_loop(dir.path());
    let prog = write_agent(dir.path(), "fail.sh", ALWAYS_FAIL);

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    // review fails every time; after two traversals of review->fix the bounded
    // edge is non-matching and review->done never matches (review failed), so
    // the run dead-ends at review (the existing no-edge behavior). Issue #42
    // finding 3: this used to surface as a bare `Err` with no explanation in
    // the log; `run()` now comes back `Ok` with the run recorded `Failed` and
    // a `RunError` event naming why.
    let res = run(dir.path(), "loopedge", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let reason = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunError { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    assert!(
        reason.is_some_and(|r| r.contains("no outgoing edge")),
        "expected a RunError naming the dead end, got {events:?}"
    );

    assert_eq!(
        count_starts(&events, "review"),
        3,
        "review: initial + 2 loops"
    );
    assert_eq!(
        count_starts(&events, "fix"),
        2,
        "fix: once per loop iteration"
    );
    assert_eq!(
        count_traversals(&events, "review", "fix"),
        2,
        "exactly two bounded-edge traversals"
    );
    // No traversal is ever journaled for the plain (unbounded) fix->review edge.
    assert_eq!(count_traversals(&events, "fix", "review"), 0);
    // The run never reached done.
    assert_eq!(count_finishes(&events, "done"), 0);
}

#[test]
fn bounded_loop_exits_when_review_succeeds_on_second_pass() {
    let dir = tempfile::tempdir().unwrap();
    seed_loop(dir.path());
    let marker = dir.path().join("flaky.marker");
    let prog = write_agent(dir.path(), "flaky.sh", &flaky_body(&marker));

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "loopedge", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(count_starts(&events, "review"), 2, "fail then succeed");
    assert_eq!(count_starts(&events, "fix"), 1, "one fix pass");
    assert_eq!(count_traversals(&events, "review", "fix"), 1);
    assert_eq!(count_finishes(&events, "done"), 1, "reached done");
}

#[test]
fn resume_preserves_bounded_edge_count() {
    let dir = tempfile::tempdir().unwrap();
    seed_loop(dir.path());
    let prog = write_agent(dir.path(), "fail.sh", ALWAYS_FAIL);

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    // First drive to the dead-end so a full run dir (manifest, config, snapshot)
    // exists, then truncate the journal to just after the FIRST fix pass to
    // simulate a crash mid-loop, and resume.
    let _ = run(dir.path(), "loopedge", None, RunOptions::default());
    let run_id = only_run_id(dir.path());

    // Truncate the journal to the first fix NodeFinished: one traversal has been
    // recorded, one fix pass completed, review is about to loop again.
    let mut seen_fix_finish = false;
    keep_through(dir.path(), &run_id, |p| {
        if let EventPayload::NodeFinished { node, .. } = p
            && node == "fix"
            && !seen_fix_finish
        {
            seen_fix_finish = true;
            return true;
        }
        false
    });

    let before = read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    assert_eq!(
        count_traversals(&before, "review", "fix"),
        1,
        "fixture holds exactly one prior traversal"
    );

    let _ = resume(dir.path(), &run_id, None);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    let after = read_all(&dir.path().join(".apb/runs").join(&run_id)).unwrap();
    // The resume folded the prior traversal from the journal, so the cap still
    // limits the total to two: it does not reset to zero (which would loop
    // forever) nor double-count. Final shape matches an uninterrupted run.
    assert_eq!(
        count_traversals(&after, "review", "fix"),
        2,
        "cap respected across the resume"
    );
    assert_eq!(count_starts(&after, "review"), 3);
    assert_eq!(count_starts(&after, "fix"), 2);
}

/// A loop whose agent node succeeds and caches; the bounded back edge lets it
/// re-execute once, and a fallback exits to `done`. With the cache enabled the
/// second execution must run the agent again (cache bypass) rather than replay
/// the first cached verdict.
const CACHE_LOOP: &str = r#"schema: 2
id: cacheloop
name: Cache Loop
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "work", cache: auto }
  - { id: tick, type: prompt, prompt: "tick" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: tick }
  - { from: tick, to: work, max_traversals: 1 }
  - { from: tick, to: done, fallback: true }
"#;

fn git(root: &Path, args: &[&str]) {
    let mut full: Vec<&str> = vec!["-C", root.to_str().unwrap(), "-c", "commit.gpgsign=false"];
    full.extend_from_slice(args);
    let ok = Command::new("git")
        .args(&full)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn loop_reexecution_bypasses_the_result_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The agent cache needs a git work tree for the workspace fingerprint; keep
    // apb's own state out of it so the fingerprint is stable between iterations.
    std::fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/cacheloop/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), CACHE_LOOP).unwrap();
    fs::write(root.join(".apb/playbooks/cacheloop/current"), "1.0.0").unwrap();
    common::seed_main(root);
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);

    let prog = write_agent(root, "ok.sh", ALWAYS_OK);
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    // Default cache mode (Auto) honors the node's `cache: auto`.
    let res = run(root, "cacheloop", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    // work runs twice; each execution spawns the agent (two attempt_started).
    // Without the loop cache bypass, the second execution would hit the entry
    // stored by the first and never spawn the agent (only one attempt_started).
    assert_eq!(count_starts(&events, "work"), 2, "work executes twice");
    assert_eq!(
        count_attempt_starts(&events, "work"),
        2,
        "the re-execution runs the agent again instead of replaying the cache"
    );
}

/// Rewrites a run's journal to keep only events up to AND including the first
/// one matching `pred`, simulating a crash cut short at that point (the run dir
/// otherwise stays intact so `resume` can drive again).
fn keep_through<F: FnMut(&EventPayload) -> bool>(root: &Path, run_id: &str, mut pred: F) {
    let dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&dir).unwrap();
    let cut = events
        .iter()
        .position(|e| pred(&e.payload))
        .expect("no matching event to truncate at");
    let mut buf = String::new();
    for e in &events[..=cut] {
        buf.push_str(&serde_json::to_string(e).unwrap());
        buf.push('\n');
    }
    fs::write(dir.join("events.jsonl"), buf).unwrap();
}

const LOOP_FORK: &str = r#"schema: 2
id: loopfork
name: Loop Fork
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: gate, type: agent_task, prompt: "gate" }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: m, type: prompt, prompt: "m" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: a, condition: { type: node_status, node: gate, equals: failure } }
  - { from: gate, to: b, condition: { type: node_status, node: gate, equals: success } }
  - { from: a, to: m, join: all }
  - { from: b, to: m, join: all }
  - { from: m, to: gate, max_traversals: 1 }
  - { from: m, to: done, fallback: true }
"#;

fn seed_loop_fork(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/loopfork/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), LOOP_FORK).unwrap();
    fs::write(root.join(".apb/playbooks/loopfork/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

/// (d) A loop fork that changes branches: `gate` forks into `a` on the first pass (the
/// flaky stub fails first) and into `b` on the second (it succeeds after),
/// both feeding the same `join: all` merge `m`, with a bounded back edge
/// `m -> gate` and a fallback exit to `done` once the cap is spent. Routing
/// reads each node's latest execution (`parallel::routed_targets`), so the
/// branch a later pass did not select is written off for that pass. This run
/// must still reach `done` rather than dead-ending with "no
/// outgoing edge and is not finish" or "parallel batch produced no runnable
/// successor and no finish".
#[test]
fn a_loop_fork_that_changes_branches_still_reaches_finish() {
    let dir = tempfile::tempdir().unwrap();
    seed_loop_fork(dir.path());
    let marker = dir.path().join("flaky.marker");
    let prog = write_agent(dir.path(), "flaky.sh", &flaky_body(&marker));

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "loopfork", None, RunOptions::default());
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    let res = res.unwrap_or_else(|e| {
        panic!("a loop fork that changed branches must still reach finish, got: {e}")
    });
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    // Both branches actually ran, on different passes, and both delivered into
    // the merge: this is the shape the test is about, not a fixture that
    // happens to take only one branch.
    assert_eq!(count_starts(&events, "a"), 1, "the first pass took `a`");
    assert_eq!(count_starts(&events, "b"), 1, "the second pass took `b`");
    assert_eq!(count_starts(&events, "m"), 2, "the merge ran once per pass");
    assert_eq!(count_finishes(&events, "done"), 1, "the run reached finish");
}

// --- Direct self-edges (issue #171) -------------------------------------------
//
// A loop edge that points straight back at its own source (`review -> review`)
// must behave like the same loop routed through a `condition` node: each pass
// is a counted `edge_traversed`, the node runs again, and once the cap is spent
// the bounded edge stops matching and the `fallback` edge is taken. Before the
// fix the frontier dropped the node itself, so the first pass failed the run
// with "has no outgoing edge and is not finish".

/// The issue's reproduction: one script node that always succeeds, a bounded
/// self-edge on its success, and a fallback exit.
const SELF_LOOP: &str = r#"schema: 2
id: selfloop
name: Self loop
version: 1.0.0
nodes:
  - { id: start,  type: start }
  - { id: review, type: script, script: scripts/review.sh, runner: sh }
  - { id: done,   type: finish, outcome: success }
edges:
  - { from: start,  to: review }
  - { from: review, to: review, condition: { type: node_status, node: review, equals: success }, max_traversals: 2 }
  - { from: review, to: done, fallback: true }
"#;

/// Seeds playbook `id` with `yaml` and one `scripts/review.sh` of `script`.
fn seed_script_playbook(root: &Path, id: &str, yaml: &str, script: &str) {
    init_project(root).unwrap();
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(vdir.join("scripts/review.sh"), script).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
}

const REVIEW_OK: &str = "#!/bin/sh\necho reviewed\nexit 0\n";

#[test]
fn a_bounded_self_edge_loops_to_its_cap_then_takes_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    seed_script_playbook(dir.path(), "selfloop", SELF_LOOP, REVIEW_OK);

    let res = run(dir.path(), "selfloop", None, RunOptions::default()).unwrap();

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "{:#?}",
        events.iter().map(|e| &e.payload).collect::<Vec<_>>()
    );
    assert_eq!(count_starts(&events, "review"), 3, "first pass + 2 loops");
    assert_eq!(
        count_traversals(&events, "review", "review"),
        2,
        "one counted traversal per loop pass"
    );
    assert_eq!(
        count_finishes(&events, "done"),
        1,
        "left through the fallback"
    );
}

/// The self-edge stops matching before its cap: the loop ends on the
/// condition, not on the bound, and the run still leaves through the fallback.
#[test]
fn a_self_edge_whose_condition_stops_matching_exits_before_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("again.marker");
    // First pass asks for another round, every later pass does not.
    let script = format!(
        "#!/bin/sh\nif [ -f '{m}' ]; then echo stop; else touch '{m}'; echo again; fi\n",
        m = marker.display()
    );
    let yaml = SELF_LOOP
        .replace(
            "condition: { type: node_status, node: review, equals: success }, max_traversals: 2",
            "condition: { type: output_match, node: review, pattern: again }, max_traversals: 5",
        )
        .replace("id: selfloop", "id: selfstop");
    seed_script_playbook(dir.path(), "selfstop", &yaml, &script);

    let res = run(dir.path(), "selfstop", None, RunOptions::default()).unwrap();

    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(
        count_starts(&events, "review"),
        2,
        "one loop, then the exit"
    );
    assert_eq!(count_traversals(&events, "review", "review"), 1);
    assert_eq!(count_finishes(&events, "done"), 1);
}

/// A resume that starts right after the self-looping node (`StartMode::After`)
/// advances through the self-edge instead of refusing with "no pending
/// successor", and the folded count keeps the cap: the resumed run ends in the
/// same shape as an uninterrupted one.
#[test]
fn a_resume_after_a_self_looping_node_continues_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    seed_script_playbook(dir.path(), "selfloop", SELF_LOOP, REVIEW_OK);
    let res = run(dir.path(), "selfloop", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // Cut the journal right after the first `review` finished: the loop's
    // first hop has not been taken yet.
    keep_through(
        dir.path(),
        &res.run_id,
        |p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "review"),
    );
    let before = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(
        count_starts(&before, "review"),
        1,
        "fixture: one pass so far"
    );
    assert_eq!(count_traversals(&before, "review", "review"), 0);

    let resumed = resume(dir.path(), &res.run_id, None).unwrap();

    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let after = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(count_starts(&after, "review"), 3);
    assert_eq!(count_traversals(&after, "review", "review"), 2);
    assert_eq!(count_finishes(&after, "done"), 1);
}

/// A self-looping node inside a concurrent batch (`max_parallel`): `start`
/// fans out into `review` and `side`, and the batch-tail advance re-admits
/// `review` for its loop passes while `side` has already delivered into the
/// merge.
const SELF_LOOP_PARALLEL: &str = r#"schema: 2
id: selfpar
name: Self loop in a batch
version: 1.0.0
nodes:
  - { id: start,  type: start }
  - { id: review, type: script, script: scripts/review.sh, runner: sh }
  - { id: side,   type: script, script: scripts/review.sh, runner: sh }
  - { id: merge,  type: script, script: scripts/review.sh, runner: sh }
  - { id: done,   type: finish, outcome: success }
edges:
  - { from: start,  to: review }
  - { from: start,  to: side }
  - { from: review, to: review, condition: { type: node_status, node: review, equals: success }, max_traversals: 2 }
  - { from: review, to: merge, fallback: true, join: all }
  - { from: side,   to: merge, join: all }
  - { from: merge,  to: done }
"#;

#[test]
fn a_self_edge_loops_inside_a_parallel_batch() {
    let dir = tempfile::tempdir().unwrap();
    seed_script_playbook(dir.path(), "selfpar", SELF_LOOP_PARALLEL, REVIEW_OK);

    let res = run(dir.path(), "selfpar", None, RunOptions::default()).unwrap();

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "{:#?}",
        events.iter().map(|e| &e.payload).collect::<Vec<_>>()
    );
    assert_eq!(count_starts(&events, "review"), 3);
    assert_eq!(count_traversals(&events, "review", "review"), 2);
    assert_eq!(count_starts(&events, "side"), 1);
    assert_eq!(count_starts(&events, "merge"), 1, "the merge ran once");
    assert_eq!(count_finishes(&events, "done"), 1);
}

// --- A bounded edge out of a review, spent (design 7.1) ------------------------

/// The review loop with its cap on the review's own edge: a second
/// `rejected` finds that edge spent and no other edge for the decision.
const REVIEW_CAP: &str = r#"schema: 2
id: reviewcap
name: Review cap
version: 1.0.0
nodes:
  - { id: start,  type: start }
  - { id: review, type: human_review, options: [approved, rejected] }
  - { id: fix,    type: script, script: scripts/review.sh, runner: sh }
  - { id: done,   type: finish, outcome: success }
edges:
  - { from: start,  to: review }
  - { from: review, to: fix, condition: { type: review_status, equals: rejected }, max_traversals: 1 }
  - { from: review, to: done, condition: { type: review_status, equals: approved } }
  - { from: fix,    to: review }
"#;

/// A review whose decision has no edge left (its only edge for it is spent)
/// fails the run as before, but through the unhandled-failure stop: the run
/// error names the review, its decision and the spent edge's address, not a
/// driver error with no node.
#[test]
fn a_spent_bounded_edge_out_of_a_review_fails_the_run_naming_it() {
    let (errors, events) = dead_end_after(REVIEW_CAP, &["rejected", "rejected"]);
    assert_eq!(
        errors,
        vec![(
            Some("review".to_string()),
            "no edge can be taken from `review`: decision `rejected`; exhausted: e:review>fix#1"
                .to_string()
        )]
    );
    assert_eq!(count_finishes(&events, "fix"), 1, "the loop ran once");
}

/// A review with two capped loops, one per decision: once both are spent,
/// the dead end names only the edge of the decision that found no edge,
/// never the other decision's spent edge.
const TWO_CAPS: &str = r#"schema: 2
id: reviewcap
name: Review cap
version: 1.0.0
nodes:
  - { id: start,  type: start }
  - { id: review, type: human_review, options: [approved, rejected, later] }
  - { id: fix,    type: script, script: scripts/review.sh, runner: sh }
  - { id: redo,   type: script, script: scripts/review.sh, runner: sh }
  - { id: done,   type: finish, outcome: success }
edges:
  - { from: start,  to: review }
  - { from: review, to: redo, condition: { type: review_status, equals: later }, max_traversals: 1 }
  - { from: review, to: fix, condition: { type: review_status, equals: rejected }, max_traversals: 1 }
  - { from: review, to: done, condition: { type: review_status, equals: approved } }
  - { from: redo,   to: review }
  - { from: fix,    to: review }
"#;

#[test]
fn the_dead_end_names_only_the_spent_edge_of_its_own_decision() {
    let (errors, _) = dead_end_after(TWO_CAPS, &["later", "rejected", "rejected"]);
    assert_eq!(
        errors,
        vec![(
            Some("review".to_string()),
            "no edge can be taken from `review`: decision `rejected`; exhausted: e:review>fix#1"
                .to_string()
        )]
    );
}

/// Runs `yaml` (playbook `reviewcap`), deciding its review with each of
/// `decisions` in turn, to its failed end; returns its run errors and its
/// journal.
fn dead_end_after(yaml: &str, decisions: &[&str]) -> (Vec<(Option<String>, String)>, Vec<Event>) {
    let dir = tempfile::tempdir().unwrap();
    seed_script_playbook(dir.path(), "reviewcap", yaml, REVIEW_OK);
    let root = dir.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "reviewcap", None, RunOptions::default()));
    });
    let runs = dir.path().join(".apb/runs");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let rd = loop {
        if let Some(e) = fs::read_dir(&runs)
            .ok()
            .and_then(|mut d| d.find_map(|e| e.ok()))
            .filter(|e| e.path().join("events.jsonl").is_file())
        {
            break e.path();
        }
        assert!(std::time::Instant::now() < deadline, "no run directory");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    for (i, decision) in decisions.iter().enumerate() {
        let visit = i + 1;
        // Gated on the journal: the review asks for its `visit`th decision.
        loop {
            let asked = read_all(&rd)
                .unwrap_or_default()
                .iter()
                .filter(|e| matches!(e.payload, EventPayload::ReviewRequested { .. }))
                .count();
            if asked >= visit {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "review {visit} never asked"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        apb_engine::review::post_review(
            &rd,
            apb_engine::review::ReviewCommand {
                node: "review".into(),
                decision: decision.to_string(),
                note: String::new(),
            },
        )
        .unwrap();
    }
    let res = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&rd).unwrap();
    let errors = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunError { node, reason } => Some((node.clone(), reason.clone())),
            _ => None,
        })
        .collect();
    (errors, events)
}

/// `check` fails once, then succeeds: its failure edge and then its success
/// edge spend their one traversal each.
const STATUS_CAPS: &str = r#"schema: 2
id: statuscap
name: Status cap
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: check, type: script, script: scripts/review.sh, runner: sh }
  - { id: again, type: script, script: scripts/review.sh, runner: sh }
  - { id: fix,   type: script, script: scripts/review.sh, runner: sh }
edges:
  - { from: start, to: check }
  - { from: check, to: again, condition: { type: node_status, node: check, equals: success }, max_traversals: 1 }
  - { from: check, to: fix, condition: { type: node_status, node: check, equals: failure }, max_traversals: 1 }
  - { from: again, to: check }
  - { from: fix,   to: check }
"#;

/// Fails `check` on its first run and succeeds from its second on (a
/// counter in the working directory); `again` and `fix` succeed.
const CHECK_THEN_FAIL: &str = r#"#!/bin/sh
case "$APB_NODE_ID" in
  check)
    n=$(cat .checks 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > .checks
    [ "$n" -eq 1 ] && { echo broken 1>&2; exit 1; }
    echo ok ;;
  *) echo ok ;;
esac
"#;

/// A node's status dead end names its spent edge for that status only: the
/// spent failure edge is not why a succeeded `check` stops. (A failed node
/// nothing routes keeps the failure policy's own reason, so a status dead
/// end is always a success.)
#[test]
fn the_dead_end_names_only_the_spent_edge_of_its_own_status() {
    let dir = tempfile::tempdir().unwrap();
    seed_script_playbook(dir.path(), "statuscap", STATUS_CAPS, CHECK_THEN_FAIL);
    let res = run(dir.path(), "statuscap", None, RunOptions::default()).unwrap();
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed, "{events:#?}");
    let errors: Vec<(Option<String>, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunError { node, reason } => Some((node.clone(), reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        errors,
        vec![(
            Some("check".to_string()),
            "no edge can be taken from `check`: status `succeeded`; exhausted: e:check>again#1"
                .to_string()
        )],
        "{events:#?}"
    );
    assert_eq!(count_starts(&events, "check"), 3, "{events:#?}");
}
