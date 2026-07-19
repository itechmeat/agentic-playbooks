//! Node cache engine tests (spec 2026-07-19-node-cache-design, Task 5).
//!
//! Each test drives a real run of a `start -> lint(script) -> done` playbook
//! in a git-initialized temp workdir and asserts on the cache events in the
//! run's event log. `.apb/` is gitignored so the run's own artifacts (cache,
//! run dirs) never perturb the git-aware workspace fingerprint between runs.

use crate::common;
use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::run_config::CacheRunMode;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use std::path::Path;
use std::process::Command;

const PLAYBOOK: &str = r#"
schema: 1
id: cachewf
name: Cache Playbook
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: lint, type: script, script: "scripts/lint.sh", runner: sh, cache: auto }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: lint }
  - { from: lint, to: done }
"#;

/// A script that leaves the tracked workspace untouched: it appends a run
/// marker to a gitignored `.apb/` path (invisible to the fingerprint) and
/// prints `linted`. The marker lets a test count real executions and thus
/// prove a cache hit skipped the script.
const CLEAN_SCRIPT: &str = "echo ran >> .apb/marker.txt\necho linted\n";

/// A script that dirties the workspace by writing an UNDECLARED file. The
/// post-execution fingerprint then differs from the pre-execution one, so
/// admission must reject it.
const DIRTY_SCRIPT: &str = "echo dirty > extra.txt\necho linted\n";

/// Runs git in `root` with commit signing disabled so a developer's global
/// `commit.gpgsign = true` cannot make a fixture commit hang or fail.
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

/// Builds a temp project with the `cachewf` playbook (its script body given by
/// `script_body`), a tracked `src/work.txt`, and an initial git commit.
fn seed(root: &Path, script_body: &str) {
    // Tracked workspace content (the git-aware fingerprint tracks this file).
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/work.txt"), "hello\n").unwrap();
    // Keep apb's own state out of the fingerprint.
    std::fs::write(root.join(".gitignore"), ".apb/\n").unwrap();

    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/cachewf/1.0.0");
    std::fs::create_dir_all(vdir.join("scripts")).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    common::write_sync(&vdir.join("scripts/lint.sh"), script_body);
    std::fs::write(root.join(".apb/playbooks/cachewf/current"), "1.0.0").unwrap();

    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);
}

/// Drives one run and returns (run_id, events).
fn run_once(root: &Path, cache: CacheRunMode) -> (String, Vec<Event>) {
    let opts = RunOptions {
        cache,
        ..Default::default()
    };
    let res = run(root, "cachewf", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded, "run should succeed");
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    (res.run_id, events)
}

fn hit_source(events: &[Event], node: &str) -> Option<String> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeCacheHit {
            node: n,
            source_run,
            ..
        } if n == node => Some(source_run.clone()),
        _ => None,
    })
}

fn has_miss(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeCacheMiss { node: n, .. } if n == node))
}

fn has_stored(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeCacheStored { node: n, .. } if n == node))
}

fn rejected_reason(events: &[Event], node: &str) -> Option<String> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeCacheRejected { node: n, reason } if n == node => Some(reason.clone()),
        _ => None,
    })
}

fn has_attempt_started(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node))
}

fn node_output(events: &[Event], node: &str) -> Option<String> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node: n, output, ..
        } if n == node => Some(output.clone()),
        _ => None,
    })
}

fn any_cache_event(events: &[Event]) -> bool {
    events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::NodeCacheHit { .. }
                | EventPayload::NodeCacheMiss { .. }
                | EventPayload::NodeCacheStored { .. }
                | EventPayload::NodeCacheRejected { .. }
        )
    })
}

/// Number of times the clean script actually ran, from its `.apb/marker.txt`.
fn run_count(root: &Path) -> usize {
    std::fs::read_to_string(root.join(".apb/marker.txt"))
        .map(|s| s.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0)
}

// Scenario 1: miss+store on the first run, hit on the second (same workdir),
// with the script NOT re-executed on the hit and the same output preserved.
#[test]
fn second_run_hits_cache_and_skips_script() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, CLEAN_SCRIPT);

    let (run1, ev1) = run_once(root, CacheRunMode::Auto);
    assert!(has_miss(&ev1, "lint"), "run 1 should miss");
    assert!(has_stored(&ev1, "lint"), "run 1 should store");
    assert!(hit_source(&ev1, "lint").is_none(), "run 1 is not a hit");
    assert_eq!(node_output(&ev1, "lint").as_deref(), Some("linted"));
    assert_eq!(run_count(root), 1, "script ran once on run 1");

    let (_run2, ev2) = run_once(root, CacheRunMode::Auto);
    assert_eq!(
        hit_source(&ev2, "lint").as_deref(),
        Some(run1.as_str()),
        "run 2 should hit with run 1 as source"
    );
    assert!(!has_miss(&ev2, "lint"), "run 2 should not miss");
    assert!(!has_stored(&ev2, "lint"), "run 2 should not re-store");
    // A script node never emits AttemptStarted, so this alone is weak; the
    // marker count is the load-bearing proof the script did not run.
    assert!(!has_attempt_started(&ev2, "lint"));
    assert_eq!(run_count(root), 1, "script must NOT run again on a hit");
    assert_eq!(
        node_output(&ev2, "lint").as_deref(),
        Some("linted"),
        "cached output is preserved"
    );
}

// Scenario 2: with cache Off there are no cache events at all.
#[test]
fn cache_off_emits_no_cache_events() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, CLEAN_SCRIPT);

    let (_run1, ev1) = run_once(root, CacheRunMode::Off);
    assert!(!any_cache_event(&ev1), "Off must emit no cache events");
    // A second Off run must also stay uncached (no hit despite an identical workspace).
    let (_run2, ev2) = run_once(root, CacheRunMode::Off);
    assert!(!any_cache_event(&ev2), "Off must emit no cache events");
    assert_eq!(run_count(root), 2, "the script runs every time under Off");
}

// Scenario 3: changing a tracked workspace file makes the second run a miss.
#[test]
fn tracked_file_change_is_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, CLEAN_SCRIPT);

    let (_run1, ev1) = run_once(root, CacheRunMode::Auto);
    assert!(has_stored(&ev1, "lint"), "run 1 should store");

    // Touch a tracked file: the git-aware fingerprint changes, so the key changes.
    std::fs::write(root.join("src/work.txt"), "changed\n").unwrap();

    let (_run2, ev2) = run_once(root, CacheRunMode::Auto);
    assert!(
        has_miss(&ev2, "lint"),
        "a changed tracked file must miss the cache"
    );
    assert!(hit_source(&ev2, "lint").is_none(), "run 2 must not hit");
    assert_eq!(run_count(root), 2, "the script re-runs on a miss");
}

// Scenario 4: a script that writes an undeclared workspace file is rejected.
#[test]
fn undeclared_workspace_write_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, DIRTY_SCRIPT);

    let (_run1, ev1) = run_once(root, CacheRunMode::Auto);
    assert!(has_miss(&ev1, "lint"), "first run misses");
    let reason = rejected_reason(&ev1, "lint").expect("admission should be rejected");
    assert!(
        reason.contains("workspace"),
        "reject reason should mention the workspace change, got: {reason}"
    );
    assert!(
        !has_stored(&ev1, "lint"),
        "a rejected admission must not store"
    );
}

// Refresh mode: the lookup is skipped (no hit) even when a valid entry exists,
// and admission overwrites, so the script re-executes and re-stores.
#[test]
fn refresh_mode_skips_hit_and_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, CLEAN_SCRIPT);

    let (_run1, ev1) = run_once(root, CacheRunMode::Auto);
    assert!(has_stored(&ev1, "lint"), "run 1 stores a valid entry");
    assert_eq!(run_count(root), 1);

    let (_run2, ev2) = run_once(root, CacheRunMode::Refresh);
    assert!(
        hit_source(&ev2, "lint").is_none(),
        "Refresh must not take the hit"
    );
    assert!(
        has_stored(&ev2, "lint"),
        "Refresh still admits (overwrites)"
    );
    assert_eq!(run_count(root), 2, "Refresh re-executes the script");

    // After the refresh, a plain Auto run hits again (proving the store is populated).
    let (_run3, ev3) = run_once(root, CacheRunMode::Auto);
    assert!(
        hit_source(&ev3, "lint").is_some(),
        "Auto hits after refresh"
    );
    assert_eq!(run_count(root), 2, "the hit does not re-run the script");
}
