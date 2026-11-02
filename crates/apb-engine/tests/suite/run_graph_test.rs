//! The run graph read model over synthetic journals: what is taken, not
//! taken and not established, how records become deliveries, which
//! diagnostics are attributed, what each "why it did not run" says, the
//! judge comparisons, the overlay alignment, the statistics membership and
//! the redaction of what a surface shows.

use std::collections::BTreeMap;

use apb_core::schema::Playbook;
use apb_engine::diagnostics::Diagnostic;
use apb_engine::diagnostics::route::{RouteCheck, RouteEvaluated};
use apb_engine::event::Event;
use apb_engine::run_definitions::{
    Boundary, BoundaryKind, DefinitionsInForce, Resolved, Segment, UnknownReason, digest,
};
use apb_engine::run_outcome::route_explain::{ExplainClass, NotRunCode};
use apb_engine::run_outcome::{EdgeState, GraphInput, RunGraph};
use serde_json::{Value, json};

fn events(body: &[Value]) -> Vec<Event> {
    body.iter()
        .enumerate()
        .map(|(i, v)| {
            let mut v = v.clone();
            v["seq"] = json!(i as u64 + 1);
            v["ts"] = json!(1_000 + i as u64);
            serde_json::from_value(v).unwrap()
        })
        .collect()
}

fn started_run() -> Value {
    json!({"type": "run_started", "playbook": "p", "version": "1.0.0"})
}
fn ns(node: &str) -> Value {
    json!({"type": "node_started", "node": node, "attempt": 1})
}
fn nf(node: &str, status: &str) -> Value {
    json!({"type": "node_finished", "node": node, "status": status, "attempt": 1, "output": ""})
}
fn nf_out(node: &str, output: &str) -> Value {
    json!({"type": "node_finished", "node": node, "status": "succeeded", "attempt": 1, "output": output})
}
fn hop(from: &str, to: &str, edges: &[&str]) -> Value {
    json!({"type": "edge_traversed", "from": from, "to": to, "uncounted": true, "edges": edges})
}
fn counted(from: &str, to: &str, edges: &[&str]) -> Value {
    json!({"type": "edge_traversed", "from": from, "to": to, "edges": edges})
}
fn policy(from: &str, to: &str) -> Value {
    json!({"type": "edge_traversed", "from": from, "to": to, "via_policy": true})
}
fn finished_run(outcome: &str) -> Value {
    json!({"type": "run_finished", "outcome": outcome})
}

fn known(yaml: &str) -> Resolved {
    Resolved::Known {
        playbook: Box::new(Playbook::from_yaml(yaml).unwrap()),
        digest: digest(yaml.as_bytes()),
    }
}

/// One segment from the start, executing `yaml`.
fn one(yaml: &str) -> DefinitionsInForce {
    DefinitionsInForce {
        segments: vec![Segment {
            from_seq: 0,
            to_seq: None,
            boundary: Some(Boundary {
                seq: 1,
                definition: Some(digest(yaml.as_bytes())),
                kind: BoundaryKind::Start,
            }),
            resolved: known(yaml),
            mismatch_recorded: false,
        }],
        playbook_changed: false,
    }
}

fn graph(ev: &[Event], defs: &DefinitionsInForce, diags: &[Diagnostic]) -> RunGraph {
    RunGraph::build(&GraphInput {
        events: ev,
        definitions: defs,
        diagnostics: diags,
        statuses: &BTreeMap::new(),
    })
}

fn state<'a>(g: &'a RunGraph, rendered: &str) -> &'a EdgeState {
    &g.edges
        .iter()
        .find(|e| e.rendered == rendered)
        .unwrap_or_else(|| panic!("no edge {rendered}: {:?}", g.edges))
        .state
}

const DUP: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: condition }
  - { id: discard, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: discard, condition: { type: node_status, node: gate, equals: failure } }
  - { from: gate, to: discard, fallback: true }
"#;

#[test]
fn only_the_named_duplicate_is_taken_and_the_other_is_not() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "gate", &["e:start>gate#1"]),
        ns("gate"),
        nf("gate", "succeeded"),
        hop("gate", "discard", &["e:gate>discard#2"]),
        nf("discard", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&ev, &one(DUP), &[]);
    assert_eq!(
        state(&g, "e:gate>discard#2"),
        &EdgeState::Taken { count: 1 }
    );
    // One-of routing takes one edge: its sibling is proven not taken.
    assert_eq!(state(&g, "e:gate>discard#1"), &EdgeState::NotTaken);
}

#[test]
fn a_duplicate_pair_without_addresses_is_not_established() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "gate", &[]),
        ns("gate"),
        nf("gate", "succeeded"),
        hop("gate", "discard", &[]),
        nf("discard", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&ev, &one(DUP), &[]);
    // A unique pair resolves; a duplicate pair cannot.
    assert_eq!(state(&g, "e:start>gate#1"), &EdgeState::Taken { count: 1 });
    for e in ["e:gate>discard#1", "e:gate>discard#2"] {
        assert!(
            matches!(state(&g, e), EdgeState::NotEstablished { reason } if reason == "duplicate_pair_before_edges"),
            "{e}: {:?}",
            state(&g, e)
        );
    }
    let row = g.traversals.iter().find(|t| t.to == "discard").unwrap();
    assert!(!row.resolved);
}

const RETRY: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: condition }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: work, max_traversals: 3, condition: { type: node_status, node: work, equals: failure } }
  - { from: work, to: done, fallback: true }
"#;

#[test]
fn a_bounded_retry_counts_each_execution_once() {
    let mut body = vec![
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "work", &[]),
    ];
    for _ in 0..3 {
        body.extend([
            ns("work"),
            nf("work", "failed"),
            hop("work", "work", &["e:work>work#1"]),
            counted("work", "work", &["e:work>work#1"]),
        ]);
    }
    // A resume that advanced the third execution again wrote only another
    // counted record: still one delivery.
    body.push(json!({"type": "run_resumed", "from_node": "work", "mode": "after"}));
    body.push(counted("work", "work", &["e:work>work#1"]));
    body.extend([
        ns("work"),
        nf("work", "succeeded"),
        hop("work", "done", &["e:work>done#1"]),
        nf("done", "succeeded"),
        finished_run("succeeded"),
    ]);
    let ev = events(&body);
    // The resume opened a segment over the same bytes.
    let mut defs = one(RETRY);
    let resume_seq = ev
        .iter()
        .find(|e| {
            matches!(
                e.payload,
                apb_engine::event::EventPayload::RunResumed { .. }
            )
        })
        .unwrap()
        .seq;
    defs.segments[0].to_seq = Some(resume_seq - 1);
    defs.segments.push(Segment {
        from_seq: resume_seq,
        to_seq: None,
        boundary: Some(Boundary {
            seq: resume_seq,
            definition: Some(digest(RETRY.as_bytes())),
            kind: BoundaryKind::Resume,
        }),
        resolved: known(RETRY),
        mismatch_recorded: false,
    });
    let g = graph(&ev, &defs, &[]);
    // Counts are per segment: the three retries were routed before the
    // resume, the last execution after it, so summing the segments (as the
    // overlay and a canvas over the same bytes do) counts each once.
    let of = |seg: usize, r: &str| {
        g.edges
            .iter()
            .find(|e| e.segment == seg && e.rendered == r)
            .map(|e| e.state.clone())
            .unwrap()
    };
    assert_eq!(of(0, "e:work>work#1"), EdgeState::Taken { count: 3 });
    assert_eq!(of(1, "e:work>work#1"), EdgeState::NotTaken);
    assert_eq!(of(0, "e:work>done#1"), EdgeState::NotTaken);
    assert_eq!(of(1, "e:work>done#1"), EdgeState::Taken { count: 1 });
    // Raw records keep their accounting flag.
    let counted_rows = g
        .traversals
        .iter()
        .filter(|t| t.counted && t.from == "work" && t.to == "work")
        .count();
    assert_eq!(counted_rows, 4);
    let work = g.nodes.iter().find(|n| n.node == "work").unwrap();
    assert_eq!((work.executions, work.open), (4, 0));
    assert_eq!(work.by_status.get("failed"), Some(&3));
}

const POLICY: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { on_failure: handler }
nodes:
  - { id: start, type: start }
  - { id: a, type: condition }
  - { id: handler, type: finish, outcome: failure }
edges:
  - { from: start, to: a }
  - { from: a, to: handler, condition: { type: node_status, node: a, equals: success } }
"#;

#[test]
fn a_policy_delivery_never_resolves_through_the_explicit_pair() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "a", &[]),
        ns("a"),
        nf("a", "failed"),
        policy("a", "handler"),
        nf("handler", "succeeded"),
        finished_run("failed"),
    ]);
    let g = graph(&ev, &one(POLICY), &[]);
    assert_eq!(state(&g, "e:a>handler#1"), &EdgeState::NotTaken);
    let row = g.traversals.iter().find(|t| t.via_policy).unwrap();
    assert!(row.edges.is_empty());
    assert_eq!(row.policy.as_deref(), Some("defaults"));
}

const FAN: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: s, type: condition }
  - { id: a, type: finish, outcome: success }
  - { id: b, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: a }
  - { from: s, to: b }
"#;

#[test]
fn one_hop_of_a_fan_out_does_not_prove_its_sibling_not_taken() {
    for end in [false, true] {
        let mut body = vec![
            started_run(),
            ns("start"),
            nf("start", "succeeded"),
            hop("start", "s", &[]),
            ns("s"),
            nf("s", "succeeded"),
            hop("s", "a", &["e:s>a#1"]),
        ];
        if end {
            body.push(json!({"type": "run_aborted", "reason": "stopped"}));
        }
        let ev = events(&body);
        let g = graph(&ev, &one(FAN), &[]);
        assert_eq!(state(&g, "e:s>a#1"), &EdgeState::Taken { count: 1 });
        assert!(
            matches!(state(&g, "e:s>b#1"), EdgeState::NotEstablished { .. }),
            "{:?}",
            state(&g, "e:s>b#1")
        );
        let b = g.not_run.iter().find(|n| n.node == "b").unwrap();
        assert_eq!(b.class, ExplainClass::Unknown, "{b:?}");
        assert_ne!(b.code, NotRunCode::RoutedElsewhere);
    }
}

const DEAD: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: condition }
  - { id: x, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: x, condition: { type: node_status, node: a, equals: failure } }
"#;

fn dead_end(after_seq: u64, execution: u32) -> Diagnostic {
    Diagnostic::Route(RouteEvaluated {
        after_seq,
        node: "a".into(),
        execution,
        item: None,
        mode: "dead_end".into(),
        checks: vec![RouteCheck {
            edge: "e:a>x#1".into(),
            rank: 0,
            kind: "node_status".into(),
            result: "not_matched".into(),
            observed: Some(json!("succeeded")),
        }],
        chosen: Vec::new(),
    })
}

#[test]
fn a_dead_end_diagnostic_is_attributed_after_its_own_finish_and_bounded() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "a", &[]),
        ns("a"),
        nf("a", "succeeded"),
        finished_run("succeeded"),
    ]);
    // The diagnostic follows the finish at seq 6 with no hop in between.
    let g = graph(&ev, &one(DEAD), &[dead_end(6, 1)]);
    assert_eq!(state(&g, "e:a>x#1"), &EdgeState::NotTaken);
    let x = g.not_run.iter().find(|n| n.node == "x").unwrap();
    assert_eq!(
        (x.class, x.code),
        (ExplainClass::Observed, NotRunCode::ConditionNotMatched)
    );
    // A wrong ordinal, or a record past the journal read, explains nothing.
    for d in [dead_end(6, 2), dead_end(99, 1)] {
        let g = graph(&ev, &one(DEAD), &[d]);
        assert!(
            matches!(state(&g, "e:a>x#1"), EdgeState::NotEstablished { .. }),
            "{:?}",
            state(&g, "e:a>x#1")
        );
    }
}

#[test]
fn without_a_diagnostic_a_finished_dead_end_is_inferred_from_the_recorded_state() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "a", &[]),
        ns("a"),
        nf("a", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&ev, &one(DEAD), &[]);
    assert!(matches!(
        state(&g, "e:a>x#1"),
        EdgeState::NotEstablished { .. }
    ));
    let x = g.not_run.iter().find(|n| n.node == "x").unwrap();
    assert_eq!(
        (x.class, x.code),
        (ExplainClass::Inferred, NotRunCode::ConditionNotMatched)
    );
}

const HANDLER: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { on_failure: report }
nodes:
  - { id: start, type: start }
  - { id: work, type: condition }
  - { id: report, type: finish, outcome: failure }
  - { id: done, type: finish, outcome: success }
  - { id: other, type: condition }
  - { id: join, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
  - { from: other, to: join }
  - { from: work, to: join, condition: { type: node_status, node: work, equals: failure } }
"#;

#[test]
fn an_unexecuted_failure_handler_reads_no_failure_to_route() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "work", &[]),
        ns("work"),
        nf("work", "succeeded"),
        hop("work", "done", &[]),
        nf("done", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&ev, &one(HANDLER), &[]);
    let report = g.not_run.iter().find(|n| n.node == "report").unwrap();
    assert_eq!(
        (report.class, report.code),
        (ExplainClass::Observed, NotRunCode::NoFailureToRoute),
        "{report:?}"
    );
    // `other` has no way in at all.
    let other = g.not_run.iter().find(|n| n.node == "other").unwrap();
    assert_eq!(other.code, NotRunCode::Unreachable);
    assert_eq!(other.class, ExplainClass::Inferred);
}

#[test]
fn a_node_with_two_inputs_is_explained_only_when_both_are_proven() {
    // `join` has two inputs: `other` never ran (observed), and `work` took an
    // unconditional fan-out edge whose one-of sibling is not recorded as
    // evaluated, so that entry is proven too (the routing took `done`).
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "work", &[]),
        ns("work"),
        nf("work", "succeeded"),
        hop("work", "done", &[]),
        nf("done", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&ev, &one(HANDLER), &[]);
    let join = g.not_run.iter().find(|n| n.node == "join").unwrap();
    assert_eq!(join.inputs.len(), 2, "{join:?}");
    assert_eq!(join.code, NotRunCode::NoInputDelivered, "{join:?}");
    // Drop the proof of `work`'s routing: the summary becomes unknown.
    let partial = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "work", &[]),
        ns("work"),
        nf("work", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&partial, &one(HANDLER), &[]);
    let join = g.not_run.iter().find(|n| n.node == "join").unwrap();
    assert_eq!(join.class, ExplainClass::Unknown, "{join:?}");
}

#[test]
fn an_old_journal_establishes_nothing() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "gate", &[]),
    ]);
    let defs = DefinitionsInForce {
        segments: vec![Segment {
            from_seq: 0,
            to_seq: None,
            boundary: Some(Boundary {
                seq: 1,
                definition: None,
                kind: BoundaryKind::Start,
            }),
            resolved: Resolved::Unknown(UnknownReason::NotJournaled),
            mismatch_recorded: false,
        }],
        playbook_changed: false,
    };
    let g = graph(&ev, &defs, &[]);
    assert!(g.edges.is_empty() && g.not_run.is_empty());
    assert_eq!(g.shown_segment, None);
    assert!(!g.segments[0].known);
    assert!(g.traversals.iter().all(|t| !t.resolved));
}

const A: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: condition }
  - { id: discard, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: discard, fallback: true }
"#;

#[test]
fn a_patched_run_reads_each_traversal_in_its_own_segment() {
    // Segment 0 runs DUP (two gate -> discard edges); a patch installs A
    // (one). The traversal before the patch names `#2`, which exists only
    // in DUP; the one after names `#1` of A.
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "gate", &["e:start>gate#1"]),
        ns("gate"),
        nf("gate", "succeeded"),
        hop("gate", "discard", &["e:gate>discard#2"]),
        json!({"type": "patch_applied", "version": "1.0.1", "classification": "minor", "continue_from": "gate"}),
        ns("gate"),
        nf("gate", "succeeded"),
        hop("gate", "discard", &["e:gate>discard#1"]),
    ]);
    let defs = DefinitionsInForce {
        segments: vec![
            Segment {
                from_seq: 0,
                to_seq: Some(7),
                boundary: Some(Boundary {
                    seq: 1,
                    definition: Some(digest(DUP.as_bytes())),
                    kind: BoundaryKind::Start,
                }),
                resolved: known(DUP),
                mismatch_recorded: false,
            },
            Segment {
                from_seq: 8,
                to_seq: None,
                boundary: Some(Boundary {
                    seq: 8,
                    definition: Some(digest(A.as_bytes())),
                    kind: BoundaryKind::Patch,
                }),
                resolved: known(A),
                mismatch_recorded: false,
            },
        ],
        playbook_changed: false,
    };
    let g = graph(&ev, &defs, &[]);
    let of = |seg: usize, r: &str| {
        g.edges
            .iter()
            .find(|e| e.segment == seg && e.rendered == r)
            .map(|e| e.state.clone())
    };
    assert_eq!(
        of(0, "e:gate>discard#2"),
        Some(EdgeState::Taken { count: 1 })
    );
    assert_eq!(of(0, "e:gate>discard#1"), Some(EdgeState::NotTaken));
    assert_eq!(
        of(1, "e:gate>discard#1"),
        Some(EdgeState::Taken { count: 1 })
    );
    assert_eq!(g.shown_segment, Some(1));
}

/// A run of DUP, patched to A, then patched back to DUP: each segment
/// counts its own deliveries, so the two DUP segments together count each
/// delivery once.
#[test]
fn a_run_patched_away_and_back_counts_each_delivery_in_its_own_segment() {
    let patch = |d: &str| {
        json!({"type": "patch_applied", "version": "1.0.0", "classification": "minor",
            "continue_from": "gate", "definition": digest(d.as_bytes())})
    };
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "gate", &["e:start>gate#1"]),
        ns("gate"),
        nf("gate", "succeeded"),
        hop("gate", "discard", &["e:gate>discard#2"]),
        patch(A),
        ns("gate"),
        nf("gate", "succeeded"),
        hop("gate", "discard", &["e:gate>discard#1"]),
        patch(DUP),
        ns("gate"),
        nf("gate", "succeeded"),
        hop("gate", "discard", &["e:gate>discard#2"]),
    ]);
    let seg = |from: u64, to: Option<u64>, kind: BoundaryKind, y: &str| Segment {
        from_seq: from,
        to_seq: to,
        boundary: Some(Boundary {
            seq: from.max(1),
            definition: Some(digest(y.as_bytes())),
            kind,
        }),
        resolved: known(y),
        mismatch_recorded: false,
    };
    let defs = DefinitionsInForce {
        segments: vec![
            seg(0, Some(7), BoundaryKind::Start, DUP),
            seg(8, Some(11), BoundaryKind::Patch, A),
            seg(12, None, BoundaryKind::Patch, DUP),
        ],
        playbook_changed: false,
    };
    let g = graph(&ev, &defs, &[]);
    let count = |seg: usize, r: &str| match g
        .edges
        .iter()
        .find(|e| e.segment == seg && e.rendered == r)
        .map(|e| &e.state)
    {
        Some(EdgeState::Taken { count }) => *count,
        _ => 0,
    };
    assert_eq!(
        (count(0, "e:gate>discard#2"), count(2, "e:gate>discard#2")),
        (1, 1)
    );
    assert_eq!(count(1, "e:gate>discard#1"), 1);
    let total: u32 = (0..3).map(|s| count(s, "e:gate>discard#2")).sum();
    assert_eq!(
        total, 2,
        "two deliveries over DUP's second edge, never four"
    );
    let text = g.lines().join("\n");
    assert!(
        text.contains("edge e:gate>discard#2 [segment 0]: taken 1"),
        "{text}"
    );
    assert!(
        text.contains("edge e:gate>discard#2 [segment 2]: taken 1"),
        "{text}"
    );
}

const LOOP: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: c, type: condition, max_loops: 1 }
  - { id: again, type: finish, outcome: success }
  - { id: out, type: finish, outcome: success }
edges:
  - { from: start, to: c }
  - { from: c, to: again, condition: { type: node_status, node: c, equals: success } }
  - { from: c, to: out, fallback: true }
"#;

#[test]
fn a_loop_exit_diagnostic_explains_the_edge_it_skipped() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "c", &[]),
        ns("c"),
        nf("c", "succeeded"),
        hop("c", "out", &["e:c>out#1"]),
        nf("out", "succeeded"),
        finished_run("succeeded"),
    ]);
    let record = |mode: &str, chosen: &str| {
        Diagnostic::Route(RouteEvaluated {
            after_seq: 7,
            node: "c".into(),
            execution: 1,
            item: None,
            mode: mode.into(),
            checks: vec![RouteCheck {
                edge: "e:c>again#1".into(),
                rank: 0,
                kind: "node_status".into(),
                result: "not_reached".into(),
                observed: None,
            }],
            chosen: vec![chosen.into()],
        })
    };
    // Both records name the same execution; the loop exit is the decision.
    let diags = [
        record("one_of", "e:c>out#1"),
        record("loop_exhausted", "e:c>out#1"),
        record("one_of", "e:c>out#1"),
    ];
    let g = graph(&ev, &one(LOOP), &diags);
    let again = g.not_run.iter().find(|n| n.node == "again").unwrap();
    assert_eq!(
        (again.class, again.code),
        (ExplainClass::Observed, NotRunCode::LoopExhausted),
        "{again:?}"
    );
}

const RANKED: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: src, type: condition }
  - { id: a, type: finish, outcome: failure }
  - { id: b, type: finish, outcome: success }
  - { id: c, type: finish, outcome: success }
edges:
  - { from: start, to: src }
  - { from: src, to: a, condition: { type: node_status, node: src, equals: failure } }
  - { from: src, to: b, condition: { type: judge, question: "Ship it?", min_p: 0.7, on_unavailable: false } }
  - { from: src, to: c, fallback: true }
"#;

#[test]
fn a_judge_edge_answer_keyed_by_rank_finds_its_edge_after_a_plain_one() {
    let decision = json!({"type": "decision_made", "use_site": "judge_edge", "node": "src",
        "attempt": 1, "mode": "enforce", "applied": true,
        "answers": {"edge_1": {"value": true, "p": 0.82}}});
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "src", &[]),
        ns("src"),
        nf("src", "succeeded"),
        decision,
        hop("src", "b", &["e:src>b#1"]),
        nf("b", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&ev, &one(RANKED), &[]);
    let j = &g.judge_edges[0];
    // Rank 1 among `src`'s outgoing edges is the judge edge, not the
    // node_status edge declared before it.
    assert_eq!(j.rendered.as_deref(), Some("e:src>b#1"));
    assert_eq!(j.min_p, Some(0.7));
    assert_eq!(j.taken, Some(true));
    assert!(j.label.contains("0.82 >= 0.70"), "{}", j.label);
}

const JUDGE: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: triage
    type: judge
    questions:
      verdict: { type: choice, instructions: i, criteria: { clean: a, needs_fix: b, unclear: c } }
      risky: { type: noul, instructions: i }
      effort: { type: score, instructions: i, levels: [a, b, c, d] }
    thresholds:
      verdict: { min_confidence: 0.7, below: unclear }
      risky: { yes_at: 0.6 }
      effort: { bands: { low: [0, 1], high: [2, 3] } }
    on_unavailable: { route: done }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: triage }
  - { from: triage, to: done }
"#;

#[test]
fn judge_answers_compare_the_measure_each_question_routes_on() {
    let decision = json!({
        "type": "decision_made", "use_site": "judge_node", "node": "triage",
        "mode": "enforce", "applied": true, "provider": "fake", "model": "fake-1",
        "answers": {
            "verdict": {"value": "needs_fix", "p": 0.95, "confidence": 0.30},
            "risky": {"value": true, "p": 0.55},
            "effort": {"value": 1.5}
        }
    });
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "triage", &[]),
        ns("triage"),
        decision,
        nf_out(
            "triage",
            r#"{"verdict":"unclear","risky":false,"effort_score":1.5,"decided_by":"fake/fake-1"}"#,
        ),
        hop("triage", "done", &[]),
        nf("done", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&ev, &one(JUDGE), &[]);
    let j = &g.judge_nodes[0];
    assert_eq!(j.decided_by.as_deref(), Some("fake/fake-1"));
    let q = apb_engine::run_outcome::judge_answers::questions_by_id(j);
    let verdict = &q["verdict"];
    let c = verdict.answers[0].comparison.as_ref().unwrap();
    // A choice routes on its confidence: 0.95 is the probability, 0.30
    // failed the threshold and the engine substituted `below`.
    assert_eq!(c.measure, "confidence");
    assert_eq!(c.passed, Some(false));
    assert!(c.label.contains("0.30 < 0.70"), "{}", c.label);
    assert_eq!(verdict.routed_value, Some(json!("unclear")));
    assert_eq!(verdict.answers[0].value, Some(json!("needs_fix")));
    let risky = q["risky"].answers[0].comparison.as_ref().unwrap();
    assert_eq!((risky.measure.as_str(), risky.passed), ("p", Some(false)));
    let effort = q["effort"].answers[0].comparison.as_ref().unwrap();
    assert_eq!(effort.band, None, "1.5 lies in the gap between the bands");
    assert!(effort.label.contains("no band"), "{}", effort.label);
}

#[test]
fn a_judge_without_any_decision_still_shows_its_fallback_route() {
    let ev = events(&[
        started_run(),
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "triage", &[]),
        ns("triage"),
        nf_out(
            "triage",
            r#"{"decided_by":"unavailable","reason":"not_configured"}"#,
        ),
        hop("triage", "done", &[]),
        nf("done", "succeeded"),
        finished_run("succeeded"),
    ]);
    let g = graph(&ev, &one(JUDGE), &[]);
    let j = &g.judge_nodes[0];
    assert!(j.evidence.is_empty());
    assert_eq!(j.decided_by.as_deref(), Some("unavailable"));
    assert_eq!(j.reason.as_deref(), Some("not_configured"));
    assert_eq!(j.fallback.as_deref(), Some("route"));
}

#[test]
fn native_and_emulation_answers_stay_apart() {
    let native = json!({"type": "decision_made", "use_site": "judge_node", "node": "triage",
        "mode": "enforce", "applied": false, "error": "unavailable", "answers": {}});
    let emulated = json!({"type": "decision_made", "use_site": "judge_node", "node": "triage",
        "mode": "enforce", "applied": true, "provider": "profile",
        "answers": {"verdict": {"value": "clean", "p": 0.9, "confidence": 0.9}}});
    let ev = events(&[
        started_run(),
        ns("triage"),
        native,
        emulated,
        nf_out(
            "triage",
            r#"{"verdict":"clean","decided_by":"emulated:profile"}"#,
        ),
    ]);
    let g = graph(&ev, &one(JUDGE), &[]);
    let sources: Vec<&str> = g.judge_nodes[0]
        .evidence
        .iter()
        .map(|e| e.source.as_str())
        .collect();
    assert_eq!(sources, ["native", "emulation"]);
    let verdict =
        &apb_engine::run_outcome::judge_answers::questions_by_id(&g.judge_nodes[0])["verdict"];
    assert_eq!(verdict.answers.len(), 1);
    assert_eq!(verdict.answers[0].source, "emulation");
}

#[test]
fn a_presented_graph_is_redacted_before_it_is_clipped() {
    let secret = "s3cr3t-value-that-is-long-enough-to-scrub";
    // The secret straddles the clip point: clipping first would leave a
    // fragment no scrubber recognizes.
    let long = format!("{}{secret}{}", "x".repeat(220), "y".repeat(100));
    let output = json!({"verdict": long, "decided_by": "fake/fake-1"}).to_string();
    let ev = events(&[started_run(), ns("triage"), nf_out("triage", &output)]);
    let g = graph(&ev, &one(JUDGE), &[]);
    let root = tempfile::tempdir().unwrap();
    let run_dir = root.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("run.yaml"),
        format!("params:\n  api_token: {secret}\n"),
    )
    .unwrap();
    let redactor = apb_engine::run_redact::RunRedactor::for_run(root.path(), &run_dir);
    let shown = g.present(&redactor).to_string();
    assert!(!shown.contains("s3cr3t"), "{shown}");
    assert!(shown.contains("..."), "clipped after redaction: {shown}");
}

/// Writes run `id` under `root`: a journal opening with `version` at `ts`,
/// journaling `definition` (stored when given) and optionally a patch to
/// `patched` (stored).
fn write_run(
    root: &std::path::Path,
    id: &str,
    version: &str,
    ts: u64,
    definition: Option<&str>,
    patched: Option<&str>,
) {
    let dir = root.join(".apb/runs").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut started =
        json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": "p", "version": version});
    if let Some(y) = definition {
        started["definition"] =
            json!(apb_engine::run_definitions::store(&dir, y.as_bytes()).unwrap());
        std::fs::write(dir.join("playbook.yaml"), y).unwrap();
    }
    let mut lines = vec![started];
    if let Some(y) = patched {
        let d = apb_engine::run_definitions::store(&dir, y.as_bytes()).unwrap();
        lines.push(
            json!({"seq": 2, "ts": ts + 1, "type": "patch_applied", "version": version,
            "classification": "minor", "continue_from": "gate", "definition": d}),
        );
    }
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
}

#[test]
fn the_overlay_aligns_each_run_and_finds_the_default_beyond_the_page() {
    use apb_engine::run_outcome::overlay::{Alignment, OverlayRequest, PAGE, overlay};
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    // The overrides case: the same version, other bytes, no patch.
    let other_bytes = format!("{A}# overridden\n");
    write_run(r, "exact", "1.0.0", 1_000, Some(A), None);
    write_run(r, "overridden", "1.0.0", 1_100, Some(&other_bytes), None);
    write_run(r, "patched", "1.0.0", 1_200, Some(A), Some(DUP));
    write_run(r, "old", "1.0.0", 1_300, None, None);
    // Its snapshot matches the shown bytes, but no boundary journaled a
    // digest: still unknown, never exact.
    std::fs::write(r.join(".apb/runs/old/playbook.yaml"), A).unwrap();
    // A, patched to DUP, then back to A is not exact either.
    write_run(r, "back", "1.0.0", 1_350, Some(A), Some(A));
    for i in 0..(PAGE + 5) {
        write_run(
            r,
            &format!("v2-{i:02}"),
            "2.0.0",
            2_000 + i as u64,
            Some(DUP),
            None,
        );
    }
    fn req<'a>(
        r: &'a std::path::Path,
        run: Option<&'a str>,
        before: Option<&'a str>,
    ) -> OverlayRequest<'a> {
        OverlayRequest {
            root: r,
            playbook: "p",
            version: "1.0.0",
            yaml: A,
            run,
            before,
        }
    }
    let o = overlay(&req(r, None, None));
    assert_eq!(o.candidates.len(), PAGE);
    assert!(
        o.candidates
            .iter()
            .all(|c| c.alignment == Alignment::OtherVersion)
    );
    // "back" journaled A at both boundaries: exact by its digests.
    assert_eq!(o.default_run.as_deref(), Some("back"));
    assert_eq!(o.chosen.as_deref(), Some("back"));
    let next = o.next_before.unwrap();
    let page2 = overlay(&req(r, None, Some(&next)));
    let by_id: BTreeMap<&str, Alignment> = page2
        .candidates
        .iter()
        .map(|c| (c.run_id.as_str(), c.alignment))
        .collect();
    assert_eq!(by_id["exact"], Alignment::Exact);
    assert_eq!(by_id["overridden"], Alignment::SameVersionOtherDefinition);
    assert_eq!(by_id["patched"], Alignment::Patched);
    assert_eq!(by_id["old"], Alignment::Unknown);
    // `run=` resolves a run that is not on the shown page.
    let chosen = overlay(&req(r, Some("patched"), None));
    assert_eq!(chosen.chosen.as_deref(), Some("patched"));
    assert_eq!(chosen.alignment, Some(Alignment::Patched));
    // Each known segment has its own mapping onto the shown version.
    assert_eq!(chosen.mapping.len(), 2);
    assert!(chosen.mapping.iter().all(|m| m.correspondence.is_some()));
    let unknown = overlay(&req(r, Some("old"), None));
    assert!(unknown.mapping.iter().all(|m| m.correspondence.is_none()));
}

#[test]
fn overlay_pages_keep_runs_that_share_a_start_time() {
    use apb_engine::run_outcome::overlay::{OverlayRequest, PAGE, overlay};
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    // Every two runs share a start time, so a page boundary falls inside a
    // tie.
    let total = 2 * PAGE + 3;
    for i in 0..total {
        write_run(
            r,
            &format!("run-{i:02}"),
            "1.0.0",
            1_000 + (i / 2) as u64,
            Some(A),
            None,
        );
    }
    let mut seen: Vec<String> = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let o = overlay(&OverlayRequest {
            root: r,
            playbook: "p",
            version: "1.0.0",
            yaml: A,
            run: None,
            before: before.as_deref(),
        });
        seen.extend(o.candidates.iter().map(|c| c.run_id.clone()));
        match o.next_before {
            Some(n) => before = Some(n),
            None => break,
        }
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(seen.len(), total, "every run listed once: {seen:?}");
    assert_eq!(unique.len(), total);
}

#[test]
fn statistics_membership_counts_established_definitions_and_unknown_apart() {
    use apb_engine::run_stats::{StatsFilter, StatsRun, build};
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    // Run 1 executes `discard` under DUP, then a patch installs FAN, which
    // has no `discard` (the final snapshot lacks it); run 2 is
    // an old run with no journaled definition.
    write_run(r, "r1", "1.0.0", 1_000, Some(DUP), None);
    let dir = r.join(".apb/runs/r1");
    let more = [
        ns("start"),
        nf("start", "succeeded"),
        hop("start", "gate", &["e:start>gate#1"]),
        ns("gate"),
        nf("gate", "succeeded"),
        hop("gate", "discard", &["e:gate>discard#2"]),
        nf("discard", "succeeded"),
        json!({"type": "patch_applied", "version": "1.0.0", "classification": "minor",
            "continue_from": "gate",
            "definition": apb_engine::run_definitions::store(&dir, FAN.as_bytes()).unwrap()}),
        finished_run("succeeded"),
    ];
    std::fs::write(dir.join("playbook.yaml"), FAN).unwrap();
    let mut text = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    for (i, v) in more.iter().enumerate() {
        let mut v = v.clone();
        v["seq"] = json!(i as u64 + 2);
        v["ts"] = json!(1_001 + i as u64);
        text.push_str(&format!("{v}\n"));
    }
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
    write_run(r, "r2", "1.0.0", 1_100, None, None);
    std::fs::write(r.join(".apb/runs/r2/playbook.yaml"), DUP).unwrap();
    let runs: Vec<StatsRun> = ["r1", "r2"]
        .iter()
        .map(|id| StatsRun::load(&r.join(".apb/runs").join(id)).unwrap())
        .collect();
    let report = build(&runs, &StatsFilter::default());
    let v = &report.versions[0];
    let node = |id: &str| v.nodes.iter().find(|n| n.node == id).unwrap();
    assert_eq!((node("discard").runs, node("discard").in_runs), (1, 1));
    assert_eq!(node("discard").in_runs_unknown, 1);
    assert!(node("discard").runs <= node("discard").in_runs);
}
