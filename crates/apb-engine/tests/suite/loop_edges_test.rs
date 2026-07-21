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

fn count_traversals(events: &[Event], from_id: &str, to_id: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::EdgeTraversed { from, to } if from == from_id && to == to_id)
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
