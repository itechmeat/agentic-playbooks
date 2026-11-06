use apb_core::schema::ContextBudget;
use apb_engine::context::{OutputClip, build_context, build_context_for_render, render};
use apb_engine::event::{Event, EventPayload};
use apb_engine::state::ReviewDecision;
use std::collections::BTreeMap;

/// No output reference is clipped: these tests pin what `render` resolves.
fn unlimited_outputs() -> OutputClip<'static> {
    OutputClip {
        run_dir: std::path::Path::new("/run"),
        max_bytes: 0,
        retro: None,
    }
}

fn finished(seq: u64, node: &str, output: &str) -> Event {
    ev(
        seq,
        EventPayload::node_finished(node, "succeeded", 1, output),
    )
}

/// Issue #136 item 1: a loop re-runs a node, and only its latest output is
/// worth prompt space; an earlier run shrinks to a pointer to `context.md`.
/// When the sections still exceed `max_bytes`, the oldest outputs give way to
/// pointers at their files, and the newest one is always kept.
#[test]
fn render_context_drops_superseded_runs_and_oldest_outputs_over_budget() {
    let run_dir = tempfile::tempdir().unwrap();
    let events = vec![
        finished(0, "draft", "FIRST-DRAFT"),
        finished(1, "review", &"r".repeat(900)),
        finished(2, "draft", "SECOND-DRAFT"),
        finished(3, "check", &"c".repeat(900)),
    ];
    let budget = ContextBudget {
        max_bytes: 1500,
        ..ContextBudget::UNLIMITED
    };
    let rendered = build_context_for_render(run_dir.path(), &events, None, &budget).unwrap();

    assert!(
        !rendered.contains("FIRST-DRAFT"),
        "superseded run kept:\n{rendered}"
    );
    assert!(rendered.contains("superseded by a later run of `draft`"));
    assert!(rendered.contains("SECOND-DRAFT"));
    assert!(
        !rendered.contains(&"r".repeat(900)),
        "the oldest output must give way under the budget"
    );
    assert!(
        rendered.contains(
            &run_dir
                .path()
                .join("node-outputs/review.md")
                .display()
                .to_string()
        )
    );
    assert!(
        rendered.contains(&"c".repeat(900)),
        "the newest output always stays"
    );
}

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
            EventPayload::node_finished("lint", "failed", 1, "2 errors"),
        ),
        ev(
            1,
            EventPayload::node_finished("fix", "succeeded", 1, "patched"),
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
    let text = "T: {{params.task}} | I: {{run.instruction}} | O: {{nodes.lint.output}} | R: {{nodes.lint.report}} | RN: {{nodes.gate.review_note}} | RD: {{nodes.gate.review_decision}} | RO: {{nodes.lint.rejected_output}} | H: {{run.hooks.ci}} | ctx: {{run.context}}";
    let out = render(
        text,
        &params,
        Some("be careful"),
        &outputs,
        &reviews,
        &rejected_outputs,
        &hooks,
        "CTXBODY",
        &BTreeMap::new(),
        &unlimited_outputs(),
    );
    assert_eq!(
        out,
        "T: ship it | I: be careful | O: 2 errors | R: 2 errors | RN: lgtm | RD: approved | RO: interim only | H: /api/hooks/run-1/secret-xyz | ctx: CTXBODY"
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
        EventPayload::node_finished("lint", "succeeded", 1, "ok"),
    )];
    let run_dir = tempfile::tempdir().unwrap();
    let rendered = build_context_for_render(
        run_dir.path(),
        &events,
        Some("stay within budget"),
        &ContextBudget::UNLIMITED,
    )
    .unwrap();
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
    let rendered =
        build_context_for_render(run_dir.path(), &events, None, &ContextBudget::UNLIMITED).unwrap();
    assert!(
        !rendered.contains("## run instruction"),
        "expected no instruction section when absent, got:\n{rendered}"
    );
}

/// `{{nodes.<id>.output.<field>}}` projects ONE top-level field of a node
/// output that parses as a JSON object. A string, bool or number renders as
/// `output_field_value` reads it; an array or an object renders as compact
/// JSON, so a reviewer's findings array reaches a later node. `.report` is
/// the same alias it is for the bare form.
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
        &BTreeMap::new(),
        &unlimited_outputs(),
    );
    assert_eq!(out, "V: failed | C: 3 | O: true");
}

/// An array or an object field renders as compact JSON, so an agent that
/// writes a real findings array hands the next node every finding.
#[test]
fn an_array_or_object_field_renders_as_compact_json() {
    let mut outputs = BTreeMap::new();
    outputs.insert(
        "collect".to_string(),
        r#"{"findings":[{"id":"a"}]}"#.to_string(),
    );
    let out = render(
        "F: {{nodes.collect.output.findings}}",
        &BTreeMap::new(),
        None,
        &outputs,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        "",
        &BTreeMap::new(),
        &unlimited_outputs(),
    );
    assert_eq!(out, r#"F: [{"id":"a"}]"#);
}

/// Every shape the projection cannot read renders as the empty string, never an
/// error: a node with no output, an output that is not JSON, JSON that is not an
/// object, an absent field, and a null value. An array or an object value
/// renders as compact JSON, so it is no longer among the unreadable shapes.
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
            &BTreeMap::new(),
            &unlimited_outputs(),
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
        &BTreeMap::new(),
        &unlimited_outputs(),
    );
    assert_eq!(out, "[]");
}

// The seen reference of a `no_new_items` loop (spec loops-and-graphs D2)
// renders the rendered seen text the run's JudgeInputs carry, keyed
// `<node>.<field>`; absent from the map, or on a first visit, it renders
// empty.
#[test]
fn a_seen_reference_renders_from_the_map_and_empty_when_absent() {
    let mut seen = BTreeMap::new();
    seen.insert("hunt.findings".to_string(), "a\nb".to_string());
    let out = render(
        "S: [{{nodes.hunt.seen.findings}}] E: [{{nodes.ghost.seen.findings}}]",
        &BTreeMap::new(),
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        "",
        &seen,
        &unlimited_outputs(),
    );
    assert_eq!(out, "S: [a\nb] E: []");
}
