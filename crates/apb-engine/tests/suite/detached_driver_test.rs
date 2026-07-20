//! Task 7: detached run drivers.
//!
//! The engine-side half of the feature, all of it exercised in-process (the
//! actual `apb __drive-run` child process is covered end-to-end from the CLI
//! crate, which is the only one that can reach the real binary through
//! `CARGO_BIN_EXE_apb`):
//!
//!   * `runs/<id>/driver.pid` names the OS process driving the run for exactly
//!     as long as the drive lasts, and is gone once it ends.
//!   * `drive_run_from_dir` re-opens a prepared-but-undriven run purely from
//!     `runs/<id>` and drives it to a terminal state, which is what the
//!     detached child does after the parent prepared the run.
//!   * the workdir lock is handed from the preparing process to the driver
//!     process instead of being dropped and re-taken through a window.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::driver::read_driver_pid;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{
    RunOptions, drive_run_from_dir, prepare_supervised_background, run_background,
};
use apb_engine::state::{RunState, RunStatus};
use apb_engine::workdir::{acquire, acquire_handover};

const POLL_DEADLINE: Duration = Duration::from_secs(20);
const POLL_STEP: Duration = Duration::from_millis(20);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if start.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for: {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

// A script node makes the run take the workdir lock (`takes_workdir_lock`) and
// gives every assertion below a window in which the run is provably still in
// flight.
const SLOWSCRIPT: &str = r#"
schema: 1
id: slowscript
name: Slow Script
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/slowscript/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), SLOWSCRIPT).unwrap();
    fs::write(vdir.join("scripts/slow.sh"), "#!/bin/sh\nsleep 1\n").unwrap();
    fs::write(root.join(".apb/playbooks/slowscript/current"), "1.0.0").unwrap();
}

fn wait_for_finish(run_dir: &Path) {
    poll_until("a RunFinished event in events.jsonl", || {
        let events = read_all(run_dir).ok()?;
        events
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .map(|_| ())
    });
}

// Scenario 1: every drive invocation publishes the pid of the process driving
// the run, and takes it back down on a clean exit. Tasks 8 and 9 read this
// file to tell "a driver is alive" from "the driver died mid-run", so both
// halves matter: present with a LIVE pid while the run is in flight, absent
// once it is over.
#[test]
fn driver_pid_names_the_live_driver_and_is_removed_after_completion() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let run_id = run_background(dir.path(), "slowscript", None, RunOptions::default()).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    let pid = poll_until("driver.pid to appear while the run is in flight", || {
        read_driver_pid(&run_dir)
    });
    // The in-process background drive runs on a thread of this very process,
    // so the pid it publishes is ours - and it is trivially alive.
    assert_eq!(
        pid,
        std::process::id(),
        "an in-process drive must publish this process's pid as the driver"
    );

    wait_for_finish(&run_dir);

    // RunFinished is journalled inside drive, a hair before the guard that
    // owns driver.pid is dropped, so poll rather than assert once.
    poll_until("driver.pid to be removed after a clean completion", || {
        if read_driver_pid(&run_dir).is_none() {
            Some(())
        } else {
            None
        }
    });
    assert!(
        !run_dir.join("driver.pid").is_file(),
        "driver.pid must not survive a completed run"
    );
}

// Scenario 2: the heart of the detached driver. The preparing process only
// prepares - a DIFFERENT caller re-opens the run from `runs/<id>` (manifest,
// journal, config and playbook snapshot are all already on disk) and drives it
// to a terminal state. In production those are two OS processes; here the
// second caller is simply the test itself, which proves the re-open needs
// nothing but the run directory.
#[test]
fn drive_run_from_dir_completes_a_prepared_but_undriven_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let prepared =
        prepare_supervised_background(dir.path(), "slowscript", None, RunOptions::default())
            .unwrap();
    let run_id = prepared.run_id().to_string();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // Nothing has driven the run yet: only the start-up events are journalled.
    let state = RunState::fold(&read_all(&run_dir).unwrap());
    assert!(
        state.nodes.is_empty(),
        "prepare must not execute any node, got {:?}",
        state.nodes
    );

    // The preparing side lets go of the run (and its workdir lock), exactly as
    // it does when the work is handed to another process.
    drop(prepared);

    let res = drive_run_from_dir(dir.path(), &run_id).unwrap();
    assert_eq!(res.run_id, run_id);
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "a re-opened prepared run must drive through to succeeded"
    );

    let state = RunState::fold(&read_all(&run_dir).unwrap());
    assert_eq!(state.run_status, RunStatus::Succeeded);
    assert!(
        state.nodes.contains_key("work"),
        "the script node must actually have executed, got {:?}",
        state.nodes
    );
}

// Scenario 3: `drive_run_from_dir` is a one-shot for a run that has not been
// driven yet. Driving an already-driven run again would replay nodes against a
// journal that has moved on, so a second call is refused rather than silently
// re-executing (`resume` is the supported way back into a finished run).
#[test]
fn drive_run_from_dir_refuses_an_already_driven_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let prepared =
        prepare_supervised_background(dir.path(), "slowscript", None, RunOptions::default())
            .unwrap();
    let run_id = prepared.run_id().to_string();
    drop(prepared);

    drive_run_from_dir(dir.path(), &run_id).unwrap();
    let err = drive_run_from_dir(dir.path(), &run_id).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("already"),
        "expected a refusal mentioning that the run was already driven, got: {msg}"
    );
}

// Scenario 4: the workdir lock is HANDED OVER, never released into a window.
// The preparing process holds it (so a second write-run is refused
// synchronously, before any run dir exists), then rewrites it with the driver
// process's pid and stops owning it. The driver adopts the lock it already
// owns on paper instead of deadlocking against its own pid.
#[test]
fn workdir_lock_is_handed_from_the_preparing_process_to_the_driver() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let lock_path = dir.path().join(".apb/workdir.lock");
    let prepared =
        prepare_supervised_background(dir.path(), "slowscript", None, RunOptions::default())
            .unwrap();
    assert!(
        lock_path.is_file(),
        "preparation of a write-run must hold the workdir lock"
    );

    // Stand in for the freshly spawned driver process.
    let driver_pid = std::process::id();
    prepared.hand_over_workdir_lock(driver_pid).unwrap();

    assert_eq!(
        fs::read_to_string(&lock_path).unwrap().trim(),
        driver_pid.to_string(),
        "the lock file must name the driver process after the handover"
    );

    // A plain acquire sees a live foreign holder and refuses - that is the
    // whole point of the handover, the lock never lapses.
    assert!(
        acquire(dir.path(), false).is_err(),
        "the handed-over lock must still look busy to an unrelated acquire"
    );

    // The driver itself adopts it.
    let guard = acquire_handover(dir.path()).unwrap();
    assert!(
        guard.is_some(),
        "the driver process must adopt the lock handed to its own pid"
    );
    drop(guard);
    assert!(
        !lock_path.is_file(),
        "the adopted lock must be released when the driver's guard is dropped"
    );
}
