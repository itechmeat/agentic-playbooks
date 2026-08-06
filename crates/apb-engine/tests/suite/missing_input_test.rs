//! Missing template inputs are observable, not silent and not fatal (spec
//! 2026-08-05 section 1.5, Task 4).
//!
//! A `{{nodes.<id>.output}}` read that has nothing to fill it keeps rendering as
//! an empty string - the rendered prompt is what an `agent_task` cache key is
//! derived from, so changing it would move every key - but the engine journals
//! an anomaly naming the reading node and the reference it could not fill.
//!
//! The criterion is what the read ACTUALLY renders, not the source's status: the
//! fold records an output for every terminal status, so a failure handler reading
//! its failed source gets real text and is no anomaly, while a source that
//! finished with an empty output is one whatever its status was.

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, WakeTrigger, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::path::Path;

/// An either-or fork whose merge reads the branch that was NOT taken. This is
/// the shape the spec calls legitimate: the read must not fail the node, so the
/// only way the hole becomes visible is the journaled anomaly.
const READS_THE_OTHER_BRANCH: &str = r#"
schema: 1
id: mi
name: MI
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: pick, type: condition }
  - { id: a, type: prompt, prompt: "a ran" }
  - { id: b, type: prompt, prompt: "b ran" }
  - { id: m, type: prompt, prompt: "before[{{nodes.b.output}}]after" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: pick }
  - { from: pick, to: a, condition: { type: node_status, node: pick, equals: success } }
  - { from: pick, to: b, condition: { type: node_status, node: pick, equals: failure } }
  - { from: a, to: m }
  - { from: b, to: m }
  - { from: m, to: done }
"#;

/// The canonical failure-handler topology: `defaults.on_failure: handler`, and
/// the handler's prompt reads the failed node's output. `RunState::fold` records
/// an output for EVERY terminal status, so this read renders the failed node's
/// real text - there is no hole here, whatever the source's status was.
const HANDLER_READS_THE_FAILURE: &str = r#"
schema: 1
id: mi
name: MI
version: 1.0.0
defaults:
  on_failure: handler
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/work.sh", runner: sh }
  - { id: handler, type: prompt, prompt: "recovering from[{{nodes.work.output}}]" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: handler, to: done }
"#;

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/mi/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/mi/current"), "1.0.0").unwrap();
}

/// [`seed`] plus the one script the handler fixtures reference. A script node is
/// the cheapest node that can fail with a chosen stdout (its output IS its
/// stdout), so both halves of the criterion are exactly controllable.
fn seed_with_script(root: &Path, yaml: &str, body: &str) {
    seed(root, yaml);
    let scripts = root.join(".apb/playbooks/mi/1.0.0/scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("work.sh"), body).unwrap();
}

/// Every anomaly wake raised for `node`, as its detail text.
fn anomalies(events: &[Event], node: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::WakeRaised {
                trigger: WakeTrigger::Anomaly,
                node: n,
                detail,
            } if n == node => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

fn node_output(events: &[Event], node: &str) -> String {
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n, output, ..
            } if n == node => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("node `{node}` never finished"))
}

#[test]
fn a_read_of_a_node_that_never_ran_journals_an_anomaly_and_still_renders_empty() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), READS_THE_OTHER_BRANCH);

    let res = run(dir.path(), "mi", None, RunOptions::default())
        .expect("a missing input must never fail the run");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();

    // The rendered text is unchanged: the hole stays an empty string, byte for
    // byte, because agent-task cache keys are derived from the rendered prompt.
    assert_eq!(
        node_output(&events, "m"),
        "before[]after",
        "a missing input must still render as an empty string"
    );

    let found = anomalies(&events, "m");
    assert_eq!(
        found.len(),
        1,
        "exactly one missing-input anomaly per execution, got: {found:?}"
    );
    let detail = &found[0];
    assert!(
        detail.contains("nodes.b.output"),
        "the anomaly names the missing reference, got: {detail}"
    );
    assert!(
        detail.contains("`m`"),
        "the anomaly names the reading node, got: {detail}"
    );
}

/// The other half of the contract: a read whose source DID succeed is not an
/// anomaly, so the event is evidence rather than noise on every run.
#[test]
fn a_read_of_a_succeeded_node_journals_no_anomaly() {
    const READS_A_FINISHED_NODE: &str = r#"
schema: 1
id: mi
name: MI
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a ran" }
  - { id: m, type: prompt, prompt: "got[{{nodes.a.output}}]" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: m }
  - { from: m, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), READS_A_FINISHED_NODE);

    let res = run(dir.path(), "mi", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();

    assert_eq!(node_output(&events, "m"), "got[a ran]");
    assert!(
        anomalies(&events, "m").is_empty(),
        "a satisfied read must journal nothing: {events:?}"
    );
}

/// The criterion is "does this read actually render empty", not "did the source
/// succeed". A failed source with real output fills the reference, so the
/// canonical failure handler must stay silent - otherwise the anomaly fires on
/// every handled failure and its "renders empty" claim is false.
#[test]
fn a_handler_reading_a_failed_nodes_real_output_journals_no_anomaly() {
    let dir = tempfile::tempdir().unwrap();
    seed_with_script(
        dir.path(),
        HANDLER_READS_THE_FAILURE,
        "echo real failure text\nexit 1\n",
    );

    let res = run(dir.path(), "mi", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded, "the handler recovers");
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();

    // The premise this test needs: the failed node really did produce output.
    assert_eq!(
        node_output(&events, "work"),
        "real failure text",
        "premise: a failed script's stdout is its node output"
    );
    assert_eq!(
        node_output(&events, "handler"),
        "recovering from[real failure text]",
        "the handler's read is filled, not a hole"
    );
    assert!(
        anomalies(&events, "handler").is_empty(),
        "a filled read must journal nothing, whatever the source's status: {events:?}"
    );
}

/// The other side of the same criterion: a source that finished with NO output
/// leaves a hole even though it has a record, which the old status-based
/// criterion could see only for a non-succeeded source.
#[test]
fn a_read_of_a_node_that_finished_empty_journals_the_anomaly() {
    let dir = tempfile::tempdir().unwrap();
    seed_with_script(dir.path(), HANDLER_READS_THE_FAILURE, "exit 1\n");

    let res = run(dir.path(), "mi", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();

    // Premise: the source finished with an empty output.
    assert_eq!(
        node_output(&events, "work"),
        "",
        "premise: a silent failing script leaves an empty node output"
    );
    assert_eq!(node_output(&events, "handler"), "recovering from[]");
    let found = anomalies(&events, "handler");
    assert_eq!(
        found.len(),
        1,
        "an empty record is still a hole, got: {found:?}"
    );
    assert!(
        found[0].contains("nodes.work.output") && found[0].contains("empty output"),
        "the anomaly names the reference and why it is empty, got: {}",
        found[0]
    );
}
