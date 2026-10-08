//! MCP `run_retro_context` (issue #192 part 3).

use std::fs;

use apb_mcp::tools::{ToolError, run_retro_context};
use serde_json::json;

/// A finished host-mode run whose one host task the host submitted on
/// `opus-4` after 4 s.
fn seed(root: &std::path::Path) {
    let dir = root.join(".apb/runs/r1");
    fs::create_dir_all(&dir).unwrap();
    let lines = [
        json!({"seq": 1, "ts": 0, "type": "run_started", "playbook": "p", "version": "1.0.0"}),
        json!({"seq": 2, "ts": 0, "type": "node_started", "node": "work", "attempt": 1}),
        json!({"seq": 3, "ts": 0, "type": "attempt_started", "node": "work", "attempt": 1, "agent": "claude", "model": "sonnet"}),
        json!({"seq": 4, "ts": 1000, "type": "host_task_requested", "task_id": "work-1", "node": "work", "attempt": 1}),
        json!({"seq": 5, "ts": 5000, "type": "host_task_submitted", "task_id": "work-1", "status": "succeeded", "submitted_by": "host", "model": "opus-4"}),
        json!({"seq": 6, "ts": 5000, "type": "attempt_finished", "node": "work", "attempt": 1, "status": "succeeded", "duration_ms": 5000}),
        json!({"seq": 7, "ts": 5000, "type": "node_finished", "node": "work", "status": "succeeded", "attempt": 1, "output": ""}),
        json!({"seq": 8, "ts": 6000, "type": "run_finished", "outcome": "succeeded"}),
    ];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("events.jsonl"), body).unwrap();
}

#[test]
fn run_retro_context_returns_the_actual_model_and_host_wait() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let out = run_retro_context(root.path(), "r1", None).unwrap();
    assert_eq!(out["outcome"], "succeeded");
    let work = &out["nodes"][0];
    assert_eq!(work["node"], "work");
    assert_eq!(work["models"], json!(["opus-4"]));
    assert_eq!(work["host_wait_ms"], 4000);
    assert_eq!(work["attempts"][0]["model_source"], "host");
    assert_eq!(work["attempts"][0]["declared_model"], "sonnet");
    // No earlier run of the version on this machine.
    assert!(out.get("baseline").is_none());
}

#[test]
fn run_retro_context_refuses_an_unknown_or_unsafe_run() {
    let root = tempfile::tempdir().unwrap();
    for id in ["nope", "../../etc"] {
        let err = run_retro_context(root.path(), id, Some(3)).unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)), "{id}: {err:?}");
    }
}
