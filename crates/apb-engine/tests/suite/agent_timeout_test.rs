use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

// agent_task with timeout_seconds: 1 and an agent sleeping 5s. The engine must kill
// the process on timeout (~1s), mark the attempt timed_out, the node - TimedOut,
// and steer the run down the failure branch.
const PLAYBOOK: &str = r#"
schema: 1
id: to
name: Timeout
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do", timeout_seconds: 1 }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: ok, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: no, fallback: true }
"#;

/// The stub sleeps past its node's `timeout_seconds`, then leaves a marker. The
/// marker is the kill evidence: if the engine ever let the process run to the
/// end of its sleep, the file exists.
///
/// This replaces an earlier wall-clock assertion (`run()` had to return in under
/// 3s while the stub slept 5s). That bound was not a property of the engine: on
/// this project's macOS machines the per-launch security scan of a freshly
/// written `sh` stub (BUILD-OPTIMIZATION rule 8) measured 3.9s to 53.5s of pure
/// spawn and kill stall on an otherwise idle tree, which failed the 3s budget in
/// 5 of 8 isolated runs while the engine behaved correctly every time. A marker
/// file bounds the same claim by construction instead of by clock.
fn write_slow_agent(root: &Path, marker: &Path) -> String {
    let path = root.join("slow-agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nsleep 5\necho done\n: > {}\n",
            marker.to_string_lossy()
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/to/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/to/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

#[test]
fn agent_task_timeout_kills_and_fails() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let marker = dir.path().join("agent-ran-to-completion");
    let prog = write_slow_agent(dir.path(), &marker);
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let res = run(dir.path(), "to", None, RunOptions::default()).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }

    // Timeout steered the run through the fallback into a failure finish.
    assert_eq!(res.outcome, RunStatus::Failed);
    // Killed on timeout: the stub never reached the line past its 5s sleep.
    assert!(
        !marker.exists(),
        "agent not killed on timeout: it ran past its sleep and wrote {}",
        marker.display()
    );

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload, EventPayload::AttemptFinished { status, .. } if status == "timed_out")),
        "expected an attempt marked timed_out"
    );
    assert!(
        events.iter().any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, status, .. } if node == "work" && status == "timed_out")),
        "work node must finish timed_out"
    );
}

/// An agent whose `TERM` handling is `on_term` (a shell `trap` action), that
/// otherwise waits far past its node's one-second timeout and then leaves
/// `marker`. The wait runs in the background so the trap fires at once.
///
/// Right after setting the trap the agent leaves `ready`, so a test can tell
/// that the trap was in place before any signal: a `TERM` that arrived
/// earlier would have ended the shell before `ready`. With
/// `APB_STUB_WARM_UP` set it stops there, which [`warm_up`] uses.
fn write_term_agent(root: &Path, on_term: &str, ready: &Path, marker: &Path) -> String {
    let path = root.join("term-agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\ntrap {on_term} TERM\n: > {}\n[ -n \"$APB_STUB_WARM_UP\" ] && exit 0\nsleep 30 &\nwait $!\n: > {}\n",
            ready.to_string_lossy(),
            marker.to_string_lossy()
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// Launches the freshly written `agent` once, outside any run, and waits for
/// it to leave `ready`. A first launch of a new script can stall for seconds
/// (a per-launch security scan on some machines), longer than the node's
/// one-second timeout; after this one the run's launch is quick, so the
/// timeout's `TERM` finds the trap in place.
fn warm_up(agent: &str, ready: &Path) {
    let status = std::process::Command::new(agent)
        .env("APB_STUB_WARM_UP", "1")
        .status()
        .unwrap();
    assert!(status.success() && ready.exists(), "warm-up: {status}");
    fs::remove_file(ready).unwrap();
}

/// Runs the timeout playbook under `agent` with the timeout grace `grace_ms`
/// and returns its single `attempt_finished`.
fn run_timed_out(root: &Path, agent: &str, grace_ms: &str) -> EventPayload {
    unsafe {
        std::env::set_var("APB_AGENT_CMD", agent);
        std::env::set_var("APB_TIMEOUT_GRACE_MS", grace_ms);
    }
    let res = run(root, "to", None, RunOptions::default());
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
        std::env::remove_var("APB_TIMEOUT_GRACE_MS");
    }
    let res = res.unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    read_all(&root.join(".apb/runs").join(&res.run_id))
        .unwrap()
        .into_iter()
        .find(|e| matches!(e.payload, EventPayload::AttemptFinished { .. }))
        .expect("an attempt_finished")
        .payload
}

#[test]
fn a_timed_out_agent_that_handles_sigterm_keeps_its_reported_usage() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let ready = dir.path().join("agent-trap-set");
    let marker = dir.path().join("agent-ran-to-completion");
    // On SIGTERM the agent prints its final result, a SUCCESS report
    // included, and exits.
    let record = r#"{"type":"result","is_error":false,"result":"ok\n\n```yaml\nstatus: success\nsummary: ok\n```","usage":{"input_tokens":11,"output_tokens":3},"total_cost_usd":0.02}"#;
    let agent = write_term_agent(
        dir.path(),
        &format!("'printf \"%s\\n\" '\\''{record}'\\''; exit 143'"),
        &ready,
        &marker,
    );
    warm_up(&agent, &ready);
    let finished = run_timed_out(dir.path(), &agent, "5000");
    assert!(ready.exists(), "the trap was set before the timeout's TERM");

    let EventPayload::AttemptFinished { status, usage, .. } = finished else {
        unreachable!()
    };
    // What the agent printed does not change the verdict.
    assert_eq!(status, "timed_out");
    let usage = usage.expect("the usage of the final record");
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 3));
    assert_eq!(usage.cost_usd, Some(0.02));
    assert!(!marker.exists());
}

#[test]
fn a_timed_out_agent_that_ignores_sigterm_is_killed_and_its_usage_stays_unknown() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let ready = dir.path().join("agent-trap-set");
    let marker = dir.path().join("agent-ran-to-completion");
    let agent = write_term_agent(dir.path(), "''", &ready, &marker);
    warm_up(&agent, &ready);
    let finished = run_timed_out(dir.path(), &agent, "300");
    assert!(ready.exists(), "the trap was set before the timeout's TERM");

    let EventPayload::AttemptFinished { status, usage, .. } = finished else {
        unreachable!()
    };
    assert_eq!(status, "timed_out");
    // Nothing was printed: the cost is unknown, never zero.
    assert_eq!(usage, None);
    // SIGKILL after the grace: the agent never got past its wait.
    assert!(!marker.exists());
}
