//! End-to-end test: `apb mcp` starts a stdio MCP server and responds to
//! `tools/list` after the full MCP handshake (initialize ->
//! notifications/initialized).
//!
//! Frames are newline-delimited JSON-RPC (one message per line), as required
//! by the `rmcp::transport::stdio()` transport (see `AsyncRwTransport` /
//! `JsonRpcMessageCodec` in rmcp 2.2.0). The protocol version "2024-11-05" is
//! one of the versions known to rmcp 2.2.0
//! (`ProtocolVersion::V_2024_11_05`); the server recognizes and negotiates it
//! without errors.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Spawns `apb mcp` in `dir` with extra env and completes the MCP handshake
/// (initialize, then notifications/initialized). Response lines arrive on the
/// returned channel, so every wait can be bounded.
pub(crate) fn start_mcp(
    dir: &Path,
    env: &[(&str, &str)],
) -> (Child, ChildStdin, mpsc::Receiver<String>) {
    let mut cmd = crate::common::apb_std();
    cmd.arg("mcp")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // A separate thread reads lines and hands them over via a channel, so the
    // main thread can bound the wait with a timeout (a blocking read_line
    // does not cancel itself if the child never sends a response).
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // 1) initialize: the required first request of the MCP handshake.
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();
    let init_line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("no response to initialize within timeout");
    assert!(
        init_line.contains("\"id\":1") && init_line.contains("protocolVersion"),
        "unexpected initialize response: {init_line}"
    );

    // 2) notifications/initialized: without it, rmcp will not serve tools/list.
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    writeln!(stdin, "{initialized}").unwrap();
    stdin.flush().unwrap();
    (child, stdin, rx)
}

/// Sends one JSON-RPC request and returns the response line with that id.
pub(crate) fn request(
    stdin: &mut ChildStdin,
    rx: &mpsc::Receiver<String>,
    id: u32,
    body: &str,
) -> String {
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":{id},{body}}}"#).unwrap();
    stdin.flush().unwrap();
    let tag = format!("\"id\":{id}");
    loop {
        let line = rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("no response to request {id} within 10s"));
        if line.contains(&tag) {
            return line;
        }
    }
}

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

#[test]
fn mcp_initialize_and_list_tools() {
    let dir = tempfile::tempdir().unwrap();

    // init + seed a minimal playbook without an agent_task node.
    crate::common::apb_std()
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let vdir = dir.path().join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(dir.path().join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();

    let (mut child, mut stdin, rx) = start_mcp(dir.path(), &[]);

    // 3) tools/list - only now.
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    writeln!(stdin, "{list}").unwrap();
    stdin.flush().unwrap();

    let mut saw_tools = false;
    for _ in 0..20 {
        let Ok(line) = rx.recv_timeout(Duration::from_secs(10)) else {
            break;
        };
        if line.contains("playbook_list") && line.contains("playbook_run") {
            saw_tools = true;
            break;
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        saw_tools,
        "tools/list must include playbook_list and playbook_run"
    );
}

/// The `apb mcp` a background supervisor agent connects to (it inherits
/// `APB_MCP_ROLE=supervisor` from the agent apb spawned) serves only the
/// supervisor tools and read-only tools: a supervisor limited by its
/// capabilities cannot reach the same run through the operator's tools, and
/// cannot answer a question as the human.
#[test]
fn a_supervisor_mcp_serves_only_supervisor_and_read_only_tools() {
    let dir = tempfile::tempdir().unwrap();
    crate::common::apb_std()
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let (mut child, mut stdin, rx) = start_mcp(dir.path(), &[("APB_MCP_ROLE", "supervisor")]);

    let list = request(&mut stdin, &rx, 2, r#""method":"tools/list","params":{}"#);
    let answer = request(
        &mut stdin,
        &rx,
        3,
        r#""method":"tools/call","params":{"name":"run_answer","arguments":{"run_id":"r-1","answer":"yes"}}"#,
    );
    let _ = child.kill();
    let _ = child.wait();

    let list: serde_json::Value = serde_json::from_str(&list).unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"supervisor_wait_event"), "{names:?}");
    assert!(
        names.contains(&"run_status"),
        "read-only tools stay: {names:?}"
    );
    for tool in [
        "run_stop",
        "run_pause",
        "run_resume",
        "review_decide",
        "playbook_create",
        "playbook_run",
        "playbook_execute_plan",
    ] {
        assert!(!names.contains(&tool), "{tool} listed: {names:?}");
    }
    assert!(
        answer.contains("supervisor_session_requires_token"),
        "run_answer by run_id is the operator's path: {answer}"
    );
}

/// The JSON a `tools/call` answered, from its first content block.
fn tool_json(line: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no content: {line}"));
    serde_json::from_str(text).unwrap_or_else(|_| panic!("not JSON: {text}"))
}

/// What a person checks before a real run, through the shipped binary and
/// its MCP server: `apb demo` installs the keyless demo; `playbook_scenarios`
/// lists its scenario, never played; `playbook_scenario_run` plays it at
/// quick through `apb __scenario-run` (passed, with the run graph, nothing
/// under `.apb/runs`), and the list shows that result fresh; the judge
/// sandbox evaluates the "check the task first" example's judge with given
/// answers and routes each answer, asking no one.
#[test]
fn the_demo_scenario_and_the_judge_sandbox_answer_through_apb_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::create_dir_all(base.join("home")).unwrap();
    let cfg = base.join("cfg");
    let home = base.join("home");
    let env = [
        ("APB_CONFIG_DIR", cfg.to_str().unwrap()),
        ("HOME", home.to_str().unwrap()),
    ];
    let mut demo = crate::common::apb_std();
    demo.args(["demo", "--json"]).current_dir(base);
    for (k, v) in env {
        demo.env(k, v);
    }
    let out = demo.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let project = cfg.join("demo");
    let example = project.join(".apb/playbooks/task-check-first");
    fs::create_dir_all(example.join("1.0.0")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/playbooks/task-check-first.yaml"),
        example.join("1.0.0/playbook.yaml"),
    )
    .unwrap();
    fs::write(example.join("current"), "1.0.0").unwrap();

    let (mut child, mut stdin, rx) = start_mcp(&project, &env);
    let call = |stdin: &mut ChildStdin, id: u32, tool: &str, args: serde_json::Value| {
        let body = serde_json::json!({
            "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        })
        .to_string();
        tool_json(&request(stdin, &rx, id, &body[1..body.len() - 1]))
    };

    let listed = call(
        &mut stdin,
        2,
        "playbook_scenarios",
        serde_json::json!({ "id": "demo-report" }),
    );
    assert_eq!(listed["scenarios"][0]["name"], "approve", "{listed}");
    assert_eq!(listed["scenarios"][0]["latest"], serde_json::json!([]));

    let played = call(
        &mut stdin,
        3,
        "playbook_scenario_run",
        serde_json::json!({ "id": "demo-report", "scenario": "approve", "level": "quick" }),
    );
    assert_eq!(played["report"]["verdict"], "passed", "{played}");
    let run = played["report"]["run_id"].as_str().unwrap().to_string();
    assert!(played["graph"]["nodes"].is_array(), "{played}");
    assert!(project.join(".apb/scenario-runs").join(&run).is_dir());
    let runs = fs::read_dir(project.join(".apb/runs"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(runs, 0, "a scenario run left a run behind");

    let listed = call(
        &mut stdin,
        4,
        "playbook_scenarios",
        serde_json::json!({ "id": "demo-report" }),
    );
    let latest = &listed["scenarios"][0]["latest"][0];
    assert_eq!(
        (&latest["verdict"], &latest["fresh"], &latest["run_id"]),
        (
            &serde_json::json!("passed"),
            &serde_json::json!(true),
            &serde_json::json!(run)
        ),
        "{listed}"
    );

    let judged = call(
        &mut stdin,
        5,
        "judge_evaluate",
        serde_json::json!({
            "id": "task-check-first",
            "node": "check",
            "inputs": { "instruction": "Fix the parser" },
            "answers": { "sufficient": { "value": "clear", "p": 0.9, "confidence": 0.9 } },
        }),
    );
    assert_eq!(
        judged["evaluation"]["at_enforce"]["output"]["sufficient"], "clear",
        "{judged}"
    );
    assert_eq!(
        judged["route_at_enforce"],
        serde_json::json!(["e:check>work#1"])
    );
    assert_eq!(judged["evaluation"]["requests"], 0);
    let _ = child.kill();
    let _ = child.wait();
}
