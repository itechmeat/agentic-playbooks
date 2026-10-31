use apb_mcp::tools::review_decide;
use std::fs;

#[test]
fn review_decide_writes_reviews_channel() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join(".apb/runs/r1");
    fs::create_dir_all(&run_dir).unwrap();

    let res = review_decide(dir.path(), "r1", "gate", "approved", "lgtm", None).unwrap();
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
    assert!(review_decide(dir.path(), "ghost", "gate", "approved", "", None).is_err());
}

#[test]
fn review_decide_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    assert!(review_decide(dir.path(), "../evil", "gate", "approved", "", None).is_err());
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

    let err = review_decide(dir.path(), "r1", "ghost", "approved", "", None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "a node that is not a gate must be not-found, got: {err:?}"
    );

    let err = review_decide(dir.path(), "r1", "gate", "approved", "", None).unwrap_err();
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
        prompt: None,
        recommendation: None,
        plan_digest: None,
    })
    .unwrap();
    let res = review_decide(dir.path(), "r1", "gate", "approved", "", None).unwrap();
    assert_eq!(res["posted_seq"], serde_json::json!(0));
}

const PUBLISH: &str = r#"
schema: 2
id: pub
name: Publish
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/work.sh", runner: sh }
  - { id: publish, type: publish, from: work, remote: origin }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: publish }
  - { from: publish, to: done }
"#;

/// A run parked on a publish gate: the pin, the plan and the open request,
/// as the drive journals them. Returns the plan's nonce.
fn parked_on_publish(run_dir: &std::path::Path) -> String {
    use apb_engine::event::{CommittedArtifact, EventPayload, PublishCommit};
    fs::create_dir_all(run_dir).unwrap();
    fs::write(run_dir.join("playbook.yaml"), PUBLISH).unwrap();
    let after = "a".repeat(40);
    let url = "file:///srv/remote.git";
    let commits = [after.clone()];
    let digest = apb_core::publish::plan_digest(&apb_core::publish::PlanFacts {
        node: "publish",
        url,
        branch: "main",
        after: &after,
        remote_before: None,
        commits: &commits,
    });
    let nonce = apb_core::publish::nonce(&digest);
    let mut log = apb_engine::event::EventLog::open(run_dir).unwrap();
    for payload in [
        EventPayload::PublishTargetPinned {
            node: "publish".into(),
            remote: "origin".into(),
            url: Some(url.into()),
            scheme: Some("file".into()),
            common_dir: "/srv/tree/.git".into(),
            refusal: None,
        },
        EventPayload::PublishPlanned {
            node: "publish".into(),
            from: "work".into(),
            from_seq: 1,
            branch: "main".into(),
            local_branch: "refs/heads/main".into(),
            after: after.clone(),
            remote_before: None,
            url: url.into(),
            commits: vec![PublishCommit {
                commit: CommittedArtifact {
                    sha: after.clone(),
                    subject: "work of the run".into(),
                    apb_run: Some(vec!["r1".into()]),
                },
                this_run: true,
            }],
            omitted: 0,
            this_run: 1,
            outside_push_suspected: false,
            plan_digest: digest.clone(),
            nonce: nonce.clone(),
        },
        EventPayload::ReviewRequested {
            node: "publish".into(),
            options: vec!["publish".into(), "skip".into()],
            title: None,
            instruction: format!("Publish with nonce {nonce}"),
            prompt: None,
            recommendation: None,
            plan_digest: Some(digest),
        },
    ] {
        log.append(payload).unwrap();
    }
    nonce
}

/// A publish gate: `run_status` carries the plan in `pending_review` and
/// the `publish` block, and `review_decide` passes the nonce on: missing
/// or stale is refused, the current one is queued with the decision.
#[test]
fn review_decide_answers_a_publish_gate_only_with_the_plans_nonce() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join(".apb/runs/r1");
    let nonce = parked_on_publish(&run_dir);

    let status = apb_mcp::tools::run_status(dir.path(), "r1").unwrap();
    let pending = &status["pending_review"];
    assert_eq!(pending["node"], "publish");
    assert_eq!(pending["publish"]["nonce"], nonce);
    assert_eq!(pending["publish"]["commits"][0]["this_run"], true);
    assert_eq!(pending["publish"]["url"], "file:///srv/remote.git");
    assert_eq!(status["publish"][0]["awaiting_consent"], true);
    assert_eq!(status["publish"][0]["plan"]["nonce"], nonce);

    let missing = review_decide(dir.path(), "r1", "publish", "publish", "", None).unwrap_err();
    assert!(
        missing.to_string().contains(&nonce),
        "names the current plan: {missing}"
    );
    let stale = review_decide(
        dir.path(),
        "r1",
        "publish",
        "publish",
        "",
        Some("publish-00000000000000000000000000000000"),
    )
    .unwrap_err();
    assert!(
        matches!(stale, apb_mcp::tools::ToolError::Conflict(ref m) if m.contains(&nonce)),
        "{stale:?}"
    );
    assert!(!run_dir.join("reviews.jsonl").exists());

    let res = review_decide(dir.path(), "r1", "publish", "publish", "ok", Some(&nonce)).unwrap();
    assert_eq!(res["posted_seq"], serde_json::json!(0));
    let channel = fs::read_to_string(run_dir.join("reviews.jsonl")).unwrap();
    let line: serde_json::Value = serde_json::from_str(channel.lines().next().unwrap()).unwrap();
    assert_eq!(line["decision"], "publish");
    assert_eq!(line["nonce"], nonce);
}
