use apb_engine::context::{build_context, render};
use apb_engine::event::{Event, EventPayload};
use apb_engine::state::ReviewDecision;
use std::collections::BTreeMap;

fn ev(seq: u64, p: EventPayload) -> Event {
    Event {
        seq,
        ts: 0,
        payload: p,
    }
}

#[test]
fn builds_context_sections_in_seq_order() {
    let events = vec![
        ev(
            0,
            EventPayload::NodeFinished {
                node: "lint".into(),
                status: "failed".into(),
                attempt: 1,
                output: "2 errors".into(),
                artifacts: Vec::new(),
            },
        ),
        ev(
            1,
            EventPayload::NodeFinished {
                node: "fix".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: "patched".into(),
                artifacts: Vec::new(),
            },
        ),
    ];
    let ctx = build_context(&events);
    let lint_at = ctx.find("lint").unwrap();
    let fix_at = ctx.find("fix").unwrap();
    assert!(lint_at < fix_at, "sections must follow seq order");
    assert!(ctx.contains("2 errors"));
    assert!(ctx.contains("failed"));
}

#[test]
fn renders_all_template_refs() {
    let mut params = BTreeMap::new();
    params.insert("task".to_string(), "ship it".to_string());
    let mut outputs = BTreeMap::new();
    outputs.insert("lint".to_string(), "2 errors".to_string());
    let mut reviews = BTreeMap::new();
    reviews.insert(
        "gate".to_string(),
        ReviewDecision {
            decision: "approved".into(),
            note: "lgtm".into(),
        },
    );
    let mut hooks = BTreeMap::new();
    hooks.insert("ci".to_string(), "/api/hooks/run-1/secret-xyz".to_string());
    let text = "T: {{params.task}} | I: {{run.instruction}} | O: {{nodes.lint.output}} | R: {{nodes.lint.report}} | RN: {{nodes.gate.review_note}} | H: {{run.hooks.ci}} | ctx: {{run.context}}";
    let out = render(
        text,
        &params,
        Some("be careful"),
        &outputs,
        &reviews,
        &hooks,
        "CTXBODY",
    );
    assert_eq!(
        out,
        "T: ship it | I: be careful | O: 2 errors | R: 2 errors | RN: lgtm | H: /api/hooks/run-1/secret-xyz | ctx: CTXBODY"
    );
}

#[test]
fn unknown_refs_become_empty() {
    let out = render(
        "[{{params.ghost}}]",
        &BTreeMap::new(),
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        "",
    );
    assert_eq!(out, "[]");
}
