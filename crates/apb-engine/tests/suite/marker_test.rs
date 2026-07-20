//! Task 6: the stdout marker scan, exercised at the adapter boundary.
//!
//! These are pure adapter tests (no drive loop): a `ClaudeAdapter` runs a stub
//! whose stdout carries the question marker, and we assert how the scan reacts
//! to the `AgentTask.interactive` gate and to malformed JSON. No waits (the
//! stub exits immediately), so nothing here needs a bounded poll.

use apb_engine::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, ErrorClass};
use apb_engine::invocation::builtin;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use crate::common;

fn stub_agent(dir: &std::path::Path, body: &str) -> String {
    let path = dir.join("stub-agent.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().to_string()
}

fn task<'a>(
    prompt: &'a str,
    workdir: &'a std::path::Path,
    interactive: bool,
    node: &'a str,
    policy: &'a apb_engine::adapter::ConnectorEnvPolicy,
) -> AgentTask<'a> {
    AgentTask {
        prompt,
        model: "haiku",
        workdir,
        timeout: None,
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        connector_policy: policy,
        interactive,
        node,
    }
}

/// An interactive task whose stub prints the marker plus valid JSON parses the
/// question, and the `options` list survives into the report.
#[test]
fn interactive_marker_parses_question_with_options() {
    let dir = tempfile::tempdir().unwrap();
    let ad = ClaudeAdapter {
        program: stub_agent(
            dir.path(),
            "printf '%s\\n' '<<<apb:question>>>'\nprintf '%s\\n' '{\"question\":\"Which DB?\",\"options\":[\"pg\",\"sqlite\"]}'",
        ),
        spec: builtin("claude").unwrap(),
    };
    let policy = Default::default();
    let report = ad
        .run(&task("go", dir.path(), true, "ask", &policy))
        .expect("interactive marker run succeeds");
    let q = report.question.expect("a question was scanned");
    assert_eq!(q.question, "Which DB?");
    assert_eq!(q.options, vec!["pg".to_string(), "sqlite".to_string()]);
}

/// Malformed JSON after the marker on an interactive task fails the attempt
/// with a Transport error that names the node and the marker (never a silent
/// `None`).
#[test]
fn interactive_malformed_marker_json_fails_naming_node() {
    let dir = tempfile::tempdir().unwrap();
    let ad = ClaudeAdapter {
        program: stub_agent(
            dir.path(),
            // Truncated JSON object: not parseable as AskedQuestion.
            "printf '%s\\n' '<<<apb:question>>>'\nprintf '%s\\n' '{\"question\":'",
        ),
        spec: builtin("claude").unwrap(),
    };
    let policy = Default::default();
    let (class, msg) = ad
        .run(&task("go", dir.path(), true, "ask", &policy))
        .expect_err("malformed marker JSON must fail the attempt");
    assert!(
        matches!(class, ErrorClass::Transport),
        "class was {class:?}"
    );
    assert!(msg.contains("ask"), "error must name the node: {msg}");
    assert!(
        msg.contains("marker"),
        "error must mention the marker: {msg}"
    );
    assert!(
        msg.contains("<<<apb:question>>>"),
        "error must show the marker: {msg}"
    );
}

/// A non-interactive task treats the marker line as ordinary output: the scan
/// does not run, no question is produced, and the marker text survives in raw.
#[test]
fn non_interactive_marker_is_ordinary_output() {
    let dir = tempfile::tempdir().unwrap();
    let ad = ClaudeAdapter {
        program: stub_agent(
            dir.path(),
            "printf '%s\\n' '<<<apb:question>>>'\nprintf '%s\\n' '{\"question\":\"ignored\"}'\nprintf '%s\\n' done",
        ),
        spec: builtin("claude").unwrap(),
    };
    let policy = Default::default();
    let report = ad
        .run(&task("go", dir.path(), false, "plain", &policy))
        .expect("non-interactive run succeeds");
    assert!(
        report.question.is_none(),
        "a non-interactive node must not produce a question"
    );
    assert!(
        report.raw.contains("<<<apb:question>>>"),
        "the literal marker text must remain in the output: {}",
        report.raw
    );
}
