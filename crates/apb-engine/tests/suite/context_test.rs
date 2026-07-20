use apb_engine::context::{build_context, build_context_for_render, render};
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

// Task 4 completion-plan defect 3, Important fix-review item: `{{run.context}}`
// in an actual node prompt resolves through `build_context_for_render`, NOT
// through the context.md file `rebuild_context_md` writes - a fix that only
// touched context.md would leave every rendered node prompt still missing the
// run instruction. This exercises that exact function directly with a
// non-empty instruction (the uncompacted path - no ContextCompacted event in
// `events`, so `run_dir` is never actually read).
#[test]
fn build_context_for_render_leads_with_run_instruction_when_present() {
    let events = vec![ev(
        0,
        EventPayload::NodeFinished {
            node: "lint".into(),
            status: "succeeded".into(),
            attempt: 1,
            output: "ok".into(),
            artifacts: Vec::new(),
        },
    )];
    let run_dir = tempfile::tempdir().unwrap();
    let rendered =
        build_context_for_render(run_dir.path(), &events, Some("stay within budget")).unwrap();
    assert!(
        rendered.starts_with("## run instruction\n\nstay within budget\n\n"),
        "expected the rendered context to lead with the run instruction, got:\n{rendered}"
    );
    assert!(
        rendered.contains("## lint ("),
        "the node section must still follow the instruction, got:\n{rendered}"
    );
}

#[test]
fn build_context_for_render_has_no_instruction_section_when_absent() {
    let events: Vec<Event> = Vec::new();
    let run_dir = tempfile::tempdir().unwrap();
    let rendered = build_context_for_render(run_dir.path(), &events, None).unwrap();
    assert!(
        !rendered.contains("## run instruction"),
        "expected no instruction section when absent, got:\n{rendered}"
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
