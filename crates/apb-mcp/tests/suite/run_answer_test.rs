//! `tools::run_answer` and `run_status`'s `pending_question` (spec
//! 2026-07-20-interactive-nodes, Task 8). The supervisor-token dispatch
//! itself (mint_token, resolve_session, the `#[tool] run_answer` handler) is
//! tested in `crates/apb-mcp/src/server/tests.rs`, the only place with access
//! to those private WfMcp methods; these tests exercise the same
//! `answered_by` policy at the `tools::run_answer` level it is built on, plus
//! `run_status` and the wake plumbing.

use apb_engine::event::{EventLog, EventPayload, WakeTrigger};
use apb_engine::question::{post_question, read_answers_after};
use apb_mcp::tools::{run_answer, run_status, supervisor_wait_event};
use std::fs;
use std::path::{Path, PathBuf};

const HUMAN_ASK_PB: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: f }\n";

const SUPERVISOR_ASK_PB: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true, answer_by: supervisor }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: f }\n";

const RUN_STARTED_EVENTS: &str =
    "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n";

fn bare_run_dir(root: &Path, run_id: &str) -> PathBuf {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    run_dir
}

#[test]
fn run_answer_human_path_posts_answers_channel_entry() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r1");
    fs::write(run_dir.join("playbook.yaml"), HUMAN_ASK_PB).unwrap();
    post_question(
        &run_dir,
        "ask",
        1,
        "which way",
        vec!["left".into(), "right".into()],
    )
    .unwrap();

    let res = run_answer(dir.path(), "r1", None, "left", "human").unwrap();
    assert_eq!(res["posted_seq"], 0);

    let answers = read_answers_after(&run_dir, None).unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].node, "ask");
    assert_eq!(answers[0].answer, "left");
    assert_eq!(answers[0].answered_by, "human");
}

#[test]
fn run_status_carries_pending_question_and_clears_after_answer() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r2");
    fs::write(run_dir.join("playbook.yaml"), SUPERVISOR_ASK_PB).unwrap();
    fs::write(run_dir.join("events.jsonl"), RUN_STARTED_EVENTS).unwrap();
    post_question(
        &run_dir,
        "ask",
        1,
        "which way",
        vec!["left".into(), "right".into()],
    )
    .unwrap();

    let status = run_status(dir.path(), "r2").unwrap();
    assert_eq!(status["progress"]["waiting_kind"], "question");
    let pq = &status["pending_question"];
    assert_eq!(pq["node"], "ask");
    assert_eq!(pq["question"], "which way");
    assert_eq!(pq["options"], serde_json::json!(["left", "right"]));
    assert_eq!(pq["answer_by"], "supervisor");
    assert!(
        pq.get("asked_at").is_some(),
        "asked_at key must be present, got {pq}"
    );

    run_answer(dir.path(), "r2", Some("ask"), "left", "supervisor").unwrap();

    let status2 = run_status(dir.path(), "r2").unwrap();
    assert!(
        status2["pending_question"].is_null(),
        "pending_question must clear after an answer, got {status2}"
    );
    assert!(status2["progress"]["waiting_kind"].is_null());
}

#[test]
fn run_answer_supervisor_path_accepted_on_answer_by_supervisor_node() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r3");
    fs::write(run_dir.join("playbook.yaml"), SUPERVISOR_ASK_PB).unwrap();
    post_question(&run_dir, "ask", 1, "which way", Vec::new()).unwrap();

    let res = run_answer(dir.path(), "r3", None, "left", "supervisor").unwrap();
    assert_eq!(res["posted_seq"], 0);

    let answers = read_answers_after(&run_dir, None).unwrap();
    assert_eq!(answers[0].answered_by, "supervisor");
}

#[test]
fn run_answer_supervisor_path_rejected_on_answer_by_human_node() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r4");
    fs::write(run_dir.join("playbook.yaml"), HUMAN_ASK_PB).unwrap();
    post_question(&run_dir, "ask", 1, "which way", Vec::new()).unwrap();

    let err = run_answer(dir.path(), "r4", None, "left", "supervisor").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("relay"),
        "expected a relay instruction, got: {msg}"
    );

    // The rejected supervisor answer must not have been appended.
    let answers = read_answers_after(&run_dir, None).unwrap();
    assert!(
        answers.is_empty(),
        "a rejected answer must not land in the channel, got {answers:?}"
    );
}

/// Task 4 already raises a `WakeRaised` the moment a question is asked (the
/// drive loop, not this task); Task 8's own responsibility is that the
/// plumbing from that wake through to `supervisor_wait_event` still works
/// after a facade answers the question. The event log is hand-built rather
/// than driven through a live agent: `wait_wake` is a pure scan over
/// `events.jsonl`, so this proves the same thing a live drive would without
/// the flakiness of spawning one.
#[test]
fn wake_from_asked_question_still_reaches_supervisor_wait_event_after_answer() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r5");
    fs::write(run_dir.join("playbook.yaml"), HUMAN_ASK_PB).unwrap();

    let mut log = EventLog::create(&run_dir).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "p".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::NodeStarted {
        node: "ask".into(),
        attempt: 1,
    })
    .unwrap();
    log.append(EventPayload::QuestionAsked {
        node: "ask".into(),
        question: "which way".into(),
        options: Vec::new(),
    })
    .unwrap();
    log.append(EventPayload::WakeRaised {
        trigger: WakeTrigger::Anomaly,
        node: "ask".into(),
        detail: "interactive question".into(),
    })
    .unwrap();
    drop(log);
    post_question(&run_dir, "ask", 1, "which way", Vec::new()).unwrap();

    run_answer(dir.path(), "r5", None, "left", "human").unwrap();

    let out = supervisor_wait_event(dir.path(), "r5", None, Some(2_000)).unwrap();
    assert!(
        !out["wake"].is_null(),
        "expected a non-null wake within the timeout, got {out}"
    );
    assert_eq!(out["wake"]["node"], "ask");
}
