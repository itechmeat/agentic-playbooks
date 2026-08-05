use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

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

/// The slow branch's stub leaves a marker on the line past its sleep, so the
/// marker's ABSENCE is the proof that the branch was killed rather than waited
/// out.
///
/// This replaces an earlier wall-clock assertion (`run()` had to return in under
/// 3s while the slow branch slept 5s). That bound measured the machine, not the
/// engine: on this project's macOS machines the per-launch security scan of a
/// freshly written `sh` stub (BUILD-OPTIMIZATION rule 8) measured 3.9s to 53.5s
/// of pure spawn and kill stall on an otherwise idle tree, failing the 3s budget
/// in 5 of 8 isolated runs while the cancel path worked every time.
fn write_mock_agent(root: &Path, marker: &Path) -> String {
    // Adapter arguments: -p <prompt> --model <model>. $2 = the prompt.
    let path = root.join("mock-agent.sh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$2\" in *slow*) sleep 5; : > {} ;; esac\necho done\n",
            marker.to_string_lossy()
        ),
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
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let marker = dir.path().join("slow-branch-ran-to-completion");
    let prog = write_mock_agent(dir.path(), &marker);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let res = run(dir.path(), "cancel", None, RunOptions::default()).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }

    assert_eq!(res.outcome, RunStatus::Succeeded);
    // Killed, not waited out: the slow stub never reached the line past its sleep.
    assert!(
        !marker.exists(),
        "slow branch not killed: it ran past its sleep and wrote {}",
        marker.display()
    );

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    // The slow branch is marked cancelled.
    assert!(
        events.iter().any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, status, .. } if node == "sa" && status == "cancelled")),
        "slow branch `sa` must be cancelled"
    );
}
