//! Approval of an exact snapshot set: binding a request, deciding it with
//! its nonce, and the checks before a node that requires the approval.
//!
//! Every test drives a real run of script nodes (no agent, no model) and
//! decides the gate through the channel every decision surface posts to.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::approval::{ReviewBinding, binding_nonce};
use apb_engine::decision_channel::DecisionChannel;
use apb_engine::error::EngineError;
use apb_engine::event::{DecidedBy, Event, EventPayload, read_all};
use apb_engine::review::{ReviewCommand, post_decision_via};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const DEADLINE: Duration = Duration::from_secs(60);

fn seed(root: &Path, yaml: &str, scripts: &[(&str, &str)]) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/ap/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/ap/current"), "1.0.0").unwrap();
    for (name, body) in scripts {
        crate::common::write_sync(&vdir.join("scripts").join(name), body);
    }
}

/// Starts the run on a thread; the receiver yields its outcome and run dir.
fn start(root: &Path, opts: RunOptions) -> mpsc::Receiver<(RunStatus, PathBuf)> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let r = run(&root, "ap", None, opts).unwrap();
        let _ = tx.send((r.outcome, root.join(".apb/runs").join(&r.run_id)));
    });
    rx
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn the_run_dir(root: &Path) -> PathBuf {
    poll("the run dir", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir())
    })
}

/// The seq and binding of the `n`-th (1-based) request of gate `g`.
fn request(rd: &Path, n: usize) -> (u64, Option<ReviewBinding>) {
    poll("the review request", || {
        read_all(rd)
            .ok()?
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::ReviewRequested { node, binding, .. } if node == "g" => {
                    Some((e.seq, binding))
                }
                _ => None,
            })
            .nth(n - 1)
    })
}

fn decide(rd: &Path, decision: &str, nonce: Option<&str>) -> Result<u64, EngineError> {
    post_decision_via(
        rd,
        ReviewCommand {
            node: "g".into(),
            decision: decision.into(),
            note: String::new(),
        },
        nonce.map(str::to_string),
        None,
        DecisionChannel::Terminal,
    )
}

fn finish(rx: &mpsc::Receiver<(RunStatus, PathBuf)>) -> (RunStatus, Vec<Event>) {
    let (status, rd) = rx.recv_timeout(DEADLINE).expect("the run ended in time");
    (status, read_all(&rd).unwrap())
}

/// The output `node` settled with, last first.
fn output_of(events: &[Event], node: &str) -> Option<(String, String)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node: n,
            status,
            output,
            ..
        } if n == node => Some((status.clone(), output.clone())),
        _ => None,
    })
}

fn attempted(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node))
}

/// `w` writes `report.md`, the gate binds it, `edit` runs the given body,
/// `d` requires the approval and leaves `acted.txt` when it acts.
fn linear(root: &Path, requires: &str, edit: &str) {
    let yaml = format!(
        "schema: 2\nid: ap\nname: ap\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: script, script: scripts/w.sh, runner: sh, results: [ {{ path: report.md }} ] }}\n  - {{ id: g, type: human_review, options: [approve, reject], approves: {{ results: [w] }} }}\n  - {{ id: edit, type: script, script: scripts/edit.sh, runner: sh }}\n  - {{ id: d, type: script, script: scripts/d.sh, runner: sh, requires_approval: {requires} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: g }}\n  - {{ from: g, to: edit }}\n  - {{ from: edit, to: d }}\n  - {{ from: d, to: done }}\n"
    );
    seed(
        root,
        &yaml,
        &[
            ("w.sh", "printf 'draft one\\n' > report.md\necho ok\n"),
            ("edit.sh", edit),
            ("d.sh", "echo acted > acted.txt\necho ok\n"),
        ],
    );
}

/// The request binds the latest snapshot of `w` under a nonce over the run,
/// the gate and the visit; approving needs that nonce, a wrong one is a
/// conflict, and the approved node acts. The decision is journaled with the
/// nonce and the channel.
#[test]
fn an_approval_binds_the_snapshot_and_lets_the_node_act() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    linear(root, "{ gate: g }", "echo ok\n");
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let (_, binding) = request(&rd, 1);
    let b = binding.expect("the request is bound");
    let events = read_all(&rd).unwrap();
    let (w_seq, w_stamp) = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node,
                result: Some(s),
                ..
            } if node == "w" => Some((e.seq, s.snapshot_id.clone())),
            _ => None,
        })
        .expect("w was snapshotted");
    assert_eq!(b.snapshots.len(), 1, "{b:?}");
    assert_eq!(
        (b.snapshots[0].settlement_seq, &b.snapshots[0].snapshot_id),
        (w_seq, &w_stamp)
    );
    let run_id = rd.file_name().unwrap().to_str().unwrap();
    assert_eq!(b.nonce, binding_nonce(run_id, "g", 1, &b.snapshots, &[]));

    let err = decide(&rd, "approve", None).unwrap_err();
    assert!(
        matches!(&err, EngineError::Invalid(m) if m.contains(&b.nonce)),
        "{err:?}"
    );
    let err = decide(&rd, "approve", Some("approval-0000")).unwrap_err();
    assert!(matches!(err, EngineError::Conflict(_)), "{err:?}");
    decide(&rd, "approve", Some(&b.nonce)).unwrap();

    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Succeeded);
    let decided = events.iter().find_map(|e| match &e.payload {
        EventPayload::ReviewDecided { binding, via, .. } => Some((binding.clone(), *via)),
        _ => None,
    });
    assert_eq!(
        decided,
        Some((Some(b.nonce.clone()), Some(DecisionChannel::Terminal)))
    );
    assert!(root.join("acted.txt").is_file());
}

/// The refusal of an agent's channel on a person-only bound gate names a
/// command a person can run as is: it carries the open binding's nonce and
/// request, and that command's decision is accepted at a terminal.
#[test]
fn a_person_only_refusal_names_the_binding_a_person_decides_with() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    linear(root, "{ gate: g }", "echo ok\n");
    let yaml = root.join(".apb/playbooks/ap/1.0.0/playbook.yaml");
    let text = fs::read_to_string(&yaml).unwrap();
    fs::write(
        &yaml,
        text.replace("approves: {", "person_only: true, approves: {"),
    )
    .unwrap();
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let (seq, binding) = request(&rd, 1);
    let b = binding.expect("the request is bound");
    let refused = post_decision_via(
        &rd,
        ReviewCommand {
            node: "g".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        Some(b.nonce.clone()),
        None,
        DecisionChannel::Mcp,
    );
    let Err(EngineError::ChannelRefused { message, .. }) = refused else {
        panic!("{refused:?}");
    };
    let args = format!(" --nonce {} --request review:{seq}`", b.nonce);
    assert!(message.contains(&args), "{message}");
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    let (status, _) = finish(&rx);
    assert_eq!(status, RunStatus::Succeeded);
}

/// `live: check`: a bound file changed after the approval refuses the node
/// before it starts any attempt, and the run follows the failure.
#[test]
fn a_changed_file_refuses_the_node_before_any_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    linear(
        root,
        "{ gate: g }",
        "printf 'edited\\n' > report.md\necho ok\n",
    );
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Failed);
    let (st, out) = output_of(&events, "d").expect("d settled");
    assert_eq!(st, "failed");
    assert!(
        out.starts_with("approval_stale: report.md changed"),
        "{out}"
    );
    assert!(!attempted(&events, "d"));
    assert!(!root.join("acted.txt").exists());
}

/// The check runs again before the attempt of a resumed visit, not only at
/// admission: `d` was admitted and is paused `--now` mid-attempt, the
/// approved file changes during the pause, and the resume settles `d`
/// failed `approval_stale` without a second attempt.
#[cfg(unix)]
#[test]
fn a_file_changed_during_a_pause_refuses_the_resumed_attempt() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    linear(root, "{ gate: g }", "echo ok\n");
    crate::common::write_sync(
        &root.join(".apb/playbooks/ap/1.0.0/scripts/d.sh"),
        "touch d_running\nwhile [ -f slow ]; do sleep 0.05; done\necho acted > acted.txt\necho ok\n",
    );
    fs::write(root.join("slow"), "").unwrap();
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    poll("d to start", || {
        root.join("d_running").is_file().then_some(())
    });
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            reason: Some("later".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Now,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(finish(&rx).0, RunStatus::Paused);
    fs::remove_file(root.join("slow")).unwrap();
    fs::write(root.join("report.md"), "edited during the pause\n").unwrap();
    fs::remove_file(root.join("d_running")).unwrap();

    let res = apb_engine::scheduler::resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&rd).unwrap();
    let (st, out) = output_of(&events, "d").expect("d settled");
    assert_eq!(st, "failed");
    assert!(
        out.starts_with("approval_stale: report.md changed"),
        "{out}"
    );
    assert!(
        !root.join("d_running").exists(),
        "the script did not run after the resume"
    );
    assert!(!root.join("acted.txt").exists());
}

/// `live: ignore` lets the node act on live content that differs.
#[test]
fn live_ignore_lets_the_node_act_on_a_changed_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    linear(
        root,
        "{ gate: g, live: ignore }",
        "printf 'edited\\n' > report.md\necho ok\n",
    );
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    let (status, _) = finish(&rx);
    assert_eq!(status, RunStatus::Succeeded);
    assert!(root.join("acted.txt").is_file());
}

/// A decision that does not approve needs no nonce (it is stamped with the
/// open one) and gives the required node no approval.
#[test]
fn a_decision_that_does_not_approve_is_approval_missing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    linear(root, "{ gate: g }", "echo ok\n");
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    decide(&rd, "reject", None).unwrap();
    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Failed);
    let binding = events.iter().find_map(|e| match &e.payload {
        EventPayload::ReviewDecided { binding, .. } => binding.clone(),
        _ => None,
    });
    assert_eq!(binding, Some(b.nonce));
    let (_, out) = output_of(&events, "d").unwrap();
    assert!(
        out.starts_with("approval_missing: gate `g` decided `reject`"),
        "{out}"
    );
    assert!(!attempted(&events, "d"));
}

/// A producer that ran again after the approval makes it stale.
#[test]
fn a_producer_that_ran_again_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: ap
name: ap
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: scripts/w.sh, runner: sh, results: [ { path: report.md } ] }
  - { id: g, type: human_review, options: [approve, reject], approves: { results: [w] } }
  - { id: d, type: script, script: scripts/d.sh, runner: sh, requires_approval: { gate: g, live: ignore } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: g, condition: { type: output_field, node: w, field: round, equals: "1" } }
  - { from: w, to: d, fallback: true }
  - { from: g, to: w, max_traversals: 3 }
  - { from: d, to: done }
"#;
    let w = "n=$(cat .n 2>/dev/null || echo 0); n=$((n+1)); echo $n > .n\nprintf 'round %s\\n' \"$n\" > report.md\necho \"{\\\"round\\\":\\\"$n\\\"}\"\n";
    seed(
        root,
        yaml,
        &[("w.sh", w), ("d.sh", "echo acted > acted.txt\necho ok\n")],
    );
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Failed);
    let (_, out) = output_of(&events, "d").unwrap();
    assert_eq!(out, "approval_stale: w ran again after approval");
    assert!(!root.join("acted.txt").exists());
}

/// A bound snapshot that no longer verifies when the decision is consumed
/// ends the run failed before anything is decided.
#[test]
fn a_tampered_snapshot_ends_the_run_at_consumption() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    linear(root, "{ gate: g }", "echo ok\n");
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    let objects = rd.join("results/objects");
    for shard in fs::read_dir(&objects).unwrap() {
        for obj in fs::read_dir(shard.unwrap().path()).unwrap() {
            fs::write(obj.unwrap().path(), "tampered").unwrap();
        }
    }
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Failed);
    let reason = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunError { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    assert!(
        reason
            .as_deref()
            .is_some_and(|r| r.starts_with("approval_snapshot_unreadable: snapshot ")),
        "{reason:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDecided { .. }))
    );
}

/// A gate asked again over the same snapshot binds a new nonce: the
/// nonce of the earlier visit is a conflict, never an answer.
#[test]
fn a_nonce_of_an_earlier_visit_does_not_answer_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: ap
name: ap
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: scripts/w.sh, runner: sh, results: [ { path: report.md } ] }
  - { id: g, type: human_review, options: [approve, reject], approves: { results: [w] } }
  - { id: x, type: script, script: scripts/x.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: g }
  - { from: g, to: x, condition: { type: review_status, equals: reject }, max_traversals: 2 }
  - { from: x, to: g }
  - { from: g, to: done, condition: { type: review_status, equals: approve } }
"#;
    seed(
        root,
        yaml,
        &[
            ("w.sh", "printf 'draft\\n' > report.md\necho ok\n"),
            ("x.sh", "echo ok\n"),
        ],
    );
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let first = request(&rd, 1).1.unwrap();
    decide(&rd, "reject", None).unwrap();
    let second = request(&rd, 2).1.unwrap();
    assert_eq!(first.snapshots, second.snapshots);
    assert_ne!(first.nonce, second.nonce);
    let err = decide(&rd, "approve", Some(&first.nonce)).unwrap_err();
    assert!(matches!(err, EngineError::Conflict(_)), "{err:?}");
    decide(&rd, "approve", Some(&second.nonce)).unwrap();
    assert_eq!(finish(&rx).0, RunStatus::Succeeded);
}

/// An unattended run's declared approval is stamped with the open binding,
/// labelled `unattended` without a channel, and satisfies the node that
/// requires it.
#[test]
fn an_unattended_decide_binds_the_nonce_and_satisfies_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: ap
name: ap
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: scripts/w.sh, runner: sh, results: [ { path: report.md } ] }
  - { id: g, type: human_review, options: [approve, reject], approves: { results: [w] }, unattended: { decide: approve, note: ci } }
  - { id: d, type: script, script: scripts/d.sh, runner: sh, requires_approval: { gate: g } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: g }
  - { from: g, to: d }
  - { from: d, to: done }
"#;
    seed(
        root,
        yaml,
        &[
            ("w.sh", "printf 'draft\\n' > report.md\necho ok\n"),
            ("d.sh", "echo acted > acted.txt\necho ok\n"),
        ],
    );
    let rx = start(
        root,
        RunOptions {
            unattended: true,
            ..RunOptions::default()
        },
    );
    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Succeeded);
    let open = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested {
                binding: Some(b), ..
            } => Some(b.nonce.clone()),
            _ => None,
        })
        .expect("a bound request");
    let decided = events.iter().find_map(|e| match &e.payload {
        EventPayload::ReviewDecided {
            binding,
            decided_by,
            via,
            ..
        } => Some((binding.clone(), *decided_by, *via)),
        _ => None,
    });
    assert_eq!(
        decided,
        Some((Some(open), Some(DecidedBy::Unattended), None))
    );
    assert!(root.join("acted.txt").is_file());
}

/// A node a gate's `approves` names takes a snapshot of its output even
/// without `results:` of its own, so the request binds it.
#[test]
fn a_node_named_only_by_approves_is_snapshotted_and_bound() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: ap
name: ap
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: scripts/w.sh, runner: sh }
  - { id: g, type: human_review, options: [approve, reject], approves: { results: [w] } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: g }
  - { from: g, to: done }
"#;
    seed(root, yaml, &[("w.sh", "echo the summary\n")]);
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    assert_eq!(b.snapshots.len(), 1, "{b:?}");
    assert!(b.missing.is_empty(), "{b:?}");
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    assert_eq!(finish(&rx).0, RunStatus::Succeeded);
}

/// A line with the binding nonce but without the request stamp (an older
/// poster, a hand edit) never answers a bound request, and never stands in
/// the way of a decision that does.
#[test]
fn an_entry_without_the_request_stamp_is_never_consumed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    linear(root, "{ gate: g }", "echo ok\n");
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    fs::write(
        rd.join("reviews.jsonl"),
        format!(
            "{{\"seq\":0,\"node\":\"g\",\"decision\":\"approve\",\"note\":\"\",\"nonce\":\"{}\"}}\n",
            b.nonce
        ),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        !read_all(&rd)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDecided { .. })),
        "an unstamped entry was consumed"
    );
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Succeeded);
    // Exactly one decision was taken, the stamped one: both entries say
    // `approve` with the same nonce, so the outcome alone cannot tell.
    let decided: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided { binding, via, .. } => Some((binding.clone(), *via)),
            _ => None,
        })
        .collect();
    assert_eq!(
        decided,
        [(Some(b.nonce.clone()), Some(DecisionChannel::Terminal))]
    );
}

/// A batch member that requires an approval it does not have runs nothing
/// and settles failed, while its sibling runs.
#[test]
fn a_batch_member_without_its_approval_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: ap
name: ap
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: scripts/w.sh, runner: sh, results: [ { path: report.md } ] }
  - { id: g, type: human_review, options: [approve, reject], approves: { results: [w] } }
  - { id: edit, type: script, script: scripts/edit.sh, runner: sh }
  - { id: d, type: script, script: scripts/d.sh, runner: sh }
  - { id: e, type: script, script: scripts/e.sh, runner: sh, requires_approval: { gate: g } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: g }
  - { from: g, to: edit }
  - { from: edit, to: d }
  - { from: edit, to: e }
  - { from: d, to: done }
  - { from: e, to: done }
"#;
    seed(
        root,
        yaml,
        &[
            ("w.sh", "printf 'draft\\n' > report.md\necho ok\n"),
            ("edit.sh", "printf 'edited\\n' > report.md\necho ok\n"),
            ("d.sh", "echo ran > d.txt\necho ok\n"),
            ("e.sh", "echo acted > acted.txt\necho ok\n"),
        ],
    );
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let b = request(&rd, 1).1.unwrap();
    decide(&rd, "approve", Some(&b.nonce)).unwrap();
    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Failed);
    let (_, out) = output_of(&events, "e").expect("e settled");
    assert!(
        out.starts_with("approval_stale: report.md changed"),
        "{out}"
    );
    assert!(!attempted(&events, "e"));
    assert!(!root.join("acted.txt").exists());
    assert!(root.join("d.txt").is_file());
}

/// `w` runs (failing once when `flaky`), `g` is person-only with no
/// `approves`, and `d`, which declares nothing, leaves `acted.txt`.
fn person_only_gate(root: &Path, w: &str) {
    seed(
        root,
        "schema: 2\nid: ap\nname: ap\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: script, script: scripts/w.sh, runner: sh }\n  - { id: g, type: human_review, options: [approve, reject], person_only: true }\n  - { id: d, type: script, script: scripts/d.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: g }\n  - { from: g, to: d }\n  - { from: d, to: done }\n",
        &[("w.sh", w), ("d.sh", "echo acted > acted.txt\necho ok\n")],
    );
}

/// The `directive_refused` actions journaled so far, as `(target, detail)`.
fn refused_moves(rd: &Path) -> Vec<(String, String)> {
    read_all(rd)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::SupervisorAction {
                action,
                node: Some(node),
                detail,
            } if action == apb_engine::event::supervisor_action::DIRECTIVE_REFUSED => {
                Some((node, detail))
            }
            _ => None,
        })
        .collect()
}

/// `run_continue_from` and `node_retry` of a node behind a person-only gate
/// no person decided are refused while the run waits on the gate: the
/// refusal is journaled, the request stays open, nothing past the gate
/// runs, and the person's decision then moves the run on.
#[test]
fn a_move_past_an_undecided_person_only_gate_is_refused_at_the_gate() {
    use apb_engine::control::{Control, post_control};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    person_only_gate(root, "echo ok\n");
    let rx = start(root, RunOptions::default());
    let rd = the_run_dir(root);
    let (first, _) = request(&rd, 1);
    post_control(&rd, Control::ContinueFrom { node: "d".into() }).unwrap();
    post_control(
        &rd,
        Control::Retry {
            node: "d".into(),
            prompt_override: None,
        },
    )
    .unwrap();
    let refused = poll("both refusals", || {
        let r = refused_moves(&rd);
        (r.len() == 2).then_some(r)
    });
    for (node, detail) in &refused {
        assert_eq!(node, "d");
        assert!(
            detail.starts_with("approval_weakened: `d` skips gate `g`"),
            "{detail}"
        );
    }
    let events = read_all(&rd).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewWithdrawn { .. })),
        "the request stays open"
    );
    assert!(!attempted(&events, "d"));
    assert!(!root.join("acted.txt").exists());
    // The same request is still the one a person answers.
    post_decision_via(
        &rd,
        ReviewCommand {
            node: "g".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        Some(first),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let (status, events) = finish(&rx);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(
        events
            .iter()
            .filter(
                |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "g")
            )
            .count(),
        1
    );
    assert!(root.join("acted.txt").is_file());
}

/// A supervised run parked on a failed node before the gate: a
/// `run_continue_from` past the gate is refused and the park goes on
/// waiting; a retry of the failed node is taken.
#[test]
fn a_move_past_a_person_only_gate_is_refused_at_a_failed_node() {
    use apb_engine::control::{Control, post_control};
    use apb_engine::scheduler::RunMode;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    person_only_gate(
        root,
        "if [ -f once ]; then echo ok; else touch once; echo boom >&2; exit 1; fi\n",
    );
    let rx = start(
        root,
        RunOptions {
            mode: RunMode::Supervised,
            ..Default::default()
        },
    );
    let rd = the_run_dir(root);
    poll("the wake on w", || {
        read_all(&rd)
            .ok()?
            .into_iter()
            .find(|e| matches!(e.payload, EventPayload::WakeRaised { .. }))
    });
    post_control(&rd, Control::ContinueFrom { node: "d".into() }).unwrap();
    poll("the refusal", || {
        (refused_moves(&rd).len() == 1).then_some(())
    });
    assert!(!attempted(&read_all(&rd).unwrap(), "d"));
    post_control(
        &rd,
        Control::Retry {
            node: "w".into(),
            prompt_override: None,
        },
    )
    .unwrap();
    let (seq, _) = request(&rd, 1);
    post_decision_via(
        &rd,
        ReviewCommand {
            node: "g".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        None,
        Some(seq),
        DecisionChannel::Terminal,
    )
    .unwrap();
    let (status, _) = finish(&rx);
    assert_eq!(status, RunStatus::Succeeded);
    assert!(root.join("acted.txt").is_file());
}
