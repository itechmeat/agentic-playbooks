use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

mod common;

// The profile references the `mock` agent, whose program is set in the global
// config (agents.mock.program). A successful run without APB_AGENT_CMD proves
// that the agent program is taken from the config (spec 7.1). Own test process
// per file guarantees the absence of APB_AGENT_CMD from other tests.
const PLAYBOOK: &str = r#"
schema: 2
id: gc
name: Global
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", profile: cheap }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

fn write_mock_agent(root: &Path) -> String {
    let path = root.join("mock-agent.sh");
    common::write_sync(&path, "#!/bin/sh\necho ok\n");
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/gc/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/gc/current"), "1.0.0").unwrap();
    common::seed_profile(root, "cheap", "mock", "haiku", &[]);
}

#[test]
fn agent_program_from_config_is_used() {
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path());

    // Global config in a separate directory.
    let cfg_dir = tempfile::tempdir().unwrap();
    let mock = write_mock_agent(cfg_dir.path());
    let cfg_yaml = format!("agents:\n  mock: {{ program: {mock} }}\n");
    fs::write(cfg_dir.path().join("config.yaml"), cfg_yaml).unwrap();

    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg_dir.path());
        // Explicitly remove the override so the adapter takes the program FROM the config.
        std::env::remove_var("APB_AGENT_CMD");
    }

    let res = run(proj.path(), "gc", None, RunOptions::default());

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }

    let res = res.expect("run should prepare and drive with global config");
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "run must succeed using the config-defined agent program"
    );
}
