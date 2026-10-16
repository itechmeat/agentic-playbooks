//! A re-run resume revives the joins fed by the re-run node (issue #203),
//! driven end to end with script nodes.
//!
//! `c` fails until a `pass` marker exists, so the first pass fails through
//! `c`'s failure edge and the join writes `c`'s branch off as dead. The
//! resume from `c` must make the join wait for that branch again, and must
//! not let a finish node the old pass routed into end the run early.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;

/// The report's shape: `b` delivers into the implicit join `j` before `c`
/// fails, so `c2` never runs, is written off, and the failure finish ends the
/// run before the join runs.
const REPORT: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: b, type: script, script: "scripts/b.sh", runner: sh }
  - { id: c, type: script, script: "scripts/slowgate.sh", runner: sh }
  - { id: c2, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: j }
  - { from: c, to: c2, condition: { type: node_status, node: c, equals: success } }
  - { from: c, to: failed, condition: { type: node_status, node: c, equals: failure } }
  - { from: c2, to: j }
  - { from: j, to: done }
"#;

/// The issue's acceptance diamond: `c` feeds the join directly and only on
/// success, and `b` is slower than `c`, so the join runs without `c` and
/// routes into `done` on the first pass.
const DIAMOND: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: b, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: c, type: script, script: "scripts/gate.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: j }
  - { from: c, to: j, condition: { type: node_status, node: c, equals: success } }
  - { from: c, to: failed, condition: { type: node_status, node: c, equals: failure } }
  - { from: j, to: done }
"#;

const PASS: &str = "pass";
const B_DONE: &str = "b-done";
const C_RAN: &str = "c-ran";

/// Waits (at most 30 s) for a marker file, then half a second more, so the
/// branch order of the first pass does not depend on scheduling luck.
fn wait_for(root: &Path, marker: &str) -> String {
    format!(
        "i=0; while [ ! -e '{m}' ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done; sleep 0.5\n",
        m = root.join(marker).display()
    )
}

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/jr/1.0.0");
    let scripts = vdir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/jr/current"), "1.0.0").unwrap();
    let mark = |m: &str| format!(": > '{}'\n", root.join(m).display());
    fs::write(scripts.join("ok.sh"), "echo ok\n").unwrap();
    // `b` of REPORT: delivers first and says so.
    fs::write(scripts.join("b.sh"), format!("{}echo ok\n", mark(B_DONE))).unwrap();
    // `b` of DIAMOND: delivers only after `c` ran.
    fs::write(
        scripts.join("slow.sh"),
        format!("{}echo ok\n", wait_for(root, C_RAN)),
    )
    .unwrap();
    let gate = format!(
        "{}if [ -e '{}' ]; then echo ok; else echo broken; exit 1; fi\n",
        mark(C_RAN),
        root.join(PASS).display()
    );
    fs::write(scripts.join("gate.sh"), &gate).unwrap();
    // `c` of REPORT: fails only after `b` delivered.
    fs::write(
        scripts.join("slowgate.sh"),
        format!("{}{gate}", wait_for(root, B_DONE)),
    )
    .unwrap();
}

/// Runs the playbook (it fails at `c`), lets `c` pass, resumes from `c`, and
/// returns the outcome and the events the resumed pass journaled.
fn fail_then_resume_from_c(root: &Path, yaml: &str) -> (RunStatus, Vec<Event>, Vec<Event>) {
    seed(root, yaml);
    let first = run(root, "jr", None, RunOptions::default()).unwrap();
    assert_eq!(first.outcome, RunStatus::Failed);
    let run_dir = root.join(".apb/runs").join(&first.run_id);
    let before = read_all(&run_dir).unwrap();
    fs::write(root.join(PASS), "").unwrap();
    let res = resume(root, &first.run_id, Some("c")).unwrap();
    let all = read_all(&run_dir).unwrap();
    let resumed_at = all
        .iter()
        .rposition(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .expect("the resume journals run_resumed");
    (res.outcome, before, all[resumed_at..].to_vec())
}

fn pos(events: &[Event], pred: impl Fn(&EventPayload) -> bool) -> Option<usize> {
    events.iter().position(|e| pred(&e.payload))
}

fn started_at(events: &[Event], node: &str) -> Option<usize> {
    pos(
        events,
        |p| matches!(p, EventPayload::NodeStarted { node: n, .. } if n == node),
    )
}

fn finished_at(events: &[Event], node: &str) -> Option<usize> {
    pos(
        events,
        |p| matches!(p, EventPayload::NodeFinished { node: n, .. } if n == node),
    )
}

fn dead_inputs(events: &[Event]) -> Vec<(String, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::JoinInputDead { node, sources } => Some((node.clone(), sources.clone())),
            _ => None,
        })
        .collect()
}

fn revived_inputs(events: &[Event]) -> Vec<(String, Vec<String>, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::JoinInputRevived {
                node,
                sources,
                from_node,
            } => Some((node.clone(), sources.clone(), from_node.clone())),
            _ => None,
        })
        .collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

/// Acceptance 2 and 3: the surviving branch was already traversed into the
/// join before the failure. The resumed join waits for `c2` and the revival
/// is journaled.
#[test]
fn a_resume_from_node_makes_the_join_wait_for_the_revived_branch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (outcome, before, resumed) = fail_then_resume_from_c(root, REPORT);
    assert_eq!(dead_inputs(&before), vec![("j".to_string(), strs(&["c2"]))]);
    assert!(started_at(&before, "j").is_none());

    assert_eq!(outcome, RunStatus::Succeeded);
    let c_done = finished_at(&resumed, "c").expect("c re-ran");
    let c2_done = finished_at(&resumed, "c2").expect("c2 ran this time");
    let j_start = started_at(&resumed, "j").expect("the join ran");
    assert!(
        c_done < j_start,
        "the join started together with the re-run node"
    );
    assert!(
        c2_done < j_start,
        "the join started before its revived input"
    );
    assert_eq!(
        revived_inputs(&resumed),
        vec![("j".to_string(), strs(&["c2"]), "c".to_string())]
    );
    // The record comes before the re-run node starts, and a checkpoint follows.
    let revived_at = pos(&resumed, |p| {
        matches!(p, EventPayload::JoinInputRevived { .. })
    })
    .unwrap();
    assert!(revived_at < started_at(&resumed, "c").unwrap());
    assert!(
        resumed[revived_at..]
            .iter()
            .any(|e| e.payload.is_checkpoint())
    );
}

/// Acceptance 1 and the second symptom: the join ran without `c` on the first
/// pass and routed into `done`. The resume re-runs the join on `c`'s new
/// input instead of letting the stale `done` head finish the run.
#[test]
fn a_resume_from_node_reruns_the_join_before_the_old_finish() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (outcome, before, resumed) = fail_then_resume_from_c(root, DIAMOND);
    assert_eq!(dead_inputs(&before), vec![("j".to_string(), strs(&["c"]))]);

    assert_eq!(outcome, RunStatus::Succeeded);
    let c_done = finished_at(&resumed, "c").expect("c re-ran");
    let j_start = started_at(&resumed, "j").expect("the join re-ran on c's input");
    let j_done = finished_at(&resumed, "j").unwrap();
    // A finish node journals only its `node_finished`.
    let done_at = finished_at(&resumed, "done").expect("the run finished through done");
    assert!(
        c_done < j_start,
        "the join started together with the re-run node"
    );
    assert!(
        j_done < done_at,
        "the stale done head finished the run first"
    );
    assert_eq!(
        revived_inputs(&resumed),
        vec![("j".to_string(), strs(&["c"]), "c".to_string())]
    );
    // The failure route of the old pass is not taken again.
    assert!(finished_at(&resumed, "failed").is_none());
}
