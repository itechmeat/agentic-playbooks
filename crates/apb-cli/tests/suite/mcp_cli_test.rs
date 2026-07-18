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
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

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
    Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let vdir = dir.path().join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(dir.path().join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("mcp")
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

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
