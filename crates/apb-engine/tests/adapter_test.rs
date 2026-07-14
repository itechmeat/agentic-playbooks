use apb_engine::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, ErrorClass, adapter_for};
use apb_engine::invocation::builtin;
use apb_engine::state::NodeStatus;
use std::fs;
use std::os::unix::fs::PermissionsExt;

mod common;

// Prepares a stub agent: a shell script with the given body.
fn stub_agent(dir: &std::path::Path, body: &str) -> String {
    let path = dir.join("stub-agent.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().to_string()
}

// Tests that spawn a process construct ClaudeAdapter directly with an explicit program,
// to avoid mutating the global APB_AGENT_CMD (a race under parallel tests).
#[test]
fn claude_adapter_success_via_stub() {
    let dir = tempfile::tempdir().unwrap();
    let ad = ClaudeAdapter {
        program: stub_agent(dir.path(), "echo pong"),
        spec: builtin("claude").unwrap(),
    };
    let report = ad
        .run(&AgentTask {
            prompt: "ping",
            model: "haiku",
            workdir: dir.path(),
            timeout: None,
            stream_log: None,
            soul: None,
        })
        .unwrap();
    assert_eq!(report.status, NodeStatus::Succeeded);
    assert_eq!(report.summary, "pong");
}

#[test]
fn claude_adapter_nonzero_exit_is_process_exit() {
    let dir = tempfile::tempdir().unwrap();
    let ad = ClaudeAdapter {
        program: stub_agent(dir.path(), "echo boom 1>&2\nexit 3"),
        spec: builtin("claude").unwrap(),
    };
    let err = ad
        .run(&AgentTask {
            prompt: "ping",
            model: "haiku",
            workdir: dir.path(),
            timeout: None,
            stream_log: None,
            soul: None,
        })
        .unwrap_err();
    assert!(matches!(err.0, ErrorClass::ProcessExit));
}

#[test]
fn adapter_for_maps_known_and_rejects_unknown() {
    assert!(adapter_for("claude-code").is_ok());
    assert!(adapter_for("borg").is_err());
}
