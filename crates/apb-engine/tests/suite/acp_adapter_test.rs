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
            interactive: false,
            node: "test",
            agent: "claude",
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
            interactive: false,
            node: "test",
            agent: "claude",
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
            interactive: false,
            node: "test",
            agent: "claude",
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
            interactive: false,
            node: "test",
            agent: "claude",
        })
        .unwrap_err();
    assert!(matches!(err.0, ErrorClass::ProcessExit), "got: {err:?}");
}

// Task 6 (deferred): the marker scan runs on the STREAM path too, over the
// terminal `result` event's text. A well-formed marker+question in the result
// text is parsed into `AgentReport.question`.
#[test]
fn acp_stream_result_marker_parses_into_question() {
    let dir = tempfile::tempdir().unwrap();
    // The result text carries the marker line then a JSON question, exactly the
    // shape a resume/reprompt agent prints inside its streamed message.
    let body = "printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"<<<apb:question>>>\\n{\\\"question\\\":\\\"Which DB?\\\",\\\"options\\\":[\\\"pg\\\",\\\"sqlite\\\"]}\"}'";
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
            interactive: true,
            node: "ask",
            agent: "claude",
        })
        .unwrap();
    let q = report
        .question
        .expect("the stream marker must parse into a question");
    assert_eq!(q.question, "Which DB?");
    assert_eq!(q.options, vec!["pg".to_string(), "sqlite".to_string()]);
}

// Task 6 (deferred): a malformed question after the marker on the stream path
// fails the attempt with a Transport error that NAMES the node - it must not be
// swallowed nor rewritten into a generic timeout/structured-output error.
#[test]
fn acp_stream_marker_malformed_json_fails_naming_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let body = "printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"<<<apb:question>>>\\n{\\\"question\\\":\"}'";
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
            interactive: true,
            node: "ask",
            agent: "claude",
        })
        .unwrap_err();
    assert!(matches!(err.0, ErrorClass::Transport), "got: {err:?}");
    assert!(
        err.1.contains("ask") && err.1.contains("marker"),
        "the malformed-marker error must name the node and the marker: {}",
        err.1
    );
}

// Task 6 (deferred): a non-interactive node's literal marker line in the result
// text is ordinary output - the scan does not run and no question is produced.
#[test]
fn acp_stream_marker_ignored_on_non_interactive_node() {
    let dir = tempfile::tempdir().unwrap();
    let body = "printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"<<<apb:question>>>\\n{\\\"question\\\":\\\"ignored\\\"}\"}'";
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
            interactive: false,
            node: "plain",
            agent: "claude",
        })
        .unwrap();
    assert!(
        report.question.is_none(),
        "a non-interactive node's literal marker must not raise a question"
    );
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
            interactive: false,
            node: "test",
            agent: "claude",
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
                interactive: false,
                node: "test",
                agent: "claude",
            },
            &cancel,
            None,
            None,
            None,
            None,
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
