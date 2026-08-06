//! Structured-verdict routing (spec 2026-08-05 section 2.5, issue #74 finding
//! 5): an `output_field` edge condition parses the SOURCE node's output as JSON
//! and compares ONE top-level field against a string.
//!
//! These tests drive the whole chain the feature exists for: a stub agent writes
//! `{"status":"success","outputs":{"verdict":...}}` to `$APB_STATUS_FILE`, the
//! engine makes that compact JSON the node output, and the edge routes on one
//! field of it. Nothing here inspects the condition in isolation - the point is
//! that the status-file verdict a real agent writes is what the route reads.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

// `verify` reports a structured verdict; the two `output_field` edges route on
// it. `fix` exists only so a test can see WHICH branch the run took.
const PLAYBOOK: &str = r#"
schema: 1
id: ofld
name: OutputField
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: verify, type: agent_task, prompt: "verify the work" }
  - { id: fix, type: prompt, prompt: "fixing what verify rejected" }
  - { id: done, type: finish, outcome: success }
  - { id: repaired, type: finish, outcome: success }
edges:
  - { from: start, to: verify }
  - { from: verify, to: fix, condition: { type: output_field, node: verify, field: verdict, equals: failed } }
  - { from: verify, to: done, condition: { type: output_field, node: verify, field: verdict, equals: ok } }
  - { from: fix, to: repaired }
"#;

/// Restores `APB_AGENT_CMD` on every path out of a test, including a panic.
struct AgentCmdGuard(Option<std::ffi::OsString>);

impl AgentCmdGuard {
    fn set(prog: &str) -> Self {
        let prev = std::env::var_os("APB_AGENT_CMD");
        unsafe { std::env::set_var("APB_AGENT_CMD", prog) };
        Self(prev)
    }
}

impl Drop for AgentCmdGuard {
    fn drop(&mut self) {
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var("APB_AGENT_CMD", v),
                None => std::env::remove_var("APB_AGENT_CMD"),
            }
        }
    }
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/ofld/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/ofld/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

/// A stub agent that writes `verdict` into the status file's `outputs` object,
/// which is exactly how an author is meant to publish a structured verdict.
fn verdict_agent(root: &Path, verdict: &str) -> String {
    let path = root.join("verdict-agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nprintf '%s' '{{\"status\":\"success\",\"outputs\":\
             {{\"verdict\":\"{verdict}\",\"note\":\"details\"}}}}' > \"$APB_STATUS_FILE\"\n\
             printf 'checked\\n'\n"
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// Whether the run reached `node`. Read from `NodeFinished`, which every node
/// kind journals (a finish node has no `NodeStarted`).
fn reached(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
}

/// Runs the playbook with a stub agent reporting `verdict` and returns the log.
fn run_with_verdict(verdict: &str) -> Vec<Event> {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let _agent = AgentCmdGuard::set(&verdict_agent(dir.path(), verdict));
    let res = run(dir.path(), "ofld", None, RunOptions::default()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "the run itself must succeed for verdict `{verdict}`"
    );
    read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap()
}

// Plan step 1a: the verdict the agent wrote is `failed`, so the run must take
// the fix branch.
#[test]
fn a_failed_output_field_verdict_routes_to_the_fix_branch() {
    let events = run_with_verdict("failed");
    // The premise: the status file's outputs really became the node output.
    let output = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "verify" => {
                Some(output.clone())
            }
            _ => None,
        })
        .expect("verify must have finished");
    assert!(
        output.contains("\"verdict\":\"failed\""),
        "the status-file outputs must be the node output, got: {output}"
    );
    assert!(
        reached(&events, "fix"),
        "the `failed` verdict must route to the fix branch"
    );
    assert!(
        reached(&events, "repaired"),
        "the fix branch must run to its own finish node"
    );
    assert!(
        !reached(&events, "done"),
        "the success finish must not be reached on a `failed` verdict"
    );
}

// Plan step 1b: the same graph with an `ok` verdict takes the done branch, so
// the routing reads the FIELD and is not just "any output_field edge matches".
#[test]
fn an_ok_output_field_verdict_routes_to_done() {
    let events = run_with_verdict("ok");
    assert!(
        reached(&events, "done"),
        "the `ok` verdict must route to the success finish"
    );
    assert!(
        !reached(&events, "fix"),
        "the fix branch must not run on an `ok` verdict"
    );
}
