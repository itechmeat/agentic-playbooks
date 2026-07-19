use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use apb_core::config::Transport;
use apb_engine::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, ErrorClass};
use apb_engine::invocation::builtin;
use apb_engine::state::NodeStatus;

use crate::common;

// Stub agent that prints given stream-json lines. Ignores its own
// arguments (-p/--model/--output-format/--verbose), the same way a real agent
// in streaming mode prints NDJSON to stdout.
fn stub(dir: &Path, body: &str) -> String {
    let path = dir.join("acp-stub.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().to_string()
}

fn acp(program: String) -> ClaudeAdapter {
    let mut spec = builtin("claude").unwrap();
    spec.transport = Transport::Acp;
    ClaudeAdapter { program, spec }
}

#[test]
fn acp_success_extracts_result_and_streams_to_log() {
    let dir = tempfile::tempdir().unwrap();
    let body = "echo '{\"type\":\"system\",\"subtype\":\"init\"}'\n\
                echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done ok\"}'";
    let ad = acp(stub(dir.path(), body));
    let log = dir.path().join("stream/work-1.jsonl");
    let report = ad
        .run(&AgentTask {
            prompt: "go",
            model: "haiku",
            workdir: dir.path(),
            timeout: None,
            stream_log: Some(&log),
            soul: None,
            grant_autonomy: false,
            connector_policy: &Default::default(),
        })
        .unwrap();

    assert_eq!(report.status, NodeStatus::Succeeded);
    assert_eq!(report.summary, "done ok");
    // Stream log is created and contains both NDJSON events, one per line.
    let streamed = fs::read_to_string(&log).unwrap();
    assert!(
        streamed.contains("\"type\":\"system\""),
        "stream log missing init event: {streamed}"
    );
    assert!(
        streamed.contains("\"result\":\"done ok\""),
        "stream log missing result event: {streamed}"
    );
}

#[test]
fn acp_result_is_error_maps_to_failed_status() {
    let dir = tempfile::tempdir().unwrap();
    let body = "echo '{\"type\":\"result\",\"is_error\":true,\"result\":\"nope\"}'";
    let ad = acp(stub(dir.path(), body));
    let report = ad
        .run(&AgentTask {
            prompt: "go",
            model: "haiku",
            workdir: dir.path(),
            timeout: None,
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            connector_policy: &Default::default(),
        })
        .unwrap();
    // agent_reported_failure: the report is valid, status failure - NOT a transport error.
    assert_eq!(report.status, NodeStatus::Failed);
    assert_eq!(report.summary, "nope");
}

#[test]
fn acp_no_result_event_is_structured_output_missing() {
    let dir = tempfile::tempdir().unwrap();
    let body = "echo '{\"type\":\"assistant\",\"message\":\"thinking\"}'";
    let ad = acp(stub(dir.path(), body));
    let err = ad
        .run(&AgentTask {
            prompt: "go",
            model: "haiku",
            workdir: dir.path(),
            timeout: None,
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            connector_policy: &Default::default(),
        })
        .unwrap_err();
    assert!(
        matches!(err.0, ErrorClass::StructuredOutputMissing),
        "got: {err:?}"
    );
}

#[test]
fn acp_nonzero_exit_is_process_exit() {
    let dir = tempfile::tempdir().unwrap();
    let body = "echo '{\"type\":\"system\"}'\nexit 4";
    let ad = acp(stub(dir.path(), body));
    let err = ad
        .run(&AgentTask {
            prompt: "go",
            model: "haiku",
            workdir: dir.path(),
            timeout: None,
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            connector_policy: &Default::default(),
        })
        .unwrap_err();
    assert!(matches!(err.0, ErrorClass::ProcessExit), "got: {err:?}");
}

#[test]
fn acp_timeout_kills_streaming_agent() {
    let dir = tempfile::tempdir().unwrap();
    let body = "sleep 5\necho '{\"type\":\"result\",\"is_error\":false,\"result\":\"late\"}'";
    let ad = acp(stub(dir.path(), body));
    let started = Instant::now();
    let err = ad
        .run(&AgentTask {
            prompt: "go",
            model: "haiku",
            workdir: dir.path(),
            timeout: Some(Duration::from_secs(1)),
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            connector_policy: &Default::default(),
        })
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(matches!(err.0, ErrorClass::Timeout), "got: {err:?}");
    assert!(
        elapsed < Duration::from_secs(3),
        "streaming agent not killed on timeout: {elapsed:?}"
    );
}

#[test]
fn acp_cancel_stops_streaming_agent() {
    let dir = tempfile::tempdir().unwrap();
    let body = "sleep 5\necho '{\"type\":\"result\",\"is_error\":false,\"result\":\"late\"}'";
    let ad = acp(stub(dir.path(), body));
    // Cancel flag is already set: the very first check in the loop kills the process.
    let cancel = AtomicBool::new(true);
    let started = Instant::now();
    let err = ad
        .run_cancellable(
            &AgentTask {
                prompt: "go",
                model: "haiku",
                workdir: dir.path(),
                timeout: None,
                stream_log: None,
                soul: None,
                grant_autonomy: false,
                connector_policy: &Default::default(),
            },
            &cancel,
        )
        .unwrap_err();
    assert!(
        matches!(err.0, ErrorClass::Transport),
        "cancel should surface as transport: {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "cancel did not stop the agent promptly"
    );
}
