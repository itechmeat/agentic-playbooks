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

/// A run whose driver is in the middle of appending a line is a live run, not
/// a corrupt one: the listing used to drop it for that instant, so it blinked
/// out of `apb runs`, MCP `runs_list` and the dashboard list.
#[test]
fn list_runs_keeps_a_run_whose_last_line_is_still_being_written() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let run_dir = dir.path().join(".apb/runs/live-1");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        run_dir.join("events.jsonl"),
        format!("{GOOD_EVENTS}{{\"seq\":4,\"ts\":5,\"type\":\"node_sta"),
    )
    .unwrap();

    let runs = list_runs(dir.path()).unwrap();

    let run = runs
        .iter()
        .find(|r| r.run_id == "live-1")
        .expect("a run with a torn last line must stay listed");
    assert_eq!(run.status, "succeeded");
}

/// The run list shows how long a finished run took: the listing carries its
/// active duration from `run_started` to `run_finished`, and none for a run
/// that has not ended.
#[test]
fn list_runs_carries_the_duration_of_a_finished_run_only() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();

    let done = dir.path().join(".apb/runs/good-1");
    fs::create_dir_all(&done).unwrap();
    fs::write(done.join("events.jsonl"), GOOD_EVENTS).unwrap();

    let open = dir.path().join(".apb/runs/open-1");
    fs::create_dir_all(&open).unwrap();
    fs::write(open.join("events.jsonl"), RUNNING_EVENTS).unwrap();

    let runs = list_runs(dir.path()).unwrap();
    let by_id = |id: &str| runs.iter().find(|r| r.run_id == id).expect("listed");
    assert_eq!(by_id("good-1").duration_ms, Some(3));
    assert_eq!(by_id("open-1").duration_ms, None);
}

/// Writes one run journal under `.apb/runs/<id>`.
fn write_run(root: &std::path::Path, id: &str, events: &str) {
    let dir = root.join(format!(".apb/runs/{id}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), events).unwrap();
}

/// The duration is active time: a pause inside the run is left out, a run
/// that ends with `run_aborted` has one too, and a finished run resumed again
/// is live, so it has none until it ends once more (as `apb stats` counts).
#[test]
fn list_runs_duration_leaves_out_pauses_and_follows_a_resume() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_run(
        dir.path(),
        "paused-1",
        r#"{"seq":0,"ts":100,"type":"run_started","playbook":"p","version":"1.0.0"}
{"seq":1,"ts":110,"type":"run_paused","reason":"operator"}
{"seq":2,"ts":150,"type":"run_resumed","from_node":"start"}
{"seq":3,"ts":170,"type":"run_finished","outcome":"succeeded"}
"#,
    );
    write_run(
        dir.path(),
        "aborted-1",
        r#"{"seq":0,"ts":100,"type":"run_started","playbook":"p","version":"1.0.0"}
{"seq":1,"ts":125,"type":"run_aborted","reason":"stop"}
"#,
    );
    let resumed = r#"{"seq":0,"ts":100,"type":"run_started","playbook":"p","version":"1.0.0"}
{"seq":1,"ts":120,"type":"run_finished","outcome":"failed"}
{"seq":2,"ts":200,"type":"run_resumed","from_node":"start"}
"#;
    write_run(dir.path(), "resumed-1", resumed);
    write_run(
        dir.path(),
        "refinished-1",
        &format!(
            "{resumed}{{\"seq\":3,\"ts\":230,\"type\":\"run_finished\",\"outcome\":\"succeeded\"}}\n"
        ),
    );

    let runs = list_runs(dir.path()).unwrap();
    let by_id = |id: &str| runs.iter().find(|r| r.run_id == id).expect("listed");
    assert_eq!(by_id("paused-1").duration_ms, Some(30));
    // Once ended, the duration already leaves the pause out.
    assert_eq!(by_id("paused-1").paused_ms, None);
    assert_eq!(by_id("aborted-1").duration_ms, Some(25));
    assert_eq!(by_id("resumed-1").duration_ms, None);
    assert_eq!(by_id("refinished-1").duration_ms, Some(130));
}

/// A live run that was paused and resumed carries the paused time so far, so
/// the run list can count its active time up without the pause.
#[test]
fn list_runs_carries_the_paused_time_of_a_live_run() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_run(
        dir.path(),
        "live-paused",
        r#"{"seq":0,"ts":100,"type":"run_started","playbook":"p","version":"1.0.0"}
{"seq":1,"ts":110,"type":"run_paused","reason":"operator"}
{"seq":2,"ts":150,"type":"run_resumed","from_node":"start"}
"#,
    );
    write_run(dir.path(), "live-plain", RUNNING_EVENTS);

    let runs = list_runs(dir.path()).unwrap();
    let by_id = |id: &str| runs.iter().find(|r| r.run_id == id).expect("listed");
    assert_eq!(by_id("live-paused").duration_ms, None);
    assert_eq!(by_id("live-paused").paused_ms, Some(40));
    assert_eq!(by_id("live-plain").paused_ms, None);
}
