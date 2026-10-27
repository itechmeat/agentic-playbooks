//! Findings and warnings an attempt reports in its status file: journaled
//! once per attempt before its `attempt_finished`, malformed entries dropped
//! and counted, and never part of the verdict, the output or the routing.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

/// `w` routes to `ok` on success and to `no` otherwise, so the outcome shows
/// what decided the attempt.
const PLAYBOOK: &str = r#"
schema: 1
id: notes
name: Notes
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

const FINDING: &str = r#"{"kind":"bug","location":"src/a.rs:3","evidence":"off by one","why_not_fixed":"out of scope"}"#;
const WARNING: &str = r#"{"subject":"deploy","reason":"not verified"}"#;

/// Runs the playbook with a stub agent that writes `status_json` to its
/// status file and prints `reply`.
fn run_with(status_json: &str, reply: &str) -> (RunStatus, Vec<Event>) {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/notes/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/notes/current"), "1.0.0").unwrap();
    common::seed_main(root);
    fs::write(root.join("status.json"), status_json).unwrap();
    fs::write(root.join("reply.txt"), reply).unwrap();
    let agent = root.join("agent.sh");
    let r = root.display();
    common::write_sync(
        &agent,
        &format!(
            "#!/bin/sh\ncat \"{r}/status.json\" > \"$APB_STATUS_FILE\"\ncat \"{r}/reply.txt\"\n"
        ),
    );
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    unsafe { std::env::set_var("APB_AGENT_CMD", &agent) };
    let res = run(root, "notes", None, RunOptions::default());
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    let res = res.unwrap();
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    (res.outcome, events)
}

fn position(events: &[Event], pred: impl Fn(&EventPayload) -> bool) -> Option<usize> {
    events.iter().position(|e| pred(&e.payload))
}

fn notes(events: &[Event]) -> Vec<&EventPayload> {
    events
        .iter()
        .map(|e| &e.payload)
        .filter(|p| matches!(p, EventPayload::AttemptNotes { .. }))
        .collect()
}

#[test]
fn status_file_notes_are_journaled_before_the_attempt_finishes_and_route_nothing() {
    let (outcome, events) = run_with(
        &format!(r#"{{"status":"success","findings":[{FINDING}],"warnings":[{WARNING}]}}"#),
        "plain reply\n",
    );
    assert_eq!(outcome, RunStatus::Succeeded);
    let at = position(&events, |p| matches!(p, EventPayload::AttemptNotes { .. })).unwrap();
    let finished = position(
        &events,
        |p| matches!(p, EventPayload::AttemptFinished { node, .. } if node == "w"),
    )
    .unwrap();
    assert!(at < finished);
    let EventPayload::AttemptNotes {
        node,
        attempt,
        findings,
        warnings,
        dropped,
        ..
    } = notes(&events)[0]
    else {
        unreachable!()
    };
    assert_eq!((node.as_str(), *attempt, *dropped), ("w", 1, 0));
    assert_eq!(findings[0].location, "src/a.rs:3");
    assert_eq!(warnings[0].subject, "deploy");
    // The node's output is the reply, not the notes.
    let output = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "w" => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!output.contains("off by one"), "{output}");
}

#[test]
fn an_invalid_entry_is_dropped_and_counted_and_the_verdict_stands() {
    let (outcome, events) = run_with(
        &format!(r#"{{"status":"failure","findings":[{FINDING},{{"kind":"bug"}}]}}"#),
        "```yaml\nstatus: success\n```\n",
    );
    // The status file's failure decides, as it did before notes existed.
    assert_eq!(outcome, RunStatus::Failed);
    let EventPayload::AttemptNotes {
        findings,
        dropped,
        dropped_reasons,
        ..
    } = notes(&events)[0]
    else {
        unreachable!()
    };
    assert_eq!((findings.len(), *dropped), (1, 1));
    assert!(
        dropped_reasons[0].contains("`location` is missing"),
        "{dropped_reasons:?}"
    );
}

#[test]
fn a_notes_only_status_file_keeps_the_text_report_verdict() {
    let (outcome, events) = run_with(
        &format!(r#"{{"warnings":[{WARNING}]}}"#),
        "tried\n```yaml\nstatus: failure\n```\n",
    );
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(notes(&events).len(), 1);
    let (outcome, _) = run_with(&format!(r#"{{"warnings":[{WARNING}]}}"#), "done\n");
    assert_eq!(outcome, RunStatus::Succeeded);
}

#[test]
fn an_attempt_without_notes_journals_none() {
    let (outcome, events) = run_with(r#"{"status":"success"}"#, "done\n");
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(notes(&events).is_empty());
}
