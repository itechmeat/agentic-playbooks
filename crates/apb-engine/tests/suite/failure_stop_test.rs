//! `defaults.on_failure` (spec 2026-07-26): what a node that fails with no
//! outgoing edge to take does. `stop` ends the run as failed with the node's
//! own words as the reason; a node id routes the failure there as an edge
//! would have.
//!
//! The point of the policy is to let a playbook delete the pile of
//! `node_status: failure` edges into a negative finish node, so these tests
//! pin both halves: the policy fires where no edge takes the failure, and it
//! does not fire where one does.

use crate::common;
use std::fs;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};

/// An agent that completes normally and self-reports a failure through the
/// report contract (spec 6.2) - the ordinary way an `agent_task` fails. Its
/// reply body is the node output, and the point of the policy is that THIS
/// text becomes the run's failure reason.
fn failing_agent(dir: &Path, message: &str) -> String {
    let path = dir.join("failing_agent.sh");
    let body = format!(
        "#!/bin/sh\ncat <<'EOF'\n{message}\n```yaml\nstatus: failure\nsummary: node failed\n```\nEOF\n"
    );
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

fn run_dir_of(root: &Path, run_id: &str) -> PathBuf {
    root.join(".apb/runs").join(run_id)
}

fn run_error(events: &[Event]) -> Option<(Option<String>, String)> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::RunError { node, reason } => Some((node.clone(), reason.clone())),
        _ => None,
    })
}

const MESSAGE: &str = "implementation could not be completed: 3 failing tests";

/// The shape the policy exists for: one success edge onward and nothing that
/// takes a failure. Under `route` this is an engine error; under `stop` it is
/// a declared end of the run.
const STOP_PB: &str = r#"schema: 2
id: stopflow
name: Stop flow
version: 1.0.0
defaults:
  profile: main
  on_failure: stop
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
"#;

/// The same graph with the failure routed explicitly. An edge always wins over
/// the policy, so this must reach the negative finish node.
const ROUTED_PB: &str = r#"schema: 2
id: routedflow
name: Routed flow
version: 1.0.0
defaults:
  profile: main
  on_failure: stop
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: aborted, condition: { type: node_status, node: work, equals: failure } }
"#;

/// `STOP_PB` without the policy line: the default must still be today's
/// behavior, verbatim.
const DEFAULT_PB: &str = r#"schema: 2
id: defaultflow
name: Default flow
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
"#;

/// Two agent branches out of start: both run concurrently in one batch, so the
/// stop has to be recognized on the batch path too, and the reason has to be
/// stable rather than whichever branch happened to finish first.
const PARALLEL_PB: &str = r#"schema: 2
id: parflow
name: Parallel flow
version: 1.0.0
defaults:
  profile: main
  on_failure: stop
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "a" }
  - { id: b, type: agent_task, prompt: "b" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: done, condition: { type: node_status, node: a, equals: success } }
  - { from: b, to: done, condition: { type: node_status, node: b, equals: success } }
"#;

/// A fan-out that is NOT a concurrent batch: one agent node and one condition
/// node, so the agent runs alone and the condition is still sitting in the
/// frontier when the agent fails.
const FANOUT_PB: &str = r#"schema: 2
id: fanflow
name: Fanout flow
version: 1.0.0
defaults:
  profile: main
  on_failure: stop
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: check, type: condition, max_loops: 1 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: start, to: check }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: check, to: done }
"#;

#[test]
fn stop_policy_reports_the_node_output_as_the_failure_reason() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "stopflow", STOP_PB);
    let prog = failing_agent(dir.path(), MESSAGE);

    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", &prog) };
    let res = run(dir.path(), "stopflow", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Failed);

    let events = read_all(&run_dir_of(dir.path(), &res.run_id)).unwrap();
    let (node, reason) =
        run_error(&events).unwrap_or_else(|| panic!("expected a RunError, got {events:?}"));
    assert_eq!(
        node.as_deref(),
        Some("work"),
        "the stop must name the node that failed"
    );
    assert!(
        reason.contains("3 failing tests"),
        "the reason must be the node's own output, got: {reason}"
    );
    assert!(
        !reason.contains("no outgoing edge"),
        "a declared stop must not read as a missing-edge complaint, got: {reason}"
    );

    let state = RunState::fold(&events);
    assert_eq!(state.run_status, RunStatus::Failed);
    assert_eq!(
        state
            .failure_reason
            .expect("failure_reason must be set")
            .reason,
        reason
    );
}

#[test]
fn an_explicit_failure_edge_wins_over_the_policy() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "routedflow", ROUTED_PB);
    let prog = failing_agent(dir.path(), MESSAGE);

    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", &prog) };
    let res = run(dir.path(), "routedflow", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Failed);

    let events = read_all(&run_dir_of(dir.path(), &res.run_id)).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::NodeFinished { node, .. } if node == "aborted")),
        "the run must reach the negative finish node, got {events:?}"
    );
    assert!(
        run_error(&events).is_none(),
        "a routed failure is not an anomaly and must not journal a RunError, got {events:?}"
    );
}

#[test]
fn without_the_policy_an_unhandled_failure_is_still_an_engine_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "defaultflow", DEFAULT_PB);
    let prog = failing_agent(dir.path(), MESSAGE);

    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", &prog) };
    let res = run(dir.path(), "defaultflow", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Failed);

    let events = read_all(&run_dir_of(dir.path(), &res.run_id)).unwrap();
    let (_, reason) =
        run_error(&events).unwrap_or_else(|| panic!("expected a RunError, got {events:?}"));
    assert!(
        reason.contains("no outgoing edge"),
        "the default policy must keep today's reason verbatim, got: {reason}"
    );
}

#[test]
fn a_stop_inside_a_concurrent_batch_names_the_failing_branch() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "parflow", PARALLEL_PB);
    let prog = failing_agent(dir.path(), MESSAGE);

    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", &prog) };
    let res = run(dir.path(), "parflow", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Failed);

    let events = read_all(&run_dir_of(dir.path(), &res.run_id)).unwrap();
    let (node, reason) =
        run_error(&events).unwrap_or_else(|| panic!("expected a RunError, got {events:?}"));
    assert_eq!(
        node.as_deref(),
        Some("a"),
        "the branch named must be the first in graph order, not the first to finish"
    );
    assert!(
        reason.contains("3 failing tests"),
        "the batch path must report the branch's own output too, got: {reason}"
    );
}

#[test]
fn a_stop_cancels_the_branches_that_will_never_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "fanflow", FANOUT_PB);
    let prog = failing_agent(dir.path(), MESSAGE);

    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", &prog) };
    let res = run(dir.path(), "fanflow", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    drop(_env);

    assert_eq!(
        res.outcome,
        RunStatus::Failed,
        "a failure the author declared fatal must not be outlived by a sibling branch"
    );

    let events = read_all(&run_dir_of(dir.path(), &res.run_id)).unwrap();
    let check_status = events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeFinished { node, status, .. } if node == "check" => Some(status.clone()),
        _ => None,
    });
    assert_eq!(
        check_status.as_deref(),
        Some("cancelled"),
        "the branch left in the frontier must be journaled, not left pending: {events:?}"
    );
}

/// The form these playbooks actually want: the negative finish node keeps
/// composing its answer, but no node draws an edge into it.
const ROUTE_TO_NODE_PB: &str = r#"schema: 2
id: routeflow
name: Route flow
version: 1.0.0
defaults:
  profile: main
  on_failure: aborted
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
"#;

/// The handler itself fails. The policy must not send it to itself, or the run
/// would route in a circle until the step budget ran out.
const SELF_ROUTE_PB: &str = r#"schema: 2
id: selfroute
name: Self route
version: 1.0.0
defaults:
  profile: main
  on_failure: handler
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: handler, type: agent_task, prompt: "explain" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: handler, to: done, condition: { type: node_status, node: handler, equals: success } }
"#;

#[test]
fn a_node_policy_routes_the_failure_to_that_node() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "routeflow", ROUTE_TO_NODE_PB);
    let prog = failing_agent(dir.path(), MESSAGE);

    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", &prog) };
    let res = run(dir.path(), "routeflow", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Failed);

    let events = read_all(&run_dir_of(dir.path(), &res.run_id)).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::NodeFinished { node, .. } if node == "aborted")),
        "the failure must reach the declared handler even with no edge into it: {events:?}"
    );
    assert!(
        run_error(&events).is_none(),
        "a routed failure is not an anomaly and must not journal a RunError: {events:?}"
    );
}

#[test]
fn a_node_policy_does_not_route_the_handler_to_itself() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "selfroute", SELF_ROUTE_PB);
    let prog = failing_agent(dir.path(), MESSAGE);

    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", &prog) };
    let res = run(dir.path(), "selfroute", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    drop(_env);

    // `work` fails and routes to `handler`, which fails too and has nowhere
    // left to go: that is the ordinary unhandled-failure error, not a loop.
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&run_dir_of(dir.path(), &res.run_id)).unwrap();
    let handler_runs = events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "handler"),
        )
        .count();
    assert_eq!(
        handler_runs, 1,
        "the handler must run once, not in a circle"
    );
    let (_, reason) =
        run_error(&events).unwrap_or_else(|| panic!("expected a RunError, got {events:?}"));
    assert!(reason.contains("no outgoing edge"), "got: {reason}");
}
