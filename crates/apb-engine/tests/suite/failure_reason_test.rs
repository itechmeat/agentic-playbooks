//! Issue #42 finding 3: every terminal engine error must append an
//! explanatory event to the run's journal before `run_finished`, and
//! `RunState::fold`'s `failure_reason` (surfaced by `run_status` in apb-mcp
//! and by `apb doctor --run` via `run_doctor::failure_reason_check`) must
//! carry it.
//!
//! The "no matching outgoing edge" scheduler-drive-loop shape is covered end
//! to end by `supervised_drive_test::autonomous_mode_unchanged_errors_without_fallback_edge`
//! (updated by this same fix - it used to assert a bare `Err`). This module
//! covers the other observed shape: a prepare/refusal error (a sub-playbook
//! child refused for a missing connector permit), which composes with the
//! no-outgoing-edge case once the parent node it belongs to has nowhere else
//! to go.

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};
use std::fs;
use std::path::Path;

use crate::common;

fn write_pb(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

// Parent -> sub-playbook child; the child binds a connector but the parent
// carries no connector permit at all (an ungated `run()` call, exactly like
// `RunOptions::default()` in every other engine-level test in this suite -
// permit resolution is a CLI/MCP-layer concern, see `apb-cli/src/run.rs`'s
// `connector_permits_for`). `child`'s prepare refuses with "connector
// bindings present but no connector permit" (finding 2/3b of issue #42)
// before the child's own RunStarted is ever journaled. `c -> f` is
// success-conditioned with no fallback, so the parent node `c` fails and the
// parent run itself then dies the same "no outgoing edge" way as
// `supervised_drive_test`'s scenario - the two findings compose end to end.
const PARENT_PB: &str = r#"schema: 2
id: parent
name: parent
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: s, type: start }
  - { id: c, type: playbook, playbook: child }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: c }
  - { from: c, to: f, condition: { type: node_status, node: c, equals: success } }
"#;

const CHILD_CONNECTOR_PB: &str = r#"schema: 2
id: child
name: child
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: s, type: start }
  - id: a
    type: agent_task
    prompt: hi
    connectors: [{ name: mock-tracker, functions: read_only }]
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: f }
"#;

#[test]
fn child_prepare_refusal_is_recorded_on_the_node_and_fails_the_parent() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    common::seed_main(dir.path());
    write_pb(dir.path(), "parent", PARENT_PB);
    write_pb(dir.path(), "child", CHILD_CONNECTOR_PB);

    let res = run(dir.path(), "parent", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    // Node `c`'s own NodeFinished names the exact refusal reason - the
    // explanatory record for the child prepare failure specifically.
    let c_output = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "c" => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a NodeFinished for node `c`, got {events:?}"));
    assert!(
        c_output.contains("no connector permit"),
        "node c's output must name the refusal: {c_output}"
    );

    // The parent run itself then dies with no outgoing edge out of `c`
    // (issue #42 finding 3), and that is ALSO recorded, not just the node.
    let run_error_reason = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunError { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    let run_error_reason =
        run_error_reason.unwrap_or_else(|| panic!("expected a RunError event, got {events:?}"));
    assert!(run_error_reason.contains("no outgoing edge"));
    let run_finished_seq = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
        .expect("expected a terminal run_finished event")
        .seq;
    let run_error_seq = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::RunError { .. }))
        .unwrap()
        .seq;
    assert!(run_error_seq < run_finished_seq);

    let state = RunState::fold(&events);
    assert_eq!(
        state
            .failure_reason
            .expect("failure_reason must be set")
            .reason,
        run_error_reason
    );

    // The child's OWN run directory (created before the refusal, since
    // `prepare_run_target` creates the run dir/log before building the
    // manifest) is not left silently empty either: it carries its own
    // RunError + run_finished(failed). `ChildRunStarted` is journaled on the
    // PARENT only once `prepare_run_target` returns `Ok` - which it does not
    // here - so the child's run dir is found by its `child-<millis>` prefix
    // among the project's runs instead.
    let runs_dir = dir.path().join(".apb/runs");
    let child_dir = fs::read_dir(&runs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("child-"))
        })
        .unwrap_or_else(|| panic!("expected a child-* run dir under {runs_dir:?}"));
    let child_events = read_all(&child_dir).unwrap();
    assert!(
        child_events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunError { .. })),
        "child's own journal must carry a RunError too: {child_events:?}"
    );
    let child_state = RunState::fold(&child_events);
    assert_eq!(child_state.run_status, RunStatus::Failed);
}
