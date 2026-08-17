use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::review::{ReviewCommand, post_review};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(10);

const WF_REVIEW: &str = r#"
schema: 1
id: rev
name: Review
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

const WF_REVIEW_TITLED: &str = r#"
schema: 1
id: rev
name: Review
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, title: "Approve the release", options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

const WF_OUTPUT_MATCH: &str = r#"
schema: 1
id: rev
name: Review
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: build, type: script, script: "scripts/build.sh", runner: sh }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: build }
  - { from: build, to: ok, condition: { type: output_match, node: build, pattern: "BUILD OK" } }
  - { from: build, to: no, fallback: true }
"#;

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/rev/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/rev/current"), "1.0.0").unwrap();
}

fn run_in_background(root: &Path) -> mpsc::Receiver<RunResult> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let res = run(&root, "rev", None, RunOptions::default()).unwrap();
        let _ = tx.send(res);
    });
    rx
}

fn latest_run_dir(root: &Path) -> std::path::PathBuf {
    poll_until("run dir to appear", || {
        let runs = root.join(".apb/runs");
        let entry = fs::read_dir(&runs)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())?;
        Some(entry.path())
    })
}

fn decide(root: &Path, run_dir: &Path, decision: &str) {
    // Wait for the review request to be announced, then post the decision.
    poll_until("review_requested", || {
        read_all(run_dir).ok()?.iter().any(|e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "gate")).then_some(())
    });
    post_review(
        run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: decision.into(),
            note: "n".into(),
        },
    )
    .unwrap();
    let _ = root;
}

fn wait_result(rx: &mpsc::Receiver<RunResult>) -> RunResult {
    rx.recv_timeout(POLL_DEADLINE)
        .unwrap_or_else(|_| panic!("run did not finish within {POLL_DEADLINE:?}"))
}

#[test]
fn human_review_approved_routes_to_success() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_REVIEW);
    let rx = run_in_background(dir.path());
    let run_dir = latest_run_dir(dir.path());
    decide(dir.path(), &run_dir, "approved");
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::ReviewDecided { decision, .. } if decision == "approved")));
}

#[test]
fn human_review_rejected_routes_to_failure() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_REVIEW);
    let rx = run_in_background(dir.path());
    let run_dir = latest_run_dir(dir.path());
    decide(dir.path(), &run_dir, "rejected");
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
}

#[test]
fn human_review_entry_event_carries_instruction_and_options() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_REVIEW_TITLED);
    let rx = run_in_background(dir.path());
    let run_dir = latest_run_dir(dir.path());
    // Wait for the gate to declare itself, then inspect the entry event.
    let (options, title, instruction) = poll_until("review_requested with instruction", || {
        read_all(&run_dir)
            .ok()?
            .into_iter()
            .find_map(|e| match e.payload {
                EventPayload::ReviewRequested {
                    node,
                    options,
                    title,
                    instruction,
                } if node == "gate" => Some((options, title, instruction)),
                _ => None,
            })
    });
    assert_eq!(
        options,
        vec!["approved".to_string(), "rejected".to_string()]
    );
    assert_eq!(title.as_deref(), Some("Approve the release"));
    // The owner-facing instruction names the gate, lists the options, and says
    // how to answer (apb review CLI and the review_decide MCP tool).
    assert!(
        instruction.contains("Approve the release"),
        "got: {instruction}"
    );
    assert!(instruction.contains("approved"), "got: {instruction}");
    assert!(instruction.contains("rejected"), "got: {instruction}");
    assert!(instruction.contains("apb review"), "got: {instruction}");
    assert!(instruction.contains("review_decide"), "got: {instruction}");
    // Do not leave the run hanging: decide and let it finish.
    decide(dir.path(), &run_dir, "approved");
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
}

#[test]
fn output_match_routes_on_substring() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_OUTPUT_MATCH);
    let scripts = dir.path().join(".apb/playbooks/rev/1.0.0/scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("build.sh"), "echo 'BUILD OK'\n").unwrap();
    let result = run(dir.path(), "rev", None, RunOptions::default()).unwrap();
    assert_eq!(result.outcome, RunStatus::Succeeded);
}

// --- node validation (#103.1) ----------------------------------------------
//
// `post_review` is the single entry point every decision surface uses (the
// `apb review` CLI, MCP `review_decide`, the HTTP endpoint), so the check that
// the decided node is really a pending gate of THIS run lives there rather
// than in any one caller. These cases build the run dir by hand: the shapes
// under test (a node that is not a gate, a gate with no open request) are
// exactly the ones a live drive never produces, and a hand-built journal pins
// them without racing one.

/// A run dir carrying the given playbook snapshot plus the given journal.
fn synthetic_run_dir(yaml: &str, payloads: &[EventPayload]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("playbook.yaml"), yaml).unwrap();
    if !payloads.is_empty() {
        let mut log = apb_engine::event::EventLog::open(dir.path()).unwrap();
        for p in payloads {
            log.append(p.clone()).unwrap();
        }
    }
    dir
}

fn review_requested(node: &str) -> EventPayload {
    EventPayload::ReviewRequested {
        node: node.into(),
        options: vec!["approved".into(), "rejected".into()],
        title: None,
        instruction: String::new(),
    }
}

fn decide_on(run_dir: &Path, node: &str) -> Result<u64, apb_engine::EngineError> {
    post_review(
        run_dir,
        ReviewCommand {
            node: node.into(),
            decision: "approved".into(),
            note: String::new(),
        },
    )
}

#[test]
fn post_review_on_an_unknown_node_is_not_found() {
    let dir = synthetic_run_dir(WF_REVIEW, &[review_requested("gate")]);
    let err = decide_on(dir.path(), "ghost").unwrap_err();
    assert!(
        matches!(err, apb_engine::EngineError::NotFound(_)),
        "an unknown node must be NotFound, got: {err:?}"
    );
    assert!(
        !dir.path().join("reviews.jsonl").exists(),
        "a rejected decision must not be written to the channel"
    );
}

#[test]
fn post_review_on_a_node_that_is_not_a_gate_is_not_found() {
    let dir = synthetic_run_dir(WF_REVIEW, &[review_requested("gate")]);
    let err = decide_on(dir.path(), "start").unwrap_err();
    assert!(
        matches!(err, apb_engine::EngineError::NotFound(_)),
        "a non-human_review node must be NotFound, got: {err:?}"
    );
}

#[test]
fn post_review_on_a_gate_that_is_not_pending_is_a_conflict() {
    // The gate exists but nothing has requested a decision on it yet.
    let dir = synthetic_run_dir(WF_REVIEW, &[]);
    let err = decide_on(dir.path(), "gate").unwrap_err();
    assert!(
        matches!(err, apb_engine::EngineError::Conflict(_)),
        "a gate with no open request must be Conflict, got: {err:?}"
    );

    // Already decided: the request is consumed, so a second decision is a
    // conflict too.
    let dir = synthetic_run_dir(
        WF_REVIEW,
        &[
            review_requested("gate"),
            EventPayload::ReviewDecided {
                node: "gate".into(),
                decision: "approved".into(),
                note: String::new(),
            },
        ],
    );
    let err = decide_on(dir.path(), "gate").unwrap_err();
    assert!(
        matches!(err, apb_engine::EngineError::Conflict(_)),
        "an already-decided gate must be Conflict, got: {err:?}"
    );
}

#[test]
fn post_review_on_the_pending_gate_is_accepted() {
    let dir = synthetic_run_dir(WF_REVIEW, &[review_requested("gate")]);
    assert_eq!(decide_on(dir.path(), "gate").unwrap(), 0);
    let channel = fs::read_to_string(dir.path().join("reviews.jsonl")).unwrap();
    assert!(channel.contains("approved"), "got: {channel}");
}

#[test]
fn post_review_without_a_run_snapshot_stays_permissive() {
    // Pre-snapshot runs carry no playbook.yaml, so there is nothing to
    // validate the node against. Those keep the old accept-everything
    // behavior rather than becoming undecidable.
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(decide_on(dir.path(), "gate").unwrap(), 0);
}
