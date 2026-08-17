use apb_mcp::tools::review_decide;
use std::fs;

#[test]
fn review_decide_writes_reviews_channel() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join(".apb/runs/r1");
    fs::create_dir_all(&run_dir).unwrap();

    let res = review_decide(dir.path(), "r1", "gate", "approved", "lgtm").unwrap();
    assert!(res["posted_seq"].is_number());

    let channel = fs::read_to_string(run_dir.join("reviews.jsonl")).unwrap();
    let line: serde_json::Value = serde_json::from_str(channel.lines().next().unwrap()).unwrap();
    assert_eq!(line["node"], "gate");
    assert_eq!(line["decision"], "approved");
    assert_eq!(line["note"], "lgtm");
}

#[test]
fn review_decide_unknown_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    assert!(review_decide(dir.path(), "ghost", "gate", "approved", "").is_err());
}

#[test]
fn review_decide_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    assert!(review_decide(dir.path(), "../evil", "gate", "approved", "").is_err());
}

const GATE: &str = r#"
schema: 1
id: gated
name: Gated
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

/// #103.1 inherited: the node check lives in `apb_engine::post_review`, so the
/// MCP tool refuses a node that is not a gate of this run and a gate with
/// nothing pending, and the existing `EngineError -> ToolError` conversion
/// carries the distinction (not-found vs conflict) to the agent.
#[test]
fn review_decide_validates_the_node_against_the_run_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join(".apb/runs/r1");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("playbook.yaml"), GATE).unwrap();

    let err = review_decide(dir.path(), "r1", "ghost", "approved", "").unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "a node that is not a gate must be not-found, got: {err:?}"
    );

    let err = review_decide(dir.path(), "r1", "gate", "approved", "").unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::Conflict(_)),
        "a gate with nothing pending must be a conflict, got: {err:?}"
    );

    // With the request journaled, the same call is accepted.
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::ReviewRequested {
        node: "gate".into(),
        options: vec!["approved".into(), "rejected".into()],
        title: None,
        instruction: String::new(),
    })
    .unwrap();
    let res = review_decide(dir.path(), "r1", "gate", "approved", "").unwrap();
    assert_eq!(res["posted_seq"], serde_json::json!(0));
}
