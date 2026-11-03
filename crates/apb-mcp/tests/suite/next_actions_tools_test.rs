//! Next actions on the MCP run tools: `run_wait`, `run_status` and
//! `run_report` carry the same actions, `next` is the primary action's
//! reason on every row, and each action names the tool and the request it
//! is taken with.

use std::fs;
use std::path::Path;
use std::time::Duration;

use apb_engine::event::EventLog;
use apb_mcp::tools;
use apb_mcp::tools::actions::ActionCaller;
use serde_json::{Value, json};

const GATED: &str = "schema: 2\nid: g\nname: g\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true }\n  - { id: gate, type: human_review, options: [approved, rejected] }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: gate }\n  - { from: gate, to: f }\n";

/// A run of the gated playbook with the given journal lines after its start.
fn run(root: &Path, id: &str, lines: &[Value]) -> u64 {
    let dir = root.join(".apb/runs").join(id);
    let mut log = EventLog::create(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), GATED).unwrap();
    let mut last = 0;
    for v in std::iter::once(json!({"type": "run_started", "playbook": "g", "version": "1.0.0"}))
        .chain(lines.iter().cloned())
    {
        last = log.append(serde_json::from_value(v).unwrap()).unwrap().seq;
    }
    last
}

fn review() -> Value {
    json!({"type": "review_requested", "node": "gate", "options": ["approved", "rejected"]})
}

fn paused(until: Option<u64>) -> Value {
    json!({"type": "run_paused", "reason": "operator", "by": "operator", "until": until})
}

fn waited(root: &Path, id: &str) -> Value {
    let res = apb_engine::run_wait::wait_run(root, id, Duration::from_secs(5)).unwrap();
    tools::run_wait_result(root, id, &res).unwrap()
}

fn codes(out: &Value) -> Vec<String> {
    out["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["code"].as_str().unwrap().to_string())
        .collect()
}

/// For every row: `next` is the primary's reason, and the three run tools
/// list the same actions.
#[test]
fn next_is_the_primary_reason_on_every_row_and_the_tools_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    apb_core::registry::init_project(root).unwrap();
    let in_hour = apb_core::clock::now_ms_u64() + 3_600_000;
    let rows: Vec<(&str, Vec<Value>, &str)> = vec![
        ("review", vec![review()], "decide_review"),
        ("paused", vec![paused(None)], "resume_run"),
        ("paused-until", vec![paused(Some(in_hour))], "resume_run"),
        (
            "paused-review",
            vec![review(), paused(Some(in_hour))],
            "decide_review",
        ),
        (
            "finished",
            vec![json!({"type": "run_finished", "outcome": "succeeded"})],
            "read_report",
        ),
    ];
    for (id, lines, primary) in rows {
        run(root, id, &lines);
        let wait = waited(root, id);
        let i = wait["primary"].as_u64().expect("a primary") as usize;
        assert_eq!(wait["next_actions"][i]["code"], primary, "{id}: {wait}");
        assert_eq!(wait["next"], wait["next_actions"][i]["reason"], "{id}");
        let status = tools::run_status(root, id).unwrap();
        let report = tools::run_report(root, id).unwrap();
        assert_eq!(codes(&status), codes(&wait), "{id}");
        assert_eq!(codes(&report), codes(&wait), "{id}");
        assert_eq!(status["stop"], wait["stop"], "{id}");
    }
}

/// A review action is taken with `review_decide`, carries its request, and
/// passing it back after the decision is a conflict.
#[test]
fn a_review_action_carries_its_request_and_a_second_decision_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    apb_core::registry::init_project(root).unwrap();
    let seq = run(root, "r", &[review()]);
    let status = tools::run_status(root, "r").unwrap();
    let a = &status["next_actions"][0];
    assert_eq!(a["tool"], "review_decide");
    assert_eq!(a["arguments"]["request"], seq);
    assert_eq!(a["arguments"]["node"], "gate");
    assert_eq!(a["fill"][0]["options"], json!(["approved", "rejected"]));
    tools::review_decide(root, "r", "gate", "approved", "", None, Some(seq)).unwrap();
    let again = tools::review_decide(root, "r", "gate", "rejected", "", None, Some(seq));
    assert!(
        matches!(again, Err(tools::ToolError::Conflict(_))),
        "{again:?}"
    );
}

/// A supervisor session sees the person's review as not its own to take.
#[test]
fn a_supervisor_caller_sees_a_human_review_as_not_available() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    apb_core::registry::init_project(root).unwrap();
    run(root, "r", &[review()]);
    let out = tools::run_status_as(root, "r", ActionCaller::Supervisor).unwrap();
    assert_eq!(out["next_actions"][0]["available_to_caller"], false);
    assert_eq!(out["next_actions"][0]["caller_refusal"], "relay_to_person");
    let host = tools::run_status(root, "r").unwrap();
    assert_eq!(host["next_actions"][0]["available_to_caller"], true);
    assert_eq!(host["next_actions"][0]["relay_to_person"], true);
}

/// Every `run_status` action carries what an unattended run would do at
/// its wait, on an attended run too: a plain review fails the run
/// (`hitl_required`), a review that declares `unattended: { decide }`
/// decides that option unless the drive's recheck would refuse it (here a
/// sub-playbook in reach that the run pinned no version of, so its effects
/// are unknown and the run would fail `hitl_required`), and an operator
/// pause is not a wait.
#[test]
fn every_action_says_what_an_unattended_run_does_at_its_wait() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    apb_core::registry::init_project(root).unwrap();
    run(root, "plain", &[review()]);
    let status = tools::run_status(root, "plain").unwrap();
    assert_eq!(status["next_actions"][0]["code"], "decide_review");
    assert_eq!(
        status["next_actions"][0]["unattended"],
        json!({ "kind": "fails_run", "code": "hitl_required" }),
        "{status}"
    );

    run(root, "decided", &[review()]);
    let dir = root.join(".apb/runs/decided");
    fs::write(
        dir.join("playbook.yaml"),
        GATED.replace(
            "options: [approved, rejected] }",
            "options: [approved, rejected], unattended: { decide: approved } }",
        ),
    )
    .unwrap();
    let status = tools::run_status(root, "decided").unwrap();
    assert_eq!(
        status["next_actions"][0]["unattended"],
        json!({ "kind": "decides", "option": "approved" }),
        "{status}"
    );

    run(root, "refused", &[review()]);
    fs::write(
        root.join(".apb/runs/refused/playbook.yaml"),
        GATED
            .replace(
                "options: [approved, rejected] }",
                "options: [approved, rejected], unattended: { decide: approved } }\n  - { id: sub, type: playbook, playbook: other }",
            )
            .replace("{ from: gate, to: f }", "{ from: gate, to: sub }\n  - { from: sub, to: f }"),
    )
    .unwrap();
    let status = tools::run_status(root, "refused").unwrap();
    assert_eq!(
        status["next_actions"][0]["unattended"],
        json!({ "kind": "fails_run", "code": "hitl_required" }),
        "{status}"
    );

    run(root, "paused", &[paused(None)]);
    let status = tools::run_status(root, "paused").unwrap();
    assert_eq!(status["next_actions"][0]["code"], "resume_run");
    assert!(
        status["next_actions"][0].get("unattended").is_none(),
        "{status}"
    );
}

const BOUND: &str = "schema: 2\nid: b\nname: b\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: w, type: script, script: \"x.sh\", runner: sh }\n  - { id: gate, type: human_review, person_only: true, approves: { results: [w] } }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: w }\n  - { from: w, to: gate }\n  - { from: gate, to: f }\n";

/// Run `id` of [`BOUND`] with its person-only gate bound to `w`'s
/// snapshot.
fn bound_run(root: &Path, id: &str) {
    let dir = root.join(".apb/runs").join(id);
    let mut log = EventLog::create(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), BOUND).unwrap();
    for v in [
        json!({"type": "run_started", "playbook": "b", "version": "1.0.0"}),
        json!({"type": "review_requested", "node": "gate", "options": ["approve", "reject"],
            "person_only": true,
            "binding": {"nonce": "approval-0123456789abcdef0123456789abcdef",
                "snapshots": [{"node": "w", "execution": 1, "settlement_seq": 3,
                    "snapshot_id": "5a5a5a5a5a5a5a5a5a5a"}]}}),
    ] {
        log.append(serde_json::from_value(v).unwrap()).unwrap();
    }
}

/// A person-only, bound gate: its pending block carries the binding and
/// `person_only`; the action names only a person, carries the binding
/// nonce, and a host session gets it as not its own to take
/// (`agent_channel`), never as a call to make.
#[test]
fn a_person_only_bound_gate_is_shown_but_not_offered_to_a_host() {
    let _env = crate::common::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    apb_core::registry::init_project(root).unwrap();
    bound_run(root, "r");
    let status = tools::run_status(root, "r").unwrap();
    let pending = &status["pending_review"];
    assert_eq!(pending["person_only"], true, "{pending}");
    assert_eq!(
        pending["binding"]["nonce"],
        "approval-0123456789abcdef0123456789abcdef"
    );
    assert!(
        !pending["how_to_decide"]
            .as_str()
            .unwrap()
            .contains("review_decide"),
        "{pending}"
    );
    let a = &status["next_actions"][0];
    assert_eq!(a["code"], "decide_review");
    assert_eq!(a["principals"], json!(["person"]));
    assert_eq!(
        a["arguments"]["nonce"],
        "approval-0123456789abcdef0123456789abcdef"
    );
    assert_eq!(a["available_to_caller"], false, "{a}");
    assert_eq!(a["caller_refusal"], "agent_channel", "{a}");
    assert!(
        a["reason"].as_str().unwrap().contains("5a5a5a5a5a5a"),
        "{a}"
    );
    let wait = waited(root, "r");
    assert!(
        !wait["next"].as_str().unwrap().contains("review_decide"),
        "{wait}"
    );
}
