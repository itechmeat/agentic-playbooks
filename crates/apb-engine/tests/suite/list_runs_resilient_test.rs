use apb_core::registry::init_project;
use apb_engine::scheduler::list_runs;
use std::fs;

/// Valid events in the engine format (see crates/apb-engine/src/event.rs):
/// `Event { seq, ts, #[serde(flatten)] payload }`, `EventPayload` tagged with
/// `type` in snake_case. Here - a minimal successful run.
const GOOD_EVENTS: &str = r#"{"seq":0,"ts":1,"type":"run_started","playbook":"good","version":"1.0.0"}
{"seq":1,"ts":2,"type":"node_started","node":"start","attempt":1}
{"seq":2,"ts":3,"type":"node_finished","node":"start","status":"succeeded","attempt":1,"output":""}
{"seq":3,"ts":4,"type":"run_finished","outcome":"succeeded"}
"#;

/// Legacy line: `ts` is serialized as a JSON string, not a number - this exact
/// thing broke `list_runs` entirely before the fix (serde panics on an invalid number).
const LEGACY_LINE: &str = r#"{"ts":"1783580252038","kind":"run_started","node":null}
"#;

/// The same minimal journal as `GOOD_EVENTS`, without the terminal
/// `run_finished` line, so the pure fold reads `running`.
const RUNNING_EVENTS: &str = r#"{"seq":0,"ts":1,"type":"run_started","playbook":"dead","version":"1.0.0"}
{"seq":1,"ts":2,"type":"node_started","node":"start","attempt":1}
"#;

/// A pid that existed and is provably gone. Bounded by construction: `exit 0`
/// cannot fail to exit.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn a throwaway child to borrow a pid from");
    let pid = child.id();
    child.wait().expect("reap the throwaway child");
    pid
}

/// #85 finding 4: a run whose driver was killed reads `running` from the pure
/// fold, which is exactly right, and exactly useless in a list. The listing now
/// carries the driver verdict alongside the status.
#[test]
fn list_runs_marks_a_run_whose_driver_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    let dead = dir.path().join(".apb/runs/dead-1");
    fs::create_dir_all(&dead).unwrap();
    fs::write(dead.join("events.jsonl"), RUNNING_EVENTS).unwrap();
    fs::write(dead.join("driver.pid"), format!("{}\n", dead_pid())).unwrap();

    let healthy = dir.path().join(".apb/runs/good-1");
    fs::create_dir_all(&healthy).unwrap();
    fs::write(healthy.join("events.jsonl"), GOOD_EVENTS).unwrap();

    let runs = list_runs(dir.path()).unwrap();
    let d = runs
        .iter()
        .find(|r| r.run_id == "dead-1")
        .expect("dead-1 listed");
    assert!(
        d.driver_dead,
        "a run with a provably dead driver.pid must be marked"
    );
    let g = runs
        .iter()
        .find(|r| r.run_id == "good-1")
        .expect("good-1 listed");
    assert!(
        !g.driver_dead,
        "a run with no drive claim at all is not marked"
    );
}

#[test]
fn list_runs_skips_unreadable_run_dir_but_keeps_good_ones() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    let good_dir = dir.path().join(".apb/runs/good-1");
    fs::create_dir_all(&good_dir).unwrap();
    fs::write(good_dir.join("events.jsonl"), GOOD_EVENTS).unwrap();

    let legacy_dir = dir.path().join(".apb/runs/legacy-1");
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::write(legacy_dir.join("events.jsonl"), LEGACY_LINE).unwrap();

    let runs = list_runs(dir.path()).expect("list_runs must not fail because of one bad run dir");

    assert!(
        runs.iter()
            .any(|r| r.run_id == "good-1" && r.playbook == "good" && r.status == "succeeded"),
        "expected good-1 run to be listed, got: {runs:?}"
    );
    assert!(
        !runs.iter().any(|r| r.run_id == "legacy-1"),
        "legacy-1 must be skipped, not listed, got: {runs:?}"
    );
    assert_eq!(
        runs.len(),
        1,
        "only the good run should survive, got: {runs:?}"
    );
}

/// A run whose pure fold is `Interrupted` (open attempt, no finish) but whose
/// attempt pid is still live must list as `running`, not `interrupted`.
/// Same driver-liveness overlay the doctor and `run_status` use.
#[test]
fn list_runs_reports_running_for_live_open_attempt() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    let live_pid = std::process::id();
    let events = format!(
        r#"{{"seq":0,"ts":1000,"type":"run_started","playbook":"live-pb","version":"1.0.0"}}
{{"seq":1,"ts":2000,"type":"node_started","node":"a","attempt":1}}
{{"seq":2,"ts":3000,"type":"attempt_started","node":"a","attempt":1,"agent":"stub","pid":{live_pid}}}
"#
    );

    let run_dir = dir.path().join(".apb/runs/live-1");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("events.jsonl"), events).unwrap();

    let runs = list_runs(dir.path()).expect("list_runs must succeed");
    let live = runs
        .iter()
        .find(|r| r.run_id == "live-1")
        .expect("live-1 must be listed");
    assert_eq!(
        live.status, "running",
        "open attempt with live pid must list as running, got: {live:?}"
    );
    assert_eq!(live.playbook, "live-pb");
}
