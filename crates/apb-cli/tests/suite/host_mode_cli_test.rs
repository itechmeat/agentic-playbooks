//! Host execution mode (0.23.0) through the binary: `apb run --execution
//! host`, `apb tasks` list and submit, resume around a dead driver, and a
//! whole run driven by a stub MCP client over stdio that acts as the host.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_engine::event::{EventPayload, read_all};
use serde_json::{Value, json};

const TWO_NODES: &str = r#"
schema: 1
id: hm
name: Host mode
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: plan, type: agent_task, prompt: "Plan it" }
  - { id: build, type: agent_task, prompt: "Build from {{nodes.plan.output}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: plan }
  - { from: plan, to: build }
  - { from: build, to: done }
"#;

fn seed(root: &Path) {
    seed_with(root, TWO_NODES);
}

fn seed_with(root: &Path, playbook: &str) {
    crate::common::apb_std()
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let v = root.join(".apb/playbooks/hm/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), playbook).unwrap();
    fs::write(root.join(".apb/playbooks/hm/current"), "1.0.0").unwrap();
    let p = root.join(".apb/profiles/main");
    fs::create_dir_all(&p).unwrap();
    // An agent that does not exist: host mode must never try to spawn it.
    fs::write(
        p.join("profile.yaml"),
        "name: main\ndescription: d\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(p.join("SOUL.md"), "You plan and build.").unwrap();
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_dir(root: &Path, run_id: &str) -> PathBuf {
    root.join(".apb/runs").join(run_id)
}

fn tasks_json(root: &Path, run_id: &str) -> Vec<Value> {
    let out = crate::common::apb_std()
        .args(["tasks", run_id, "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<Vec<Value>>(&out.stdout).unwrap()
}

fn task_of(root: &Path, run_id: &str, node: &str) -> Value {
    poll(&format!("a host task of {node}"), || {
        tasks_json(root, run_id)
            .into_iter()
            .find(|t| t["node"] == node)
    })
}

fn submit(root: &Path, run_id: &str, task_id: &str, reply: &str) {
    let file = root.join(format!("reply-{task_id}.md"));
    fs::write(&file, reply).unwrap();
    let out = crate::common::apb_std()
        .args(["tasks", "submit", run_id, task_id, "--status", "succeeded"])
        .arg("--output-file")
        .arg(&file)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "submit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn start_detached(root: &Path) -> String {
    let out = crate::common::apb_std()
        .args(["run", "hm", "--execution", "host", "--detach"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("apb tasks"),
        "names where tasks wait: {stdout}"
    );
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string()
}

fn wait_outcome(root: &Path, run_id: &str) -> String {
    let dir = run_dir(root, run_id);
    poll("the run to finish", || {
        read_all(&dir)
            .ok()?
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                EventPayload::RunFinished { outcome } => Some(outcome.clone()),
                _ => None,
            })
    })
}

fn requested_ids(root: &Path, run_id: &str) -> Vec<String> {
    read_all(&run_dir(root, run_id))
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::HostTaskRequested { task_id, .. } => Some(task_id),
            _ => None,
        })
        .collect()
}

#[test]
fn apb_tasks_lists_and_submits_until_the_run_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let run_id = start_detached(root);
    let plan = task_of(root, &run_id, "plan");
    assert_eq!(plan["role_prompt"], "You plan and build.");
    // The human-readable listing names the task and how to answer it.
    let listing = crate::common::apb_std()
        .args(["tasks"])
        .current_dir(root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&listing.stdout);
    assert!(text.contains(plan["task_id"].as_str().unwrap()), "{text}");
    assert!(text.contains("apb tasks submit"), "{text}");
    submit(root, &run_id, plan["task_id"].as_str().unwrap(), "PLAN-7");
    let build = task_of(root, &run_id, "build");
    assert!(
        build["prompt"]
            .as_str()
            .unwrap()
            .contains("Build from PLAN-7")
    );
    submit(root, &run_id, build["task_id"].as_str().unwrap(), "built");
    assert_eq!(wait_outcome(root, &run_id), "succeeded");
    // A second submission of a closed task is refused.
    let again = crate::common::apb_std()
        .args([
            "tasks",
            "submit",
            &run_id,
            plan["task_id"].as_str().unwrap(),
        ])
        .args(["--status", "succeeded", "--output-file", "-"])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!again.status.success());
}

fn kill_driver(root: &Path, run_id: &str) {
    let dir = run_dir(root, run_id);
    let pid = poll("driver.pid", || apb_engine::driver::read_driver_pid(&dir));
    crate::common::sig::kill_pid(pid);
    poll("the driver to die", || {
        (!apb_engine::liveness::pid_is_live(pid)).then_some(())
    });
}

fn resume(root: &Path, run_id: &str) {
    let out = crate::common::apb_std()
        .args(["resume", run_id])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "resume failed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_resume_before_submission_re_exposes_the_same_task() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let run_id = start_detached(root);
    let plan = task_of(root, &run_id, "plan");
    let id = plan["task_id"].as_str().unwrap().to_string();
    kill_driver(root, &run_id);
    resume(root, &run_id);
    // The new driver adopted the open task: same id, no second one.
    poll("the adopted request", || {
        (requested_ids(root, &run_id).len() == 2).then_some(())
    });
    assert_eq!(requested_ids(root, &run_id), vec![id.clone(), id.clone()]);
    assert_eq!(task_of(root, &run_id, "plan")["task_id"], id.as_str());
    submit(root, &run_id, &id, "PLAN");
    let build = task_of(root, &run_id, "build");
    submit(root, &run_id, build["task_id"].as_str().unwrap(), "built");
    assert_eq!(wait_outcome(root, &run_id), "succeeded");
}

#[test]
fn a_resume_after_submission_consumes_it_without_a_new_task() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let run_id = start_detached(root);
    let plan = task_of(root, &run_id, "plan");
    let id = plan["task_id"].as_str().unwrap().to_string();
    kill_driver(root, &run_id);
    // The host submits while no driver runs.
    submit(root, &run_id, &id, "PLAN-OFFLINE");
    resume(root, &run_id);
    let build = task_of(root, &run_id, "build");
    assert!(
        build["prompt"]
            .as_str()
            .unwrap()
            .contains("Build from PLAN-OFFLINE")
    );
    let plan_tasks: Vec<String> = requested_ids(root, &run_id)
        .into_iter()
        .filter(|t| t.starts_with("plan-"))
        .collect();
    assert!(
        plan_tasks.iter().all(|t| *t == id),
        "no new plan task: {plan_tasks:?}"
    );
    submit(root, &run_id, build["task_id"].as_str().unwrap(), "built");
    assert_eq!(wait_outcome(root, &run_id), "succeeded");
}

/// Resumed with `require_verdict`, the attempt's prompt changes (the
/// interruption note, the status-file path of a restarted attempt counter):
/// the open task is still the one re-exposed, alone.
#[test]
fn a_require_verdict_resume_re_exposes_only_the_open_task() {
    for submit_offline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed_with(
            root,
            &TWO_NODES.replace(
                "prompt: \"Plan it\" }",
                "prompt: \"Plan it\", require_verdict: true }",
            ),
        );
        let run_id = start_detached(root);
        let plan = task_of(root, &run_id, "plan");
        let id = plan["task_id"].as_str().unwrap().to_string();
        kill_driver(root, &run_id);
        if submit_offline {
            submit(root, &run_id, &id, "PLAN-OFFLINE");
        }
        resume(root, &run_id);
        let build = if submit_offline {
            task_of(root, &run_id, "build")
        } else {
            poll("the adopted request", || {
                (requested_ids(root, &run_id).len() == 2).then_some(())
            });
            let pending: Vec<String> = tasks_json(root, &run_id)
                .iter()
                .map(|t| t["task_id"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(pending, vec![id.clone()], "only the open task");
            submit(root, &run_id, &id, "PLAN-LATER");
            task_of(root, &run_id, "build")
        };
        let expected = if submit_offline {
            "Build from PLAN-OFFLINE"
        } else {
            "Build from PLAN-LATER"
        };
        assert!(
            build["prompt"].as_str().unwrap().contains(expected),
            "{}",
            build["prompt"]
        );
        let plan_tasks: Vec<String> = requested_ids(root, &run_id)
            .into_iter()
            .filter(|t| t.starts_with("plan-"))
            .collect();
        assert!(
            plan_tasks.iter().all(|t| *t == id),
            "no new plan task: {plan_tasks:?}"
        );
        submit(root, &run_id, build["task_id"].as_str().unwrap(), "built");
        assert_eq!(wait_outcome(root, &run_id), "succeeded");
    }
}

#[test]
fn the_kill_switch_over_execution_host_is_said_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    // The forced cli run spawns the profile's CLI: a stub that answers.
    let agent = root.join("agent.sh");
    fs::write(&agent, "#!/bin/sh\necho ok\n").unwrap();
    let mut perm = fs::metadata(&agent).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&agent, perm).unwrap();
    let out = crate::common::apb_std()
        .args(["run", "hm", "--execution", "host"])
        .env("APB_EXECUTION", "cli")
        .env("APB_AGENT_CMD", &agent)
        .current_dir(root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("execution: host was requested")
            && stderr.contains("APB_EXECUTION=cli forces cli"),
        "{stderr}"
    );
}

#[test]
fn doctor_states_the_execution_mode() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let out = crate::common::apb_std()
        .arg("doctor")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("execution") && text.contains("cli by default"),
        "{text}"
    );
}

// --- A stub MCP client as the host ------------------------------------------

struct Mcp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    rx: mpsc::Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(root: &Path) -> Self {
        let mut child = crate::common::apb_std()
            .arg("mcp")
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
            json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"stub-host","version":"0"}}),
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

    /// Calls a tool and returns its JSON payload.
    fn call(&mut self, name: &str, args: Value) -> Value {
        let v = self.request("tools/call", json!({"name": name, "arguments": args}));
        let result = &v["result"];
        assert_ne!(result["isError"], true, "{name} failed: {v}");
        if !result["structuredContent"].is_null() {
            return result["structuredContent"].clone();
        }
        let text = result["content"][0]["text"].as_str().expect("text content");
        serde_json::from_str(text).unwrap()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_stub_mcp_host_drives_a_host_mode_run_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let mut mcp = Mcp::start(root);
    let tools = mcp.request("tools/list", json!({}));
    let list = tools["result"]["tools"].as_array().unwrap();
    let submit_tool = list
        .iter()
        .find(|t| t["name"] == "run_task_submit")
        .expect("run_task_submit is served");
    assert!(
        submit_tool["description"]
            .as_str()
            .unwrap()
            .contains("pending_tasks")
    );
    let run_tool = list.iter().find(|t| t["name"] == "playbook_run").unwrap();
    assert!(
        run_tool["description"]
            .as_str()
            .unwrap()
            .contains("execution: \"host\"")
    );
    let started = mcp.call(
        "playbook_run",
        json!({"id": "hm", "execution": "host", "acknowledge_untrusted": true}),
    );
    assert_eq!(started["execution"]["mode"], "host", "{started}");
    assert!(
        started["next"]
            .as_str()
            .unwrap()
            .contains("run_task_submit")
    );
    let run_id = started["run_id"].as_str().unwrap().to_string();
    let mut submitted = Vec::new();
    loop {
        let waited = mcp.call("run_wait", json!({"run_id": run_id, "timeout_ms": 30000}));
        if waited["reason"] == "finished" {
            assert_eq!(waited["run_status"], "succeeded", "{waited}");
            break;
        }
        assert_eq!(waited["reason"], "needs_input", "{waited}");
        assert_eq!(waited["needs"], "host_task", "{waited}");
        let tasks = waited["pending_tasks"].as_array().expect("pending_tasks");
        assert!(
            waited["next"].as_str().unwrap().contains("run_task_submit"),
            "{waited}"
        );
        for t in tasks {
            let node = t["node"].as_str().unwrap();
            let reply = format!("{node} done\n\n```yaml\nstatus: success\nsummary: ok\n```");
            let r = mcp.call(
                "run_task_submit",
                json!({
                    "run_id": run_id,
                    "task_id": t["task_id"],
                    "status": "succeeded",
                    "output": reply,
                    "usage": {"input_tokens": 3, "output_tokens": 2},
                }),
            );
            assert_eq!(r["status"], "succeeded");
            submitted.push(node.to_string());
        }
        assert!(submitted.len() <= 2, "{submitted:?}");
    }
    assert_eq!(submitted, vec!["plan", "build"]);
    let status = mcp.call("run_status", json!({"run_id": run_id}));
    assert_eq!(status["execution"]["mode"], "host");
    assert!(status.get("pending_tasks").is_none(), "{status}");
    let events = read_all(&run_dir(root, &run_id)).unwrap();
    let clients: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::HostTaskSubmitted {
                client,
                submitted_by,
                ..
            } if submitted_by == "host" => Some(client.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        clients,
        vec![Some("stub-host".to_string()), Some("stub-host".to_string())]
    );
}
