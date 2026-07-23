use apb_mcp::tools::{playbook_run, run_events, run_status, runs_list};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

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

fn seed(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
}

#[test]
fn run_then_inspect() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run = playbook_run(
        dir.path(),
        "noagent",
        None,
        params,
        None,
        None,
        None,
        None,
        Default::default(),
        Default::default(),
        None,
    )
    .unwrap();
    assert_eq!(run["outcome"], "succeeded");
    let run_id = run["run_id"].as_str().unwrap().to_string();

    let listed = runs_list(dir.path()).unwrap();
    assert_eq!(listed[0]["run_id"], run_id.as_str());

    let status = run_status(dir.path(), &run_id).unwrap();
    assert_eq!(status["run_status"], "succeeded");
    assert_eq!(status["nodes"]["note"], "succeeded");

    let ev = run_events(dir.path(), &run_id, None).unwrap();
    assert!(ev["events"].as_array().unwrap().len() >= 3);
    // pagination: from_seq cuts off earlier events
    let ev2 = run_events(dir.path(), &run_id, Some(2)).unwrap();
    let first_seq = ev2["events"][0]["seq"].as_u64().unwrap();
    assert!(first_seq >= 2);
}

#[test]
fn runs_list_and_status_expose_lineage_fields() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let first = playbook_run(
        dir.path(),
        "noagent",
        None,
        params.clone(),
        None,
        None,
        None,
        None,
        Default::default(),
        Default::default(),
        None,
    )
    .unwrap();
    let first_id = first["run_id"].as_str().unwrap().to_string();

    let second = playbook_run(
        dir.path(),
        "noagent",
        None,
        params,
        None,
        None,
        None,
        None,
        Default::default(),
        Default::default(),
        Some(first_id.clone()),
    )
    .unwrap();
    let second_id = second["run_id"].as_str().unwrap().to_string();

    let listed = runs_list(dir.path()).unwrap();
    let pred = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_id"] == first_id)
        .unwrap();
    let succ = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_id"] == second_id)
        .unwrap();
    assert_eq!(pred["superseded_by"], second_id);
    assert_eq!(succ["continued_from"], first_id);

    let pred_status = run_status(dir.path(), &first_id).unwrap();
    let succ_status = run_status(dir.path(), &second_id).unwrap();
    assert_eq!(pred_status["superseded_by"], second_id);
    assert_eq!(succ_status["continued_from"], first_id);
}

#[test]
fn status_unknown_run_is_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    assert!(run_status(dir.path(), "ghost-1").is_err());
}

#[test]
fn resume_unknown_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = apb_mcp::tools::run_resume(dir.path(), "ghost-1", None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn status_traversal_run_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    for bad in ["../../etc", "/etc", "..", "a/b"] {
        let err = run_status(dir.path(), bad).unwrap_err();
        assert!(
            matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
            "id {bad:?}: expected NotFound, got {err:?}"
        );
    }
}

#[test]
fn events_traversal_run_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = run_events(dir.path(), "../../etc", None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

/// Same linear playbook as `apb_engine::progress::tests::linear_pb` (weights
/// 100 and 300 by `expected_duration`), so the expected percent below is
/// independently anchored to that module's own `weights_by_expected_seconds`
/// test rather than re-derived here.
const PROGRESS_PB: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }
  - { id: b, type: agent_task, prompt: hi, expected_duration: 300 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: b }
  - { from: b, to: f }
"#;

/// A bare run directory - `run_status` only reads `runs/<id>/{events.jsonl,
/// playbook.yaml}` (via `resolve_run_dir`), no registry entry needed. Mirrors
/// the fixture style already used by `apb_mcp::tools::progress_tests::
/// run_progress_report_posts_a_command`.
fn bare_run_dir(root: &Path, run_id: &str) -> std::path::PathBuf {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    run_dir
}

/// Populated path: a run dir with a playbook.yaml snapshot (nodes carrying
/// `expected_duration`) and an events.jsonl reporting node `a` succeeded.
/// `run_status`'s "progress" key must carry the computed summary, with the
/// full `{ percent, label, waiting_on }` shape present.
#[test]
fn run_status_progress_reflects_expected_duration_weighting() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r-progress");
    fs::write(run_dir.join("playbook.yaml"), PROGRESS_PB).unwrap();
    fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n",
    )
    .unwrap();

    let status = run_status(dir.path(), "r-progress").unwrap();
    let progress = &status["progress"];
    assert!(
        !progress.is_null(),
        "expected a progress object, got {status}"
    );
    // a (100s) done of a+b (400s) total = 25%.
    assert_eq!(progress["percent"], 25);
    assert!(
        progress.get("label").is_some(),
        "label field must be present in the progress shape"
    );
    assert!(
        progress.get("waiting_on").is_some(),
        "waiting_on field must be present in the progress shape"
    );
    assert!(
        progress.get("waiting_kind").is_some(),
        "waiting_kind field must be present in the progress shape"
    );
    // plan_key is the work-plan identity the web uses as its reset signal; it
    // is always a string (version plus cyclic-group totals), never null.
    assert!(
        progress.get("plan_key").and_then(|v| v.as_str()).is_some(),
        "plan_key must be present as a string in the progress shape"
    );
    // No RunProgress event and no human_review/wait node running - all three
    // default to null, but the KEYS must still be present (asserted above).
    assert!(progress["label"].is_null());
    assert!(progress["waiting_on"].is_null());
    assert!(progress["waiting_kind"].is_null());
}

/// A run parked on a human_review gate (issue #42 finding 4): `run_status`
/// must expose a first-class `pending_review` block naming the node, its
/// options, an owner-facing instruction, and how to decide, so an intermediary
/// that reads status is forced to see the pending decision.
const REVIEW_GATE_PB: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: gate, type: human_review, title: "Approve the release", options: [approved, rejected] }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: gate }
  - { from: gate, to: f }
"#;

#[test]
fn run_status_exposes_pending_review_block_for_a_gate() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r-gate");
    fs::write(run_dir.join("playbook.yaml"), REVIEW_GATE_PB).unwrap();
    // A ReviewRequested with no matching ReviewDecided: the run is waiting on
    // the gate. The instruction in the block is rebuilt from the snapshot, so
    // the fixture event need not carry it.
    fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":0,\"type\":\"review_requested\",\"node\":\"gate\",\"options\":[\"approved\",\"rejected\"]}\n",
    )
    .unwrap();

    let status = run_status(dir.path(), "r-gate").unwrap();
    // The progress summary flags the wait as a human_review gate.
    assert_eq!(status["progress"]["waiting_on"], "gate");
    assert_eq!(status["progress"]["waiting_kind"], "human_review");
    // The first-class block, lifted to the top level like pending_question.
    let pr = &status["pending_review"];
    assert!(
        !pr.is_null(),
        "expected a pending_review block, got {status}"
    );
    assert_eq!(pr["node"], "gate");
    assert_eq!(pr["title"], "Approve the release");
    assert!(status["pending_supervisor"].is_null());
    assert_eq!(
        pr["options"],
        serde_json::json!(["approved", "rejected"]),
        "got {pr}"
    );
    let instruction = pr["instruction"].as_str().expect("instruction is a string");
    assert!(
        instruction.contains("Approve the release"),
        "got: {instruction}"
    );
    assert!(instruction.contains("approved"), "got: {instruction}");
    assert!(instruction.contains("apb review"), "got: {instruction}");
    assert!(instruction.contains("review_decide"), "got: {instruction}");
    let how_to = pr["how_to_decide"]
        .as_str()
        .expect("how_to_decide is a string");
    assert!(how_to.contains("apb review"), "got: {how_to}");
    assert!(how_to.contains("review_decide"), "got: {how_to}");
}

/// Null path: a run dir with events.jsonl but no playbook.yaml snapshot (e.g.
/// a legacy run whose snapshot was never captured) must report
/// `"progress": null`, not omit the key or error.
#[test]
fn run_status_progress_is_null_without_playbook_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r-no-snapshot");
    fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();

    let status = run_status(dir.path(), "r-no-snapshot").unwrap();
    assert!(
        status["progress"].is_null(),
        "expected progress: null without a playbook snapshot, got {status}"
    );
}

#[test]
fn run_status_carries_answer_key() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let started = playbook_run(
        dir.path(),
        "noagent",
        None,
        params,
        None,
        None,
        None,
        None,
        Default::default(),
        Default::default(),
        None,
    )
    .unwrap();
    let run_id = started["run_id"].as_str().unwrap();
    let status = run_status(dir.path(), run_id).unwrap();
    assert!(status.get("answer").is_some(), "answer key present");
    assert!(
        status["answer"].is_null(),
        "no-prompt finish -> null answer"
    );
}

#[test]
fn run_status_children_empty_for_childless_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let started = playbook_run(
        dir.path(),
        "noagent",
        None,
        params,
        None,
        None,
        None,
        None,
        Default::default(),
        Default::default(),
        None,
    )
    .unwrap();
    let status = run_status(dir.path(), started["run_id"].as_str().unwrap()).unwrap();
    assert_eq!(status["children"].as_array().unwrap().len(), 0);
}

/// Task 8 smoke: the `run_stop` tool finalizes a run whose driver is gone (the
/// engine fixture from `stop_run_test.rs`, rebuilt here through the public
/// event API) and reports the outcome it took.
#[test]
fn run_stop_finalizes_a_run_whose_driver_is_gone() {
    use apb_engine::event::{EventLog, EventPayload, read_all};

    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_dir = dir.path().join(".apb/runs/noagent-dead");
    fs::create_dir_all(&run_dir).unwrap();
    let mut log = EventLog::create(&run_dir).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "noagent".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::NodeStarted {
        node: "note".into(),
        attempt: 1,
    })
    .unwrap();
    drop(log);

    let out = apb_mcp::tools::run_stop(dir.path(), "noagent-dead").unwrap();
    assert_eq!(out["outcome"], "finalized_dead_run");
    assert!(
        read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunAborted { .. })),
        "run_stop must have finalized the abandoned run"
    );

    // Idempotent: the run is terminal now, so a second stop is a no-op.
    let again = apb_mcp::tools::run_stop(dir.path(), "noagent-dead").unwrap();
    assert_eq!(again["outcome"], "already_terminal");
}

// ---------------------------------------------------------------------------
// Task 9: liveness in run_status.
//
// The incident these cover: a crashed attempt kept reading `running` for 19
// minutes, and `run_status` carried no timestamps at all, so "is it stuck or
// working?" was unanswerable from the API. The journal is hand-built here so
// the pid under test is chosen rather than observed: no fixture can reliably
// produce a *dead* pid otherwise.
// ---------------------------------------------------------------------------

/// Epoch milliseconds, for building a journal whose timestamps are recent
/// enough that `attempt_age_ms` is a small number.
fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// A real pid that is reliably absent: a child spawned, waited for and reaped,
/// so the number was genuinely valid and is now free.
///
/// Deliberately not `u32::MAX`. An impossible pid exercises the invalid-pid
/// rejection (tested directly in `apb_engine::liveness`) rather than the
/// stale-holder property these fixtures are about, and using one here hid a
/// real bug: `u32::MAX` narrows to `-1`, the `kill(2)` "every process"
/// wildcard, so the probe reported it as running.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn a throwaway child to borrow a pid from");
    let pid = child.id();
    // Bounded by construction: `exit 0` cannot fail to exit.
    child.wait().expect("reap the throwaway child");
    pid
}

/// A short-lived child that exists only to lend the test a real, live,
/// foreign pid. Killed and waited for on drop, so no path out of the test
/// leaks a process or leaves a zombie holding the pid (a zombie would probe
/// as `NotFound` and silently invert what the test is asserting).
struct Sleeper(std::process::Child);

impl Sleeper {
    fn spawn() -> Self {
        Sleeper(
            std::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("`sleep` must be available to spawn a live pid"),
        )
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn run_status_reports_a_dead_attempt_as_lost_with_node_times() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r-lost");
    let dead = dead_pid();
    // `a` opened an attempt under a pid that is not running and never wrote
    // `attempt_finished`: the crashed-attempt shape. `b` ran and finished
    // normally, so it must carry timings without any attempt fields.
    fs::write(
        run_dir.join("events.jsonl"),
        format!(
            "{{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}}\n\
             {{\"seq\":1,\"ts\":3000,\"type\":\"node_started\",\"node\":\"b\",\"attempt\":1}}\n\
             {{\"seq\":2,\"ts\":4000,\"type\":\"node_finished\",\"node\":\"b\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}}\n\
             {{\"seq\":3,\"ts\":5000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}}\n\
             {{\"seq\":4,\"ts\":6000,\"type\":\"attempt_started\",\"node\":\"a\",\"attempt\":1,\"agent\":\"stub\",\"pid\":{dead}}}\n"
        ),
    )
    .unwrap();

    let status = run_status(dir.path(), "r-lost").unwrap();

    // The headline: a running node whose journaled attempt pid is dead is no
    // longer reported as `running`.
    assert_eq!(
        status["nodes"]["a"], "lost",
        "a running node with a dead attempt pid must report `lost`, got {status}"
    );
    assert_eq!(status["nodes"]["b"], "succeeded");

    // No driver.pid at all: unknown, not false.
    assert!(
        status.get("driver_alive").is_some(),
        "driver_alive key must always be present, got {status}"
    );
    assert!(
        status["driver_alive"].is_null(),
        "driver_alive must be null without a driver.pid, got {status}"
    );

    let times = &status["node_times"];
    assert_eq!(times["a"]["started_ms"], 5000);
    assert_eq!(times["a"]["attempt_pid"], dead);
    assert!(
        times["a"]["attempt_age_ms"].as_u64().is_some(),
        "an open attempt must carry an age, got {times}"
    );
    // A finished node keeps its start time but has no open attempt.
    assert_eq!(times["b"]["started_ms"], 3000);
    assert!(
        times["b"]["attempt_age_ms"].is_null(),
        "a node with no open attempt must report a null age, got {times}"
    );
    assert!(times["b"]["attempt_pid"].is_null());
}

#[test]
fn run_status_reports_a_live_attempt_and_driver() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r-live");

    // A real, live, foreign pid for the attempt: the probe must resolve it
    // through the process table, which a fabricated number cannot exercise.
    // The guard reaps it however this test leaves the scope, so a panicking
    // assertion cannot leak a child or leave a zombie holding the pid.
    let sleeper = Sleeper::spawn();
    let live_pid = sleeper.pid();

    // A drive running on a thread of THIS process is the in-process drive
    // case, and our own pid is by definition a live driver.
    fs::write(
        run_dir.join("driver.pid"),
        std::process::id().to_string().as_bytes(),
    )
    .unwrap();

    let started = now_ms();
    fs::write(
        run_dir.join("events.jsonl"),
        format!(
            "{{\"seq\":0,\"ts\":{started},\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}}\n\
             {{\"seq\":1,\"ts\":{started},\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}}\n\
             {{\"seq\":2,\"ts\":{started},\"type\":\"attempt_started\",\"node\":\"a\",\"attempt\":1,\"agent\":\"stub\",\"pid\":{live_pid}}}\n"
        ),
    )
    .unwrap();

    let first = run_status(dir.path(), "r-live").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    let second = run_status(dir.path(), "r-live").unwrap();

    assert_eq!(
        first["driver_alive"], true,
        "our own pid in driver.pid must read as a live driver, got {first}"
    );
    // Issue #45 finding 9: a live open attempt must report as running (never
    // the pure-fold crash shape `interrupted`, and never `lost`).
    assert_eq!(
        first["nodes"]["a"], "running",
        "a node whose attempt pid is alive must report running, got {first}"
    );
    assert_eq!(
        first["run_status"], "running",
        "a run with a live open attempt must report running, got {first}"
    );
    assert_eq!(first["node_times"]["a"]["attempt_pid"], live_pid);

    let age_one = first["node_times"]["a"]["attempt_age_ms"]
        .as_u64()
        .expect("a live open attempt must carry an age");
    let age_two = second["node_times"]["a"]["attempt_age_ms"]
        .as_u64()
        .expect("a live open attempt must carry an age");
    assert!(
        age_two > age_one,
        "attempt_age_ms must grow while the attempt runs ({age_one} -> {age_two})"
    );
}

/// Issue #45 finding 4: a supervised failure wake must surface on run_status
/// as waiting_kind=supervisor with a first-class pending_supervisor block.
#[test]
fn run_status_exposes_pending_supervisor_after_failure_wake() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r-wake");
    fs::write(
        run_dir.join("playbook.yaml"),
        r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: work, type: agent_task, prompt: hi }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: work }
  - { from: work, to: f }
"#,
    )
    .unwrap();
    fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":1,\"type\":\"node_finished\",\"node\":\"work\",\"status\":\"failed\",\"attempt\":1,\"output\":\"boom\"}\n\
         {\"seq\":2,\"ts\":2,\"type\":\"wake_raised\",\"trigger\":\"node_failed\",\"node\":\"work\",\"detail\":\"boom\"}\n",
    )
    .unwrap();

    let status = run_status(dir.path(), "r-wake").unwrap();
    assert_eq!(status["run_status"], "running");
    assert_eq!(status["progress"]["waiting_on"], "work");
    assert_eq!(status["progress"]["waiting_kind"], "supervisor");
    let ps = &status["pending_supervisor"];
    assert!(!ps.is_null(), "expected pending_supervisor, got {status}");
    assert_eq!(ps["node"], "work");
    assert_eq!(ps["trigger"], "node_failed");
    assert!(
        ps["options"]
            .as_array()
            .is_some_and(|o| o.iter().any(|v| v == "retry")
                && o.iter().any(|v| v == "continue_from")
                && o.iter().any(|v| v == "abort")),
        "options must name retry/continue_from/abort, got {ps}"
    );
}

/// Issue #45 finding 10: a parent-driven child with a live parent reports
/// driver_alive true even with no (or a stale) driver.pid of its own.
#[test]
fn run_status_parent_driven_child_reports_driver_alive() {
    let dir = tempfile::tempdir().unwrap();
    let parent_dir = bare_run_dir(dir.path(), "parent-1");
    let child_dir = bare_run_dir(dir.path(), "child-1");
    fs::write(
        parent_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();
    fs::write(
        child_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"c\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();
    fs::write(
        parent_dir.join("driver.pid"),
        std::process::id().to_string().as_bytes(),
    )
    .unwrap();
    apb_engine::run_config::write_run_config(
        &child_dir,
        &apb_engine::run_config::RunConfig {
            parent_run: Some("parent-1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(child_dir.join("driven_by"), b"parent-1").unwrap();

    let status = run_status(dir.path(), "child-1").unwrap();
    assert_eq!(
        status["driver_alive"], true,
        "parent-driven child with live parent must report driver_alive true, got {status}"
    );
}
