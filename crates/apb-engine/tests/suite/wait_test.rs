use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::signals::{SignalCommand, post_signal};
use apb_engine::state::RunStatus;

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(10);

// The timer fires right away (seconds: 0) and goes to a success finish.
const WF_TIMER: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: timer, seconds: 0 }, timeout_seconds: 60 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: wait }
  - { from: wait, to: done }
"#;

// Waits for a webhook signal; success on success, failure on timeout (fallback).
const WF_WEBHOOK: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 60 }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: wait }
  - { from: wait, to: ok, condition: { type: node_status, node: wait, equals: success } }
  - { from: wait, to: no, fallback: true }
"#;

// The timeout fires immediately (timeout_seconds: 0), no signal -> failure.
const WF_TIMEOUT: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 0 }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: wait }
  - { from: wait, to: ok, condition: { type: node_status, node: wait, equals: success } }
  - { from: wait, to: no, fallback: true }
"#;

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/w/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/w/current"), "1.0.0").unwrap();
}

fn latest_run_dir(root: &Path) -> std::path::PathBuf {
    poll_until("run dir", || {
        let entry = fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())?;
        Some(entry.path())
    })
}

#[test]
fn timer_wait_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_TIMER);
    let result = run(dir.path(), "w", None, RunOptions::default()).unwrap();
    assert_eq!(result.outcome, RunStatus::Succeeded);
}

#[test]
fn webhook_signal_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_WEBHOOK);
    let (tx, rx) = mpsc::channel::<RunResult>();
    let root = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "w", None, RunOptions::default()).unwrap());
    });
    let run_dir = latest_run_dir(dir.path());
    poll_until("wait_started", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitStarted { .. }))
            .then_some(())
    });
    post_signal(&run_dir, SignalCommand { key: "ci".into() }).unwrap();
    let result = rx.recv_timeout(POLL_DEADLINE).expect("run finished");
    assert_eq!(result.outcome, RunStatus::Succeeded);
    assert!(
        read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitSignalled { .. }))
    );
}

#[test]
fn webhook_timeout_fails() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_TIMEOUT);
    let result = run(dir.path(), "w", None, RunOptions::default()).unwrap();
    assert_eq!(result.outcome, RunStatus::Failed);
    let run_dir = latest_run_dir(dir.path());
    assert!(
        read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitTimeout { .. }))
    );
}
