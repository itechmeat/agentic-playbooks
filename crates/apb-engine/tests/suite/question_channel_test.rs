//! Integration coverage for the question/answer channels (`question.rs`,
//! spec 2026-07-20-interactive-nodes), focused on the `answer_by` policy
//! check in `post_answer`, which needs a real run dir carrying a playbook
//! snapshot (`load_run_playbook` reads `<run_dir>/playbook.yaml`). Pure IO:
//! no agent is spawned and no wait is needed.

use std::fs;
use std::path::Path;

use apb_engine::error::EngineError;
use apb_engine::question::post_answer;

/// Writes a minimal schema-2 run snapshot with one interactive `agent_task`
/// node named `ask` whose `answer_by` is `answer_by`. `progress::
/// load_run_playbook` parses this via `Playbook::from_yaml` (a pure
/// structural parse, no profile files needed on disk), so this is a real
/// prepared run dir in the same sense drive would leave behind, without
/// paying for an actual drive/agent run.
fn write_run_snapshot(run_dir: &Path, answer_by: &str) {
    fs::create_dir_all(run_dir).unwrap();
    let yaml = format!(
        r#"
schema: 2
id: interactive
name: Interactive
version: 1.0.0
defaults:
  profile: main
nodes:
  - {{ id: start, type: start }}
  - {{ id: ask, type: agent_task, prompt: "q", interactive: true, answer_by: {answer_by} }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: ask }}
  - {{ from: ask, to: done }}
"#
    );
    fs::write(run_dir.join("playbook.yaml"), yaml).unwrap();
}

// Brief 1(c): `answer_by: human` rejects an answer arriving through the
// supervisor-token path, and accepts both `human` and `timeout`.
#[test]
fn answer_by_human_rejects_supervisor_but_accepts_human_and_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    write_run_snapshot(&run_dir, "human");

    let err = post_answer(&run_dir, Some("ask"), "hi", "supervisor").unwrap_err();
    match &err {
        EngineError::Invalid(msg) => {
            assert!(msg.contains("relay"), "expected `relay` in message: {msg}");
            assert!(msg.contains("human"), "expected `human` in message: {msg}");
        }
        other => panic!("expected EngineError::Invalid, got {other:?}"),
    }

    post_answer(&run_dir, Some("ask"), "hi", "human")
        .expect("answer_by: human must accept an answer posted as human");
    post_answer(&run_dir, Some("ask"), "hi", "timeout")
        .expect("answer_by: human must accept an answer posted as timeout (drive's own path)");
}

// Brief 1(d): `answer_by: supervisor` accepts both `human` and `supervisor`.
#[test]
fn answer_by_supervisor_accepts_both_human_and_supervisor() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    write_run_snapshot(&run_dir, "supervisor");

    post_answer(&run_dir, Some("ask"), "hi", "human")
        .expect("answer_by: supervisor must accept an answer posted as human");
    post_answer(&run_dir, Some("ask"), "hi", "supervisor")
        .expect("answer_by: supervisor must accept an answer posted as supervisor");
}
