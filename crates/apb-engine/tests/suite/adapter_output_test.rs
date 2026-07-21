//! Node-output semantics (issue #42, findings 1 and 6).
//!
//! The engine must use the agent's full reply body (with the trailing report
//! block stripped) as the node output, never the one-line report summary; a
//! signal-terminated attempt is a failure regardless of exit code or report
//! block; and a success with completely empty output raises an anomaly.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, WakeTrigger, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

fn write_stub(root: &Path, name: &str, body: &str) -> String {
    let path = root.join(name);
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path, id: &str, playbook: &str) {
    init_project(root).unwrap();
    let dir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
    common::seed_main(root);
}

// An `output_match` edge on the body content (a line the agent wrote ABOVE its
// report block). This is the regression for the "node has no outgoing edge"
// death: if the node output were the one-line summary, the token would live
// only in the body and the edge would never match.
const OUTPUT_MATCH_PLAYBOOK: &str = r#"
schema: 1
id: om
name: OutputMatch
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: output_match, node: w, pattern: "MAGIC_TOKEN" } }
  - { from: w, to: no, fallback: true }
"#;

#[test]
fn output_match_reads_the_body_not_the_summary() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "om", OUTPUT_MATCH_PLAYBOOK);
    // The token appears ONLY in the body; the report block's summary is an
    // unrelated string. Before the fix the node output was the summary, so
    // `output_match` on MAGIC_TOKEN never matched and the run died with no
    // outgoing edge.
    let prog = write_stub(
        dir.path(),
        "body-agent.sh",
        "printf 'here is the MAGIC_TOKEN in the body\\n'\nprintf '```yaml\\nstatus: success\\nsummary: an unrelated summary line\\n```\\n'",
    );
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "om", None, RunOptions::default());
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    let res = res.unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "output_match on body content must take the ok edge, not die with no outgoing edge"
    );

    // The stored node output is the body verbatim with the report block gone.
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let output = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "w" => Some(output.clone()),
            _ => None,
        })
        .expect("node w finished");
    assert!(
        output.contains("MAGIC_TOKEN"),
        "node output must preserve the body verbatim: {output:?}"
    );
    assert!(
        !output.contains("an unrelated summary line") && !output.contains("status:"),
        "the report block and its summary must not be in the node output: {output:?}"
    );
}

// Branch on node_status so a FAILED w has no plain edge to escape through: it
// takes the fallback to a failure finish (run Failed), while a succeeded w takes
// the ok edge (run Succeeded).
const BRANCH_PLAYBOOK: &str = r#"
schema: 1
id: sa
name: SingleAgent
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

// A signal-terminated attempt is a FAILURE even when it printed a valid success
// report block first (issue #42, finding 6). A wrapper that turned SIGTERM into
// a 0 exit, or a kill that emptied stdout, must never be journaled as success.
#[cfg(unix)]
#[test]
fn signal_terminated_attempt_is_a_failed_attempt() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sa", BRANCH_PLAYBOOK);
    // Print a full success report block, THEN kill self with SIGTERM: the
    // process is signal-terminated, so the attempt must be failed despite the
    // success block.
    let prog = write_stub(
        dir.path(),
        "signal-agent.sh",
        "printf 'did work\\n'\nprintf '```yaml\\nstatus: success\\nsummary: ok\\n```\\n'\nkill -TERM $$",
    );
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sa", None, RunOptions::default());
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    let res = res.unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "a signal-terminated agent must fail the run, not succeed"
    );

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let statuses: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished { node, status, .. } if node == "w" => {
                Some(status.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        !statuses.is_empty() && statuses.iter().all(|s| s == "failed"),
        "the signal-terminated attempt must be journaled failed, got {statuses:?}"
    );
    assert!(
        !statuses.iter().any(|s| s == "succeeded"),
        "a signal-terminated attempt must never be journaled succeeded: {statuses:?}"
    );
}

// A success with completely empty output raises a WakeRaised anomaly, visible in
// the event log (issue #42, finding 6). The node still succeeds; the emptiness
// is flagged, reusing the same anomaly mechanism as the stall watch.
#[test]
fn empty_output_success_raises_an_anomaly() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "sa", BRANCH_PLAYBOOK);
    // The reply is nothing but a success report block: the body (node output)
    // is empty.
    let prog = write_stub(
        dir.path(),
        "empty-agent.sh",
        "printf '```yaml\\nstatus: success\\nsummary: nothing to show\\n```\\n'",
    );
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "sa", None, RunOptions::default());
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    let res = res.unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "empty output stays succeeded; the anomaly is a signal, not a failure"
    );

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let anomaly = events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::WakeRaised { trigger: WakeTrigger::Anomaly, node, detail }
                if node == "w" && detail.contains("empty output")
        )
    });
    assert!(
        anomaly,
        "an empty-output success must journal a WakeRaised anomaly for node w"
    );
}
