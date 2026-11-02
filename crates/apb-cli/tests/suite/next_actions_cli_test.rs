//! Next actions on the command line: `apb runs <id>` and `apb wait` print
//! each action with its runnable command, `apb wait --follow-auto` returns
//! on the aggregated verdict, and `--request` names the request an answer
//! or a decision is for.

use std::fs;
use std::path::Path;

const GATED: &str = "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review, options: [approved, rejected] }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: f }\n";

fn apb_in(root: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

/// A project with run `r` of the gated playbook: the review requested at
/// seq 1, then the journal lines given.
fn seed(root: &Path, after: &[serde_json::Value]) {
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let dir = root.join(".apb/runs/r");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), GATED).unwrap();
    let mut lines = vec![
        serde_json::json!({"seq": 0, "ts": 1, "type": "run_started", "playbook": "g", "version": "1.0.0"}),
        serde_json::json!({"seq": 1, "ts": 2, "type": "review_requested", "node": "gate", "options": ["approved", "rejected"]}),
    ];
    for (i, v) in after.iter().enumerate() {
        let mut v = v.clone();
        v["seq"] = serde_json::json!(2 + i);
        v["ts"] = serde_json::json!(3 + i);
        lines.push(v);
    }
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

fn paused_until_in_an_hour() -> serde_json::Value {
    let until = apb_core::clock::now_ms_u64() + 3_600_000;
    serde_json::json!({"type": "run_paused", "reason": "later", "by": "operator", "until": until})
}

/// A run paused with `until` while its review stays open: the default wait
/// says stopped (4), as before; following automatic progress it needs input
/// (3) at once, with the review as the primary action.
#[test]
fn follow_auto_on_a_paused_run_with_an_open_review_needs_input() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &[paused_until_in_an_hour()]);

    let out = apb_in(dir.path(), &["wait", "r", "--timeout", "20"]);
    assert_eq!(out.status.code(), Some(4), "default: {out:?}");

    let out = apb_in(
        dir.path(),
        &["wait", "r", "--follow-auto", "--timeout", "20"],
    );
    assert_eq!(out.status.code(), Some(3), "follow-auto: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("apb review '--decision=<approved|rejected>' --request=review:1 r gate"),
        "{stdout}"
    );
}

/// `apb runs <id>` lists the next actions with their commands, and the
/// printed review command, its decision filled in, decides the review; the
/// same request again is refused.
#[test]
fn the_printed_review_command_decides_and_a_second_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &[]);
    let out = apb_in(dir.path(), &["runs", "r"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("next: node `gate` waits for a human review decision"),
        "{stdout}"
    );
    // The printed line pasted as it is, its placeholder not filled in, is
    // refused and spends nothing.
    let unfilled = [
        "review",
        "--decision=<approved|rejected>",
        "--request=review:1",
        "r",
        "gate",
    ];
    let out = apb_in(dir.path(), &unfilled);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("placeholder"),
        "{out:?}"
    );
    let decide = [
        "review",
        "--decision=approved",
        "--request=review:1",
        "r",
        "gate",
    ];
    let out = apb_in(dir.path(), &decide);
    assert!(out.status.success(), "{out:?}");
    let out = apb_in(dir.path(), &decide);
    assert!(!out.status.success(), "a second decision: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already"),
        "{out:?}"
    );
    // A request of another kind is refused before anything is posted.
    let out = apb_in(dir.path(), &["answer", "--request=review:1", "r", "x"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("question"));
}

/// `apb runs <id> --json` carries the actions as the MCP run_status object
/// does, the review's request among its arguments.
#[test]
fn runs_json_carries_the_next_actions() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &[]);
    let out = apb_in(dir.path(), &["runs", "r", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["next_actions"][0]["code"], "decide_review", "{v}");
    assert_eq!(v["next_actions"][0]["arguments"]["request"], 1);
    assert_eq!(v["stop"], "needs_input");
}

/// `apb run` of a playbook whose param has no default and no value is
/// refused before anything is written, with the start action that answers
/// it, the same refusal the dashboard and MCP give.
#[test]
fn a_start_without_a_needed_param_is_refused_with_its_action() {
    let dir = tempfile::tempdir().unwrap();
    let out = apb_in(dir.path(), &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = dir.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: target, type: text }\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n",
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();

    let out = apb_in(dir.path(), &["run", "p"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("run refused (param_needs_value)"), "{err}");
    assert!(err.contains("pass `--param target=<value>`"), "{err}");
    let runs = dir.path().join(".apb/runs");
    assert!(!runs.exists() || fs::read_dir(&runs).unwrap().next().is_none());

    let out = apb_in(dir.path(), &["run", "p", "--param", "target=x"]);
    assert!(out.status.success(), "{out:?}");
}

const ASKING: &str = "schema: 2\nid: q\nname: q\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: f }\n";

/// A free-text question's printed answer placeholder, pasted as it is, is
/// refused and spends nothing; an answer that merely looks bracketed
/// (`<ok>`) is an answer like any other.
#[test]
fn only_the_printed_placeholder_is_refused_as_an_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    assert!(apb_in(root, &["init"]).status.success());
    let run = root.join(".apb/runs/qr");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("playbook.yaml"), ASKING).unwrap();
    apb_engine::question::post_question(&run, "ask", 1, "anything else?", vec![]).unwrap();
    let lines = [
        serde_json::json!({"seq": 0, "ts": 1, "type": "run_started", "playbook": "q", "version": "1.0.0"}),
        serde_json::json!({"seq": 1, "ts": 2, "type": "question_asked", "node": "ask", "attempt": 1, "question": "anything else?", "options": []}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(run.join("events.jsonl"), text).unwrap();

    let out = apb_in(root, &["answer", "qr", "<your answer>"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("placeholder of the printed command"),
        "{out:?}"
    );
    assert!(
        apb_engine::question::read_answers_after(&run, None)
            .unwrap()
            .is_empty(),
        "nothing was posted"
    );
    let out = apb_in(root, &["answer", "qr", "<ok>"]);
    assert!(out.status.success(), "{out:?}");
    let answers = apb_engine::question::read_answers_after(&run, None).unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].answer, "<ok>");
}

/// `apb resume --retry-now` on a run a live driver holds: the expedite is
/// journaled as a person's and no driver starts; a node with nothing
/// waiting is refused with the code MCP and HTTP give. `apb runs <id>`
/// shows the waiting retry with its due time.
#[cfg(unix)]
#[test]
fn resume_retry_now_expedites_a_live_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let run = root.join(".apb/runs/rd");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("playbook.yaml"), HOUR_RETRY).unwrap();
    let now = apb_core::clock::now_ms_u64();
    // The first attempt of `work` failed and its retry waits an hour.
    let lines = [
        serde_json::json!({"seq":0,"ts":now,"type":"run_started","playbook":"hour","version":"1.0.0"}),
        serde_json::json!({"seq":1,"ts":now,"type":"node_started","node":"work","attempt":1}),
        serde_json::json!({"seq":2,"ts":now,"type":"attempt_started","node":"work","attempt":1,"agent":"claude"}),
        serde_json::json!({"seq":3,"ts":now,"type":"attempt_finished","node":"work","attempt":1,"status":"failed"}),
        serde_json::json!({"seq":4,"ts":now,"type":"retry_deferred","node":"work","after_attempt":2,"retry":1,"step":0,"binding":"b","delay_ms":3_600_000}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(run.join("events.jsonl"), text).unwrap();
    // A live process that names the run as its driver, killed by the guard
    // however the test ends.
    let mut driver = std::process::Command::new("sh")
        .args(["-c", "while :; do sleep 1; done", "drv", "--run-id", "rd"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    fs::write(run.join("driver.pid"), driver.id().to_string()).unwrap();
    let _guard = crate::common::RunGuard::new(root, "rd");

    let out = apb_in(root, &["runs", "rd"]);
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        shown.contains("work\tretry_waiting\tretry 1 at "),
        "{shown}"
    );
    assert!(
        shown.contains("apb resume --retry-now --node=work rd"),
        "{shown}"
    );

    let out = apb_in(root, &["resume", "rd", "--retry-now", "--node", "nope"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no_deferred_retry"),
        "{out:?}"
    );

    let out = apb_in(root, &["resume", "rd", "--retry-now", "--node", "work"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("retry of `work` starts now"),
        "{out:?}"
    );
    let journal = fs::read_to_string(run.join("events.jsonl")).unwrap();
    let last: serde_json::Value = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
    assert_eq!(last["type"], "retry_expedited");
    assert_eq!(last["node"], "work");
    assert_eq!(last["by"], "operator");
    let _ = driver.kill();
    let _ = driver.wait();
}

const HOUR_RETRY: &str = r#"
schema: 2
id: hour
name: Hour
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do", max_retries: 1, retry_delay: { initial: 1h } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// A project `apb init` creates at `root`, whose run of [`HOUR_RETRY`] released itself (exit
/// 4) with its only retry waiting an hour. Every command runs with the
/// config dir `cfg`, which also registers the project there.
pub(crate) struct ReleasedRun {
    pub(crate) run_id: String,
    /// The agent stub; a driver that resumes the run needs it as
    /// `APB_AGENT_CMD`.
    pub(crate) stub: std::path::PathBuf,
    calls: std::path::PathBuf,
    cfg: std::path::PathBuf,
    root: std::path::PathBuf,
    _guard: crate::common::RunGuard,
}

#[cfg(unix)]
pub(crate) fn released_hour_run(root: &Path, cfg: &Path) -> ReleasedRun {
    use std::os::unix::fs::PermissionsExt;
    let out = apb_cfg(root, cfg, Path::new("/bin/false"), &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = root.join(".apb/playbooks/hour/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), HOUR_RETRY).unwrap();
    fs::write(root.join(".apb/playbooks/hour/current"), "1.0.0").unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    // The first call fails, every later one answers.
    let stub = root.join("agent.sh");
    let calls = root.join("calls");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf x >> '{c}'\nn=$(wc -c < '{c}' | tr -d ' ')\nif [ \"$n\" -eq 1 ]; then echo 'the agent made a mistake' 1>&2; exit 1; fi\necho done\n",
            c = calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let out = apb_cfg(root, cfg, &stub, &["run", "hour"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(4), "released: {out:?}");
    let run_id = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .next()
        .unwrap();
    let guard = crate::common::RunGuard::new(root, &run_id);
    assert!(stdout.contains("released"), "{stdout}");
    ReleasedRun {
        run_id,
        stub,
        calls,
        cfg: cfg.to_path_buf(),
        root: root.to_path_buf(),
        _guard: guard,
    }
}

/// `apb <args>` in `root` with the config dir `cfg` (the project registered
/// there) and the agent stub.
fn apb_cfg(root: &Path, cfg: &Path, stub: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
        .args(args)
        .current_dir(root)
        .env("APB_CONFIG_DIR", cfg)
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .env("APB_AGENT_CMD", stub)
        .env("APB_RELEASE_AFTER_MS", "300")
        .output()
        .unwrap()
}

impl ReleasedRun {
    /// The workspace id the project is registered under in the config dir.
    pub(crate) fn workspace(&self) -> String {
        fs::read_to_string(self.root.join(".apb/workspace.local"))
            .expect("the project was registered")
            .trim()
            .to_string()
    }

    /// Waits for the run to finish, then checks that the retry-now was
    /// journaled by `by` right before its `run_resumed` and that the
    /// delayed attempt ran at once (the second agent call).
    pub(crate) fn assert_resumed_by(&self, by: &str) {
        let dir = self.root.join(".apb/runs").join(&self.run_id);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let events = loop {
            let events = apb_engine::event::read_all(&dir).unwrap();
            if events.last().is_some_and(|e| {
                matches!(
                    e.payload,
                    apb_engine::event::EventPayload::RunFinished { .. }
                )
            }) {
                break events;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the resumed run never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let kinds: Vec<String> = events
            .iter()
            .map(|e| {
                serde_json::to_value(&e.payload).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let at = kinds
            .iter()
            .position(|k| k == "retry_expedited")
            .unwrap_or_else(|| panic!("no retry_expedited: {kinds:?}"));
        assert_eq!(kinds[at + 1], "run_resumed", "{kinds:?}");
        assert_eq!(serde_json::to_value(&events[at].payload).unwrap()["by"], by);
        assert_eq!(fs::read_to_string(&self.calls).unwrap().len(), 2);
    }
}

/// End to end through the real binary: a foreground run whose only retry
/// waits an hour releases the run (exit 4), and `apb resume --retry-now`
/// resumes it with the retry due at once, the expedite journaled by a
/// person right before its `run_resumed`.
#[cfg(unix)]
#[test]
fn retry_now_resumes_a_released_run_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run = released_hour_run(root, cfg.path());
    let out = apb_cfg(
        root,
        cfg.path(),
        &run.stub,
        &["resume", &run.run_id, "--retry-now"],
    );
    assert!(out.status.success(), "{out:?}");
    run.assert_resumed_by("operator");
}

/// MCP `run_resume { retry_now }` on a released run resumes it through a
/// detached driver with the retry due at once, the expedite journaled as
/// the host's.
#[cfg(unix)]
#[test]
fn mcp_retry_now_resumes_a_released_run_as_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run = released_hour_run(root, cfg.path());
    let mut cmd = crate::common::apb_std();
    cmd.env("APB_CONFIG_DIR", &run.cfg)
        .env("APB_AGENT_CMD", &run.stub);
    let mut mcp = crate::detached_driver_test::McpSession::start_with(root, cmd);
    let body = mcp.call(
        2,
        &format!(
            r#"{{"name":"run_resume","arguments":{{"run_id":"{}","retry_now":true,"acknowledge_untrusted":true}}}}"#,
            run.run_id
        ),
    );
    assert_eq!(body["detached"], true, "{body}");
    assert_eq!(body["retry_now"], serde_json::json!(["work"]), "{body}");
    run.assert_resumed_by("host");
}

/// The dashboard's resume with `retry_now` on a released run resumes it
/// through a detached driver with the retry due at once, the expedite
/// journaled as the operator's.
#[cfg(unix)]
#[test]
fn http_retry_now_resumes_a_released_run_as_the_operator() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = dir.path();
    let run = released_hour_run(root, cfg.path());
    let dashboard = crate::detached_driver_test::Dashboard::start_with(
        cfg.path(),
        &[("APB_AGENT_CMD", run.stub.as_path())],
    );
    let body = dashboard.post(
        &format!(
            "/api/runs/{}/resume?workspace={}",
            run.run_id,
            run.workspace()
        ),
        r#"{"retry_now":true}"#,
    );
    assert_eq!(body["detached"], true, "{body}");
    assert_eq!(body["retry_now"], serde_json::json!(["work"]), "{body}");
    run.assert_resumed_by("operator");
}

/// The detached driver's retry-now arguments are refused before anything
/// runs when they cannot be honored: a `--retry-now-by` that names nobody
/// (it would otherwise be journaled as a person), and a retry-now or a
/// budget change next to the engine's hold of a pause or a release (which
/// takes neither).
#[test]
fn the_driver_refuses_retry_now_arguments_it_cannot_honor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let base = ["__drive-run", "--root", root, "--run-id", "r", "--resume"];
    for extra in [
        &["--retry-now", "--retry-now-by", "martian"][..],
        &["--release-seq", "3", "--retry-now"][..],
        &["--pause-seq", "3", "--retry-now"][..],
        &["--release-seq", "3", "--budget-json", "{}"][..],
    ] {
        let args: Vec<&str> = base.iter().chain(extra).copied().collect();
        let out = apb_in(dir.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{extra:?}: {out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("martian") || err.contains("cannot be used with"),
            "{extra:?}: {err}"
        );
    }
}
