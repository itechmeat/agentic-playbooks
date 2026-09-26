//! End-to-end tests for the waits that let an agent hand a run off and spend
//! nothing until it has something to act on: `apb run --detach` + `apb wait`
//! on the CLI, and the `run_wait` MCP tool (with progress notifications) over
//! a real `apb mcp` stdio session.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const WF_REVIEW: &str = r#"
schema: 1
id: rev
name: Review
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

fn seed(root: &Path) {
    let out = crate::common::apb_std()
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = root.join(".apb/playbooks/rev/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF_REVIEW).unwrap();
    fs::write(root.join(".apb/playbooks/rev/current"), "1.0.0").unwrap();
}

fn apb_in(root: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn detach_then_wait_reports_the_gate_and_the_outcome_by_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let out = apb_in(dir.path(), &["run", "rev", "--detach"]);
    assert!(out.status.success(), "run --detach failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .trim()
        .strip_prefix("run started: ")
        .unwrap_or_else(|| panic!("unexpected output: {stdout}"))
        .to_string();
    let _guard = crate::common::RunGuard::new(dir.path(), &run_id);

    let out = apb_in(dir.path(), &["wait", &run_id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(3), "needs input: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("human review is pending"));

    let out = apb_in(
        dir.path(),
        &["review", &run_id, "gate", "--decision", "approved"],
    );
    assert!(out.status.success(), "review failed: {out:?}");

    let out = apb_in(dir.path(), &["wait", &run_id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(0), "succeeded: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("finished: succeeded"));

    // A finished run answers at once, and an unknown one is an error.
    let out = apb_in(dir.path(), &["wait", &run_id]);
    assert_eq!(out.status.code(), Some(0));
    let out = apb_in(dir.path(), &["wait", "no-such-run", "--timeout", "1"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn wait_times_out_with_its_own_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_dir = dir.path().join(".apb/runs/r-live");
    fs::create_dir_all(&run_dir).unwrap();
    // A run that started and is still going, with no gate: only the timeout
    // can end the wait.
    let mut log = apb_engine::event::EventLog::create(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "rev".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    let started = Instant::now();
    let out = apb_in(dir.path(), &["wait", "r-live", "--timeout", "1"]);
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

fn line_reader(stdout: std::process::ChildStdout) -> Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

fn wait_for(rx: &Receiver<String>, what: &str, mut pred: impl FnMut(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) if pred(&line) => return line,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for: {what}"),
        }
    }
}

struct Reaper(std::process::Child);
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn mcp_run_wait_blocks_with_progress_and_returns_compact_results() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let mut child = Reaper(
        crate::common::apb_std()
            .arg("mcp")
            .current_dir(dir.path())
            // Slice the wait finely so the keep-alive shows up in milliseconds.
            .env("APB_WAIT_SLICE_MS", "50")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stdin = child.0.stdin.take().unwrap();
    let rx = line_reader(child.0.stdout.take().unwrap());
    let mut send = |s: &str| {
        writeln!(stdin, "{s}").unwrap();
        stdin.flush().unwrap();
    };

    send(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
    );
    wait_for(&rx, "initialize", |l| l.contains("\"id\":1"));
    send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    send(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"playbook_run","arguments":{"id":"rev","background":true,"acknowledge_untrusted":true}}}"#,
    );
    let line = wait_for(&rx, "playbook_run", |l| l.contains("\"id\":2"));
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let run_id = body["run_id"].as_str().expect("run_id").to_string();
    let _guard = crate::common::RunGuard::new(dir.path(), &run_id);

    // One call waits out the gate's grace (several slices), sending progress
    // notifications meanwhile, and returns needs_input with the gate.
    send(&format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"run_wait","arguments":{{"run_id":"{run_id}","timeout_ms":20000}},"_meta":{{"progressToken":"t3"}}}}}}"#
    ));
    let mut progress = 0;
    let line = wait_for(&rx, "run_wait needs_input", |l| {
        if l.contains("notifications/progress") && l.contains("t3") {
            progress += 1;
        }
        l.contains("\"id\":3")
    });
    assert!(progress >= 1, "expected progress notifications");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["reason"], "needs_input", "{body}");
    assert_eq!(body["pending_review"]["node"], "gate", "{body}");
    assert!(body.get("outputs").is_none(), "compact: no outputs: {body}");

    send(&format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"review_decide","arguments":{{"run_id":"{run_id}","node":"gate","decision":"approved","note":""}}}}}}"#
    ));
    wait_for(&rx, "review_decide", |l| l.contains("\"id\":4"));

    send(&format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{{"name":"run_wait","arguments":{{"run_id":"{run_id}","timeout_ms":20000}}}}}}"#
    ));
    let line = wait_for(&rx, "run_wait finished", |l| l.contains("\"id\":5"));
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["reason"], "finished", "{body}");
    assert_eq!(body["run_status"], "succeeded", "{body}");
    assert!(body.get("pending_review").is_none(), "{body}");
}
