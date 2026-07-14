use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

mod common;

// Run an agent with the acp transport selected FROM the global config. Success
// plus the presence of a per-attempt stream log prove that the streaming
// path was taken (headless does not write a stream log). Own test binary per
// file: APB_CONFIG_DIR edits do not race with other binaries, and we do not
// set APB_AGENT_CMD here.
const PLAYBOOK: &str = r#"
schema: 2
id: acp
name: Acp
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", profile: acpp }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

fn write_stream_agent(root: &Path) -> String {
    let path = root.join("stream-agent.sh");
    let body = "#!/bin/sh\n\
                echo '{\"type\":\"system\",\"subtype\":\"init\"}'\n\
                echo '{\"type\":\"result\",\"is_error\":false,\"result\":\"streamed done\"}'\n";
    fs::write(&path, body).unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/acp/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/acp/current"), "1.0.0").unwrap();
    // Profile with the mock agent: acp transport comes from agents.mock in the global config.
    common::seed_profile(root, "acpp", "mock", "haiku", &[]);
}

#[test]
fn acp_transport_selected_from_config_streams_run() {
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path());

    let cfg_dir = tempfile::tempdir().unwrap();
    let agent = write_stream_agent(cfg_dir.path());
    let cfg = format!("agents:\n  mock: {{ program: {agent}, transport: acp }}\n");
    fs::write(cfg_dir.path().join("config.yaml"), cfg).unwrap();

    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg_dir.path());
        std::env::remove_var("APB_AGENT_CMD");
    }

    let res = run(proj.path(), "acp", None, RunOptions::default());

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }

    let res = res.expect("run should drive with acp transport from config");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // The streaming path writes a per-attempt NDJSON log; its presence proves
    // acp was taken, not headless.
    let stream_log = proj
        .path()
        .join(".apb/runs")
        .join(&res.run_id)
        .join("agent-stream/w-1.jsonl");
    assert!(
        stream_log.is_file(),
        "expected acp stream log at {}",
        stream_log.display()
    );
    let streamed = fs::read_to_string(&stream_log).unwrap();
    assert!(
        streamed.contains("streamed done"),
        "stream log missing result: {streamed}"
    );
}
