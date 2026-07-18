use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

// The agent self-reports success (echo ok -> no block -> success), but the
// success_check script decides the final status: exit 1 -> node Failed ->
// failure branch; exit 0 -> success.
const PLAYBOOK: &str = r#"
schema: 1
id: sc
name: SuccessCheck
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", success_check: "scripts/check.sh" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

fn seed(root: &Path, check_exit: u8) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/sc/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/sc/current"), "1.0.0").unwrap();
    common::seed_main(root);
    // The check script with the given exit code (copied to run_dir/scripts).
    fs::write(
        dir.join("scripts/check.sh"),
        format!("#!/bin/sh\nexit {check_exit}\n"),
    )
    .unwrap();
}

fn ok_agent(root: &Path) -> String {
    let path = root.join("ok-agent.sh");
    fs::write(&path, "#!/bin/sh\necho ok\n").unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

// Both branches sequentially: APB_AGENT_CMD is process-global.
#[test]
fn success_check_overrides_agent_self_assessment() {
    let _env = common::env_lock();
    // 1. The check fails (exit 1) -> node Failed despite echo ok.
    let fail = tempfile::tempdir().unwrap();
    seed(fail.path(), 1);
    let prog = ok_agent(fail.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(fail.path(), "sc", None, RunOptions::default()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "failing success_check must fail the node"
    );

    // 2. The check passes (exit 0) -> success.
    let pass = tempfile::tempdir().unwrap();
    seed(pass.path(), 0);
    let prog2 = ok_agent(pass.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog2);
    }
    let res2 = run(pass.path(), "sc", None, RunOptions::default()).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(
        res2.outcome,
        RunStatus::Succeeded,
        "passing success_check must allow success"
    );
}
