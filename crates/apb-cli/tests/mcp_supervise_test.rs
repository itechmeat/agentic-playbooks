//! End-to-end test: `apb mcp` over stdio starts a playbook in supervised mode
//! (`playbook_run` with `supervise: "self"`) and returns a supervisor token
//! that the supervisor tools (here - `supervisor_run_inspect`) actually
//! resolve to the background run through a real JSON-RPC call.
//!
//! Modeled on `mcp_cli_test.rs`: the same handshake (initialize ->
//! notifications/initialized), the same technique with a background
//! reader thread and an mpsc channel to bound the blocking read with a
//! timeout.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

// The same NOAGENT playbook without agent_task as in mcp_cli_test.rs: with no
// executable agent, the run deterministically reaches succeeded, so the test
// does not depend on an external coding-agent process.
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
fn supervise_self_over_stdio_mints_token_and_inspect_resolves_it() {
    let dir = tempfile::tempdir().unwrap();

    // init + seed the same minimal playbook without agent_task.
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

    // The same technique as in mcp_cli_test.rs: a separate thread reads lines
    // and sends them through a channel, so the main thread can bound the
    // wait with a timeout instead of an uncancelable blocking read_line.
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

    // 1) initialize.
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

    // 2) notifications/initialized.
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    writeln!(stdin, "{initialized}").unwrap();
    stdin.flush().unwrap();

    // 3) tools/call playbook_run with supervise: "self" - starts a
    // background run supervised by this same (test) session and must return
    // supervisor_token and run_id in the result body.
    // acknowledge_untrusted: the playbook was seeded directly (untrusted),
    // and the server-side gate now also applies to supervise:self - so we
    // acknowledge it explicitly.
    let run_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"playbook_run","arguments":{"id":"noagent","params":{"who":"world"},"supervise":"self","acknowledge_untrusted":true}}}"#;
    writeln!(stdin, "{run_req}").unwrap();
    stdin.flush().unwrap();

    let run_line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("no response to playbook_run(supervise:self) within timeout");
    assert!(
        !run_line.contains("\"isError\":true"),
        "playbook_run(supervise:self) returned an error: {run_line}"
    );
    assert!(
        run_line.contains("supervisor_token") && run_line.contains("run_id"),
        "playbook_run(supervise:self) response missing supervisor_token/run_id: {run_line}"
    );

    // Extract the supervisor_token: the tools/call body arrives as double
    // JSON encoding - an outer JSON-RPC response, inside which
    // result.content[0].text is a JSON string holding the tool's own
    // payload.
    let token = extract_supervisor_token(&run_line);

    // 4) tools/call supervisor_run_inspect with this token - proves that the
    // token actually resolves to the background run through a real stdio
    // MCP connection.
    let inspect_req = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"supervisor_run_inspect","arguments":{{"token":"{token}"}}}}}}"#
    );
    writeln!(stdin, "{inspect_req}").unwrap();
    stdin.flush().unwrap();

    let inspect_line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("no response to supervisor_run_inspect within timeout");

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        !inspect_line.contains("\"isError\":true"),
        "supervisor_run_inspect returned an error: {inspect_line}"
    );
    assert!(
        inspect_line.contains("run_status"),
        "supervisor_run_inspect response missing run_status: {inspect_line}"
    );
}

/// Extracts the `supervisor_token` from a JSON-RPC response line for
/// `tools/call`.
///
/// The primary path is a proper double decode: parse the whole line as JSON,
/// reach `result.content[0].text` (a JSON string holding the tool's body),
/// parse that too, and take the `supervisor_token` field. If for some reason
/// the response shape does not match expectations (e.g. escaping), the
/// fallback path extracts the substring `sv-<number>-<number>` directly from
/// the raw line: the token format is defined by the server itself
/// (`format!("sv-{millis}-{n}")` in `WfMcp::mint_token`), so the substring is
/// enough to get a working token for the next call.
fn extract_supervisor_token(line: &str) -> String {
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
    if let Ok(v) = parsed
        && let Some(text) = v["result"]["content"][0]["text"].as_str()
        && let Ok(inner) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(token) = inner["supervisor_token"].as_str()
    {
        return token.to_string();
    }

    // Fallback path: raw substring search for "sv-<millis>-<n>".
    let idx = line
        .find("sv-")
        .unwrap_or_else(|| panic!("no supervisor token found in: {line}"));
    let rest = &line[idx..];
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-' || c.is_ascii_alphabetic())
        .collect();
    token
}
