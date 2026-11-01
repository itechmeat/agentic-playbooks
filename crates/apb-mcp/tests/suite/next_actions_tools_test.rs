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
    assert!(out["next_actions"][0]["caller_refusal"].is_string());
    let host = tools::run_status(root, "r").unwrap();
    assert_eq!(host["next_actions"][0]["available_to_caller"], true);
    assert_eq!(host["next_actions"][0]["relay_to_person"], true);
}
