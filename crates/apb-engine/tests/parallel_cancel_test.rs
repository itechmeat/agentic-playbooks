use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Instant;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

mod common;

// Two agent_task branches in join:any: the fast one (prompt "fast") finishes right away,
// the slow one (prompt "slow") sleeps 5s. Once join:any is satisfied, the engine must
// kill the slow branch's process (7c-3), rather than wait its 5 seconds.
const PLAYBOOK: &str = r#"
schema: 1
id: cancel
name: Cancel
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: fa, type: agent_task, prompt: "fast" }
  - { id: sa, type: agent_task, prompt: "slow" }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: fa }
  - { from: start, to: sa }
  - { from: fa, to: j, join: any }
  - { from: sa, to: j, join: any }
  - { from: j, to: done }
"#;

fn write_mock_agent(root: &Path) -> String {
    // Adapter arguments: -p <prompt> --model <model>. $2 = the prompt.
    let path = root.join("mock-agent.sh");
    fs::write(
        &path,
        "#!/bin/sh\ncase \"$2\" in *slow*) sleep 5 ;; esac\necho done\n",
    )
    .unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/cancel/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/cancel/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

#[test]
fn join_any_kills_slower_branch() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let prog = write_mock_agent(dir.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let started = Instant::now();
    let res = run(dir.path(), "cancel", None, RunOptions::default()).unwrap();
    let elapsed = started.elapsed();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }

    assert_eq!(res.outcome, RunStatus::Succeeded);
    // If the slow branch had not been killed, the run would have taken >= 5s. Threshold with margin.
    assert!(
        elapsed.as_millis() < 3000,
        "slow branch not killed: run took {elapsed:?}"
    );

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    // The slow branch is marked cancelled.
    assert!(
        events.iter().any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, status, .. } if node == "sa" && status == "cancelled")),
        "slow branch `sa` must be cancelled"
    );
}
