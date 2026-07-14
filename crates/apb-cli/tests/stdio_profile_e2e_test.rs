#![cfg(unix)]
//! End-to-end stdio-MCP e2e (completion-plan Task 13): a real `apb mcp`
//! server, real tool schemas, and the server-side policy-to-permit handoff
//! (not in-process Rust calls). profile_write -> playbook_create ->
//! playbook_approve -> playbook_run against a stub agent -> Succeeded. Then
//! the skill is edited on disk -> a second playbook_run without
//! acknowledge_untrusted is rejected by the
//! `untrusted_profile_requires_acknowledge` gate.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const PLAYBOOK: &str = r#"
schema: 1
id: p
name: P
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: t, type: agent_task, prompt: "do", profile: arch }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: t }
  - { from: t, to: done }
"#;

fn make_stub(dir: &std::path::Path) -> String {
    let path = dir.join("stub.sh");
    fs::write(&path, "#!/bin/sh\necho done\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().to_string()
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: i64,
}

impl Server {
    /// Sends tools/call and returns the first response line with this id.
    fn call(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        });
        writeln!(self.stdin, "{req}").unwrap();
        self.stdin.flush().unwrap();
        let needle = format!("\"id\":{id}");
        for _ in 0..40 {
            let line = self
                .rx
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("no response to {name} (id {id})"));
            if line.contains(&needle) {
                return serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("bad json for {name}: {e}: {line}"));
            }
        }
        panic!("no matching response for {name} (id {id})");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Extracts the text of the first content block of a tool-call result.
fn result_text(resp: &serde_json::Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn stdio_profile_write_run_then_skill_edit_refuses() {
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let stub = make_stub(bin.path());

    // init + seed skill.
    Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("init")
        .current_dir(proj.path())
        .output()
        .unwrap();
    let skill = proj.path().join(".agents/skills/cs");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "v1").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("mcp")
        .current_dir(proj.path())
        .env("APB_AGENT_CMD", &stub)
        .env("HOME", home.path())
        .env("APB_CONFIG_DIR", cfg.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
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

    // Handshake.
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();
    rx.recv_timeout(Duration::from_secs(10))
        .expect("initialize response");
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut srv = Server {
        child,
        stdin,
        rx,
        next_id: 100,
    };

    // profile_write: creates the arch profile (skill cs) and auto-approves its bundle.
    let r = srv.call(
        "profile_write",
        serde_json::json!({
            "name": "arch", "scope": "project", "agent": "claude", "model": "haiku",
            "soul_md": "You are the architect.", "skills": ["cs"],
        }),
    );
    assert!(
        result_text(&r).contains("bundle_digest"),
        "profile_write result: {r}"
    );

    // playbook_create referencing the profile.
    srv.call(
        "playbook_create",
        serde_json::json!({ "id": "p", "yaml": PLAYBOOK }),
    );
    // playbook_approve: activate + trust the digest.
    srv.call(
        "playbook_approve",
        serde_json::json!({ "id": "p", "scope": "project" }),
    );

    // playbook_run against the stub -> Succeeded.
    let r = srv.call("playbook_run", serde_json::json!({ "id": "p" }));
    let text = result_text(&r);
    assert!(
        text.contains("succeeded") || text.contains("Succeeded"),
        "run must succeed: {text}"
    );

    // Editing the skill on disk changes the bundle -> the second run without
    // acknowledge is rejected by the gate.
    fs::write(proj.path().join(".agents/skills/cs/SKILL.md"), "v2 changed").unwrap();
    let r = srv.call("playbook_run", serde_json::json!({ "id": "p" }));
    let text = result_text(&r);
    assert!(
        text.contains("untrusted_profile_requires_acknowledge"),
        "edited skill must be refused by the gate: {text}"
    );
}
