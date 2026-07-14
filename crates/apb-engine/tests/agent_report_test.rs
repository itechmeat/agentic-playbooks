use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

mod common;

// An agent node branching on node_status: success -> ok, otherwise -> no (fallback).
// The node status must come from the agent report block (spec 6.2), not from the
// process return code.
const PLAYBOOK: &str = r#"
schema: 1
id: rep
name: Report
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

// Stub agent prints some text plus a report block with the given status. Ignores
// its own arguments (like a headless agent reading the task from -p).
fn write_agent(root: &Path, status: &str) -> String {
    let path = root.join(format!("agent-{status}.sh"));
    let body = format!(
        "#!/bin/sh\nprintf 'did work\\n'\nprintf '```yaml\\n'\nprintf 'status: {status}\\n'\nprintf 'summary: {status} case\\n'\nprintf '```\\n'\n"
    );
    fs::write(&path, body).unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/rep/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/rep/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

// Both branches sequentially in one test: APB_AGENT_CMD is process-global,
// parallel #[test]s would race over it.
#[test]
fn node_status_comes_from_agent_report_block() {
    // 1. Self-reported failure with return code 0 -> failure branch.
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let prog = write_agent(dir.path(), "failure");
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "rep", None, RunOptions::default()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "self-reported failure must take the failure branch"
    );

    // 2. Self-reported success -> success branch.
    let dir2 = tempfile::tempdir().unwrap();
    seed(dir2.path());
    let prog2 = write_agent(dir2.path(), "success");
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog2);
    }
    let res2 = run(dir2.path(), "rep", None, RunOptions::default()).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res2.outcome,
        RunStatus::Succeeded,
        "self-reported success must take the success branch"
    );
}
