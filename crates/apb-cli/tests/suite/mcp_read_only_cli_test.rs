//! `apb mcp --read-only` end to end, from a cold state: a fresh config
//! directory (no installation key), a project with `.apb/` but no
//! workspace identity, a second registered project, a run with a live
//! driver that waits on a host task, a finished run whose secret param its
//! nodes echoed (and its child), a connector account, and playbooks with an
//! isolated node and a publish node. Both trees and the config directory
//! are snapshotted (path, bytes, mode) before the server starts; every
//! allowed tool is called over stdio; after the server exits the snapshot
//! is equal and no stub program on `PATH` (`ps`, `git`, `bwrap`, the agent
//! CLIs, the secret helper) left its marker. Then the account's secret
//! comes from a helper program, and from a variable the server lacks: the
//! server cannot resolve it, so it withholds the runs' text.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::run_secrets_cli_test::{poll, seed_host};
use crate::secret_runs::{SECRET, seed_secret_runs};

/// Programs a read-only server must never start, each a stub that leaves
/// a marker.
const STUBS: &[&str] = &[
    "ps",
    "git",
    "bwrap",
    "claude",
    "codex",
    "opencode",
    "gemini",
    "qwen",
    "zcode",
    "cursor-agent",
    "helper",
];

const ISOLATED: &str = "schema: 2\nid: iso\nname: Iso\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: do, profile: main, isolation: { level: full, network: none } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";

const PUBLISH: &str = "schema: 2\nid: pub\nname: Publish\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: \"scripts/work.sh\", runner: sh }\n  - { id: publish, type: publish, from: work, remote: origin, branch: \"apb/out\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: publish }\n  - { from: publish, to: done }\n";

const CONNECTOR: &str = "name: helped\nversion: 0.1.0\naccount_fields:\n  - name: token\n    required: true\n    secret: true\nfunctions:\n  - name: ping\n    description: d\n    read_only: true\n    mock: { status: 200, body: {} }\n";

/// Path, bytes and mode of every entry under each root.
type Snapshot = BTreeMap<PathBuf, (Vec<u8>, u32)>;

fn snapshot(roots: &[&Path]) -> Snapshot {
    use std::os::unix::fs::PermissionsExt as _;
    let mut out = Snapshot::new();
    let mut stack: Vec<PathBuf> = roots.iter().map(|r| r.to_path_buf()).collect();
    while let Some(p) = stack.pop() {
        let meta = fs::symlink_metadata(&p).unwrap();
        let mode = meta.permissions().mode();
        if meta.is_dir() {
            out.insert(p.clone(), (Vec::new(), mode));
            for e in fs::read_dir(&p).unwrap().flatten() {
                stack.push(e.path());
            }
        } else if meta.file_type().is_symlink() {
            let target = fs::read_link(&p).unwrap();
            out.insert(p, (target.to_string_lossy().as_bytes().to_vec(), mode));
        } else {
            out.insert(p.clone(), (fs::read(&p).unwrap(), mode));
        }
    }
    out
}

fn install(root: &Path, id: &str, yaml: &str) {
    let v = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), yaml).unwrap();
    fs::write(v.join("scripts/work.sh"), "true\n").unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn stub_dir(markers: &Path) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = tempfile::tempdir().unwrap();
    for name in STUBS {
        let path = bin.path().join(name);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\ntouch '{}'\necho value\n",
                markers.join(name).display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// Kills the sleeping stand-in driver however the test ends.
struct Sleeper(Child);

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A host-mode run of `hm` in `root` whose driver is gone and whose
/// `driver.pid` names a live sleeping process; it waits on a host task.
fn host_run_with_live_driver(root: &Path) -> (String, Sleeper) {
    seed_host(root);
    let out = crate::common::apb_std()
        .args(["run", "hm", "--execution", "host", "--detach"])
        .arg("--param")
        .arg(format!("message={SECRET}"))
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "the host-mode run must start");
    let run_id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let run_dir = root.join(".apb/runs").join(&run_id);
    poll("the host task", || {
        let out = crate::common::apb_std()
            .args(["tasks", &run_id, "--json"])
            .current_dir(root)
            .output()
            .ok()?;
        let tasks: Vec<Value> = serde_json::from_slice(&out.stdout).ok()?;
        (!tasks.is_empty()).then_some(())
    });
    // The real driver goes, so nothing writes while the server reads.
    let pid = apb_engine::driver::read_driver_pid(&run_dir).expect("a driver");
    crate::common::sig::kill_group(pid);
    crate::common::sig::kill_pid(pid);
    poll("the driver to end", || {
        (!crate::common::sig::alive(pid)).then_some(())
    });
    let sleeper = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    fs::write(run_dir.join("driver.pid"), sleeper.id().to_string()).unwrap();
    (run_id, Sleeper(sleeper))
}

/// A stdio MCP session with `apb mcp --read-only`.
struct Session {
    /// Killed and reaped however the test ends.
    child: Killed,
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
    next_id: u64,
}

impl Session {
    fn start(dir: &Path, env: &[(&str, &Path)]) -> Self {
        let mut cmd = crate::common::apb_std();
        cmd.args(["mcp", "--read-only"])
            .current_dir(dir)
            .env_remove("CI")
            .env_remove("APB_NO_REGISTRY")
            .env(
                apb_engine::adapter::MCP_ROLE_ENV,
                apb_engine::adapter::MCP_ROLE_SUPERVISOR,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut s = Session {
            child: Killed(child),
            stdin,
            rx,
            next_id: 1,
        };
        s.request(
            "initialize",
            json!({ "protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": { "name": "t", "version": "0" } }),
        );
        writeln!(
            s.stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        s
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let line = self
                .rx
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("no answer to {method} within 60s"));
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// The JSON answer of an allowed tool; panics on a protocol error.
    fn call(&mut self, tool: &str, args: Value) -> Value {
        let answer = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        assert!(answer.get("error").is_none(), "{tool}: {answer}");
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }

    /// Closes stdin and waits for the server to exit; one that does not is
    /// killed when the session drops.
    fn close(self) {
        let Session {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if child.0.try_wait().unwrap().is_some() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the server did not exit"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A server process killed and reaped however the test ends.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unchanged(before: &Snapshot, after: &Snapshot) {
    if before != after {
        let changed: Vec<&PathBuf> = before
            .keys()
            .chain(after.keys())
            .filter(|p| before.get(*p) != after.get(*p))
            .collect();
        panic!("the read-only session changed {changed:?}");
    }
}

/// Every pending task of `v` (at `key`) carries no prompt and says why.
fn withheld(v: &Value) {
    let tasks = v.as_array().expect("pending tasks");
    assert!(!tasks.is_empty());
    for t in tasks {
        assert_eq!(t["prompt_withheld"], "read_only", "{t}");
        assert_eq!(t["prompt"], "", "{t}");
        assert!(t.get("role_prompt").is_none_or(Value::is_null), "{t}");
        assert_eq!(t["prompt_path"], "", "{t}");
        assert!(t["task_id"].is_string() && t["node"].is_string(), "{t}");
    }
}

/// Every action that changes the run is not available to the caller.
fn no_mutating_action(v: &Value) {
    let actions = v["next_actions"].as_array().expect("next_actions");
    for a in actions {
        let reads = matches!(a["tool"].as_str(), Some("run_wait" | "run_report"));
        if !reads {
            assert_eq!(a["available_to_caller"], false, "{a}");
            assert_eq!(a["caller_refusal"], "read_only_server", "{a}");
        }
    }
}

#[test]
fn a_read_only_session_changes_nothing_and_starts_nothing() {
    let project = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let markers = tempfile::tempdir().unwrap();
    let a = project.path();
    let b = other.path();

    // Project A: the runs, the playbooks, the connector account.
    let (parent, child) = seed_secret_runs(a);
    let (host, _sleeper) = host_run_with_live_driver(a);
    let _guard = crate::common::RunGuard::new(a, &host);
    install(a, "iso", ISOLATED);
    install(a, "pub", PUBLISH);
    let bin = stub_dir(markers.path());
    let connector = cfg.path().join("connectors/helped");
    fs::create_dir_all(&connector).unwrap();
    fs::write(connector.join("connector.yaml"), CONNECTOR).unwrap();
    let account = apb_core::connector::config::project_config_path(a, "helped");
    fs::create_dir_all(account.parent().unwrap()).unwrap();
    fs::write(
        &account,
        "accounts:\n  - name: main\n    token: \"{{env.HELPED_TOKEN}}\"\n",
    )
    .unwrap();

    // Project B is registered in the fresh config; then the installation
    // key and A's workspace identity are removed, for a cold state.
    apb_core::registry::init_project(b).unwrap();
    install(b, "other", ISOLATED);
    let registered = crate::common::apb_std()
        .arg("list")
        .current_dir(b)
        .env("APB_CONFIG_DIR", cfg.path())
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .unwrap();
    assert!(registered.status.success());
    assert!(
        cfg.path().join("projects.json").exists(),
        "B must be registered"
    );
    let workspace_b = fs::read_to_string(b.join(".apb/workspace.local"))
        .unwrap()
        .trim()
        .to_string();
    let _ = fs::remove_file(cfg.path().join("run-origin.key"));
    let _ = fs::remove_file(a.join(".apb/workspace.local"));

    let roots = [a, b, cfg.path(), home.path()];
    let before = snapshot(&roots);

    let mut s = Session::start(
        a,
        &[
            ("APB_CONFIG_DIR", cfg.path()),
            ("HOME", home.path()),
            ("PATH", bin.path()),
            ("HELPED_TOKEN", Path::new("helped-token-value")),
        ],
    );

    // The tool list is the allowlist (the supervisor role in the
    // environment does not apply).
    let listed = s.request("tools/list", json!({}));
    let mut names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    let mut allowed: Vec<String> = apb_mcp::server::read_only::READ_ONLY
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();
    allowed.sort();
    assert_eq!(names, allowed);

    // A removed tool called directly, with arguments it would take, is an
    // unknown tool to the dispatcher.
    for (removed, arguments) in [
        (
            "playbook_run",
            json!({ "id": "hm", "params": { "message": "x" } }),
        ),
        ("supervisor_run_inspect", json!({ "token": "t" })),
        (
            "run_task_submit",
            json!({ "run_id": host, "task_id": "t", "status": "succeeded", "output": "x" }),
        ),
    ] {
        let answer = s.request(
            "tools/call",
            json!({ "name": removed, "arguments": arguments }),
        );
        assert_eq!(
            answer["error"]["code"],
            json!(-32602),
            "{removed}: {answer}"
        );
        assert_eq!(
            answer["error"]["message"], "tool not found",
            "{removed}: {answer}"
        );
    }

    // Run surfaces: no secret value, no prompt, liveness unknown, no
    // mutating action.
    for run in [&parent, &child, &host] {
        let status = s.call("run_status", json!({ "run_id": run }));
        assert_eq!(status["run_id"], json!(run), "{status}");
        assert!(
            !status.to_string().contains(SECRET),
            "run_status leaked the value"
        );
        assert_eq!(status["driver_alive"], "unknown");
        // Every referenced secret resolves here: nothing is withheld.
        assert!(status.get("secrets_unresolved").is_none(), "{status}");
        no_mutating_action(&status);
        for (tool, args) in [
            ("run_events", json!({ "run_id": run })),
            ("run_report", json!({ "run_id": run })),
            ("run_retro_context", json!({ "run_id": run })),
            ("run_grades", json!({ "run_id": run })),
        ] {
            let out = s.call(tool, args);
            assert!(!out.to_string().contains(SECRET), "{tool} leaked the value");
        }
    }
    let status = s.call("run_status", json!({ "run_id": host }));
    withheld(&status["pending_tasks"]);
    withheld(&status["progress"]["pending_tasks"]);
    let waited = s.call("run_wait", json!({ "run_id": host, "timeout_ms": 1000 }));
    assert!(
        waited["next_actions"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "a run waiting on a host task has next actions: {waited}"
    );
    withheld(&waited["pending_tasks"]);
    no_mutating_action(&waited);
    assert!(!waited.to_string().contains(SECRET));
    let runs = s.call("runs_list", json!({}));
    assert!(
        !runs.to_string().contains(SECRET),
        "runs_list leaked the value"
    );

    // Fit: what would start a program is not checked.
    for id in ["iso", "pub"] {
        let report = s.call("playbook_adopt_report", json!({ "id": id }));
        assert!(
            report
                .to_string()
                .contains("not checked by a read-only server"),
            "{id}: {report}"
        );
    }

    // Restricted tools.
    let catalog = s.call(
        "playbook_catalog",
        json!({ "query": "repeat a message", "fit": true }),
    );
    assert_eq!(
        catalog["ranking"],
        json!({ "skipped": "read_only" }),
        "{catalog}"
    );
    let detect = s.call("agents_detect", json!({}));
    assert!(
        detect.to_string().contains("not probed: read-only server"),
        "{detect}"
    );
    assert_eq!(
        s.call("agents_detect", json!({ "refresh": true }))["error"],
        "read_only"
    );
    s.call("playbook_memory", json!({ "id": "hm" }));
    let set = s.call(
        "playbook_memory",
        json!({ "id": "hm", "action": "set", "params": { "message": "x" } }),
    );
    assert_eq!(set["error"], "read_only");

    // Every other allowed tool, with representative arguments.
    for (tool, args) in [
        ("playbook_list", json!({})),
        ("playbook_list", json!({ "workspace": workspace_b })),
        ("playbook_get", json!({ "id": "secretpb" })),
        ("playbook_validate", json!({ "id": "pub" })),
        ("playbook_howto", json!({})),
        ("playbook_interview", json!({})),
        ("setup_howto", json!({})),
        ("playbook_trash_list", json!({})),
        ("playbook_candidate", json!({ "id": "hm" })),
        ("playbook_known_problems", json!({ "id": "secretpb" })),
        ("playbook_discover_results", json!({})),
        ("projects_list", json!({})),
        ("profile_list", json!({})),
        ("profile_get", json!({ "name": "main", "scope": "project" })),
        ("profile_howto", json!({})),
        ("connectors_list", json!({})),
        ("trust_list", json!({})),
        ("playbook_adopt_report", json!({})),
    ] {
        let out = s.call(tool, args.clone());
        assert!(!out.to_string().contains(SECRET), "{tool} leaked the value");
    }
    s.close();
    unchanged(&before, &snapshot(&roots));

    // A secret the server cannot resolve: a helper program it may not
    // start (a command source lives in the global account config only),
    // then a variable missing from its environment. The runs' text is
    // withheld, apb's own ids and states are kept.
    let global = cfg.path().join("connector-config/helped.yaml");
    for (path, other, token) in [
        (
            &global,
            &account,
            format!("{{{{cmd:{}}}}}", bin.path().join("helper").display()),
        ),
        (&account, &global, "{{env.HELPED_TOKEN}}".to_string()),
    ] {
        let _ = fs::remove_file(other);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("accounts:\n  - name: main\n    token: \"{token}\"\n"),
        )
        .unwrap();
        let before = snapshot(&roots);
        let mut s = Session::start(
            a,
            &[
                ("APB_CONFIG_DIR", cfg.path()),
                ("HOME", home.path()),
                ("PATH", bin.path()),
            ],
        );
        for run in [&parent, &child] {
            let status = s.call("run_status", json!({ "run_id": run }));
            assert_eq!(status["secrets_unresolved"], true, "{token}: {status}");
            assert_eq!(status["run_id"], json!(run), "{status}");
            // apb's own state stays readable.
            assert!(
                matches!(status["run_status"].as_str(), Some("failed" | "succeeded")),
                "{status}"
            );
            assert!(
                status.to_string().contains("[secrets_unresolved]"),
                "{status}"
            );
            assert!(!status.to_string().contains(SECRET), "{status}");
            let events = s.call("run_events", json!({ "run_id": run }));
            assert_eq!(events["secrets_unresolved"], true, "{events}");
            assert!(!events.to_string().contains(SECRET), "{events}");
        }
        s.close();
        unchanged(&before, &snapshot(&roots));
    }

    let ran: Vec<String> = fs::read_dir(markers.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(ran.is_empty(), "the read-only session started {ran:?}");
}
