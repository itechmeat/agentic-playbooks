//! The decisions report over a completion check under rule `reply/1`
//! (eval-doubles D5): a finished reply followed by a failing node is not a
//! label of the reply.

use apb_engine::decision::report::{ReportFilter, ReportSettings, RunJournal, build};
use serde_json::json;

/// A run `a -> b`: the completion check judged `a`'s reply finished
/// (`final_result` high), `a` succeeded, and `b` failed.
fn write_run(runs: &std::path::Path) -> std::path::PathBuf {
    let run = runs.join("r1");
    std::fs::create_dir_all(&run).unwrap();
    let lines = [
        json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        json!({"type": "node_started", "node": "a", "attempt": 1}),
        json!({"type": "attempt_started", "node": "a", "attempt": 1, "agent": "claude", "model": "m"}),
        json!({"type": "decision_made", "use_site": "completion_check", "node": "a", "attempt": 1,
            "provider": "main", "model": "jev-1.13.0", "calibrated": true, "mode": "shadow",
            "questions_digest": "sha256:q", "state_digest": "sha256:s", "state_bytes": 1,
            "answers": {"final_result": {"p": 0.97}},
            "applied": false, "latency_ms": 1, "cached": false, "error": null}),
        json!({"type": "attempt_finished", "node": "a", "attempt": 1, "status": "succeeded"}),
        json!({"type": "node_finished", "node": "a", "status": "succeeded", "attempt": 1, "output": "the report"}),
        json!({"type": "node_started", "node": "b", "attempt": 1}),
        json!({"type": "node_finished", "node": "b", "status": "failed", "attempt": 1, "output": ""}),
        json!({"type": "run_finished", "outcome": "failed"}),
    ];
    let text: String = lines
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let mut v = v.clone();
            v["seq"] = json!(i + 1);
            v["ts"] = json!(1_000 + i);
            format!("{v}\n")
        })
        .collect();
    std::fs::write(run.join("events.jsonl"), text).unwrap();
    run
}

#[test]
fn a_report_on_a_finished_reply_and_a_failing_next_node() {
    let dir = tempfile::tempdir().unwrap();
    let run = write_run(dir.path());
    let journal = RunJournal::load(&run).expect("the journal reads");
    let report = build(
        &[journal],
        &ReportFilter::default(),
        &ReportSettings::default(),
    );
    assert_eq!(report.groups.len(), 1, "{:#?}", report.groups);
    let g = &report.groups[0];
    assert_eq!(g.use_site, "completion_check");
    assert_eq!((g.decisions, g.answered, g.labelled), (1, 1, 0));
    assert_eq!((g.act_labels, g.keep_labels), (0, 0));
    assert_eq!(
        g.unlabelled
            .get("no person approved the reply at the gate that follows it"),
        Some(&1)
    );
    assert_eq!(
        g.rule.as_deref(),
        Some(apb_core::decision_thresholds::COMPLETION_RULE)
    );
    assert_eq!(g.suggested_threshold, None);
    assert!(!g.eligible);
}
