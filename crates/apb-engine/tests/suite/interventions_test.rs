//! Intervention counters (U24): the pure fold over synthetic journals and
//! the version figures `apb stats` reports.

use std::fs;
use std::path::{Path, PathBuf};

use apb_engine::event::Event;
use apb_engine::run_stats::interventions::{
    ATTRIBUTION_SINCE, RunInterventions, interventions, version_stats, writer_of,
};
use apb_engine::run_stats::{StatsFilter, stats};
use serde_json::{Value, json};

/// A journal from bare payloads, seq from 1.
fn journal(payloads: &[Value]) -> Vec<Event> {
    payloads
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut line = p.clone();
            line["seq"] = json!(i as u64 + 1);
            line["ts"] = json!(i as u64 * 1000);
            serde_json::from_value(line).expect("event")
        })
        .collect()
}

fn start(apb: Option<&str>) -> Vec<Value> {
    let mut prov = json!({"type": "run_provenance", "origin": "project"});
    if let Some(v) = apb {
        prov["apb"] = json!(v);
    }
    vec![
        json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        prov,
    ]
}

fn fold(payloads: Vec<Value>) -> RunInterventions {
    let ev = journal(&payloads);
    let writer = writer_of(&ev);
    interventions(&ev, &apb_engine::identity::index(&ev), writer.as_deref())
}

fn current(mut rest: Vec<Value>) -> RunInterventions {
    let mut p = start(Some(ATTRIBUTION_SINCE));
    p.append(&mut rest);
    fold(p)
}

fn request(node: &str) -> Value {
    json!({"type": "review_requested", "node": node, "options": ["approve", "reject"]})
}

fn decided(node: &str, decision: &str, extra: Value) -> Value {
    let mut v = json!({"type": "review_decided", "node": node, "decision": decision, "note": ""});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn decision(use_site: &str, node: &str, attempt: u32, applied: bool, cached: bool) -> Value {
    json!({"type": "decision_made", "use_site": use_site, "node": node, "attempt": attempt,
           "applied": applied, "cached": cached})
}

fn action(name: &str, node: Option<&str>, detail: &str) -> Value {
    json!({"type": "supervisor_action", "action": name, "node": node, "detail": detail})
}

/// A stop of the parent propagates into the child as `run_aborted` with
/// reason `parent aborted` by the same actor: one `apb stop` is one
/// operator action, on the parent's playbook, never also on the child's.
#[test]
fn a_propagated_child_abort_is_not_an_operator_action_of_the_child() {
    let r = current(vec![
        json!({"type": "node_started", "node": "w", "attempt": 1}),
        json!({"type": "run_aborted", "reason": "parent aborted", "by": "operator"}),
    ]);
    assert!(r.operator.is_empty(), "{:?}", r.operator);
    assert_eq!(r.engine["abort_propagated"], 1);
    assert_eq!(r.total, Some(0));
}

/// The engine journals a gate's applied `review_triage` decision before it
/// journals the visit's `review_requested`; the decision still marks the
/// visit automatic.
#[test]
fn a_review_triage_before_the_request_marks_the_visit_automatic() {
    let r = current(vec![
        decision("review_triage", "gate", 1, true, false),
        request("gate"),
        decided("gate", "approve", json!({})),
    ]);
    assert_eq!(r.reviews.automatic, 1);
    assert_eq!(r.reviews.channel, 0);
    assert_eq!(r.total, Some(0));
    assert_eq!(r.automatic_total, Some(1));
}

#[test]
fn an_expedite_by_host_is_not_an_operator_action() {
    let r = current(vec![
        json!({"type": "retry_expedited", "node": "w", "by": "host"}),
    ]);
    assert!(r.operator.is_empty(), "{:?}", r.operator);
    assert_eq!(r.engine["expedite_by_host"], 1);
    assert_eq!(r.total, Some(0));
}

/// A broadcast interrupt is a run correction even when only one attempt was
/// running to observe it; a targeted interrupt of one node stays a node
/// correction. The control entry the interrupt came from names its target.
#[test]
fn a_broadcast_interrupt_is_a_run_correction_at_any_attempt_count() {
    let r = current(vec![
        action("control_received", Some("a"), "interrupt (control seq 4)"),
        action(
            "attempt_interrupted",
            Some("a"),
            "supervisor requested interrupt of node `a` attempt 1 (control seq 4): stop",
        ),
    ]);
    assert_eq!(r.supervisor_run["interrupt"], 1);
    assert!(r.supervisor_node.is_empty(), "{:?}", r.supervisor_node);
    assert_eq!(r.total, Some(1));
    let targeted = current(vec![
        action(
            "control_received",
            Some("a"),
            "interrupt node `a` (control seq 4)",
        ),
        action(
            "attempt_interrupted",
            Some("a"),
            "supervisor requested interrupt of node `a` attempt 1 (control seq 4): stop",
        ),
    ]);
    assert_eq!(targeted.supervisor_node["interrupt"], 1);
    assert_eq!(targeted.total, Some(1));
}

#[test]
fn gates_split_by_channel_automatic_and_withdrawn() {
    let r = current(vec![
        request("gate"),
        decided("gate", "approve", json!({"via": "terminal"})),
        // The real order: the engine journals the applied triage decision
        // before the visit it advises is requested.
        decision("review_triage", "gate", 1, true, false),
        request("gate"),
        decided("gate", "approve", json!({"decided_by": "model"})),
        request("gate"),
        decided("gate", "reject", json!({"decided_by": "unattended"})),
        request("gate"),
        json!({"type": "review_withdrawn", "node": "gate", "reason": "node_retry"}),
        request("gate"),
        decided("gate", "approve", json!({"via": "mcp"})),
    ]);
    assert_eq!(r.reviews.visits, 5);
    assert_eq!(r.reviews.channel, 2);
    assert_eq!(r.reviews.automatic, 2);
    assert_eq!(r.reviews.withdrawn, 1);
    assert_eq!(r.reviews.unattributed, 0);
    assert_eq!(r.reviews.via["terminal"], 1);
    assert_eq!(r.reviews.via["mcp"], 1);
    assert_eq!(r.reviews.options["approve"], 3);
    assert_eq!(r.reviews.options["reject"], 1);
    // Two channel decisions are the only interventions; the enforced
    // recommendation and the unattended outcome are automatic.
    assert_eq!(r.total, Some(2));
    assert_eq!(r.automatic_total, Some(2));
    assert_eq!(r.automatic["review_triage"].applied, 1);
}

#[test]
fn an_engine_decision_without_via_on_a_current_writer_is_automatic() {
    let r = current(vec![
        request("gate"),
        decided("gate", "approve", json!({"decided_by": "unattended"})),
        request("gate"),
        decided("gate", "approve", json!({})),
    ]);
    assert_eq!(r.reviews.automatic, 1);
    // A current writer always records `via` for a surface; one without it
    // and without `decided_by` still counts as the channel, surface not
    // recorded.
    assert_eq!(r.reviews.channel, 1);
    assert_eq!(r.reviews.via["not_recorded"], 1);
    assert_eq!(r.total, Some(1));
}

#[test]
fn questions_split_by_who_answered() {
    let ask = |n: &str| json!({"type": "question_asked", "node": n, "question": "which?"});
    let answer = |n: &str, by: &str| json!({"type": "question_answered", "node": n, "answer": "a", "answered_by": by});
    let r = current(vec![
        ask("work"),
        answer("work", "human"),
        ask("work"),
        answer("work", "supervisor"),
        ask("work"),
        answer("work", "timeout"),
        ask("work"),
        answer("work", "unattended"),
        ask("work"),
        action("question_abandoned", Some("work"), "stopped"),
    ]);
    assert_eq!(r.questions.asked, 5);
    assert_eq!(r.questions.abandoned, 1);
    assert_eq!(r.question_total(), 2);
    assert_eq!(r.total, Some(2));
    assert_eq!(r.automatic_total, Some(1), "the unattended answer");
}

#[test]
fn a_cached_replay_counts_once_and_a_later_execution_again() {
    let r = current(vec![
        json!({"type": "node_started", "node": "work", "attempt": 1}),
        decision("completion_check", "work", 1, true, false),
        json!({"type": "run_paused", "reason": "x", "by": "engine"}),
        json!({"type": "run_resumed", "from_node": "work", "by": "engine", "apb": ATTRIBUTION_SINCE}),
        decision("completion_check", "work", 1, true, true),
        json!({"type": "node_finished", "node": "work", "status": "succeeded", "output": "", "attempt": 1}),
        json!({"type": "node_started", "node": "work", "attempt": 1}),
        decision("completion_check", "work", 1, true, false),
        json!({"type": "node_finished", "node": "work", "status": "succeeded", "output": "", "attempt": 1}),
    ]);
    let u = r.automatic["completion_check"];
    assert_eq!((u.asked, u.applied), (2, 2));
    assert_eq!(
        r.total,
        Some(0),
        "engine pause and resume are not interventions"
    );
    assert_eq!(r.engine["pause"], 1);
    assert_eq!(r.engine["resume"], 1);
    assert_eq!(r.automatic_total, Some(2));
}

#[test]
fn a_broadcast_interrupt_counts_once() {
    let r = current(vec![
        action("control_received", Some("a"), "interrupt (control seq 4)"),
        action(
            "attempt_interrupted",
            Some("a"),
            "supervisor requested interrupt of node `a` attempt 1 (control seq 4): stop",
        ),
        action("control_received", Some("b"), "interrupt (control seq 4)"),
        action(
            "attempt_interrupted",
            Some("b"),
            "supervisor requested interrupt of node `b` attempt 1 (control seq 4): stop",
        ),
        action(
            "attempt_interrupted",
            Some("b"),
            "supervisor requested interrupt of node `b` attempt 2 (control seq 9): again",
        ),
    ]);
    // Each broadcast is one run correction however many attempts observed
    // it; the second entry is another control entry's broadcast.
    assert_eq!(r.supervisor_run["interrupt"], 2);
    assert!(r.supervisor_node.is_empty(), "{:?}", r.supervisor_node);
    assert_eq!(r.total, Some(2));
}

#[test]
fn a_note_counts_as_a_supervisor_channel_correction() {
    // `apb note` posts the same `context_append` as the MCP tool.
    let r = current(vec![
        action("context_append", None, "please look at the tests"),
        action("node_retry", Some("work"), ""),
        action("run_continue_from", Some("check"), ""),
        json!({"type": "profile_rebound", "node": "work", "profile": "project/b", "bundle": "x", "reason": ""}),
        json!({"type": "patch_applied", "version": "1.0.1", "classification": "minor", "continue_from": "work"}),
        json!({"type": "run_paused", "reason": "look", "by": "supervisor"}),
    ]);
    assert_eq!(r.supervisor_run["context_append"], 1);
    assert_eq!(r.supervisor_run["patch"], 1);
    assert_eq!(r.supervisor_run["pause"], 1);
    assert_eq!(r.supervisor_node["node_retry"], 1);
    assert_eq!(r.supervisor_node["run_continue_from"], 1);
    assert_eq!(r.supervisor_node["rebind"], 1);
    assert_eq!(r.total, Some(6));
}

#[test]
fn engine_markers_and_automatic_effects_are_never_interventions() {
    let r = current(vec![
        action("infra_retry", Some("w"), ""),
        action("timeout_continuation", Some("w"), ""),
        action("quota_probe", None, ""),
        action("attempt_stalled", Some("w"), ""),
        action("interaction_downgraded", Some("w"), ""),
        action("retry_superseded_by_stop", Some("w"), ""),
        action("triage_retry", Some("w"), ""),
        action("retry_advice", Some("w"), ""),
        json!({"type": "run_paused", "reason": "budget (tokens)", "by": "engine"}),
        json!({"type": "retry_expedited", "node": "w", "by": "engine"}),
    ]);
    assert_eq!(r.total, Some(0));
    assert_eq!(r.engine.len(), 8);
    assert!(r.other.is_empty());
    assert!(r.supervisor_node.is_empty() && r.supervisor_run.is_empty());
}

#[test]
fn an_operator_resume_of_a_budget_pause_and_an_expedite() {
    let r = current(vec![
        json!({"type": "run_paused", "reason": "budget (tokens)", "by": "engine"}),
        json!({"type": "run_resumed", "from_node": "w", "by": "operator", "apb": ATTRIBUTION_SINCE}),
        json!({"type": "retry_expedited", "node": "w", "by": "operator"}),
        json!({"type": "retry_expedited", "node": "w", "by": "engine"}),
        json!({"type": "run_aborted", "reason": "stop", "by": "operator"}),
    ]);
    assert_eq!(r.operator["resume"], 1);
    assert_eq!(r.operator["expedite"], 1);
    assert_eq!(r.operator["abort"], 1);
    assert_eq!(r.engine["pause"], 1);
    assert_eq!(r.engine["expedite"], 1);
    assert_eq!(r.total, Some(3));
}

#[test]
fn an_older_writer_reads_attribution_unknown_but_counts_requests() {
    for apb in [None, Some("1.0.0-beta.2")] {
        let mut p = start(apb);
        p.extend([
            request("gate"),
            decided("gate", "approve", json!({})),
            request("gate"),
        ]);
        let r = fold(p);
        assert!(!r.attribution_known);
        assert_eq!(r.reviews.visits, 2, "requests are known for every run");
        assert_eq!(r.reviews.open, 1);
        assert_eq!(r.reviews.unattributed, 1);
        assert_eq!(r.reviews_channel(), None);
        assert_eq!(r.total, None, "unknown, never zero");
        assert_eq!(r.automatic_total, None);
    }
}

#[test]
fn a_resume_by_an_older_binary_makes_the_writer_older() {
    let mut p = start(Some(ATTRIBUTION_SINCE));
    p.push(
        json!({"type": "run_resumed", "from_node": "w", "by": "operator", "apb": "1.0.0-beta.2"}),
    );
    let ev = journal(&p);
    assert_eq!(writer_of(&ev).as_deref(), Some("1.0.0-beta.2"));
    p.push(json!({"type": "run_resumed", "from_node": "w", "by": "operator"}));
    assert_eq!(writer_of(&journal(&p)), None);
}

#[test]
fn a_pause_without_by_reads_actor_unknown() {
    let r = current(vec![
        json!({"type": "run_paused", "reason": "x"}),
        json!({"type": "run_resumed", "from_node": "w", "apb": ATTRIBUTION_SINCE}),
    ]);
    assert_eq!(r.actor_unknown["pause"], 1);
    assert_eq!(r.actor_unknown["resume"], 1);
    assert!(r.supervisor_run.is_empty(), "never read as the supervisor");
    assert_eq!(r.operator_total(), None);
    assert_eq!(r.total, None);
}

#[test]
fn list_item_decisions_count_per_item() {
    let item = |i: &str| {
        let mut d = decision("completion_check", "each", 1, true, false);
        d["item"] = json!(i);
        d
    };
    let r = current(vec![
        json!({"type": "node_started", "node": "each", "attempt": 1}),
        item("a"),
        item("b"),
        item("b"),
        item("c"),
        json!({"type": "node_finished", "node": "each", "status": "succeeded", "output": "", "attempt": 1}),
    ]);
    assert_eq!(r.automatic["completion_check"].applied, 3);
    assert_eq!(r.items, 3);
}

#[test]
fn an_unknown_action_name_is_other_and_outside_totals() {
    let r = current(vec![action("newer_marker", Some("w"), "")]);
    assert_eq!(r.other["newer_marker"], 1);
    assert_eq!(r.total, Some(0));
}

#[test]
fn version_figures_say_where_they_are_known() {
    let known = current(vec![
        request("g"),
        decided("g", "approve", json!({"via": "terminal"})),
    ]);
    let mut old = start(None);
    old.extend([request("g"), decided("g", "approve", json!({}))]);
    let old = fold(old);
    let s = version_stats(&[known, old]).unwrap();
    assert_eq!(s.runs, 2);
    assert_eq!((s.total.value, s.total.known_runs, s.total.runs), (1, 1, 2));
    assert_eq!(s.total.text(), "1 (known in 1 of 2 runs)");
    let review = s
        .categories
        .iter()
        .find(|c| c.category == "review")
        .unwrap();
    assert_eq!(review.denominator, 2, "both visits are counted");
    assert_eq!(review.count.known_runs, 1);
    assert!(version_stats(&[]).is_none());
}

// --- through `apb stats` over run directories ---

fn write_run(root: &Path, id: &str, payloads: &[Value]) -> PathBuf {
    write_run_with(root, id, payloads, None)
}

fn write_run_with(
    root: &Path,
    id: &str,
    payloads: &[Value],
    manifest: Option<apb_engine::manifest::RunExecutionManifest>,
) -> PathBuf {
    let dir = root.join(".apb/runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for e in journal(payloads) {
        text.push_str(&serde_json::to_string(&e).unwrap());
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
    if let Some(m) = manifest {
        apb_engine::manifest::write(&dir, &m).unwrap();
    }
    // `apb stats` reads only the runs apb created on this machine.
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::run_origin::stamp(&dir, id).unwrap();
    assert!(apb_core::run_origin::verify(&dir, id));
    dir
}

fn finished(mut rest: Vec<Value>) -> Vec<Value> {
    let mut p = start(Some(ATTRIBUTION_SINCE));
    p.append(&mut rest);
    p.push(json!({"type": "run_finished", "outcome": "succeeded"}));
    p
}

/// The three-run version: one channel decision; one automatic decision and
/// an engine action; a channel decision, an operator action and a
/// supervisor correction.
#[test]
fn three_runs_give_counts_and_denominators() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_run(
        root,
        "r1",
        &finished(vec![
            request("g"),
            decided("g", "approve", json!({"via": "terminal"})),
        ]),
    );
    write_run(
        root,
        "r2",
        &finished(vec![
            request("g"),
            decided("g", "approve", json!({"decided_by": "unattended"})),
            action("infra_retry", Some("w"), ""),
        ]),
    );
    write_run(
        root,
        "r3",
        &finished(vec![
            request("g"),
            decided("g", "reject", json!({"via": "dashboard_session"})),
            json!({"type": "run_paused", "reason": "x", "by": "operator"}),
            action("context_append", None, "note"),
        ]),
    );
    // A scenario run of the same playbook never counts.
    let m = apb_engine::manifest::RunExecutionManifest {
        origin: Some(apb_engine::manifest::RunOrigin::Scenario {
            level: apb_core::scenario::ScenarioLevel::Quick,
            scenario: "green".into(),
            scenario_digest: "sha256:s".into(),
        }),
        ..Default::default()
    };
    write_run_with(
        root,
        "s1",
        &finished(vec![
            request("g"),
            decided("g", "approve", json!({"via": "scenario"})),
        ]),
        Some(m),
    );
    let report = stats(&[root.to_path_buf()], &StatsFilter::default());
    let v = &report.versions[0];
    assert_eq!(v.runs, 3);
    let i = v.interventions.as_ref().expect("finished runs");
    let review = i
        .categories
        .iter()
        .find(|c| c.category == "review")
        .unwrap();
    assert_eq!(review.denominator, 3);
    assert_eq!(review.split["channel"].value, 2);
    assert_eq!(review.split["automatic"].value, 1);
    let operator = i
        .categories
        .iter()
        .find(|c| c.category == "operator")
        .unwrap();
    assert_eq!(operator.count.value, 1);
    let sup = i
        .categories
        .iter()
        .find(|c| c.category == "supervisor_run")
        .unwrap();
    assert_eq!(sup.count.value, 1);
    assert_eq!(i.engine["infra_retry"], 1);
    assert_eq!((i.total.value, i.total.known_runs), (4, 3));
    assert_eq!(i.per_run_median, Some(1.0));
    assert_eq!(
        (
            i.without_any.value,
            i.without_any.known_runs,
            i.without_any.runs
        ),
        (1, 3, 3)
    );
    let text = apb_engine::run_stats::render_text(&report);
    assert!(text.contains("interventions"), "{text}");
    assert!(
        !text.to_lowercase().contains("human intervention"),
        "{text}"
    );
}

/// The propagated-abort reason is a literal in the interventions fold, kept
/// apart from `crate::stop` to avoid an import cycle. This pins the
/// spelling to the reason `stop::abort_children` journals.
#[test]
fn the_propagated_abort_reason_matches_the_stop_side() {
    assert_eq!(
        apb_engine::run_stats::interventions::PROPAGATED_ABORT,
        "parent aborted"
    );
}
