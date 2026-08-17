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
    let mut rejected_outputs = BTreeMap::new();
    rejected_outputs.insert("lint".to_string(), "interim only".to_string());
    let mut hooks = BTreeMap::new();
    hooks.insert("ci".to_string(), "/api/hooks/run-1/secret-xyz".to_string());
    let text = "T: {{params.task}} | I: {{run.instruction}} | O: {{nodes.lint.output}} | R: {{nodes.lint.report}} | RN: {{nodes.gate.review_note}} | RO: {{nodes.lint.rejected_output}} | H: {{run.hooks.ci}} | ctx: {{run.context}}";
    let out = render(
        text,
        &params,
        Some("be careful"),
        &outputs,
        &reviews,
        &rejected_outputs,
        &hooks,
        "CTXBODY",
    );
    assert_eq!(
        out,
        "T: ship it | I: be careful | O: 2 errors | R: 2 errors | RN: lgtm | RO: interim only | H: /api/hooks/run-1/secret-xyz | ctx: CTXBODY"
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

/// `{{nodes.<id>.output.<field>}}` projects ONE top-level field of a node
/// output that parses as a JSON object, with the exact `output_field` edge
/// condition semantics. `.report` is the same alias it is for the bare form.
#[test]
fn renders_a_top_level_field_selector_on_output_and_report() {
    let mut outputs = BTreeMap::new();
    outputs.insert(
        "verify".to_string(),
        r#"{"verdict":"failed","count":3,"ok":true}"#.to_string(),
    );
    let out = render(
        "V: {{nodes.verify.output.verdict}} | C: {{nodes.verify.output.count}} | \
         O: {{nodes.verify.report.ok}}",
        &BTreeMap::new(),
        None,
        &outputs,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        "",
    );
    assert_eq!(out, "V: failed | C: 3 | O: true");
}

/// Every shape the projection cannot read renders as the empty string, never an
/// error: a node with no output, an output that is not JSON, JSON that is not an
/// object, an absent field, and a value with no unambiguous string form.
#[test]
fn a_field_selector_renders_empty_for_every_unreadable_shape() {
    let mut outputs = BTreeMap::new();
    outputs.insert("prose".to_string(), "all checks failed".to_string());
    outputs.insert("arr".to_string(), r#"["failed"]"#.to_string());
    outputs.insert(
        "obj".to_string(),
        r#"{"verdict":null,"nested":{"a":1},"list":[1],"other":"x"}"#.to_string(),
    );
    for (text, why) in [
        ("[{{nodes.ghost.output.verdict}}]", "node never ran"),
        ("[{{nodes.prose.output.verdict}}]", "output is not JSON"),
        ("[{{nodes.arr.output.verdict}}]", "JSON is not an object"),
        ("[{{nodes.obj.output.missing}}]", "absent field"),
        ("[{{nodes.obj.output.verdict}}]", "null value"),
        ("[{{nodes.obj.output.nested}}]", "object value"),
        ("[{{nodes.obj.output.list}}]", "array value"),
    ] {
        let out = render(
            text,
            &BTreeMap::new(),
            None,
            &outputs,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            "",
        );
        assert_eq!(out, "[]", "{why} must render empty, got {out}");
    }
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
        &BTreeMap::new(),
        "",
    );
    assert_eq!(out, "[]");
}
