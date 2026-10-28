//! Which tools an agent really sees (decision record 0007): claude's
//! headless form keeps `--output-format json`, which prints no tool list, so
//! the check runs on the stream transport, where the `system/init` record
//! names the tools and MCP servers. A difference from what apb handed the
//! attempt journals `tool_exposure_mismatch` right before `attempt_finished`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::config::Transport;
use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

const AGENT: &str = "claude";

const PLAYBOOK: &str = r#"
schema: 2
id: exp
name: Exposure
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", profile: p }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

/// The headless form asks for one JSON result, never the stream: a switch
/// to stream-json would change what `outputs.extract` scans, how much
/// output reaches the limits and what the grader reads.
#[test]
fn the_headless_form_keeps_json_output() {
    let form = apb_engine::invocation::builtin(AGENT).expect("a built-in form");
    assert_eq!(form.transport, Transport::Headless);
    assert!(
        form.argv
            .windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "json"),
        "{:?}",
        form.argv
    );
    let resume = apb_engine::invocation::resume_argv(AGENT).expect("a resume form");
    assert!(
        resume
            .windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "json"),
        "{resume:?}"
    );
}

#[test]
fn the_init_record_names_the_tools_and_servers() {
    let out = apb_core::agents::lookup(AGENT).unwrap().output();
    let stream = r#"{"type":"system","subtype":"init","session_id":"s","tools":["Bash","Read","mcp__apb__ask_user"],"mcp_servers":[{"name":"apb","status":"connected"},{"name":"other","status":"failed"}]}
{"type":"result","is_error":false,"result":"done","session_id":"s"}"#;
    let seen = out.exposed_tools(stream).expect("a tool list");
    assert_eq!(seen.tools, ["Bash", "Read", "mcp__apb__ask_user"]);
    assert_eq!(
        seen.servers,
        [
            ("apb".to_string(), "connected".to_string()),
            ("other".to_string(), "failed".to_string())
        ]
    );
    // The headless result names no tools.
    assert_eq!(
        out.exposed_tools(r#"{"type":"result","is_error":false,"result":"done"}"#),
        None
    );
}

fn write_agent(dir: &Path, init: &str, exit: u8) -> String {
    let path = dir.join("stream-agent.sh");
    let body = format!(
        "#!/bin/sh\necho '{init}'\necho '{{\"type\":\"result\",\"is_error\":false,\"result\":\"streamed done\"}}'\nexit {exit}\n"
    );
    common::write_sync(&path, &body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// Runs the playbook on the stream transport with a stub that opens with
/// `init` and exits `exit`; returns the run's outcome and events.
fn run_stub(init: &str, exit: u8) -> (RunStatus, Vec<apb_engine::event::Event>) {
    let _env = common::env_lock();
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    let dir = proj.path().join(".apb/playbooks/exp/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(proj.path().join(".apb/playbooks/exp/current"), "1.0.0").unwrap();
    common::seed_profile(proj.path(), "p", AGENT, "haiku", &[]);

    let cfg_dir = tempfile::tempdir().unwrap();
    let agent = write_agent(cfg_dir.path(), init, exit);
    fs::write(
        cfg_dir.path().join("config.yaml"),
        format!("agents:\n  {AGENT}: {{ program: {agent}, transport: acp }}\n"),
    )
    .unwrap();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg_dir.path());
    let _cmd = common::EnvVar::remove("APB_AGENT_CMD");
    let res = run(proj.path(), "exp", None, RunOptions::default()).expect("the run drives");
    let events = read_all(&proj.path().join(".apb/runs").join(&res.run_id)).unwrap();
    (res.outcome, events)
}

/// [`run_stub`] for an agent that succeeds; the run succeeds too.
fn run_with_init(init: &str) -> Vec<apb_engine::event::Event> {
    let (outcome, events) = run_stub(init, 0);
    assert_eq!(outcome, RunStatus::Succeeded);
    events
}

const EXTRA_SERVER: &str = r#"{"type":"system","subtype":"init","tools":["Bash"],"mcp_servers":[{"name":"other","status":"connected"}]}"#;

/// The index of the run's one `tool_exposure_mismatch`, which the attempt's
/// `attempt_finished` follows.
fn mismatch_at(events: &[apb_engine::event::Event]) -> usize {
    let at = events
        .iter()
        .position(|e| matches!(e.payload, EventPayload::ToolExposureMismatch { .. }))
        .expect("a tool_exposure_mismatch");
    let next = events.get(at + 1).map(|e| &e.payload);
    assert!(
        matches!(next, Some(EventPayload::AttemptFinished { .. })),
        "attempt_finished must follow the mismatch, got {next:?}"
    );
    at
}

/// The minimal environment passes only apb's own MCP servers
/// (`--strict-mcp-config`), so a server the agent still reports is
/// journaled, before the attempt's finish, and the run is not failed for it.
#[test]
fn a_server_beyond_a_strict_config_is_journaled_before_the_finish() {
    let events = run_with_init(EXTRA_SERVER);
    let at = mismatch_at(&events);
    let EventPayload::ToolExposureMismatch {
        node,
        attempt,
        agent,
        unexpected_servers,
        missing_tools,
        missing_servers,
        ..
    } = &events[at].payload
    else {
        unreachable!()
    };
    assert_eq!((node.as_str(), *attempt, agent.as_str()), ("w", 1, AGENT));
    assert_eq!(unexpected_servers, &["other"]);
    assert!(missing_tools.is_empty() && missing_servers.is_empty());
}

/// A failed attempt still reports what it saw.
#[test]
fn a_failed_attempt_journals_its_mismatch_too() {
    let (outcome, events) = run_stub(EXTRA_SERVER, 3);
    assert_eq!(outcome, RunStatus::Failed);
    mismatch_at(&events);
}

#[test]
fn an_attempt_that_sees_what_it_was_handed_journals_nothing() {
    let events = run_with_init(
        r#"{"type":"system","subtype":"init","tools":["Bash","Read"],"mcp_servers":[]}"#,
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ToolExposureMismatch { .. }))
    );
}
