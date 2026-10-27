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
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "test",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
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
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "test",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
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
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "test",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        })
        .unwrap_err();
    assert!(
        matches!(err.class, ErrorClass::StructuredOutputMissing),
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
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "test",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        })
        .unwrap_err();
    assert!(matches!(err.class, ErrorClass::ProcessExit), "got: {err:?}");
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
            spawn_env: &Default::default(),
            interactive: true,
            report_contract: true,
            node: "ask",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
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
            spawn_env: &Default::default(),
            interactive: true,
            report_contract: true,
            node: "ask",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        })
        .unwrap_err();
    assert!(matches!(err.class, ErrorClass::Transport), "got: {err:?}");
    assert!(
        err.message.contains("ask") && err.message.contains("marker"),
        "the malformed-marker error must name the node and the marker: {}",
        err.message
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
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "plain",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
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
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "test",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        })
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(matches!(err.class, ErrorClass::Timeout), "got: {err:?}");
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
                spawn_env: &Default::default(),
                interactive: false,
                report_contract: true,
                node: "test",
                agent: "claude",
                extract: None,
                status_file: None,
                hermetic_settings: None,
                transcript_dir: None,
                output_limits: Default::default(),
            },
            &cancel,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err.class, ErrorClass::Transport),
        "cancel should surface as transport: {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "cancel did not stop the agent promptly"
    );
}

/// The stream transport replaces the headless form's `--output-format json`
/// instead of passing two formats, and reads the usage of the final `result`
/// line (issue #167).
#[test]
fn acp_passes_one_output_format_and_reads_the_result_usage() {
    let dir = tempfile::tempdir().unwrap();
    let argv_file = dir.path().join("argv.txt");
    let body = format!(
        "printf '%s\\n' \"$@\" > '{}'\n\
         echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done ok\",\"usage\":{{\"input_tokens\":7,\"output_tokens\":3,\"cache_read_input_tokens\":11,\"cache_creation_input_tokens\":0}}}}'",
        argv_file.display()
    );
    let ad = acp(stub(dir.path(), &body));
    let report = ad
        .run(&AgentTask {
            prompt: "go",
            model: "haiku",
            workdir: dir.path(),
            timeout: None,
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "test",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        })
        .unwrap();

    let argv: Vec<String> = fs::read_to_string(&argv_file)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let formats: Vec<&str> = argv
        .windows(2)
        .filter(|w| w[0] == "--output-format")
        .map(|w| w[1].as_str())
        .collect();
    assert_eq!(formats, vec!["stream-json"], "{argv:?}");
    let usage = report.usage.expect("the result line reports usage");
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens
        ),
        (7, 3, 11)
    );
}

/// A streaming agent stopped by its timeout gets SIGTERM first; the final
/// result it prints before exiting still gives the attempt its usage, while
/// the verdict stays a timeout.
#[test]
fn acp_timeout_keeps_the_usage_the_agent_printed_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("trap-set");
    let record = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","usage":{"input_tokens":5,"output_tokens":2}}"#;
    // The trap is set before `ready` is left, so a TERM that came earlier
    // would end the shell without `ready`. `APB_STUB_WARM_UP` stops there.
    let body = format!(
        "trap 'echo '\\''{record}'\\''; exit 143' TERM\n: > {}\n[ -n \"$APB_STUB_WARM_UP\" ] && exit 0\nsleep 30 &\nwait $!",
        ready.display()
    );
    let program = stub(dir.path(), &body);
    // A first launch of a new script can stall past the one-second timeout
    // on some machines; launch it once and wait for it, so the run's launch
    // is quick and its TERM finds the trap in place.
    let warm = std::process::Command::new(&program)
        .env("APB_STUB_WARM_UP", "1")
        .status()
        .unwrap();
    assert!(warm.success() && ready.exists(), "warm-up: {warm}");
    fs::remove_file(&ready).unwrap();

    let err = acp(program)
        .run(&AgentTask {
            prompt: "go",
            model: "haiku",
            workdir: dir.path(),
            timeout: Some(Duration::from_secs(1)),
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "test",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        })
        .unwrap_err();
    assert!(ready.exists(), "the trap was set before the timeout's TERM");
    assert_eq!(err.class, ErrorClass::Timeout, "got: {err:?}");
    let usage = err.usage.expect("usage of the result printed on SIGTERM");
    assert_eq!((usage.input_tokens, usage.output_tokens), (5, 2));
}

/// An errored stream-json result that exits non-zero still carries the usage
/// it spent, and the failure hands it on (as the headless transport does).
#[test]
fn acp_nonzero_exit_keeps_the_usage_of_the_errored_result() {
    let dir = tempfile::tempdir().unwrap();
    let body = "echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"result\":\"bad model\",\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}'\n\
                exit 1";
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
            spawn_env: &Default::default(),
            interactive: false,
            report_contract: true,
            node: "test",
            agent: "claude",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        })
        .unwrap_err();
    assert_eq!(err.class, ErrorClass::ProcessExit, "got: {err:?}");
    let usage = err.usage.expect("usage of the errored result");
    assert_eq!((usage.input_tokens, usage.output_tokens), (7, 3));
}

/// A streaming task with no stream log and tiny output limits, so the
/// in-memory copy is the only one.
fn bounded_task<'a>(dir: &'a Path, policy: &'a apb_engine::adapter::SpawnEnv) -> AgentTask<'a> {
    AgentTask {
        prompt: "go",
        model: "haiku",
        workdir: dir,
        timeout: None,
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        spawn_env: policy,
        interactive: false,
        report_contract: true,
        node: "test",
        agent: "claude",
        extract: None,
        status_file: None,
        hermetic_settings: None,
        transcript_dir: None,
        output_limits: apb_core::output_limits::OutputLimits {
            head_bytes: 256,
            tail_bytes: 1024,
            ..Default::default()
        },
    }
}

#[test]
fn acp_a_line_longer_than_the_tail_is_dropped_and_the_result_still_read() {
    let dir = tempfile::tempdir().unwrap();
    // One 5000-byte event between the init line and the result.
    let body = format!(
        "echo '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s-9\"}}'\n\
         echo '{{\"type\":\"assistant\",\"text\":\"{}\"}}'\n\
         echo '{{\"type\":\"result\",\"is_error\":false,\"result\":\"done ok\"}}'",
        "z".repeat(5000)
    );
    let report = acp(stub(dir.path(), &body))
        .run(&bounded_task(dir.path(), &Default::default()))
        .unwrap();

    assert_eq!(report.status, NodeStatus::Succeeded);
    assert_eq!(report.summary, "done ok");
    // The session id came from the head.
    assert_eq!(report.session.as_deref(), Some("s-9"));
    assert!(!report.raw.contains("zzzz"));
    assert!(!report.capture.transcript_incomplete);
    let loss = &report.capture.truncated[0];
    assert_eq!(loss.stream, "stdout");
    assert!(loss.dropped_bytes_at_least > 5000);
}

#[test]
fn acp_a_result_line_longer_than_the_tail_fails_as_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "echo '{{\"type\":\"result\",\"is_error\":false,\"result\":\"{}\"}}'",
        "z".repeat(5000)
    );
    let err = acp(stub(dir.path(), &body))
        .run(&bounded_task(dir.path(), &Default::default()))
        .unwrap_err();

    assert_eq!(err.class, ErrorClass::StructuredOutputMissing);
    assert_eq!(err.message, apb_engine::capture::INCOMPLETE_RESULT);
    assert!(err.capture.transcript_incomplete);
}
