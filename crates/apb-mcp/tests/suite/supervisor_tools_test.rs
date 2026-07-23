use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_mcp::tools::{
    ToolError, context_append, interrupt_attempt, node_retry, playbook_run_supervised, run_abort,
    run_continue_from, run_pause, run_status, supervisor_capabilities, supervisor_report,
    supervisor_wait_event, sv_run_inspect,
};

// Cargo runs #[test] functions in parallel within one process, so tests
// that mutate the shared APB_AGENT_CMD environment variable race each other unless
// access is serialized. Serialized on the shared lock (see suite/common.rs),
// since consolidation means other modules' tests run as threads in this same
// process too.
use crate::common::env_lock;

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(20);

/// Polls `f` until it returns Some(..) or the deadline elapses; otherwise panics
/// with a clear message instead of hanging.
fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if start.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for: {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Starts the same supervised run `playbook_run_supervised` starts, but on a
/// thread of THIS process.
///
/// Since Task 7 the tool hands the drive to a detached `apb __drive-run`
/// process re-exec'd from `current_exe()`, which in a test binary is the test
/// harness rather than the `apb` binary - so no real driver comes up here. The
/// scenarios below are about the supervisor TOOLS (wake, retry, report,
/// continue-from) and need a run that actually moves, so they start one
/// through the engine with the options the tool would have used. The tool's
/// own launch path is covered end-to-end against the real binary in
/// `apb-cli/tests/suite/detached_driver_test.rs`.
fn start_supervised_in_process(root: &Path, id: &str, params: BTreeMap<String, String>) -> String {
    apb_engine::run_background(
        root,
        id,
        None,
        apb_engine::RunOptions {
            params,
            mode: apb_engine::RunMode::Supervised,
            ..Default::default()
        },
    )
    .unwrap()
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

// Stub: fails on the first call, leaves a marker file, succeeds on every subsequent call.
// Same trick as in supervised_drive_test.rs::flaky_agent.
fn flaky_agent(dir: &Path) -> String {
    let marker = dir.join("mcp_flaky.marker");
    let path = dir.join("mcp_flaky.sh");
    let body = format!(
        "#!/bin/sh\nif [ -f '{m}' ]; then echo ok; exit 0; else touch '{m}'; echo firstfail 1>&2; exit 1; fi\n",
        m = marker.display()
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

fn seed(root: &Path, id: &str, yaml: &str) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
}

const NOAGENT: &str = r#"
schema: 1
id: noagent_sv
name: No Agent Supervised
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

// The only unconditional edge is `work -> done`: in supervised mode a node
// failure does not go to next_node, it raises a wake and waits for a command.
const WF_SUPERVISED: &str = r#"
schema: 1
id: supflow_mcp
name: Supervised MCP
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn wait_for_status(root: &Path, run_id: &str, expected: &str) {
    poll_until(&format!("run_status == {expected:?}"), || {
        let status = run_status(root, run_id).ok()?;
        if status["run_status"] == expected {
            Some(())
        } else {
            None
        }
    });
}

// Scenario 1: a playbook without agent_task in supervised mode - the mode only
// affects the failure path, so the run should still reach succeeded as usual.
#[test]
fn supervised_no_agent_run_reaches_succeeded() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run_id = start_supervised_in_process(dir.path(), "noagent_sv", params);

    wait_for_status(dir.path(), &run_id, "succeeded");
}

// Scenario 1b: what `playbook_run_supervised` itself owns since Task 7. It
// prepares the run fully in-process - registry, validation, run dir, manifest
// snapshot - and only then hands the drive to a detached driver, returning the
// run_id at once. The drive is NOT expected to happen here (the driver child
// is re-exec'd from `current_exe()`, which in this test binary is the harness),
// so the assertions stop at the handoff boundary.
#[test]
fn playbook_run_supervised_prepares_the_run_and_hands_off_the_drive() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let started = Instant::now();
    let res = playbook_run_supervised(
        dir.path(),
        "noagent_sv",
        None,
        BTreeMap::new(),
        None,
        None,
        None,
        None,
        Default::default(),
        Default::default(),
        None,
    )
    .unwrap();
    let elapsed = started.elapsed();

    let run_id = res["run_id"].as_str().expect("run_id in response");
    assert!(
        run_id.starts_with("noagent_sv-"),
        "unexpected run_id: {run_id}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a supervised start must not block on the run, took {elapsed:?}"
    );

    // The run is prepared on disk before the handoff: run dir, run config, and
    // the journal's start-up events are all the detached driver ever gets.
    let run_dir = dir.path().join(".apb/runs").join(run_id);
    assert!(
        run_dir.is_dir(),
        "run dir must exist after the tool returns"
    );
    // The run config must carry the MODE across, not just exist: the detached
    // driver re-opens the run from disk and has no other way to learn it is
    // supervised. A regression that dropped `mode` on the handoff path would
    // otherwise ship green, and the run would silently drive autonomously -
    // taking its own fallbacks instead of waking the supervisor.
    let cfg = fs::read_to_string(run_dir.join("run.yaml")).expect("run.yaml");
    assert!(
        cfg.contains("mode: supervised"),
        "the run config must record the supervised mode for the detached driver, got:\n{cfg}"
    );
    assert!(
        run_dir.join("playbook.yaml").is_file(),
        "the playbook snapshot must be written before the drive is handed off"
    );
}

// Scenario 2: agent_task fails on the first call, succeeds on the second. We wait for a wake via
// supervisor_wait_event, send context_append and node_retry, wait for succeeded,
// and check that sv_run_inspect contains non-empty wakes and actions.
#[test]
fn supervised_wake_context_append_and_retry_recovers() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "supflow_mcp", WF_SUPERVISED);

    let prog = flaky_agent(dir.path());
    let _env = env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let run_id = start_supervised_in_process(dir.path(), "supflow_mcp", BTreeMap::new());

    let wake = poll_until("a non-null wake from supervisor_wait_event", || {
        let out = supervisor_wait_event(dir.path(), &run_id, None, Some(2_000)).unwrap();
        if out["wake"].is_null() {
            None
        } else {
            Some(out["wake"].clone())
        }
    });
    let node = wake["node"].as_str().unwrap().to_string();

    context_append(dir.path(), &run_id, "note X").unwrap();
    node_retry(dir.path(), &run_id, &node, None).unwrap();

    // Keep APB_AGENT_CMD set until the background drive thread reaches
    // succeeded: the second agent invocation (after retry) reads the environment variable
    // at the moment it executes, not at the moment the command is posted, so
    // removing it earlier would race the background thread (see supervised_drive_test.rs).
    wait_for_status(dir.path(), &run_id, "succeeded");

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    let inspect = sv_run_inspect(dir.path(), &run_id).unwrap();
    assert!(
        !inspect["wakes"].as_array().unwrap().is_empty(),
        "expected non-empty wakes in sv_run_inspect, got {inspect:?}"
    );
    assert!(
        !inspect["actions"].as_array().unwrap().is_empty(),
        "expected non-empty actions in sv_run_inspect, got {inspect:?}"
    );
}

// Scenario 3: directory traversal in run_id must yield NotFound in all tools.
#[test]
fn traversal_run_id_is_not_found_in_all_tools() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let bad = "../../etc";

    let err = supervisor_wait_event(dir.path(), bad, None, Some(50)).unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "supervisor_wait_event: expected NotFound, got {err:?}"
    );

    let err = sv_run_inspect(dir.path(), bad).unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "sv_run_inspect: expected NotFound, got {err:?}"
    );

    let err = node_retry(dir.path(), bad, "work", None).unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "node_retry: expected NotFound, got {err:?}"
    );

    let err = run_pause(dir.path(), bad).unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "run_pause: expected NotFound, got {err:?}"
    );

    let err = run_abort(dir.path(), bad).unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "run_abort: expected NotFound, got {err:?}"
    );

    let err = context_append(dir.path(), bad, "note").unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "context_append: expected NotFound, got {err:?}"
    );

    let err = supervisor_report(dir.path(), bad, "text").unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "supervisor_report: expected NotFound, got {err:?}"
    );

    let err = run_continue_from(dir.path(), bad, "work").unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "run_continue_from traversal: expected NotFound, got {err:?}"
    );

    let err = interrupt_attempt(dir.path(), bad, None).unwrap_err();
    assert!(
        matches!(err, ToolError::NotFound(_)),
        "interrupt_attempt traversal: expected NotFound, got {err:?}"
    );
}

// Scenario 5b (finding 7 of issue #42, third item of issue #40): the interrupt
// tool posts a `Control::Interrupt` command onto an existing run's control
// channel and returns its seq. The drive is what acts on it (SIGKILL of the
// running attempt); the tool's job is only to post, single-writer.
#[test]
fn interrupt_attempt_posts_command() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run_id = start_supervised_in_process(dir.path(), "noagent_sv", params);

    let v = interrupt_attempt(dir.path(), &run_id, Some("wedged")).unwrap();
    assert!(
        v["posted_seq"].is_u64(),
        "interrupt_attempt should return posted_seq, got {v:?}"
    );

    let control = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("control.jsonl"),
    )
    .unwrap();
    assert!(
        control
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .any(|v| v.get("cmd").and_then(|c| c.as_str()) == Some("interrupt")),
        "control.jsonl must contain an interrupt command, got:\n{control}"
    );
}

// Scenario 4: supervisor_report writes report.md, the engine's read_supervisor_report reads it.
#[test]
fn supervisor_report_write_then_read() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run_id = start_supervised_in_process(dir.path(), "noagent_sv", params);
    wait_for_status(dir.path(), &run_id, "succeeded");

    supervisor_report(dir.path(), &run_id, "final summary").unwrap();

    let read = apb_engine::read_supervisor_report(dir.path(), &run_id).unwrap();
    assert!(read.unwrap().contains("final summary"));
}

// Scenario 5: run_continue_from posts a command onto an existing run.
#[test]
fn run_continue_from_posts_command() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run_id = start_supervised_in_process(dir.path(), "noagent_sv", params);

    let v = run_continue_from(dir.path(), &run_id, "note").unwrap();
    assert!(
        v["posted_seq"].is_u64(),
        "run_continue_from should return posted_seq, got {v:?}"
    );
}

// Scenario 6: supervisor_capabilities with a scalar string in capabilities.
// The author wrote `capabilities: observe` instead of `capabilities: [observe]`,
// the function should return exactly `["observe"]`, not the default `["observe", "retry"]`.
#[test]
fn supervisor_capabilities_scalar_string_returns_single_element() {
    let dir = tempfile::tempdir().unwrap();
    let yaml_with_scalar = r#"
schema: 1
id: scalar_caps
name: Scalar Capabilities
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
supervisor:
  policy:
    capabilities: observe
"#;
    seed(dir.path(), "scalar_caps", yaml_with_scalar);

    let caps = supervisor_capabilities(dir.path(), "scalar_caps", None).unwrap();
    assert_eq!(
        caps,
        vec!["observe".to_string()],
        "scalar string 'observe' should produce exactly one element, got {caps:?}"
    );
}

// Scenario 7: supervisor_capabilities with a sequence (the traditional path).
#[test]
fn supervisor_capabilities_sequence_returns_as_is() {
    let dir = tempfile::tempdir().unwrap();
    let yaml_with_sequence = r#"
schema: 1
id: seq_caps
name: Sequence Capabilities
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
supervisor:
  policy:
    capabilities: [observe, retry]
"#;
    seed(dir.path(), "seq_caps", yaml_with_sequence);

    let caps = supervisor_capabilities(dir.path(), "seq_caps", None).unwrap();
    assert_eq!(
        caps,
        vec!["observe".to_string(), "retry".to_string()],
        "sequence [observe, retry] should be returned as-is, got {caps:?}"
    );
}

// Scenario 8: supervisor_capabilities with no capabilities key at all returns the default.
#[test]
fn supervisor_capabilities_absent_returns_default() {
    let dir = tempfile::tempdir().unwrap();
    let yaml_no_caps = r#"
schema: 1
id: no_caps
name: No Capabilities
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
supervisor:
  policy: {}
"#;
    seed(dir.path(), "no_caps", yaml_no_caps);

    let caps = supervisor_capabilities(dir.path(), "no_caps", None).unwrap();
    assert_eq!(
        caps,
        vec![
            "observe".to_string(),
            "retry".to_string(),
            "rebind".to_string(),
            "patch_playbook".to_string()
        ],
        "absent capabilities key should return default (all implemented), got {caps:?}"
    );
}

// Scenario 9: supervisor_capabilities with an empty sequence - nothing is returned.
#[test]
fn supervisor_capabilities_empty_sequence_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let yaml_empty_seq = r#"
schema: 1
id: empty_caps
name: Empty Capabilities
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
supervisor:
  policy:
    capabilities: []
"#;
    seed(dir.path(), "empty_caps", yaml_empty_seq);

    let caps = supervisor_capabilities(dir.path(), "empty_caps", None).unwrap();
    assert_eq!(
        caps,
        Vec::<String>::new(),
        "empty sequence [] should produce empty list (deny all), got {caps:?}"
    );
}

// Scenario 10: the engine half of the disk fallback - a write_supervisor_session
// call made in one shot (as WfMcp::mint_token does) must resolve via
// find_session_by_token independent of any in-memory
// table. Resolution by the MCP server itself with an empty in-memory table and the actual
// #[tool] call are checked separately in crates/apb-mcp/src/server.rs (where
// resolve_session is a private function, accessible only to tests in that
// same file).
#[test]
fn write_supervisor_session_is_findable_without_in_memory_table() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run_id = start_supervised_in_process(dir.path(), "noagent_sv", params);

    apb_engine::write_supervisor_session(
        dir.path(),
        &run_id,
        "sv-disk-1",
        &["observe".to_string(), "retry".to_string()],
    )
    .unwrap();

    let found = apb_engine::find_session_by_token(dir.path(), "sv-disk-1").unwrap();
    let (found_run_id, caps) = found.expect("expected sv-disk-1 to resolve from disk");
    assert_eq!(found_run_id, run_id);
    assert_eq!(caps, vec!["observe".to_string(), "retry".to_string()]);
}
