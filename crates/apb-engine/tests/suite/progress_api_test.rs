use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::control::{Control, post_control};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::list_runs;
use apb_engine::scheduler::{RunOptions, drive_run_from_dir, prepare_supervised_background};

// Two script branches converging in join:all - the concurrent batch shape
// #78 is about: a report posted by one member must not be attributed to
// whichever node the drive loop happens to be holding.
const BATCH: &str = r#"
schema: 1
id: batchprog
name: Batch Progress
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/fast.sh", runner: sh }
  - { id: b, type: script, script: "scripts/fast.sh", runner: sh }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/batchprog/1.0.0");
    let scripts = dir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(dir.join("playbook.yaml"), BATCH).unwrap();
    fs::write(root.join(".apb/playbooks/batchprog/current"), "1.0.0").unwrap();
    fs::write(scripts.join("fast.sh"), "sleep 0.05\n").unwrap();
}

/// #78 end-to-end: a named `Control::Progress` for batch member `b`, written
/// to `control.jsonl` before the drive ever starts (so no sleep or poll is
/// needed to land it deterministically), must surface as a `RunProgress`
/// event stamped with `b` - never with `a`, `start`, or any other node the
/// drive loop happens to be attributing to at whatever point the entry is
/// actually drained.
#[test]
fn a_named_progress_report_for_a_batch_member_is_attributed_to_that_member() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let prepared =
        prepare_supervised_background(dir.path(), "batchprog", None, RunOptions::default())
            .unwrap();
    let run_id = prepared.run_id().to_string();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    post_control(
        &run_dir,
        Control::Progress {
            done: 1,
            total: 2,
            label: None,
            node: Some("b".into()),
        },
    )
    .unwrap();

    // Let go of the prepared run exactly as the detached-driver handoff does,
    // then drive it from nothing but the run directory.
    drop(prepared);

    let res = drive_run_from_dir(dir.path(), &run_id).unwrap();
    assert_eq!(res.run_id, run_id);

    let events = read_all(&run_dir).unwrap();
    let stamped = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProgress { node_id, .. } => Some(node_id.clone()),
            _ => None,
        })
        .expect("a RunProgress event must have been written");
    assert_eq!(stamped, "b");
}

#[test]
fn run_summary_includes_progress_field() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
    )
    .unwrap();
    std::fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n",
    )
    .unwrap();
    let runs = list_runs(tmp.path()).unwrap();
    let r = runs.iter().find(|r| r.run_id == "r1").unwrap();
    let p = r.progress.as_ref().expect("progress present");
    assert_eq!(p.percent, 100);
}
