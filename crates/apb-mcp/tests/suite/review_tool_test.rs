use apb_mcp::tools::review_decide;
use std::fs;

#[test]
fn review_decide_writes_reviews_channel() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join(".apb/runs/r1");
    fs::create_dir_all(&run_dir).unwrap();

    let res = review_decide(dir.path(), "r1", "gate", "approved", "lgtm", None, None).unwrap();
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
    assert!(review_decide(dir.path(), "ghost", "gate", "approved", "", None, None).is_err());
}

#[test]
fn review_decide_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    assert!(review_decide(dir.path(), "../evil", "gate", "approved", "", None, None).is_err());
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

    let err = review_decide(dir.path(), "r1", "ghost", "approved", "", None, None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "a node that is not a gate must be not-found, got: {err:?}"
    );

    let err = review_decide(dir.path(), "r1", "gate", "approved", "", None, None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::Conflict(_)),
        "a gate with nothing pending must be a conflict, got: {err:?}"
    );

    // With the request journaled, the same call is accepted.
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::review_requested(
        "gate",
        vec!["approved".into(), "rejected".into()],
        String::new(),
    ))
    .unwrap();
    let res = review_decide(dir.path(), "r1", "gate", "approved", "", None, None).unwrap();
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
            binding: None,
            person_only: false,
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

    let missing =
        review_decide(dir.path(), "r1", "publish", "publish", "", None, None).unwrap_err();
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
        None,
    )
    .unwrap_err();
    assert!(
        matches!(stale, apb_mcp::tools::ToolError::Conflict(ref m) if m.contains(&nonce)),
        "{stale:?}"
    );
    assert!(!run_dir.join("reviews.jsonl").exists());

    let res = review_decide(
        dir.path(),
        "r1",
        "publish",
        "publish",
        "ok",
        Some(&nonce),
        None,
    )
    .unwrap();
    assert_eq!(res["posted_seq"], serde_json::json!(0));
    let channel = fs::read_to_string(run_dir.join("reviews.jsonl")).unwrap();
    let line: serde_json::Value = serde_json::from_str(channel.lines().next().unwrap()).unwrap();
    assert_eq!(line["decision"], "publish");
    assert_eq!(line["nonce"], nonce);
}

const PERSON_ONLY: &str = "schema: 2\nid: po\nname: po\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, person_only: true }\n  - { id: ok, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: ok }\n";

const ORDINARY: &str = "schema: 2\nid: or\nname: or\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review }\n  - { id: ok, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: ok }\n";

/// Run `id` of `playbook` with its gate's request open.
fn gated_run(root: &std::path::Path, id: &str, playbook: &str, person_only: bool) {
    use apb_engine::event::{EventLog, EventPayload};
    let dir = root.join(".apb/runs").join(id);
    let mut log = EventLog::create(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "p".into(),
        version: "1.0.0".into(),
        definition: None,
        external_data: false,
    })
    .unwrap();
    let mut req = EventPayload::review_requested(
        "gate",
        vec!["approve".into(), "reject".into()],
        String::new(),
    );
    if let EventPayload::ReviewRequested { person_only: p, .. } = &mut req {
        *p = person_only;
    }
    log.append(req).unwrap();
}

/// Sets `APB_RUN_ID` and clears `APB_HOST_TASK` for the guard's life
/// (under the env lock); both are put back on drop.
struct StepEnv([(&'static str, Option<std::ffi::OsString>); 2]);

impl StepEnv {
    fn set(run: Option<&str>) -> StepEnv {
        let prev = ["APB_RUN_ID", "APB_HOST_TASK"].map(|k| (k, std::env::var_os(k)));
        // SAFETY: env mutation is serialized by the env lock the test holds.
        unsafe {
            match run {
                Some(r) => std::env::set_var("APB_RUN_ID", r),
                None => std::env::remove_var("APB_RUN_ID"),
            }
            std::env::remove_var("APB_HOST_TASK");
        }
        StepEnv(prev)
    }
}

impl Drop for StepEnv {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            // SAFETY: as in `set`.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// MCP never decides a person-only gate, with or without a nonce: the tool
/// error names the refusal, the terminal command and the run page, and
/// nothing is queued.
#[test]
fn review_decide_never_decides_a_person_only_gate() {
    let _env = crate::common::env_lock();
    let _step = StepEnv::set(None);
    let dir = tempfile::tempdir().unwrap();
    gated_run(dir.path(), "r1", PERSON_ONLY, true);
    for nonce in [None, Some("approval-0123")] {
        let err = review_decide(dir.path(), "r1", "gate", "approve", "", nonce, None).unwrap_err();
        let text = err.to_string();
        assert!(text.starts_with("refused (agent_channel): "), "{text}");
        assert!(text.contains("apb review r1 gate --decision"), "{text}");
        assert!(text.contains("dashboard"), "{text}");
    }
    assert!(!dir.path().join(".apb/runs/r1/reviews.jsonl").exists());
}

/// From inside a run step the MCP channel is the step's: a person-only
/// gate of another run refuses it as `run_step`, any gate of the step's own
/// run as `run_step_own_run`, and another run's ordinary gate takes it.
#[test]
fn a_run_step_decides_neither_a_person_only_gate_nor_its_own_run() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    gated_run(dir.path(), "r1", PERSON_ONLY, true);
    gated_run(dir.path(), "r2", ORDINARY, false);
    let _step = StepEnv::set(Some("other"));
    let err = review_decide(dir.path(), "r1", "gate", "approve", "", None, None).unwrap_err();
    assert!(err.to_string().starts_with("refused (run_step): "), "{err}");
    review_decide(dir.path(), "r2", "gate", "approve", "", None, None).unwrap();
    drop(_step);
    gated_run(dir.path(), "r3", ORDINARY, false);
    let _own = StepEnv::set(Some("r3"));
    let err = review_decide(dir.path(), "r3", "gate", "approve", "", None, None).unwrap_err();
    assert!(
        err.to_string().starts_with("refused (run_step_own_run): "),
        "{err}"
    );
}

/// A step never resolves a send of its own run over MCP either.
#[test]
fn a_run_step_never_resolves_a_send_of_its_own_run() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let run = dir.path().join(".apb/runs/r1");
    fs::create_dir_all(&run).unwrap();
    let lines = [
        r#"{"seq":0,"ts":1,"type":"send_intent_recorded","intent":"si-a","nonce":"n","node":"post","connector":"telegram","function":"send_message","account":"main","mode":"none","label":"hello","pid":0,"deadline_ms":0}"#,
        r#"{"seq":1,"ts":2,"type":"send_intent_settled","intent":"si-a","outcome":"unknown","source":"response","detail":"timeout"}"#,
    ];
    fs::write(run.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    let _own = StepEnv::set(Some("r1"));
    let err = apb_mcp::tools::run_intent_resolve(
        dir.path(),
        "r1",
        "si-a",
        true,
        None,
        apb_mcp::tools::IntentCaller::person("mcp"),
    )
    .unwrap_err();
    assert!(
        err.to_string().starts_with("refused (run_step_own_run): "),
        "{err}"
    );
    let events = fs::read_to_string(run.join("events.jsonl")).unwrap();
    assert_eq!(events.lines().count(), 2, "nothing was settled");
}
