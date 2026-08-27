use apb_engine::error::EngineError;
use apb_engine::run_config::{RunConfig, RunMode, read_run_config, write_run_config};
use apb_engine::workdir::{acquire, acquire_queued};
use std::collections::BTreeMap;
use std::time::Duration;

#[test]
fn run_config_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let mut params = BTreeMap::new();
    params.insert("task".to_string(), "do it".to_string());
    let cfg = RunConfig {
        params,
        instruction: Some("careful".into()),
        supervisor_expected: false,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        parent_run: None,
        continued_from: None,
        superseded_by: None,
        depth: 0,
        expected_children: None,
        expected_connectors: Default::default(),
        expected_connector_accounts: Default::default(),
        cache: Default::default(),
        mode: RunMode::Supervised,
        max_parallel: Some(2),
        workdir_queue_wait_ms: Some(60_000),
    };
    write_run_config(dir.path(), &cfg).unwrap();
    let back = read_run_config(dir.path()).unwrap();
    assert_eq!(back.params.get("task").map(String::as_str), Some("do it"));
    assert_eq!(back.instruction.as_deref(), Some("careful"));
    // The run mode is persisted: a detached driver re-opens the run from disk
    // and has no other way to learn it (Task 7).
    assert_eq!(back.mode, RunMode::Supervised);
    // Same for the concurrency cap (spec 2026-08-05 section 1.3).
    assert_eq!(back.max_parallel, Some(2));
    // And for the workdir-queue ceiling: a detached driver of a queued run has
    // no other way to learn that its wait is about another run finishing, not
    // about a sub-millisecond lock handover.
    assert_eq!(back.workdir_queue_wait_ms, Some(60_000));
}

#[test]
fn second_writer_is_refused_but_shared_allowed() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let guard = acquire(root.path(), false).unwrap();
    assert!(guard.is_some());
    // second acquire without allow_shared - rejected
    match acquire(root.path(), false) {
        Err(EngineError::WorkdirBusy(_)) => {}
        other => panic!("expected WorkdirBusy, got {other:?}"),
    }
    // with allow_shared - allowed (no guard returned)
    assert!(acquire(root.path(), true).unwrap().is_none());
    // after releasing the first lock, acquire is possible again
    drop(guard);
    assert!(acquire(root.path(), false).unwrap().is_some());
}

/// The queue's whole job: a start that would have been refused waits the
/// holder out and then runs. Without this, an event that arrives while any
/// other write-run holds the lock has nowhere to go.
#[test]
fn a_queued_waiter_takes_the_lock_once_the_holder_releases_it() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let guard = acquire(root.path(), false).unwrap().unwrap();
    let held = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        drop(guard);
    });
    let waited = acquire_queued(root.path(), Duration::from_secs(30), &mut || false)
        .expect("the queue must outlast a holder that releases within the ceiling");
    assert!(waited.is_some(), "the waiter must end up holding the lock");
    held.join().unwrap();
}

/// A queue is not an unbounded promise. Running out is a real failure the
/// caller can see, and it says which wait it was rather than repeating the
/// generic "use worktree" hint on its own.
#[test]
fn a_queued_waiter_gives_up_when_the_ceiling_passes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let _guard = acquire(root.path(), false).unwrap().unwrap();
    match acquire_queued(root.path(), Duration::from_millis(300), &mut || false) {
        Err(EngineError::WorkdirBusy(msg)) => {
            assert!(
                msg.contains("workdir queue"),
                "the give-up must name the queue, got {msg}"
            );
        }
        other => panic!("expected WorkdirBusy, got {other:?}"),
    }
}

/// A stop posted against a queued run must not have to wait for an unrelated
/// run to finish before it takes effect.
#[test]
fn a_queued_waiter_abandons_the_wait_when_the_run_is_stopped() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let _guard = acquire(root.path(), false).unwrap().unwrap();
    let started = std::time::Instant::now();
    match acquire_queued(root.path(), Duration::from_secs(300), &mut || true) {
        Err(EngineError::WorkdirBusy(msg)) => {
            assert!(
                msg.contains("stopped"),
                "the abandonment must say it was a stop, got {msg}"
            );
        }
        other => panic!("expected WorkdirBusy, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a stopped waiter must not sit out the ceiling"
    );
}

/// A free workdir is not a queue at all: nothing waits, nothing polls.
#[test]
fn a_free_workdir_is_taken_without_waiting() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let guard = acquire_queued(root.path(), Duration::from_secs(300), &mut || false).unwrap();
    assert!(guard.is_some());
}
