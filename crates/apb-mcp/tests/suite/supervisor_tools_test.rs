use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_mcp::tools::{
    ToolError, context_append, node_retry, playbook_run_supervised, run_abort, run_continue_from,
    run_pause, run_status, supervisor_capabilities, supervisor_report, supervisor_wait_event,
    sv_run_inspect,
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
fn playbook_run_supervised_no_agent_reaches_succeeded() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let res = playbook_run_supervised(
        dir.path(),
        "noagent_sv",
        None,
        params,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let run_id = res["run_id"].as_str().unwrap().to_string();

    wait_for_status(dir.path(), &run_id, "succeeded");
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

    let res = playbook_run_supervised(
        dir.path(),
        "supflow_mcp",
        None,
        BTreeMap::new(),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let run_id = res["run_id"].as_str().unwrap().to_string();

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
}

// Scenario 4: supervisor_report writes report.md, the engine's read_supervisor_report reads it.
#[test]
fn supervisor_report_write_then_read() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "noagent_sv", NOAGENT);

    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let res = playbook_run_supervised(
        dir.path(),
        "noagent_sv",
        None,
        params,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let run_id = res["run_id"].as_str().unwrap().to_string();
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
    let res = playbook_run_supervised(
        dir.path(),
        "noagent_sv",
        None,
        params,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let run_id = res["run_id"].as_str().unwrap().to_string();

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
    let res = playbook_run_supervised(
        dir.path(),
        "noagent_sv",
        None,
        params,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let run_id = res["run_id"].as_str().unwrap().to_string();

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
