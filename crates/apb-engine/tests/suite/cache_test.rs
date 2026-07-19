//! Node cache engine tests (spec 2026-07-19-node-cache-design, Task 5).
//!
//! Each test drives a real run of a `start -> lint(script) -> done` playbook
//! in a git-initialized temp workdir and asserts on the cache events in the
//! run's event log. `.apb/` is gitignored so the run's own artifacts (cache,
//! run dirs) never perturb the git-aware workspace fingerprint between runs.

use crate::common;
use apb_core::cache::CacheStore;
use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::run_config::CacheRunMode;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use std::os::unix::fs::PermissionsExt;
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

fn stored_key(events: &[Event], node: &str) -> Option<String> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeCacheStored { node: n, key } if n == node => Some(key.clone()),
        _ => None,
    })
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

// --- agent_task caching (Task 6) -------------------------------------------
//
// These runs spawn a real (stub) agent, so they mutate process env
// (APB_AGENT_CMD/HOME/APB_CONFIG_DIR) under `common::env_lock()` and clean up
// on drop. The `t` node declares `cache: auto`; its key folds in the run's
// snapshotted profile bundle digest + agent/model + rendered prompt. The
// git-init + gitignored `.apb/` machinery is shared with the script tests so
// the workspace fingerprint is stable across runs (profile and run artifacts
// all live under the ignored `.apb/`).

const AGENT_PLAYBOOK: &str = r#"
schema: 1
id: agentwf
name: Agent Cache Playbook
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: t, type: agent_task, prompt: "do the thing", profile: main, cache: auto }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: t }
  - { from: t, to: done }
"#;

/// Restores the agent env vars on drop so a mutation never leaks between tests.
struct AgentEnvGuard;
impl Drop for AgentEnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
            std::env::remove_var("APB_TEST_FIXTURE");
        }
    }
}

/// Writes an executable `#!/bin/sh` stub and returns its path (for APB_AGENT_CMD).
fn make_stub(dir: &Path, body: &str) -> String {
    let path = dir.join("stub.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut p = std::fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn set_agent_env(stub: &str, home: &Path, cfg: &Path) {
    unsafe {
        std::env::set_var("APB_AGENT_CMD", stub);
        std::env::set_var("HOME", home);
        std::env::set_var("APB_CONFIG_DIR", cfg);
    }
}

/// Seeds the `main` profile (single stub executor) with the given SOUL body.
/// Lives under the gitignored `.apb/`, so editing SOUL changes the bundle
/// digest without perturbing the workspace fingerprint.
fn seed_agent_profile(root: &Path, soul: &str) {
    let dir = root.join(".apb/profiles/main");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        "name: main\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), soul).unwrap();
}

/// Builds a temp project with the `agentwf` playbook, the `main` profile, a
/// tracked `src/work.txt`, and an initial git commit.
fn seed_agent(root: &Path, soul: &str) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/work.txt"), "hello\n").unwrap();
    std::fs::write(root.join(".gitignore"), ".apb/\n").unwrap();

    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/agentwf/1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), AGENT_PLAYBOOK).unwrap();
    std::fs::write(root.join(".apb/playbooks/agentwf/current"), "1.0.0").unwrap();
    seed_agent_profile(root, soul);

    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);
}

fn run_agent(root: &Path, cache: CacheRunMode) -> (String, Vec<Event>) {
    let opts = RunOptions {
        cache,
        ..Default::default()
    };
    let res = run(root, "agentwf", None, opts).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "agent run should succeed"
    );
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    (res.run_id, events)
}

/// Absolute path to the checked-in `mock-tracker` connector fixture (its
/// `list_items` is `read_only`, its `create_item` is not).
fn fixture_connector() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/connectors/mock-tracker/connector.yaml")
        .to_string_lossy()
        .into_owned()
}

/// A stub-agent body that, during execution, drops the mock-tracker connector
/// snapshot into the run dir (the same location `connector_call` reads) and
/// appends a `ConnectorCall` event for `function` to the run log - mirroring
/// what a real `apb connector call` subprocess does out of band - then prints a
/// fixed output. `$APB_RUN_DIR`/`$APB_TEST_FIXTURE` are supplied via env.
fn connector_stub(function: &str) -> String {
    format!(
        "mkdir -p \"$APB_RUN_DIR/connectors\"\n\
         cp \"$APB_TEST_FIXTURE\" \"$APB_RUN_DIR/connectors/mock-tracker.yaml\"\n\
         printf '%s\\n' '{{\"seq\":9990,\"ts\":1,\"type\":\"connector_call\",\"node_id\":\"t\",\"connector\":\"mock-tracker\",\"function\":\"{function}\",\"account\":\"a\",\"url\":\"\",\"outcome\":\"ok\"}}' >> \"$APB_RUN_DIR/events.jsonl\"\n\
         echo FIXEDOUT"
    )
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

// Scenario 1: an agent node with a fixed output and no workspace writes misses
// and stores on run 1, then hits on run 2 with no attempt (the agent does not
// run again).
#[test]
fn agent_node_second_run_hits_cache() {
    let _l = common::env_lock();
    let _g = AgentEnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_agent(proj.path(), "role");
    set_agent_env(
        &make_stub(bin.path(), "echo ran >> .apb/marker.txt\necho FIXEDOUT"),
        home.path(),
        cfg.path(),
    );

    let (run1, ev1) = run_agent(proj.path(), CacheRunMode::Auto);
    assert!(has_miss(&ev1, "t"), "run 1 should miss");
    assert!(has_stored(&ev1, "t"), "run 1 should store");
    assert_eq!(node_output(&ev1, "t").as_deref(), Some("FIXEDOUT"));
    assert_eq!(run_count(proj.path()), 1, "agent ran once on run 1");

    let (_run2, ev2) = run_agent(proj.path(), CacheRunMode::Auto);
    assert_eq!(
        hit_source(&ev2, "t").as_deref(),
        Some(run1.as_str()),
        "run 2 should hit with run 1 as source"
    );
    assert!(
        !has_attempt_started(&ev2, "t"),
        "a cache hit must not start an agent attempt"
    );
    assert_eq!(
        run_count(proj.path()),
        1,
        "the agent must NOT run again on a hit"
    );
    assert_eq!(
        node_output(&ev2, "t").as_deref(),
        Some("FIXEDOUT"),
        "cached output is preserved"
    );
}

// Scenario 2: editing the profile SOUL changes the bundle digest, so the next
// run misses (the key is not the same key).
#[test]
fn agent_bundle_change_is_a_miss() {
    let _l = common::env_lock();
    let _g = AgentEnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_agent(proj.path(), "OLD-SOUL");
    set_agent_env(
        &make_stub(bin.path(), "echo ran >> .apb/marker.txt\necho FIXEDOUT"),
        home.path(),
        cfg.path(),
    );

    let (_run1, ev1) = run_agent(proj.path(), CacheRunMode::Auto);
    assert!(has_stored(&ev1, "t"), "run 1 should store");
    assert_eq!(run_count(proj.path()), 1);

    // Edit the live profile SOUL: the run re-snapshots it, so bundle_digest
    // (and thus the cache key) changes for the next run.
    std::fs::write(proj.path().join(".apb/profiles/main/SOUL.md"), "NEW-SOUL").unwrap();

    let (_run2, ev2) = run_agent(proj.path(), CacheRunMode::Auto);
    assert!(
        has_miss(&ev2, "t"),
        "a changed profile bundle must miss the cache"
    );
    assert!(hit_source(&ev2, "t").is_none(), "run 2 must not hit");
    assert_eq!(run_count(proj.path()), 2, "the agent re-runs on a miss");
}

// Scenario 3: a node whose (stubbed) execution makes only a read_only connector
// call is admitted, and the stored record records connector_calls == read_only.
#[test]
fn agent_read_only_connector_call_is_admitted() {
    let _l = common::env_lock();
    let _g = AgentEnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_agent(proj.path(), "role");
    unsafe {
        std::env::set_var("APB_TEST_FIXTURE", fixture_connector());
    }
    set_agent_env(
        &make_stub(bin.path(), &connector_stub("list_items")),
        home.path(),
        cfg.path(),
    );

    let (_run1, ev1) = run_agent(proj.path(), CacheRunMode::Auto);
    let key = stored_key(&ev1, "t").expect("a read_only connector call must be admitted (stored)");
    assert!(
        rejected_reason(&ev1, "t").is_none(),
        "a read_only call must not be rejected"
    );

    // The stored record is the load-bearing proof of the verification result.
    let store = CacheStore::open(proj.path());
    let entry = store
        .load(&key, now_unix())
        .expect("the stored record loads back");
    assert_eq!(entry.record.node_type, "agent_task");
    assert_eq!(
        entry.record.verification.connector_calls, "read_only",
        "a read_only call must be recorded as read_only"
    );
}

// Scenario 4: a node whose (stubbed) execution makes a non-read_only connector
// call is rejected with a connector reason, and nothing is stored.
#[test]
fn agent_non_read_only_connector_call_is_rejected() {
    let _l = common::env_lock();
    let _g = AgentEnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_agent(proj.path(), "role");
    unsafe {
        std::env::set_var("APB_TEST_FIXTURE", fixture_connector());
    }
    set_agent_env(
        &make_stub(bin.path(), &connector_stub("create_item")),
        home.path(),
        cfg.path(),
    );

    let (_run1, ev1) = run_agent(proj.path(), CacheRunMode::Auto);
    let reason =
        rejected_reason(&ev1, "t").expect("a non-read_only connector call must be rejected");
    assert!(
        reason.contains("connector"),
        "the reject reason should mention the connector, got: {reason}"
    );
    assert!(
        !has_stored(&ev1, "t"),
        "a rejected admission must not store"
    );
}
