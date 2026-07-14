use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::review::{ReviewCommand, post_review};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(10);

const WF_REVIEW: &str = r#"
schema: 1
id: rev
name: Review
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

const WF_OUTPUT_MATCH: &str = r#"
schema: 1
id: rev
name: Review
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: build, type: script, script: "scripts/build.sh", runner: sh }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: build }
  - { from: build, to: ok, condition: { type: output_match, node: build, pattern: "BUILD OK" } }
  - { from: build, to: no, fallback: true }
"#;

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/rev/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/rev/current"), "1.0.0").unwrap();
}

fn run_in_background(root: &Path) -> mpsc::Receiver<RunResult> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let res = run(&root, "rev", None, RunOptions::default()).unwrap();
        let _ = tx.send(res);
    });
    rx
}

fn latest_run_dir(root: &Path) -> std::path::PathBuf {
    poll_until("run dir to appear", || {
        let runs = root.join(".apb/runs");
        let entry = fs::read_dir(&runs)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())?;
        Some(entry.path())
    })
}

fn decide(root: &Path, run_dir: &Path, decision: &str) {
    // Wait for the review request to be announced, then post the decision.
    poll_until("review_requested", || {
        read_all(run_dir).ok()?.iter().any(|e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "gate")).then_some(())
    });
    post_review(
        run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: decision.into(),
            note: "n".into(),
        },
    )
    .unwrap();
    let _ = root;
}

fn wait_result(rx: &mpsc::Receiver<RunResult>) -> RunResult {
    rx.recv_timeout(POLL_DEADLINE)
        .unwrap_or_else(|_| panic!("run did not finish within {POLL_DEADLINE:?}"))
}

#[test]
fn human_review_approved_routes_to_success() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_REVIEW);
    let rx = run_in_background(dir.path());
    let run_dir = latest_run_dir(dir.path());
    decide(dir.path(), &run_dir, "approved");
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::ReviewDecided { decision, .. } if decision == "approved")));
}

#[test]
fn human_review_rejected_routes_to_failure() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_REVIEW);
    let rx = run_in_background(dir.path());
    let run_dir = latest_run_dir(dir.path());
    decide(dir.path(), &run_dir, "rejected");
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
}

#[test]
fn output_match_routes_on_substring() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_OUTPUT_MATCH);
    let scripts = dir.path().join(".apb/playbooks/rev/1.0.0/scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("build.sh"), "echo 'BUILD OK'\n").unwrap();
    let result = run(dir.path(), "rev", None, RunOptions::default()).unwrap();
    assert_eq!(result.outcome, RunStatus::Succeeded);
}
