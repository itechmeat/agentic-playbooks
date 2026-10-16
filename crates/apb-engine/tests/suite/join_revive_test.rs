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
use apb_engine::state::{RunState, RunStatus};

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
  - { id: b, type: script, script: "scripts/ok.sh", runner: sh }
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

/// A script line that waits (at most 30 s) until the run's journal shows
/// `node` finished, so the branch order of the first pass is a condition, not
/// a race against a sleep.
fn after_finished(root: &Path, node: &str) -> String {
    format!(
        "i=0; until grep -qs '\"type\":\"node_finished\",\"node\":\"{node}\"' '{runs}'/*/events.jsonl; do \
         i=$((i+1)); if [ $i -ge 600 ]; then echo 'timed out waiting for {node} to finish' >&2; exit 97; fi; sleep 0.05; done\n",
        runs = root.join(".apb/runs").display()
    )
}

/// The gate script: fails until the `pass` marker exists.
fn gate(root: &Path) -> String {
    format!(
        "if [ -e '{}' ]; then echo ok; else echo broken; exit 1; fi\n",
        root.join(PASS).display()
    )
}

/// Seeds the playbook with `ok.sh` plus the named scripts.
fn seed(root: &Path, yaml: &str, scripts: &[(&str, String)]) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/jr/1.0.0");
    let dir = vdir.join("scripts");
    fs::create_dir_all(&dir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/jr/current"), "1.0.0").unwrap();
    fs::write(dir.join("ok.sh"), "echo ok\n").unwrap();
    for (name, body) in scripts {
        fs::write(dir.join(name), body).unwrap();
    }
}

/// The scripts of REPORT and DIAMOND: in REPORT `c` fails only after `b`
/// finished, in DIAMOND `b` finishes only after `c` failed.
fn report_scripts(root: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("gate.sh", gate(root)),
        (
            "slowgate.sh",
            format!("{}{}", after_finished(root, "b"), gate(root)),
        ),
        ("slow.sh", format!("{}echo ok\n", after_finished(root, "c"))),
    ]
}

/// Runs the playbook (it fails at `from`), optionally lets the gate pass,
/// resumes from `from`, and returns the outcome, the first pass's events and
/// the events of the resumed pass.
fn fail_then_resume(
    root: &Path,
    yaml: &str,
    scripts: &[(&str, String)],
    from: &str,
    pass: bool,
) -> (RunStatus, Vec<Event>, Vec<Event>) {
    seed(root, yaml, scripts);
    let first = run(root, "jr", None, RunOptions::default()).unwrap();
    // DIAMOND_NOTIFY's first pass may end through `done`: its failure route
    // starts with a non-finish step, so the join's route can win the race.
    if yaml != DIAMOND_NOTIFY {
        assert_eq!(first.outcome, RunStatus::Failed);
    }
    let run_dir = root.join(".apb/runs").join(&first.run_id);
    let before = read_all(&run_dir).unwrap();
    if pass {
        fs::write(root.join(PASS), "").unwrap();
    }
    let res = resume(root, &first.run_id, Some(from)).unwrap();
    let all = read_all(&run_dir).unwrap();
    let resumed_at = all
        .iter()
        .rposition(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .expect("the resume journals run_resumed");
    (res.outcome, before, all[resumed_at..].to_vec())
}

fn fail_then_resume_from_c(
    root: &Path,
    yaml: &str,
    pass: bool,
) -> (RunStatus, Vec<Event>, Vec<Event>) {
    let scripts = report_scripts(root);
    fail_then_resume(root, yaml, &scripts, "c", pass)
}

fn starts_of(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .count()
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

/// What an older apb that skips both new events folds. `join_input_revived`
/// is pure observability, so the fold is the same without it.
/// `nodes_revived` changes routing only: the run's history (every node's
/// status) is the same without it, so nothing an older apb reads is corrupt.
fn folds_the_same_without_the_revival(events: &[Event]) {
    let skip = |keep: fn(&EventPayload) -> bool| -> Vec<Event> {
        events
            .iter()
            .filter(|e| keep(&e.payload))
            .cloned()
            .collect()
    };
    let without_record = skip(|p| !matches!(p, EventPayload::JoinInputRevived { .. }));
    assert!(
        without_record.len() < events.len(),
        "no revival to leave out"
    );
    assert_eq!(
        format!("{:?}", RunState::fold(events)),
        format!("{:?}", RunState::fold(&without_record))
    );
    let older = skip(|p| {
        !matches!(
            p,
            EventPayload::JoinInputRevived { .. } | EventPayload::NodesRevived { .. }
        )
    });
    assert!(
        older.len() < without_record.len(),
        "no nodes_revived to leave out"
    );
    assert_eq!(RunState::fold(events).nodes, RunState::fold(&older).nodes);
}

/// Acceptance 2 and 3: the surviving branch was already traversed into the
/// join before the failure. The resumed join waits for `c2` and the revival
/// is journaled.
#[test]
fn a_resume_from_node_makes_the_join_wait_for_the_revived_branch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (outcome, before, resumed) = fail_then_resume_from_c(root, REPORT, true);
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
    assert_eq!(starts_of(&resumed, "j"), 1);
    assert_eq!(
        revived_inputs(&resumed),
        vec![("j".to_string(), strs(&["c2"]), "c".to_string())]
    );
    // The record comes before the re-run node starts.
    let revived_at = pos(&resumed, |p| {
        matches!(p, EventPayload::JoinInputRevived { .. })
    })
    .unwrap();
    assert!(revived_at < started_at(&resumed, "c").unwrap());
    let all = [before, resumed].concat();
    folds_the_same_without_the_revival(&all);
}

/// Acceptance 1 and the second symptom: the join ran without `c` on the first
/// pass and routed into `done`. The resume re-runs the join on `c`'s new
/// input instead of letting the stale `done` head finish the run.
#[test]
fn a_resume_from_node_reruns_the_join_before_the_old_finish() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (outcome, before, resumed) = fail_then_resume_from_c(root, DIAMOND, true);
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
    assert_eq!(starts_of(&resumed, "j"), 1);
    assert_eq!(
        revived_inputs(&resumed),
        vec![("j".to_string(), strs(&["c"]), "c".to_string())]
    );
    // The failure route of the old pass is not taken again.
    assert!(finished_at(&resumed, "failed").is_none());
}

/// Round 1 review: the re-run node fails again. The join that never ran is
/// re-weighed once its revived branch is dead (a fresh `join_input_dead`),
/// and the run fails through the failure edge before it runs; no stale
/// `done` finishes it.
#[test]
fn a_rerun_that_fails_again_writes_a_join_that_never_ran_off_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (outcome, _, resumed) = fail_then_resume_from_c(root, REPORT, false);
    assert_eq!(outcome, RunStatus::Failed);
    let c_done = finished_at(&resumed, "c").expect("c re-ran");
    assert_eq!(
        revived_inputs(&resumed),
        vec![("j".to_string(), strs(&["c2"]), "c".to_string())]
    );
    assert_eq!(
        dead_inputs(&resumed),
        vec![("j".to_string(), strs(&["c2"]))]
    );
    let dead_at = pos(&resumed, |p| {
        matches!(p, EventPayload::JoinInputDead { .. })
    })
    .unwrap();
    assert!(c_done < dead_at);
    assert!(finished_at(&resumed, "failed").is_some());
    assert!(finished_at(&resumed, "done").is_none());
    assert!(started_at(&resumed, "j").is_none());
}

/// DIAMOND with a non-finish step on `c`'s failure route.
const DIAMOND_NOTIFY: &str = r#"
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
  - { id: notify, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: j }
  - { from: c, to: j, condition: { type: node_status, node: c, equals: success } }
  - { from: c, to: notify, condition: { type: node_status, node: c, equals: failure } }
  - { from: notify, to: failed }
  - { from: j, to: done }
"#;

/// DIAMOND where a passing `c` routes away from the join (its output never
/// says `deliver`), into its own finish.
const DIAMOND_AWAY: &str = r#"
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
  - { id: elsewhere, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: away, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: j }
  - { from: c, to: j, condition: { type: output_match, node: c, pattern: deliver } }
  - { from: c, to: failed, condition: { type: node_status, node: c, equals: failure } }
  - { from: c, to: elsewhere, fallback: true }
  - { from: elsewhere, to: away }
  - { from: j, to: done }
"#;

/// Round 2 hole 1: the join already ran in the first pass. When the re-run
/// branch dies again, or routes away, the join is not run a second time on
/// the same old inputs.
#[test]
fn a_join_that_ran_before_is_not_rerun_when_the_revived_branch_does_not_deliver() {
    for (yaml, pass, outcome, finish) in [
        (DIAMOND_NOTIFY, false, RunStatus::Failed, "failed"),
        (DIAMOND_AWAY, true, RunStatus::Succeeded, "away"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (got, before, resumed) = fail_then_resume_from_c(root, yaml, pass);
        assert_eq!(starts_of(&before, "j"), 1, "the first pass ran the join");
        assert_eq!(got, outcome);
        assert!(finished_at(&resumed, "c").is_some());
        assert_eq!(
            starts_of(&resumed, "j"),
            0,
            "the join ran twice on its old inputs"
        );
        assert!(finished_at(&resumed, finish).is_some());
        assert!(finished_at(&resumed, "done").is_none());
    }
}

/// A loop with a fork inside: `a` forks into `x` and `y`, `x` loops back into
/// `a` once on success. `y` waits until `x` failed, so the first pass ends
/// with `y` completed and the run failed at `x`.
const LOOP_FORK: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: x, type: script, script: "scripts/gate.sh", runner: sh }
  - { id: y, type: script, script: "scripts/after_x.sh", runner: sh }
  - { id: xe, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: ye, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: x }
  - { from: a, to: y }
  - { from: x, to: a, max_traversals: 1, condition: { type: node_status, node: x, equals: success } }
  - { from: x, to: failed, condition: { type: node_status, node: x, equals: failure } }
  - { from: x, to: xe, fallback: true }
  - { from: y, to: ye }
  - { from: xe, to: done }
  - { from: ye, to: done }
"#;

fn loop_scripts(root: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("gate.sh", gate(root)),
        // `x` of LOOP_JOIN: a passing re-run takes a second, so the join that
        // runs on `y`'s old arrival visibly runs alongside it.
        (
            "slowpass.sh",
            format!(
                "[ -e '{}' ] && sleep 1\n{}",
                root.join(PASS).display(),
                gate(root)
            ),
        ),
        (
            "after_x.sh",
            format!("{}echo ok\n", after_finished(root, "x")),
        ),
    ]
}

/// Review finding: resuming from `x` inside the loop does not revive the
/// sibling `y` that already completed. `y` runs again only on the next loop
/// pass, after `a` re-runs.
#[test]
fn a_resume_inside_a_loop_does_not_rerun_a_completed_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let scripts = loop_scripts(root);
    let (outcome, before, resumed) = fail_then_resume(root, LOOP_FORK, &scripts, "x", true);
    assert_eq!(finished_at(&before, "y").map(|_| ()), Some(()));
    assert_eq!(outcome, RunStatus::Succeeded);
    let a_again = started_at(&resumed, "a").expect("x looped back into a");
    let y_again = started_at(&resumed, "y").expect("the next loop pass ran y");
    assert!(finished_at(&resumed, "x").unwrap() < a_again);
    assert!(a_again < y_again, "y re-ran before the loop came back to a");
    assert_eq!(starts_of(&resumed, "y"), 1);
}

/// The join-in-cycle shape `a -> {x, y}`, `x -> j`, `y -> j`, `j -> a`: `j`
/// has an input inside its own cycle, so it is a first-arrival merge (the
/// CLAUDE.md fan-in rule: a fan-in inside its own cycle stays first-arrival,
/// else loops deadlock), and it is an ancestor of `x`. Resuming from `x`
/// revives `x` alone: `j` is not revived, so `y`'s arrival from the old pass
/// still counts and `j` runs on it at once, alongside the re-run `x`, as it
/// would have had the run not failed. `y` itself is not re-run before the loop
/// comes back to `a`. Intended and pinned: a first-arrival merge outside the
/// revival does not wait for the re-run node.
const LOOP_JOIN: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: x, type: script, script: "scripts/slowpass.sh", runner: sh }
  - { id: y, type: script, script: "scripts/after_x.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: x }
  - { from: a, to: y }
  - { from: x, to: j, condition: { type: node_status, node: x, equals: success } }
  - { from: x, to: failed, condition: { type: node_status, node: x, equals: failure } }
  - { from: y, to: j }
  - { from: j, to: a, max_traversals: 1 }
  - { from: j, to: done, fallback: true }
"#;

#[test]
fn a_resume_inside_a_loop_keeps_a_first_arrival_merge_on_its_old_input() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let scripts = loop_scripts(root);
    let (outcome, before, resumed) = fail_then_resume(root, LOOP_JOIN, &scripts, "x", true);
    assert!(finished_at(&before, "y").is_some());
    assert!(started_at(&before, "j").is_none());
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(revived_inputs(&resumed).is_empty());
    let a_again = started_at(&resumed, "a").expect("j looped back into a");
    let y_again = started_at(&resumed, "y").expect("the next loop pass ran y");
    assert!(a_again < y_again, "y re-ran before the loop came back to a");
    // `j` ran on `y`'s old arrival alongside the re-run `x` (it finished
    // before `x` did), and once more on the next loop pass.
    let j_first = finished_at(&resumed, "j").unwrap();
    assert!(j_first < finished_at(&resumed, "x").unwrap());
    assert!(j_first < a_again);
    assert_eq!(starts_of(&resumed, "j"), 2);
}
