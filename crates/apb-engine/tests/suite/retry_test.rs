use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use crate::common;

// Cargo runs #[test] fns in parallel threads within one process, so tests that
// mutate the shared global env var APB_AGENT_CMD race with each other unless
// serialized. Hold this lock across the entire set_var..run..remove_var span.

// Agent stub: fails until a marker file is created; creates it on the first invocation.
// So: 1st invocation - fail, 2nd - success. Check that retry carries it through.
fn flaky_agent(dir: &std::path::Path) -> String {
    let marker = dir.path_marker();
    let path = dir.join("flaky.sh");
    let body = format!(
        "#!/bin/sh\nif [ -f '{m}' ]; then echo ok; exit 0; else touch '{m}'; echo firstfail 1>&2; exit 1; fi\n",
        m = marker.display()
    );
    fs::write(&path, body).unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

trait Marker {
    fn path_marker(&self) -> std::path::PathBuf;
}
impl Marker for std::path::Path {
    fn path_marker(&self) -> std::path::PathBuf {
        self.join("flaky.marker")
    }
}

const PLAYBOOK: &str = r#"
schema: 1
id: retryflow
name: Retry
version: 1.0.0
defaults:
  profile: main
  max_retries: 1
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

#[test]
fn retry_recovers_flaky_agent() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/retryflow/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(dir.path().join(".apb/playbooks/retryflow/current"), "1.0.0").unwrap();
    common::seed_main(dir.path());

    let prog = flaky_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "retryflow", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { .. })),
        "expected a retry_started event"
    );
}

// A pipeline with executor main (claude-code) and fallback claude. Both share the same
// stub program via APB_AGENT_CMD (adapter_for maps both claude-code and claude to ClaudeAdapter).
// defaults.max_retries is unset -> retries=0, i.e. one attempt per chain executor.
const WF_FALLBACK: &str = r#"
schema: 1
id: fallbackflow
name: Fallback
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

#[test]
fn fallback_recovers_when_primary_fails() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/fallbackflow/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF_FALLBACK).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/fallbackflow/current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_profile(
        dir.path(),
        "main",
        "claude-code",
        "haiku",
        &[("claude", "haiku")],
    );

    // Flaky stub: the first invocation (primary=claude-code) fails and leaves a marker,
    // the second invocation (fallback=claude, same program) - success.
    let prog = flaky_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "fallbackflow", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::FallbackTriggered { .. })),
        "expected a fallback_triggered event (primary claude-code -> fallback claude)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { .. })),
        "with max_retries absent (0) there must be no retry_started event - recovery must come from fallback, not retry"
    );
}

// A stub that always fails - to check exhaustion of the whole executor chain.
fn always_fail_agent(dir: &std::path::Path) -> String {
    let path = dir.join("always_fail.sh");
    let body = "#!/bin/sh\necho boom 1>&2\nexit 1\n";
    fs::write(&path, body).unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

// The same executor chain, but node work branches by its status into success/failure finishes,
// so the run correctly ends up Failed instead of erroring with "no outgoing edge matched".
const WF_EXHAUST: &str = r#"
schema: 1
id: exhaustflow
name: Exhaust
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }
"#;

#[test]
fn whole_chain_exhaustion_fails_run() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/exhaustflow/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF_EXHAUST).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/exhaustflow/current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_profile(
        dir.path(),
        "main",
        "claude-code",
        "haiku",
        &[("claude", "haiku")],
    );

    // The stub always fails - both primary(claude-code) and fallback(claude) get exhausted.
    let prog = always_fail_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "exhaustflow", None, RunOptions::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Failed);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::FallbackTriggered { .. })),
        "expected a fallback_triggered event (primary claude-code -> fallback claude) before exhaustion"
    );
    assert!(!events.iter().any(|e| matches!(&e.payload, EventPayload::RunFinished { outcome } if outcome == "succeeded")),
        "run must not finish successfully when the whole executor chain is exhausted");
}
