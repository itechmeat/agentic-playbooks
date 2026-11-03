//! Evidence sessions through the shipped entry points: `apb evidence read`
//! and MCP `run_evidence_read` over stdio give equal answers and budgets, a
//! read after the attempt ended is refused on both, a read-only server
//! serves no `run_evidence_read`, `apb results` shows each claim with its
//! fixed wording and fragment, and a host task carries the session in its
//! prompt and reads through MCP.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const MAIL: &str = r#"{"apb_records": 1, "complete": true, "source": "desk inbox"}
{"id": "m-1", "kind": "email", "text": "The customer reports that the invoice for October is late."}
{"id": "m-2", "kind": "email", "text": "A refund was promised on the phone."}
"#;

const PLAYBOOK: &str = r#"schema: 2
id: ev
name: Evidence
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: collect, type: script, script: scripts/collect.sh, runner: sh, results: [ { path: mail.jsonl, format: records } ] }
  - { id: use, type: agent_task, prompt: "answer from the sources", evidence: { from: [collect], kinds: [email] } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: collect }
  - { from: collect, to: use }
  - { from: use, to: done }
"#;

fn seed(root: &Path) {
    let init = crate::common::apb_std()
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(init.status.success());
    let v = root.join(".apb/playbooks/ev/1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/ev/current"), "1.0.0").unwrap();
    fs::write(
        v.join("scripts/collect.sh"),
        format!("cat > mail.jsonl <<'APB_EOF'\n{MAIL}APB_EOF\necho collected\n"),
    )
    .unwrap();
    let p = root.join(".apb/profiles/main");
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("profile.yaml"),
        "name: main\ndescription: d\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(p.join("SOUL.md"), "You answer.").unwrap();
    let r = root.display();
    let agent = root.join("agent.sh");
    fs::write(
        &agent,
        format!(
            "#!/bin/sh\nprintf '%s' \"$APB_EVIDENCE_SESSION\" > \"{r}/session.tmp\"\nmv \"{r}/session.tmp\" \"{r}/session\"\nwhile [ ! -f \"{r}/go\" ]; do sleep 0.05; done\ncat \"{r}/status.json\" > \"$APB_STATUS_FILE\"\necho done\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_dir(root: &Path, handle: &str) -> PathBuf {
    root.join(".apb/runs")
        .join(handle.rsplit_once(':').unwrap().0)
}

/// `apb evidence read` with `args`: its JSON, or its stderr on a failure.
fn cli(root: &Path, handle: &str, args: &[&str]) -> Result<Value, String> {
    let out = crate::common::apb_std()
        .args(["evidence", "read", "--session", handle])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    if out.status.success() {
        Ok(serde_json::from_slice(&out.stdout).unwrap())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// A stdio MCP client of `apb mcp` (with `extra` arguments).
struct Mcp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    rx: mpsc::Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(root: &Path, extra: &[&str]) -> Self {
        let mut child = crate::common::apb_std()
            .arg("mcp")
            .args(extra)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                if tx.send(line.clone()).is_err() {
                    break;
                }
                line.clear();
            }
        });
        let mut m = Mcp {
            child,
            stdin,
            rx,
            next_id: 1,
        };
        m.request(
            "initialize",
            json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"stub","version":"0"}}),
        );
        writeln!(
            m.stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        m
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let line = self
                .rx
                .recv_timeout(Duration::from_secs(60))
                .expect("an MCP response");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    /// A tool's JSON payload, or its error text.
    fn call(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let v = self.request("tools/call", json!({"name": name, "arguments": args}));
        if !v["error"].is_null() {
            return Err(v["error"].to_string());
        }
        let result = &v["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        if result["isError"] == true {
            return Err(text.to_string());
        }
        if !result["structuredContent"].is_null() {
            return Ok(result["structuredContent"].clone());
        }
        Ok(serde_json::from_str(text).unwrap())
    }

    fn read(&mut self, handle: &str, args: Value) -> Result<Value, String> {
        let mut a = args;
        a["session"] = json!(handle);
        self.call("run_evidence_read", a)
    }

    fn tool_names(&mut self) -> Vec<String> {
        self.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The package view of the emails in an outline answer.
fn email_view(outline: &Value) -> String {
    outline["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["ref"].as_str())
        .find(|r| r.ends_with("~email"))
        .expect("the email view")
        .trim_start_matches("n:")
        .to_string()
}

#[test]
fn the_cli_and_mcp_read_alike_and_both_refuse_once_the_attempt_ended() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let mut run = crate::common::apb_std()
        .args(["run", "ev"])
        .env("APB_AGENT_CMD", root.join("agent.sh"))
        .env("APB_TEST_BILLING_DEFAULT", "allow")
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let handle = poll("the attempt's session", || {
        fs::read_to_string(root.join("session")).ok()
    });
    let rd = run_dir(root, &handle);
    let mut mcp = Mcp::start(root, &[]);
    // The outline: the same views, nothing charged.
    let a = cli(root, &handle, &["outline"]).unwrap();
    let b = mcp.read(&handle, json!({ "op": "outline" })).unwrap();
    assert_eq!(a["items"], b["items"]);
    assert_eq!(a["read_chars"], b["read_chars"]);
    let view = email_view(&a);
    let listed = cli(root, &handle, &["children", "--view", &view]).unwrap();
    let m1 = listed["items"][0]["ref"].as_str().unwrap().to_string();
    // A whole read through each surface: equal items, and each charges the
    // same characters to the one budget.
    let before = listed["read_chars"].as_u64().unwrap();
    let a = cli(root, &handle, &["read", "--ref", &m1]).unwrap();
    let after_cli = a["read_chars"].as_u64().unwrap();
    let b = mcp
        .read(&handle, json!({ "op": "read", "ref": m1 }))
        .unwrap();
    let after_mcp = b["read_chars"].as_u64().unwrap();
    assert_eq!(a["items"], b["items"]);
    assert_eq!(a["items"][0]["citable"], true);
    assert_eq!(after_cli - before, after_mcp - after_cli);
    assert_eq!(a["budget_chars"], b["budget_chars"]);
    let a = cli(root, &handle, &["search", "--phrase", "refund"]).unwrap();
    let b = mcp
        .read(&handle, json!({ "op": "search", "phrase": "refund" }))
        .unwrap();
    assert_eq!(a["items"], b["items"]);
    assert_eq!(a["items"][0]["citable"], false);
    // A step of another run is refused.
    let out = crate::common::apb_std()
        .args(["evidence", "read", "--session", &handle, "outline"])
        .env("APB_RUN_ID", "other-run-1")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("run_step_other_run"));
    fs::write(
        root.join("status.json"),
        json!({ "status": "success", "outputs": { "claims": [
            { "id": "c1", "text": "The October invoice is late", "cites": [{ "ref": m1, "span": [26, 57] }] },
            { "id": "c2", "text": "The customer wrote", "cites": [{ "ref": m1, "span": [4, 12] }] }
        ] } })
        .to_string(),
    )
    .unwrap();
    fs::write(root.join("go"), "").unwrap();
    assert!(run.wait().unwrap().success());
    // Both surfaces refuse once the attempt ended.
    let late = cli(root, &handle, &["outline"]).unwrap_err();
    assert!(late.contains("is closed"), "{late}");
    let late = mcp.read(&handle, json!({ "op": "outline" })).unwrap_err();
    assert!(late.contains("is closed"), "{late}");
    // `apb results` shows the claims, the wording and the fragment.
    let run_id = rd.file_name().unwrap().to_string_lossy().into_owned();
    let out = crate::common::apb_std()
        .args(["results", &run_id])
        .current_dir(root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(apb_engine::evidence::STRUCTURALLY_VALID),
        "{text}"
    );
    assert!(
        text.contains("\"the invoice for October is late\""),
        "{text}"
    );
    assert!(text.contains("worth a look: short_span"), "{text}");
    let listed = mcp
        .call("run_results", json!({ "run_id": run_id }))
        .unwrap();
    let claims = &listed["evidence"][0]["claims"];
    assert_eq!(claims[0]["verdict"], "structurally_valid");
    assert_eq!(claims[1]["flags"], json!(["short_span"]));
    // A read-only server serves no `run_evidence_read`, listed or called.
    let mut ro = Mcp::start(root, &["--read-only"]);
    assert!(!ro.tool_names().iter().any(|t| t == "run_evidence_read"));
    assert!(ro.read(&handle, json!({ "op": "outline" })).is_err());
}

#[test]
fn a_host_task_carries_its_session_and_the_host_reads_through_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let out = crate::common::apb_std()
        .args(["run", "ev", "--execution", "host", "--detach"])
        .env("APB_TEST_BILLING_DEFAULT", "allow")
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}");
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let _guard = crate::common::RunGuard::new(root, &run_id);
    let task = poll("the host task", || {
        let out = crate::common::apb_std()
            .args(["tasks", &run_id, "--json"])
            .current_dir(root)
            .output()
            .ok()?;
        serde_json::from_slice::<Vec<Value>>(&out.stdout)
            .ok()?
            .into_iter()
            .find(|t| t["node"] == "use")
    });
    let prompt = task["prompt"].as_str().unwrap();
    let handle = prompt
        .split("APB_EVIDENCE_SESSION=")
        .nth(1)
        .and_then(|rest| rest.split([',', '\n', ' ']).next())
        .unwrap_or_else(|| panic!("the prompt names the session: {prompt}"))
        .to_string();
    assert!(prompt.contains("## evidence") && prompt.contains("run_evidence_read"));
    let mut mcp = Mcp::start(root, &[]);
    let outline = mcp.read(&handle, json!({ "op": "outline" })).unwrap();
    let view = email_view(&outline);
    let page = mcp
        .read(&handle, json!({ "op": "page", "view": view }))
        .unwrap();
    let m1 = page["items"][0]["ref"].as_str().unwrap().to_string();
    let reply = json!({ "claims": [
        { "id": "c1", "text": "The October invoice is late", "cites": [{ "ref": m1, "span": [26, 57] }] }
    ] })
    .to_string();
    let file = root.join("reply.json");
    fs::write(&file, &reply).unwrap();
    let out = crate::common::apb_std()
        .args([
            "tasks",
            "submit",
            &run_id,
            task["task_id"].as_str().unwrap(),
        ])
        .args(["--status", "succeeded", "--output-file"])
        .arg(&file)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = poll("the checked session", || {
        let v = mcp.call("run_results", json!({ "run_id": run_id })).ok()?;
        (v["evidence"][0]["state"] == "checked").then_some(v)
    });
    assert_eq!(
        listed["evidence"][0]["claims"][0]["verdict"],
        "structurally_valid"
    );
}
