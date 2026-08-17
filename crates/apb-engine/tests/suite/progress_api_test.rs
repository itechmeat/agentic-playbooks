use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::Control;
use apb_engine::error::EngineError;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::list_runs;
use apb_engine::scheduler::{RunOptions, RunResult, post_supervisor_command, run};

use crate::common;

const DRIVE_DEADLINE: Duration = Duration::from_secs(10);
const POLL_DEADLINE: Duration = Duration::from_secs(10);
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

fn find_run_id(root: &Path, prefix: &str) -> String {
    poll_until(&format!("a run dir with prefix `{prefix}`"), || {
        let runs_dir = root.join(".apb/runs");
        if !runs_dir.is_dir() {
            return None;
        }
        fs::read_dir(&runs_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .find(|n| n.starts_with(prefix))
    })
}

/// The `a` branch's script: announce it is genuinely in flight, then block
/// (bounded) until the test releases it. Mirrors `stop_run_test.rs`'s
/// `hold_script`; kept local because each file in this suite is its own
/// module.
fn hold_script(root: &Path) -> String {
    format!(
        "touch '{root}/chunk1_started'\n\
         i=0\n\
         while [ ! -f '{root}/release' ]; do\n\
         \x20 i=$((i + 1))\n\
         \x20 if [ \"$i\" -gt 200 ]; then break; fi\n\
         \x20 sleep 0.05\n\
         done\n",
        root = root.display(),
    )
}

// Two script branches converging in join:all - the concurrent batch shape
// #78 is about: a report posted by one member must not be attributed to
// whichever node the drive loop happens to be holding. `a` holds until the
// test releases it, so the report can be posted while the batch is genuinely
// in flight and land on the batch tail's
// `drain_progress_after_execute(..., None)` call site, instead of on
// whatever the drive was doing before it ever started.
const BATCH: &str = r#"
schema: 1
id: batchprog
name: Batch Progress
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/hold.sh", runner: sh }
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
    fs::write(scripts.join("hold.sh"), hold_script(root)).unwrap();
    fs::write(scripts.join("fast.sh"), "sleep 0.05\n").unwrap();
}

/// #78/#90 end-to-end: a named `Control::Progress` for batch member `b`,
/// posted while the batch is genuinely in flight (branch `a` is holding on
/// `release`, so the drive cannot yet have reached the batch tail's drain),
/// must surface as a `RunProgress` event stamped with `b` - never with `a`,
/// `start`, or any other node the drive loop happens to be attributing to at
/// whatever point the entry is actually drained.
///
/// This exercises `drain_progress_after_execute(..., None)` at the batch
/// tail: posting the report before the drive even starts (as the previous
/// shape of this test did) can only ever be drained by some earlier call
/// site, so it never proved the batch-tail path at all. Bounded by
/// construction - `chunk1_started` and `release` - never by a sleep.
#[test]
fn a_named_progress_report_for_a_batch_member_is_attributed_to_that_member() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "batchprog", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "batchprog-");
    poll_until("the holding branch to start", || {
        dir.path().join("chunk1_started").is_file().then_some(())
    });

    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Progress {
            done: 1,
            total: 2,
            label: None,
            node: Some("b".into()),
        },
    )
    .unwrap();

    // Only now may the holding branch finish, so the report is provably in
    // control.jsonl while the batch is still mid-flight.
    fs::write(dir.path().join("release"), "go").unwrap();

    let res = rx
        .recv_timeout(DRIVE_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return within {DRIVE_DEADLINE:?}"))
        .expect("the batch must drive to completion");
    assert_eq!(res.run_id, run_id);

    let run_dir = dir.path().join(".apb/runs").join(&run_id);
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
