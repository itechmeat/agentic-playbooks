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

/// Run `id` of [`HOUR_RETRY`] in the project at `root`, whose first attempt
/// of `work` failed and whose retry waits an hour, with a live process that
/// names the run as its driver (the returned child; the caller kills it).
#[cfg(unix)]
fn seed_live_deferred(root: &Path, id: &str) -> std::process::Child {
    let run = root.join(".apb/runs").join(id);
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("playbook.yaml"), HOUR_RETRY).unwrap();
    let now = apb_core::clock::now_ms_u64();
    let lines = [
        serde_json::json!({"seq":0,"ts":now,"type":"run_started","playbook":"hour","version":"1.0.0"}),
        serde_json::json!({"seq":1,"ts":now,"type":"node_started","node":"work","attempt":1}),
        serde_json::json!({"seq":2,"ts":now,"type":"attempt_started","node":"work","attempt":1,"agent":"claude"}),
        serde_json::json!({"seq":3,"ts":now,"type":"attempt_finished","node":"work","attempt":1,"status":"failed"}),
        serde_json::json!({"seq":4,"ts":now,"type":"retry_deferred","node":"work","after_attempt":2,"retry":1,"step":0,"binding":"b","delay_ms":3_600_000}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(run.join("events.jsonl"), text).unwrap();
    let driver = std::process::Command::new("sh")
        .args(["-c", "while :; do sleep 1; done", "drv", "--run-id", id])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    fs::write(run.join("driver.pid"), driver.id().to_string()).unwrap();
    driver
}

/// The last journal line of run `id`.
fn last_event(root: &Path, id: &str) -> serde_json::Value {
    let journal = fs::read_to_string(root.join(".apb/runs").join(id).join("events.jsonl")).unwrap();
    serde_json::from_str(journal.lines().last().unwrap()).unwrap()
}

/// `apb resume --retry-now` on a run a live driver holds: the expedite is
/// journaled and no driver starts; a node with nothing waiting is refused
/// with the code MCP and HTTP give. `apb runs <id>` shows the waiting retry
/// with its due time.
#[cfg(unix)]
#[test]
fn resume_retry_now_expedites_a_live_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let mut driver = seed_live_deferred(root, "rd");
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
    let last = last_event(root, "rd");
    assert_eq!(last["type"], "retry_expedited");
    assert_eq!(last["node"], "work");
    let _ = driver.kill();
    let _ = driver.wait();
}

/// A retry-now is journaled as a person's (`operator`) only when a person
/// typed it at a terminal: the same command without a terminal, or from a
/// run step (`APB_RUN_ID` or `APB_HOST_TASK` set, at a terminal or not), is
/// recorded as `host`, never as a person.
#[cfg(target_os = "linux")]
#[test]
fn retry_now_is_a_persons_only_at_a_terminal_outside_a_run_step() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    // (run id, at a terminal, step environment, the journaled `by`)
    type Case<'a> = (&'a str, bool, &'a [(&'a str, &'a str)], &'a str);
    let cases: [Case; 4] = [
        ("tty", true, &[], "operator"),
        ("piped", false, &[], "host"),
        ("step-tty", true, &[("APB_RUN_ID", "other-run")], "host"),
        ("task", false, &[("APB_HOST_TASK", "t-1")], "host"),
    ];
    for (id, terminal, env, want) in cases {
        let mut driver = seed_live_deferred(root, id);
        let _guard = crate::common::RunGuard::new(root, id);
        let args = ["resume", id, "--retry-now", "--node", "work"];
        let ran = if terminal {
            crate::common::on_a_terminal(root, crate::common::sandbox_config_dir(), &args, env, b"")
        } else {
            let mut cmd = crate::common::apb_std();
            for (k, v) in env {
                cmd.env(k, v);
            }
            let out = cmd.args(args).current_dir(root).output().unwrap();
            Some((out.status.code().unwrap_or(-1), format!("{out:?}")))
        };
        let _ = driver.kill();
        let _ = driver.wait();
        let Some((code, shown)) = ran else {
            eprintln!("skipped {id}: util-linux `script` is missing");
            continue;
        };
        assert_eq!(code, 0, "{id}: {shown}");
        let last = last_event(root, id);
        assert_eq!(last["type"], "retry_expedited", "{id}: {last}");
        assert_eq!(last["by"], want, "{id}: {last}");
    }
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
    released_run(root, cfg, HOUR_RETRY)
}

/// [`released_hour_run`] of `playbook` (id `hour`, profile `main`, node
/// `work` with one retry): the run `apb run` released.
#[cfg(unix)]
fn released_run(root: &Path, cfg: &Path, playbook: &str) -> ReleasedRun {
    use std::os::unix::fs::PermissionsExt;
    let out = apb_cfg(root, cfg, Path::new("/bin/false"), &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = root.join(".apb/playbooks/hour/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), playbook).unwrap();
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
        // The waker a release leaves behind inherits this: it notices its
        // wake time within 50 ms instead of 5 s.
        .env(apb_engine::pause::waker::POLL_ENV, "50")
        .output()
        .unwrap()
}

impl Drop for ReleasedRun {
    /// The waker a release leaves behind sleeps until the wake time: it is
    /// killed with the test, however the test ends.
    fn drop(&mut self) {
        crate::common::kill_wakers(&self.root, &self.run_id);
    }
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

/// [`HOUR_RETRY`] with its retry waiting 3 s: past the 300 ms release
/// threshold the tests set, short enough for the waker to wake it.
const SECONDS_RETRY: &str = r#"
schema: 2
id: hour
name: Hour
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do", max_retries: 1, retry_delay: { initial: 3s } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// The shipped release path end to end, through the real binary and no
/// one's resume: `apb run` releases the run (exit 4) and leaves the waker
/// `apb __pause-waker --release <seq>` behind; at the release's wake time
/// the waker starts the detached driver `__drive-run --resume --release-seq
/// <seq>`, which runs the delayed retry, and the run succeeds. The engine
/// resumed it once, nobody expedited it, and the retry ran no earlier than
/// its wake time.
#[cfg(unix)]
#[test]
fn a_released_run_is_woken_by_its_waker_and_succeeds() {
    use apb_engine::event::EventPayload;
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run = released_run(root, cfg.path(), SECONDS_RETRY);
    let run_dir = root.join(".apb/runs").join(&run.run_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let events = loop {
        let events = apb_engine::event::read_all(&run_dir).unwrap();
        if events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
        {
            break events;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the released run never finished: {:?}",
            events.iter().map(|e| &e.payload).collect::<Vec<_>>()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let json: Vec<serde_json::Value> = events
        .iter()
        .map(|e| serde_json::to_value(&e.payload).unwrap())
        .collect();
    let of = |kind: &str| -> Vec<usize> {
        (0..json.len())
            .filter(|&i| json[i]["type"] == kind)
            .collect()
    };
    let finished = &json[*of("run_finished").last().unwrap()];
    assert_eq!(finished["outcome"], "succeeded", "{json:?}");
    let released = of("drive_released");
    assert_eq!(released.len(), 1, "{json:?}");
    let wake_at = json[released[0]]["wake_at_ms"].as_u64().unwrap();
    let resumed = of("run_resumed");
    assert_eq!(resumed.len(), 1, "{json:?}");
    assert_eq!(json[resumed[0]]["by"], "engine", "{json:?}");
    assert!(resumed[0] > released[0], "{json:?}");
    assert!(of("retry_expedited").is_empty(), "{json:?}");
    let attempts: Vec<usize> = of("attempt_started")
        .into_iter()
        .filter(|&i| json[i]["node"] == "work")
        .collect();
    assert_eq!(attempts.len(), 2, "{json:?}");
    let second_at = u64::try_from(events[attempts[1]].ts).unwrap();
    assert!(
        second_at >= wake_at,
        "the retry ran at {second_at}, before its wake time {wake_at}"
    );
    assert_eq!(fs::read_to_string(&run.calls).unwrap().len(), 2);
}

/// `apb resume --all` on a released run whose waker died (a reboot, a
/// kill): it says a waker is armed, and one really holds the release's
/// lease afterwards.
#[cfg(target_os = "linux")]
#[test]
fn resume_all_rearms_a_released_run_whose_waker_died() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run = released_hour_run(root, cfg.path());
    let run_dir = root.join(".apb/runs").join(&run.run_id);
    let seq = apb_engine::event::read_all(&run_dir)
        .unwrap()
        .iter()
        .find(|e| {
            matches!(
                e.payload,
                apb_engine::event::EventPayload::DriveReleased { .. }
            )
        })
        .expect("the run was released")
        .seq;
    let lease = run_dir.join("wakers").join(format!("release-{seq}"));
    // Read from the kernel's lock table, never by taking the lease.
    let held_within = |want: bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while crate::common::flock_held(&lease) != want {
            assert!(
                std::time::Instant::now() < deadline,
                "the release lease never became {}",
                if want { "held" } else { "free" }
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    held_within(true);
    crate::common::kill_wakers(root, &run.run_id);
    held_within(false);

    let out = apb_cfg(root, cfg.path(), &run.stub, &["resume", "--all"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with(&run.run_id) && l.contains("a waker is armed")),
        "{stdout}"
    );
    held_within(true);
}

/// `apb resume --all` on a released run whose waker died and whose lease
/// path a writer of the run directory replaced: with a fifo, or with a hard
/// link to a file elsewhere. The waker it spawns cannot open the lease and
/// ends, so the pass says nothing wakes the run instead of "a waker is
/// armed", and the linked file keeps its mode.
#[cfg(target_os = "linux")]
#[test]
fn resume_all_says_not_armed_when_its_waker_cannot_hold_the_lease() {
    use std::os::unix::fs::PermissionsExt as _;
    for planted in ["fifo", "hard link"] {
        let dir = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run = released_hour_run(root, cfg.path());
        let path = lease_of_dead_waker(root, &run.run_id);
        std::fs::remove_file(&path).unwrap();
        let victim = cfg.path().join("victim");
        if planted == "fifo" {
            let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
            // SAFETY: a valid NUL-terminated path.
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        } else {
            std::fs::write(&victim, "x").unwrap();
            std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::hard_link(&victim, &path).unwrap();
        }

        let out = apb_cfg(root, cfg.path(), &run.stub, &["resume", "--all"]);
        assert!(out.status.success(), "{planted}: {out:?}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find(|l| l.starts_with(&run.run_id))
            .unwrap_or_else(|| panic!("{planted}: no line for the run: {stdout}"));
        assert!(!line.contains("a waker is armed"), "{planted}: {stdout}");
        assert!(
            line.contains("nothing wakes it then") && line.contains("ended before it held"),
            "{planted}: {stdout}"
        );
        if planted == "hard link" {
            let mode = std::fs::metadata(&victim).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o644, "the linked file keeps its mode");
        }
    }
}

/// The release lease of the released run `run_id`, once its waker held it
/// and was killed: the path, free.
#[cfg(target_os = "linux")]
fn lease_of_dead_waker(root: &Path, run_id: &str) -> std::path::PathBuf {
    let run_dir = root.join(".apb/runs").join(run_id);
    let seq = apb_engine::event::read_all(&run_dir)
        .unwrap()
        .iter()
        .find(|e| {
            matches!(
                e.payload,
                apb_engine::event::EventPayload::DriveReleased { .. }
            )
        })
        .expect("the run was released")
        .seq;
    let path = run_dir.join("wakers").join(format!("release-{seq}"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    // Read from the kernel's lock table, never by taking the lease: a probe
    // that holds it as the waker tries would send that waker away.
    while !crate::common::flock_held(&path) {
        assert!(
            std::time::Instant::now() < deadline,
            "no waker held the lease"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    crate::common::kill_wakers(root, run_id);
    while crate::common::flock_held(&path) {
        assert!(
            std::time::Instant::now() < deadline,
            "the lease stayed held"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    path
}

/// End to end through the real binary: a foreground run whose only retry
/// waits an hour releases the run (exit 4), and `apb resume --retry-now`
/// resumes it with the retry due at once, the expedite journaled right
/// before its `run_resumed` (as the host's: the command has no terminal).
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
    // Without a terminal the CLI cannot tell a person from a script.
    run.assert_resumed_by("host");
}

/// The hidden driver a resume re-executes, started inside a run step with
/// `--retry-now-by operator`: the expedite is journaled as the host's, never
/// a person's, whatever its arguments say. The same invocation outside a
/// run step records the person its arguments name.
#[cfg(unix)]
#[test]
fn the_hidden_driver_inside_a_run_step_never_records_a_person() {
    for (step_env, want) in [
        (Some("APB_RUN_ID"), "host"),
        (Some("APB_HOST_TASK"), "host"),
        (None, "operator"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run = released_hour_run(root, cfg.path());
        let mut cmd = crate::common::apb_std();
        cmd.args([
            "__drive-run",
            "--root",
            root.to_str().unwrap(),
            "--run-id",
            &run.run_id,
            "--resume",
            "--retry-now",
            "--retry-now-by",
            "operator",
        ])
        .current_dir(root)
        .env("APB_CONFIG_DIR", cfg.path())
        .env("APB_AGENT_CMD", &run.stub);
        if let Some(var) = step_env {
            cmd.env(var, "some-other-step");
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{step_env:?}: {out:?}");
        run.assert_resumed_by(want);
    }
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
/// through a detached driver with the retry due at once. A dashboard
/// without keys takes any request that reaches its loopback port, so the
/// expedite is journaled as the host's, not a person's.
#[cfg(unix)]
#[test]
fn http_retry_now_resumes_a_released_run_as_the_host_without_keys() {
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
    run.assert_resumed_by("host");
}

/// The detached driver's retry-now arguments are refused before anything
/// runs when they cannot be honored: a `--retry-now-by` that names nobody
/// or is missing (either would otherwise be journaled as a person), and a retry-now or a
/// budget change next to the engine's hold of a pause or a release (which
/// takes neither).
#[test]
fn the_driver_refuses_retry_now_arguments_it_cannot_honor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let base = ["__drive-run", "--root", root, "--run-id", "r", "--resume"];
    for extra in [
        &["--retry-now", "--retry-now-by", "martian"][..],
        // A retry-now that names nobody is never taken for a person's.
        &["--retry-now"][..],
        &["--release-seq", "3", "--retry-now"][..],
        &["--pause-seq", "3", "--retry-now"][..],
        &["--release-seq", "3", "--budget-json", "{}"][..],
    ] {
        let args: Vec<&str> = base.iter().chain(extra).copied().collect();
        let out = apb_in(dir.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{extra:?}: {out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("martian")
                || err.contains("cannot be used with")
                || err.contains("--retry-now-by"),
            "{extra:?}: {err}"
        );
    }
}

/// The run whose resend went out while its first send stays unknown, and
/// what every surface says about it: one fixture
/// (`crates/apb-core/tests/fixtures/resent_run.json`) for the CLI, MCP and
/// HTTP tests.
fn resent_fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../apb-core/tests/fixtures/resent_run.json"
    ))
    .unwrap()
}

/// The words of [`resent_fixture`] under `key` (`reason` or `gap`).
fn resent_text(key: &str) -> String {
    resent_fixture()[key].as_str().unwrap().to_string()
}

fn seed_resent_run(root: &std::path::Path, run_id: &str) {
    let fixture = resent_fixture();
    let dir = root.join(".apb/runs").join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("playbook.yaml"),
        fixture["playbook"].as_str().unwrap(),
    )
    .unwrap();
    let mut log = apb_engine::event::EventLog::open(&dir).unwrap();
    for line in fixture["events"].as_array().unwrap() {
        log.append(serde_json::from_value(line.clone()).unwrap())
            .unwrap();
    }
}

/// The text of `apb runs <id>` says that the resend went out and that the
/// first send is still unknown, in the outcome and in the next action.
#[test]
fn apb_runs_says_a_resend_went_out_and_the_first_send_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    seed_resent_run(root, "resent");
    let out = apb_in(root, &["runs", "resent"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(&resent_text("gap")), "{text}");
    assert!(text.contains(&resent_text("reason")), "{text}");
}

/// A retry waits an hour beside a review its person has not answered, and
/// the run's budget paused it before `b`.
const PARKED_BUDGET: &str = r#"
schema: 2
id: parked
name: Parked
version: 1.0.0
defaults: { profile: main }
budget: { tokens: 500 }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do", max_retries: 1, retry_delay: { initial: 1h } }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: b, type: agent_task, prompt: "b" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: start, to: gate }
  - { from: gate, to: b, condition: { type: review_status, equals: approved } }
  - { from: work, to: done }
  - { from: b, to: done }
"#;

/// `apb resume --retry-now` on a run whose sibling parks on a review and
/// whose budget paused it: the retry cannot start until a person decides
/// on the budget, and the command says so instead of claiming the retry
/// starts. Nothing is journaled and the retry still waits.
#[cfg(unix)]
#[test]
fn a_retry_now_that_cannot_start_beside_a_parked_review_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let run = root.join(".apb/runs/rp");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("playbook.yaml"), PARKED_BUDGET).unwrap();
    let now = apb_core::clock::now_ms_u64();
    let lines = [
        serde_json::json!({"type":"run_started","playbook":"parked","version":"1.0.0"}),
        serde_json::json!({"type":"budget_set","tokens":500,"source":"playbook"}),
        serde_json::json!({"type":"node_started","node":"work","attempt":1}),
        serde_json::json!({"type":"node_started","node":"gate","attempt":1}),
        serde_json::json!({"type":"review_requested","node":"gate","options":["approved","rejected"]}),
        serde_json::json!({"type":"attempt_started","node":"work","attempt":1,"agent":"claude"}),
        serde_json::json!({"type":"attempt_finished","node":"work","attempt":1,"status":"failed"}),
        serde_json::json!({"type":"retry_deferred","node":"work","after_attempt":2,"retry":1,"step":0,"binding":"b","delay_ms":3_600_000}),
        serde_json::json!({"type":"budget_gate","node":"b","decision":"paused","basis":"median"}),
        serde_json::json!({"type":"run_paused","reason":"budget (tokens)","by":"engine"}),
    ];
    let text: String = lines
        .iter()
        .enumerate()
        .map(|(seq, l)| {
            let mut l = l.clone();
            l["seq"] = seq.into();
            l["ts"] = now.into();
            format!("{l}\n")
        })
        .collect();
    fs::write(run.join("events.jsonl"), &text).unwrap();
    let _guard = crate::common::RunGuard::new(root, "rp");

    let shown = apb_in(root, &["runs", "rp"]);
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains("work\tretry_waiting\t"), "{shown}");

    let out = apb_in(root, &["resume", "rp", "--retry-now", "--node", "work"]);
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(2), "{stdout}\n{stderr}");
    assert!(stderr.contains("budget_paused"), "{stderr}");
    assert!(!stdout.contains("starts now"), "{stdout}");
    assert_eq!(
        fs::read_to_string(run.join("events.jsonl")).unwrap(),
        text,
        "nothing journaled: no delay waived, no resume"
    );
    let shown = apb_in(root, &["runs", "rp"]);
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains("work\tretry_waiting\t"), "{shown}");
}

/// A person-only gate's action, printed by `apb runs` without a terminal,
/// says the CLI here cannot take it and why.
#[test]
fn a_person_only_gate_is_printed_as_not_takeable_without_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &[]);
    let run = dir.path().join(".apb/runs/r");
    fs::write(
        run.join("playbook.yaml"),
        GATED.replace(
            "type: human_review,",
            "type: human_review, person_only: true,",
        ),
    )
    .unwrap();
    let events = fs::read_to_string(run.join("events.jsonl"))
        .unwrap()
        .replace(
            "\"type\":\"review_requested\"",
            "\"type\":\"review_requested\",\"person_only\":true",
        );
    fs::write(run.join("events.jsonl"), events).unwrap();
    let out = crate::common::apb_std()
        .args(["runs", "r"])
        .current_dir(dir.path())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no_terminal"), "{stdout}");
}

/// `apb review` at a pseudo-terminal: the typed confirmation a person-only
/// gate needs, refused without a terminal and inside a run step.
mod review_terminal {
    use std::fs;
    use std::path::Path;

    const PERSON_ONLY: &str = "schema: 2\nid: po\nname: po\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review, person_only: true }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: f }\n";

    const ORDINARY: &str = "schema: 2\nid: or\nname: or\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: f }\n";

    /// A project with run `id` of `playbook`, its gate's request open.
    fn seed(root: &Path, id: &str, playbook: &str, person_only: bool) {
        fs::create_dir_all(root.join(".apb/playbooks")).unwrap();
        let dir = root.join(".apb/runs").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playbook.yaml"), playbook).unwrap();
        let mut req = serde_json::json!({"seq": 1, "ts": 2, "type": "review_requested", "node": "gate", "options": ["approve", "reject"]});
        if person_only {
            req["person_only"] = serde_json::json!(true);
        }
        let lines = [
            serde_json::json!({"seq": 0, "ts": 1, "type": "run_started", "playbook": "p", "version": "1.0.0"}),
            req,
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        fs::write(dir.join("events.jsonl"), text).unwrap();
    }

    /// The channel entries queued for run `id`.
    fn queued(root: &Path, id: &str) -> Vec<serde_json::Value> {
        fs::read_to_string(root.join(".apb/runs").join(id).join("reviews.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn review(root: &Path, id: &str, env: &[(&str, &str)]) -> std::process::Output {
        let mut cmd = crate::common::apb_std();
        cmd.args(["review", id, "gate", "--decision", "approve"])
            .current_dir(root)
            .stdin(std::process::Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// Without a terminal the CLI is not a person's channel.
    #[test]
    fn without_a_terminal_a_person_only_gate_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r1", PERSON_ONLY, true);
        let out = review(tmp.path(), "r1", &[]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("no_terminal"), "{stderr}");
        assert!(queued(tmp.path(), "r1").is_empty());
    }

    /// A step never decides a gate of its own run, person-only or not.
    #[test]
    fn a_run_step_never_decides_its_own_runs_gate() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r1", ORDINARY, false);
        let out = review(tmp.path(), "r1", &[("APB_RUN_ID", "r1")]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("run_step_own_run"), "{stderr}");
        assert!(queued(tmp.path(), "r1").is_empty());
    }

    /// At a terminal, typing the option decides the gate through the terminal
    /// channel; typing anything else decides nothing; a run step at a terminal
    /// is still refused.
    #[cfg(target_os = "linux")]
    #[test]
    fn at_a_terminal_the_typed_option_decides_a_person_only_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = crate::common::sandbox_config_dir();
        seed(tmp.path(), "r1", PERSON_ONLY, true);
        let args = ["review", "r1", "gate", "--decision", "approve"];

        let Some((code, shown)) =
            crate::common::on_a_terminal(tmp.path(), cfg, &args, &[], b"reject\n")
        else {
            // Card B2 rests on this test: where it must run (CI, or the corpus
            // gate's `APB_CORPUS_REQUIRED=1`), a missing `script` fails it.
            let required = std::env::var_os("CI").is_some()
                || std::env::var("APB_CORPUS_REQUIRED").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "no `script` to make a terminal, and this test must run here"
            );
            eprintln!("skipped: no `script` to make a terminal");
            return;
        };
        assert_ne!(code, 0, "{shown}");
        assert!(shown.contains("Type the option to confirm"), "{shown}");
        assert!(queued(tmp.path(), "r1").is_empty(), "{shown}");

        let (code, shown) = crate::common::on_a_terminal(
            tmp.path(),
            cfg,
            &args,
            &[("APB_RUN_ID", "other")],
            b"approve\n",
        )
        .unwrap();
        assert_ne!(code, 0, "{shown}");
        assert!(shown.contains("run_step"), "{shown}");
        assert!(queued(tmp.path(), "r1").is_empty(), "{shown}");

        let (code, shown) =
            crate::common::on_a_terminal(tmp.path(), cfg, &args, &[], b"approve\n").unwrap();
        assert_eq!(code, 0, "{shown}");
        let entries = queued(tmp.path(), "r1");
        assert_eq!(entries.len(), 1, "{shown}");
        assert_eq!(entries[0]["channel"], "terminal");
        assert_eq!(entries[0]["request"], 1);
    }

    /// `apb resume --from-node` past a person-only gate no person decided:
    /// refused (3) without a terminal and inside a run step, and at a
    /// terminal only the typed node goes past it.
    #[test]
    fn a_resume_past_an_undecided_person_only_gate_needs_a_person() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "r1", PERSON_ONLY, true);
        let resume = |env: &[(&str, &str)]| {
            let mut cmd = crate::common::apb_std();
            cmd.args(["resume", "r1", "--from-node", "f"])
                .current_dir(tmp.path())
                .stdin(std::process::Stdio::null());
            for (k, v) in env {
                cmd.env(k, v);
            }
            cmd.output().unwrap()
        };
        for (env, refusal) in [
            (&[][..], "no_terminal"),
            (&[("APB_RUN_ID", "other")][..], "run_step"),
        ] {
            let out = resume(env);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(3), "{stderr}");
            assert!(
                stderr.contains("resume refused: approval_weakened: `f` skips gate `gate`"),
                "{stderr}"
            );
            assert!(stderr.contains(refusal), "{stderr}");
        }
        #[cfg(target_os = "linux")]
        {
            let cfg = crate::common::sandbox_config_dir();
            let args = ["resume", "r1", "--from-node", "f"];
            if let Some((code, shown)) =
                crate::common::on_a_terminal(tmp.path(), cfg, &args, &[], b"gate\n")
            {
                assert_eq!(code, 3, "{shown}");
                assert!(shown.contains("Type the node to confirm"), "{shown}");
                assert!(shown.contains("not confirmed"), "{shown}");
            }
        }
        let journal = fs::read_to_string(tmp.path().join(".apb/runs/r1/events.jsonl")).unwrap();
        assert!(!journal.contains("run_resumed"), "{journal}");
    }
}

/// A start whose param does not match its pattern is refused with
/// `param_pattern_mismatch`, naming the param and the pattern, and leaves
/// no run directory behind.
#[test]
fn run_refuses_a_param_that_does_not_match_its_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let out = apb_in(dir.path(), &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = dir.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: repo, type: text, pattern: '[A-Za-z0-9._-]{1,100}' }\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n",
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();

    let out = apb_in(dir.path(), &["run", "p", "--param", "repo=a b"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("run refused (param_pattern_mismatch)"),
        "{err}"
    );
    assert!(err.contains("repo ([A-Za-z0-9._-]{1,100})"), "{err}");
    let runs = dir.path().join(".apb/runs");
    assert!(!runs.exists() || fs::read_dir(&runs).unwrap().next().is_none());
}
