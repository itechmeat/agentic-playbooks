//! Goal criteria checked when a run reaches a finish node (C1).

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

fn playbook(goal: &str) -> String {
    format!(
        r#"
schema: 2
id: g
name: Goal
version: 1.0.0
{goal}
nodes:
  - {{ id: start, type: start }}
  - {{ id: work, type: script, script: "scripts/work.sh", runner: sh }}
  - {{ id: cleanup, type: script, script: "scripts/cleanup.sh", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: work }}
  - {{ from: work, to: cleanup }}
  - {{ from: cleanup, to: done }}
"#
    )
}

/// `work` prints a marker and writes two files; `cleanup` removes the
/// scratch one. The criteria script checks the persistent one and records
/// whether it saw the scratch file (it must not: cleanup ran first).
fn seed(root: &Path, goal: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/g/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook(goal)).unwrap();
    fs::write(
        dir.join("scripts/work.sh"),
        "echo result > result.txt\necho scratch > scratch.txt\necho WORK-DONE\n",
    )
    .unwrap();
    fs::write(dir.join("scripts/cleanup.sh"), "rm -f scratch.txt\n").unwrap();
    fs::write(
        dir.join("scripts/goal.sh"),
        "if [ -e scratch.txt ]; then echo saw-scratch > seen.txt; fi\n\
         printf '%s' \"$APB_RUN_ID\" > goal-run-id.txt\n\
         test -s result.txt\n",
    )
    .unwrap();
    fs::write(dir.join("scripts/fail.sh"), "echo tests are red\nexit 3\n").unwrap();
    fs::write(root.join(".apb/playbooks/g/current"), "1.0.0").unwrap();
}

fn checked(events: &[Event]) -> Vec<(usize, String, String, bool)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::GoalChecked {
                index,
                check,
                status,
                enforced,
                ..
            } => Some((*index, check.clone(), status.clone(), *enforced)),
            _ => None,
        })
        .collect()
}

fn run_it(goal: &str) -> (tempfile::TempDir, String, RunStatus, Vec<Event>) {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), goal);
    let res = run(tmp.path(), "g", None, RunOptions::default()).unwrap();
    let events = read_all(&tmp.path().join(".apb/runs").join(&res.run_id)).unwrap();
    (tmp, res.run_id, res.outcome, events)
}

const REPORTED: &str = r#"goal:
  statement: Produce the result file
  criteria:
    - { description: the result exists, check: { type: script, path: scripts/goal.sh } }
    - { description: the work says it is done, check: { type: marker, marker: WORK-DONE } }
    - { description: the release notes say so, check: { type: marker, marker: NOT-THERE } }
    - { description: a person reads the result }"#;

#[test]
fn criteria_are_checked_after_every_node_and_only_reported_by_default() {
    let (tmp, run_id, outcome, events) = run_it(REPORTED);
    // A failed marker does not fail the run without `enforce`.
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(
        checked(&events),
        [
            (0, "script".into(), "passed".into(), false),
            (1, "marker".into(), "passed".into(), false),
            (2, "marker".into(), "failed".into(), false),
            (3, "manual".into(), "manual".into(), false),
        ]
    );
    // The script ran in the run's tree after the cleanup node, with the
    // run context env.
    assert!(!tmp.path().join("seen.txt").exists());
    assert_eq!(
        fs::read_to_string(tmp.path().join("goal-run-id.txt")).unwrap(),
        run_id
    );
    // Journaled before the finish node's node_finished (the checkpoint).
    let last_goal = events
        .iter()
        .rposition(|e| matches!(e.payload, EventPayload::GoalChecked { .. }))
        .unwrap();
    let finish = events
        .iter()
        .position(
            |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "done"),
        )
        .unwrap();
    assert!(last_goal < finish);
    // The surfaces read the same results.
    let view =
        apb_engine::run_view::RunView::load(&tmp.path().join(".apb/runs").join(&run_id), &run_id)
            .unwrap();
    let goal = view
        .goal(&tmp.path().join(".apb/runs").join(&run_id))
        .unwrap();
    assert_eq!(goal.line(), "2 passed, 1 failed, 1 manual");
}

#[test]
fn an_enforced_failed_criterion_fails_the_run() {
    let goal = r#"goal:
  statement: Tests pass
  enforce: true
  criteria:
    - { description: the result exists, check: { type: script, path: scripts/goal.sh } }
    - { description: tests pass, check: { type: script, path: scripts/fail.sh } }"#;
    let (_tmp, _id, outcome, events) = run_it(goal);
    assert_eq!(outcome, RunStatus::Failed);
    let got = checked(&events);
    assert_eq!(got[1], (1, "script".into(), "failed".into(), true));
    let detail = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::GoalChecked {
                index: 1, detail, ..
            } => detail.clone(),
            _ => None,
        })
        .unwrap();
    assert!(detail.contains("tests are red"), "{detail}");
    let reason = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunError { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        reason.starts_with("goal criterion 2 (`tests pass`) failed"),
        "{reason}"
    );
}

#[test]
fn enforce_with_only_manual_criteria_never_fails_a_run() {
    let goal = r#"goal:
  statement: Someone is happy
  enforce: true
  criteria:
    - { description: a person reads the result }"#;
    let (_tmp, _id, outcome, events) = run_it(goal);
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(
        checked(&events),
        [(0, "manual".into(), "manual".into(), false)]
    );
}

#[test]
fn a_playbook_without_a_goal_journals_no_goal_event() {
    let (_tmp, _id, outcome, events) = run_it("");
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(checked(&events).is_empty());
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunError { .. }))
    );
}
