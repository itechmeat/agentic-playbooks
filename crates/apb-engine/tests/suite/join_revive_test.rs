//! A re-run resume revives the joins fed by the re-run node (issue #203),
//! driven end to end with script nodes.
//!
//! `c` fails until a `pass` marker exists, so the first pass fails through
//! `c`'s failure edge and the join writes `c`'s branch off as dead. The
//! resume from `c` must make the join wait for that branch again, and must
//! not let a finish node the old pass routed into end the run early.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::{Control, post_control};
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::{PauseMode, PauseRequest};
use apb_engine::review::{ReviewCommand, post_review};
use apb_engine::scheduler::{RunMode, RunOptions, StartMode, plan_resume, resume, run};
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

/// Touched by a wait that ran out of time, so a test can tell a timed-out
/// wait (exit 97) from a script that failed on purpose.
const TIMED_OUT: &str = "wait-timed-out";

/// A script line that waits (at most 30 s) until the run's journal shows
/// `node` finished, so the branch order of the first pass is a condition, not
/// a race against a sleep. On timeout it touches [`TIMED_OUT`] and exits 97.
fn after_finished(root: &Path, node: &str) -> String {
    format!(
        "i=0; until grep -qs '\"type\":\"node_finished\",\"node\":\"{node}\"' '{runs}'/*/events.jsonl; do \
         i=$((i+1)); if [ $i -ge 600 ]; then echo 'timed out waiting for {node} to finish' >&2; : > '{sentinel}'; exit 97; fi; sleep 0.05; done\n",
        runs = root.join(".apb/runs").display(),
        sentinel = root.join(TIMED_OUT).display(),
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
        // `notify` of DIAMOND_NOTIFY: finishes only after the join did, then
        // two seconds more. It cannot wait for `done` itself: reaching
        // `done` ends the run, so the wait would never see it. The margin is
        // the time the drive has to route `j` into `done` and finish there.
        (
            "after_j.sh",
            format!("{}sleep 2\necho ok\n", after_finished(root, "j")),
        ),
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
    first_outcome: RunStatus,
) -> (RunStatus, Vec<Event>, Vec<Event>) {
    seed(root, yaml, scripts);
    let first = run(root, "jr", None, RunOptions::default()).unwrap();
    assert_eq!(first.outcome, first_outcome, "the first pass");
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
    assert!(!root.join(TIMED_OUT).exists(), "a script wait timed out");
    (res.outcome, before, all[resumed_at..].to_vec())
}

/// [`fail_then_resume`] from `c` with the REPORT/DIAMOND scripts, the first
/// pass failing.
fn fail_then_resume_from_c(
    root: &Path,
    yaml: &str,
    pass: bool,
) -> (RunStatus, Vec<Event>, Vec<Event>) {
    let scripts = report_scripts(root);
    fail_then_resume(root, yaml, &scripts, "c", pass, RunStatus::Failed)
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

/// DIAMOND with a non-finish step on `c`'s failure route. `notify` finishes
/// only after the join did, so the first pass ends through `done`
/// (succeeded) every time: the join's route reaches its finish first.
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
  - { id: notify, type: script, script: "scripts/after_j.sh", runner: sh }
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
    for (case, yaml, pass, first, outcome, finish) in [
        (
            "dies again through notify",
            DIAMOND_NOTIFY,
            false,
            RunStatus::Succeeded,
            RunStatus::Failed,
            "failed",
        ),
        (
            "passes and routes away",
            DIAMOND_AWAY,
            true,
            RunStatus::Failed,
            RunStatus::Succeeded,
            "away",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let scripts = report_scripts(root);
        let (got, before, resumed) = fail_then_resume(root, yaml, &scripts, "c", pass, first);
        assert_eq!(
            starts_of(&before, "j"),
            1,
            "{case}: the first pass ran the join"
        );
        assert_eq!(got, outcome, "{case}");
        assert!(finished_at(&resumed, "c").is_some(), "{case}: c re-ran");
        assert_eq!(
            starts_of(&resumed, "j"),
            0,
            "{case}: the join ran twice on its old inputs"
        );
        assert!(
            finished_at(&resumed, finish).is_some(),
            "{case}: {finish} ended it"
        );
        assert!(
            finished_at(&resumed, "done").is_none(),
            "{case}: a stale done"
        );
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
        // `x` of LOOP_JOIN: a passing run finishes only after the join did,
        // so the join that runs on `y`'s old arrival visibly runs alongside
        // the re-run `x` (the next loop pass finds `j` finished already).
        (
            "slowpass.sh",
            format!(
                "if [ -e '{}' ]; then\n{}fi\n{}",
                root.join(PASS).display(),
                after_finished(root, "j"),
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
    let (outcome, before, resumed) =
        fail_then_resume(root, LOOP_FORK, &scripts, "x", true, RunStatus::Failed);
    assert!(finished_at(&before, "y").is_some());
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
    let (outcome, before, resumed) =
        fail_then_resume(root, LOOP_JOIN, &scripts, "x", true, RunStatus::Failed);
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

fn count(events: &[Event], pred: impl Fn(&EventPayload) -> bool) -> usize {
    events.iter().filter(|e| pred(&e.payload)).count()
}

fn is_revival(p: &EventPayload) -> bool {
    matches!(p, EventPayload::NodesRevived { .. })
}

fn drain() -> Control {
    Control::Pause(PauseRequest {
        reason: Some("test".into()),
        by: None,
        mode: PauseMode::Drain,
        until: None,
        keep_lock: false,
    })
}

/// Round 3: an automatic restart of interrupted work revives nothing. The
/// driver died while `c2` ran; the restart re-runs `c2` alone, and the nodes
/// this pass already ran keep their routing.
#[test]
fn an_interrupted_restart_revives_nothing() {
    // Cut while `c2` runs (an interrupted restart of `c2`), and after `j`
    // finished (an advance past `j`): the nodes this pass already ran keep
    // their routing, `j` is not run again.
    for cut in [
        r#""type":"node_started","node":"c2""#,
        r#""type":"node_finished","node":"j""#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed(root, REPORT, &report_scripts(root));
        fs::write(root.join(PASS), "").unwrap();
        let first = run(root, "jr", None, RunOptions::default()).unwrap();
        assert_eq!(first.outcome, RunStatus::Succeeded);
        let run_dir = root.join(".apb/runs").join(&first.run_id);
        keep_through(&run_dir, cut);

        let res = resume(root, &first.run_id, None).unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded, "{cut}");
        let events = read_all(&run_dir).unwrap();
        assert_eq!(count(&events, is_revival), 0, "{cut}");
        assert_eq!(starts_of(&events, "j"), 1, "{cut}: j ran again");
        assert!(finished_at(&events, "c2").unwrap() < started_at(&events, "j").unwrap());
        assert!(!root.join(TIMED_OUT).exists());
    }
}

/// Keeps the journal through the first line containing `cut`, as a driver
/// killed right after writing it leaves it.
fn keep_through(run_dir: &Path, cut: &str) {
    let path = run_dir.join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        kept.push(line);
        if line.contains(cut) {
            break;
        }
    }
    fs::write(&path, kept.join("\n") + "\n").unwrap();
    let _ = fs::remove_file(run_dir.join("driver.pid"));
}

/// Appends raw journal lines, `SEQ` replaced by each line's sequence number.
fn append_lines(run_dir: &Path, lines: &[&str]) {
    let path = run_dir.join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let base = text.lines().count();
    let mut out = text;
    for (i, l) in lines.iter().enumerate() {
        out.push_str(&l.replace("SEQ", &(base + i).to_string()));
        out.push('\n');
    }
    fs::write(&path, out).unwrap();
    let _ = fs::remove_file(run_dir.join("driver.pid"));
}

fn resumed_count(events: &[Event]) -> usize {
    count(events, |p| matches!(p, EventPayload::RunResumed { .. }))
}

/// Round 4 items 1 and 7: two owed re-runs (`b` and `c`) and nothing really
/// interrupted. The resume re-runs the first owed one, the rebuild puts the
/// other back (it is not the drive's current), and the run never falls back
/// to re-running its unrelated last node.
#[test]
fn two_owed_reruns_are_both_run_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, REPORT, &report_scripts(root));
    let first = run(root, "jr", None, RunOptions::default()).unwrap();
    assert_eq!(first.outcome, RunStatus::Failed);
    let run_dir = root.join(".apb/runs").join(&first.run_id);
    append_lines(
        &run_dir,
        &[
            r#"{"seq":SEQ,"ts":1,"type":"run_resumed","from_node":"b"}"#,
            r#"{"seq":SEQ,"ts":1,"type":"nodes_revived","from_node":"b","nodes":["b","done","j"]}"#,
            r#"{"seq":SEQ,"ts":1,"type":"nodes_revived","from_node":"c","nodes":["c","c2","done","failed","j"]}"#,
            r#"{"seq":SEQ,"ts":1,"type":"run_paused","reason":"test"}"#,
        ],
    );
    fs::write(root.join(PASS), "").unwrap();
    let plan = plan_resume(root, &first.run_id, None).unwrap();
    assert_eq!(
        (plan.start_node.as_str(), plan.mode),
        ("b", StartMode::Rerun)
    );

    let before = read_all(&run_dir).unwrap().len();
    let res = resume(root, &first.run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let after = &read_all(&run_dir).unwrap()[before..];
    for n in ["b", "c", "c2", "j"] {
        assert_eq!(starts_of(after, n), 1, "{n}");
    }
    assert!(finished_at(after, "c2").unwrap() < started_at(after, "j").unwrap());
    assert!(
        finished_at(after, "failed").is_none(),
        "the old last node re-ran"
    );
    assert!(!root.join(TIMED_OUT).exists());
}

/// Round 4 items 4 and 7: an advance past a node that reads as not yet run
/// is refused before anything is journaled, so no driverless `run_resumed`
/// is left behind.
#[test]
fn an_advance_past_a_revived_node_is_refused_before_journaling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, REPORT, &report_scripts(root));
    let first = run(root, "jr", None, RunOptions::default()).unwrap();
    let run_dir = root.join(".apb/runs").join(&first.run_id);
    // `failed` finished last; a revival naming it with no owed start.
    append_lines(
        &run_dir,
        &[r#"{"seq":SEQ,"ts":1,"type":"nodes_revived","nodes":["failed"]}"#],
    );
    let before = read_all(&run_dir).unwrap();
    let err = resume(root, &first.run_id, None).unwrap_err();
    assert!(err.to_string().contains("revived"), "{err}");
    let after = read_all(&run_dir).unwrap();
    assert_eq!(
        after.len(),
        before.len(),
        "the refused resume journaled something"
    );
    assert_eq!(resumed_count(&after), resumed_count(&before));
}

/// Round 4 item 11: `fail_fast` cancelled `b` when `a` failed. Re-running
/// `a` re-runs `b` too, and the join waits for both and runs once.
const FAIL_FAST: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
nodes:
  - { id: start, type: start, fork: { on_branch_failure: fail_fast, on_failure: rejected } }
  - { id: a, type: script, script: "scripts/gate.sh", runner: sh }
  - { id: b, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: rejected, type: finish, outcome: failure }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }
"#;

#[test]
fn a_rerun_runs_the_siblings_its_failure_cancelled_before_the_join() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // `b` outlives `a` (the gate) by seconds in the first pass, so
    // `fail_fast` cancels it; on the re-run it is quick.
    let scripts = vec![
        ("gate.sh", gate(root)),
        (
            "slow.sh",
            format!(
                "if [ -e '{}' ]; then sleep 0.2; else sleep 5; fi\necho ok\n",
                root.join(PASS).display()
            ),
        ),
    ];
    let (outcome, before, resumed) =
        fail_then_resume(root, FAIL_FAST, &scripts, "a", true, RunStatus::Failed);
    assert!(
        before.iter().any(|e| matches!(&e.payload,
            EventPayload::BranchCancelled { node, .. } if node == "b")),
        "the first pass cancelled b"
    );
    assert_eq!(outcome, RunStatus::Succeeded);
    let j = started_at(&resumed, "j").expect("the join ran");
    for n in ["a", "b"] {
        assert_eq!(starts_of(&resumed, n), 1, "{n}");
        assert!(
            finished_at(&resumed, n).unwrap() < j,
            "the join ran before {n}"
        );
    }
    assert_eq!(starts_of(&resumed, "j"), 1);
}

/// Round 3 hole 1, the drain window: a pause applied before the re-run node
/// starts leaves its revival standing. The next resume re-runs it (it is
/// interrupted work, not a node to advance past) and the join still waits.
#[test]
fn a_drain_pause_before_the_rerun_starts_keeps_the_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, REPORT, &report_scripts(root));
    let first = run(root, "jr", None, RunOptions::default()).unwrap();
    assert_eq!(first.outcome, RunStatus::Failed);
    let run_dir = root.join(".apb/runs").join(&first.run_id);
    fs::write(root.join(PASS), "").unwrap();
    post_control(&run_dir, drain()).unwrap();
    let paused = resume(root, &first.run_id, Some("c")).unwrap();
    assert_eq!(paused.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    let resumed_at = events
        .iter()
        .rposition(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .unwrap();
    assert_eq!(count(&events[resumed_at..], is_revival), 1);
    assert_eq!(
        starts_of(&events[resumed_at..], "c"),
        0,
        "the pause came first"
    );
    assert_eq!(
        plan_resume(root, &first.run_id, None).unwrap().start_node,
        "c"
    );

    let res = resume(root, &first.run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let after = &read_all(&run_dir).unwrap()[resumed_at..];
    let c2_done = finished_at(after, "c2").expect("c re-ran and c2 ran");
    assert!(c2_done < started_at(after, "j").expect("the join ran"));
    assert_eq!(starts_of(after, "j"), 1);
    assert_eq!(
        count(after, is_revival),
        1,
        "an automatic restart revives nothing"
    );
    assert!(!root.join(TIMED_OUT).exists());
}

/// The issue's report shape with `c` a review gate: `reject` fails the run,
/// `approve` takes `c2`.
const REVIEWED: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: b, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: c, type: human_review, options: [approve, reject] }
  - { id: c2, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: j }
  - { from: c, to: c2, condition: { type: review_status, equals: approve } }
  - { from: c, to: failed, condition: { type: review_status, equals: reject } }
  - { from: c2, to: j }
  - { from: j, to: done }
"#;

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn reviews_requested(run_dir: &Path) -> usize {
    read_all(run_dir)
        .map(|e| {
            count(
                &e,
                |p| matches!(p, EventPayload::ReviewRequested { node, .. } if node == "c"),
            )
        })
        .unwrap_or(0)
}

fn decide(run_dir: &Path, decision: &str) {
    post_review(
        run_dir,
        ReviewCommand {
            node: "c".into(),
            decision: decision.into(),
            note: String::new(),
        },
    )
    .unwrap();
}

/// Round 3 hole 1, the review window: a revived review gate journals no
/// `node_started` until it is decided. Paused while it waits, the next
/// resume still re-runs it, and the join waits for its branch.
#[test]
fn a_pause_while_a_revived_review_gate_waits_keeps_the_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    seed(&root, REVIEWED, &[]);
    let r = root.clone();
    let (first_tx, first_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || first_tx.send(run(&r, "jr", None, RunOptions::default()).unwrap()));
    let runs = root.join(".apb/runs");
    wait_until("the run dir", || {
        fs::read_dir(&runs).is_ok_and(|mut d| d.next().is_some())
    });
    let run_dir = fs::read_dir(&runs).unwrap().next().unwrap().unwrap().path();
    let run_id = run_dir.file_name().unwrap().to_string_lossy().to_string();
    wait_until("the first review request", || {
        reviews_requested(&run_dir) == 1
    });
    decide(&run_dir, "reject");
    let first = first_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the rejected first pass returns");
    assert_eq!(first.outcome, RunStatus::Failed);

    let (r, id) = (root.clone(), run_id.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(resume(&r, &id, Some("c")).unwrap()));
    wait_until("the revived gate asks again", || {
        reviews_requested(&run_dir) == 2
    });
    post_control(&run_dir, drain()).unwrap();
    let paused = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the paused re-run returns");
    assert_eq!(paused.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    let resumed_at = events
        .iter()
        .rposition(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .unwrap();
    assert_eq!(count(&events[resumed_at..], is_revival), 1);
    assert_eq!(starts_of(&events[resumed_at..], "c"), 0);

    decide(&run_dir, "approve");
    let res = resume(&root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let after = &read_all(&run_dir).unwrap()[resumed_at..];
    let c2_done = finished_at(after, "c2").expect("the approved gate took c2");
    assert!(c2_done < started_at(after, "j").expect("the join ran"));
    assert_eq!(starts_of(after, "j"), 1);
}

/// Round 5 item 1: a failed delivery into a `join: all` still fails it after
/// a supervisor moves the run elsewhere. A pin, not a regression test: the
/// drive weighs a join's readiness again from the journal when it becomes
/// `current`, so the rebuild's verdict alone never decided this. `p` fails, `q` delivers; the
/// supervisor continues at `s` (reachable only through `publish`'s failure
/// route, outside `j`'s inputs). `j` is failed by its barrier, not run, and `publish` never
/// runs.
const PARKED_JOIN_ALL: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: p, type: script, script: "scripts/gate.sh", runner: sh }
  - { id: q, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: publish, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: s, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: sdone, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: p }
  - { from: a, to: q }
  - { from: p, to: j, join: all }
  - { from: q, to: j, join: all }
  - { from: j, to: publish }
  - { from: publish, to: done, condition: { type: node_status, node: publish, equals: success } }
  - { from: publish, to: s, condition: { type: node_status, node: publish, equals: failure } }
  - { from: s, to: sdone }
  - { from: sdone, to: done }
"#;

fn wait_for_wake(run_dir: &Path, node: &str) {
    wait_until(&format!("a wake on {node}"), || {
        read_all(run_dir).is_ok_and(|e| {
            e.iter().any(
                |e| matches!(&e.payload, EventPayload::WakeRaised { node: n, .. } if n == node),
            )
        })
    });
}

#[test]
fn a_continue_from_keeps_a_failed_delivery_failing_its_join() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    seed(&root, PARKED_JOIN_ALL, &[("gate.sh", gate(&root))]);
    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let r = root.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(run(&r, "jr", None, opts)));
    let runs = root.join(".apb/runs");
    wait_until("the run dir", || {
        if let Ok(Err(e)) = rx.try_recv() {
            panic!("the run failed to start: {e}");
        }
        fs::read_dir(&runs).is_ok_and(|mut d| d.next().is_some())
    });
    let run_dir = fs::read_dir(&runs).unwrap().next().unwrap().unwrap().path();
    wait_for_wake(&run_dir, "p");
    post_control(&run_dir, Control::ContinueFrom { node: "s".into() }).unwrap();
    // The barrier fails `j`; the supervisor is woken on it and stops.
    wait_for_wake(&run_dir, "j");
    post_control(
        &run_dir,
        Control::Abort {
            reason: "test".into(),
        },
    )
    .unwrap();
    let res = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the drive returns")
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    let events = read_all(&run_dir).unwrap();
    let j_finished: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, status, .. } if node == "j" => Some(status.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(j_finished, ["failed"], "the join ran on a failed input");
    assert!(
        !events.iter().any(|e| matches!(&e.payload,
            EventPayload::AttemptStarted { node, .. } if node == "j")),
        "the join executed"
    );
    assert_eq!(starts_of(&events, "publish"), 0);
}

/// Round 5 item 2: `defaults.on_failure: triage` with a rework path back
/// (`triage -> a`). The handler hop is followed forward only, so the
/// revival of `c` still covers `c2`, `j` and `done`: `j` waits for `c2` and
/// runs once.
const TRIAGE_REWORK: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
defaults: { on_failure: triage }
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: b, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: c, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: c2, type: script, script: "scripts/slowgate.sh", runner: sh }
  - { id: j, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: triage, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: triage, to: a, max_traversals: 1 }
  - { from: a, to: b }
  - { from: a, to: c }
  - { from: b, to: j }
  - { from: c, to: c2 }
  - { from: c2, to: j, condition: { type: node_status, node: c2, equals: success } }
  - { from: c2, to: failed, condition: { type: node_status, node: c2, equals: failure } }
  - { from: j, to: done }
"#;

#[test]
fn a_failure_handler_with_a_rework_path_keeps_the_revival_downstream() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let scripts = report_scripts(root);
    let (outcome, _, resumed) =
        fail_then_resume(root, TRIAGE_REWORK, &scripts, "c", true, RunStatus::Failed);
    assert_eq!(outcome, RunStatus::Succeeded);
    let j = started_at(&resumed, "j").expect("the join ran");
    assert!(
        finished_at(&resumed, "c2").unwrap() < j,
        "the join ran before c2"
    );
    assert_eq!(starts_of(&resumed, "j"), 1);
}

/// Round 5 item 4: a node whose `Running` predates its revival is not
/// interrupted work of its own. `d` was cut while it ran; a re-run asked
/// from `c` revived it and was paused before `c` started. The next resume
/// re-runs `c` first, and `d` once, after it.
const CHAIN: &str = r#"
schema: 2
id: jr
name: Join revive
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: c, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: d, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: c }
  - { from: c, to: d }
  - { from: d, to: done }
"#;

#[test]
fn a_node_running_before_its_revival_is_not_restarted_ahead_of_the_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, CHAIN, &[]);
    let first = run(root, "jr", None, RunOptions::default()).unwrap();
    let run_dir = root.join(".apb/runs").join(&first.run_id);
    keep_through(&run_dir, r#""type":"node_started","node":"d""#);
    post_control(&run_dir, drain()).unwrap();
    let paused = resume(root, &first.run_id, Some("c")).unwrap();
    assert_eq!(paused.outcome, RunStatus::Paused);
    let at = read_all(&run_dir).unwrap().len();
    assert_eq!(
        plan_resume(root, &first.run_id, None).unwrap().start_node,
        "c"
    );
    let res = resume(root, &first.run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let after = &read_all(&run_dir).unwrap()[at..];
    assert_eq!(starts_of(after, "d"), 1, "d ran on its old input first");
    assert!(finished_at(after, "c").unwrap() < started_at(after, "d").unwrap());
}

/// Round 5 item 9: one really interrupted node (`c2` cut while it ran) and
/// one owed re-run (`b`). The resume restarts `c2`, puts `b` back, and the
/// join waits for both: each runs once.
#[test]
fn an_interrupted_node_and_an_owed_rerun_both_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, REPORT, &report_scripts(root));
    fs::write(root.join(PASS), "").unwrap();
    let first = run(root, "jr", None, RunOptions::default()).unwrap();
    let run_dir = root.join(".apb/runs").join(&first.run_id);
    keep_through(&run_dir, r#""type":"node_started","node":"c2""#);
    append_lines(
        &run_dir,
        &[r#"{"seq":SEQ,"ts":1,"type":"nodes_revived","from_node":"b","nodes":["b","done","j"]}"#],
    );
    let at = read_all(&run_dir).unwrap().len();
    assert_eq!(
        plan_resume(root, &first.run_id, None).unwrap().start_node,
        "c2"
    );
    let res = resume(root, &first.run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let after = &read_all(&run_dir).unwrap()[at..];
    for n in ["b", "c2", "j"] {
        assert_eq!(starts_of(after, n), 1, "{n}");
    }
    let j = started_at(after, "j").unwrap();
    assert!(finished_at(after, "b").unwrap() < j && finished_at(after, "c2").unwrap() < j);
    assert!(!root.join(TIMED_OUT).exists());
}
