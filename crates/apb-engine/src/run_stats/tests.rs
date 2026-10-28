use super::*;
use crate::event::Event;

fn ev(seq: u64, ts: u128, v: serde_json::Value) -> Event {
    Event {
        seq,
        ts,
        payload: serde_json::from_value(v).unwrap(),
    }
}

/// Builds a run from `(ts, event json)` pairs; the RunStarted is added.
fn run(
    id: &str,
    version: &str,
    snapshot: Option<&str>,
    body: &[(u128, serde_json::Value)],
) -> StatsRun {
    use serde_json::json;
    let mut events = vec![ev(
        0,
        1_000,
        json!({"type": "run_started", "playbook": "p", "version": version}),
    )];
    for (i, (ts, v)) in body.iter().enumerate() {
        events.push(ev(i as u64 + 1, *ts, v.clone()));
    }
    StatsRun {
        run_id: id.into(),
        playbook: "p".into(),
        version: version.into(),
        events,
        snapshot: snapshot.map(|y| apb_core::schema::Playbook::from_yaml(y).unwrap()),
        candidate_trial: false,
        expected_models: BTreeMap::new(),
        execution: Some("cli".into()),
        node_profiles: BTreeMap::new(),
        grades: Vec::new(),
    }
}

fn started(node: &str) -> serde_json::Value {
    serde_json::json!({"type": "node_started", "node": node, "attempt": 1})
}
fn finished(node: &str, status: &str, attempt: u32) -> serde_json::Value {
    serde_json::json!({"type": "node_finished", "node": node, "status": status, "attempt": attempt, "output": "", "artifacts": []})
}
fn edge(from: &str, to: &str) -> serde_json::Value {
    serde_json::json!({"type": "edge_traversed", "from": from, "to": to})
}
fn end(outcome: &str) -> serde_json::Value {
    serde_json::json!({"type": "run_finished", "outcome": outcome})
}

const SNAPSHOT: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: scripts/w.sh, runner: sh, expected_duration: 10 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// A clean run, a run with a retry, and a run that loops back into `work`.
fn three_runs() -> Vec<StatsRun> {
    use serde_json::json;
    vec![
        run(
            "clean",
            "1.0.0",
            Some(SNAPSHOT),
            &[
                (1_000, started("work")),
                (6_000, finished("work", "succeeded", 1)),
                (6_000, edge("work", "done")),
                (7_000, end("succeeded")),
            ],
        ),
        run(
            "retry",
            "1.0.0",
            Some(SNAPSHOT),
            &[
                (1_000, started("work")),
                (
                    2_000,
                    json!({"type": "retry_started", "node": "work", "attempt": 2}),
                ),
                (21_000, finished("work", "succeeded", 2)),
                (
                    22_000,
                    json!({"type": "review_requested", "node": "gate", "options": ["approve"]}),
                ),
                (
                    82_000,
                    json!({"type": "review_decided", "node": "gate", "decision": "approve", "note": ""}),
                ),
                (83_000, end("succeeded")),
            ],
        ),
        run(
            "loop",
            "1.0.0",
            Some(SNAPSHOT),
            &[
                (1_000, started("work")),
                (2_000, finished("work", "failed", 1)),
                (2_000, edge("work", "work")),
                (3_000, started("work")),
                (4_000, finished("work", "succeeded", 1)),
                // An unbounded edge into a node that ran is not a loop
                // traversal (no budget is spent on it).
                (
                    4_000,
                    serde_json::json!({"type": "edge_traversed", "from": "work", "to": "start", "uncounted": true}),
                ),
                (5_000, end("failed")),
            ],
        ),
    ]
}

#[test]
fn first_pass_counts_only_runs_with_no_retry_fallback_or_loop() {
    let r = build(&three_runs(), &StatsFilter::default());
    assert_eq!(r.runs, 3);
    let v = &r.versions[0];
    assert_eq!(v.success, Rate::new(2, 3));
    assert_eq!(v.success.text(), "2/3 (67%)");
    assert_eq!(v.first_pass.text(), "1/3 (33%)");
    assert_eq!(v.retries.total, 1);
    assert_eq!(v.loop_traversals.total, 1);
    assert_eq!(v.loop_traversals.per_run, Some(0.33));
    let work = v.nodes.iter().find(|n| n.node == "work").unwrap();
    assert_eq!(work.runs, 3);
    assert_eq!(work.first_pass.text(), "1/3 (33%)");
    assert_eq!((work.retries, work.reentries), (1, 1));
    // 5 s, 20 s, 1 s and 1 s against the declared 10 s.
    assert_eq!(work.expected_s, Some(10));
    assert_eq!(work.over_expected.unwrap().text(), "1/4 (25%)");
    assert_eq!(work.duration.median_ms, Some(1_000));
    assert!(v.note.as_deref().unwrap().contains("indicative"));
}

#[test]
fn gate_wait_pairs_each_request_with_its_decision() {
    let r = build(&three_runs(), &StatsFilter::default());
    let v = &r.versions[0];
    assert_eq!(v.gate_wait.count, 1);
    assert_eq!(v.gate_wait.median_ms, Some(60_000));
    assert_eq!(v.question_wait.count, 0);
}

#[test]
fn goal_results_are_counted_per_criterion() {
    use serde_json::json;
    let goal = |status: &str| json!({"type": "goal_checked", "index": 0, "description": "tests pass", "check": "script", "status": status});
    let manual = json!({"type": "goal_checked", "index": 1, "description": "a person reads it", "check": "manual", "status": "manual"});
    let runs = vec![
        run(
            "a",
            "1.0.0",
            None,
            &[
                (2_000, goal("passed")),
                (2_000, manual.clone()),
                (3_000, end("succeeded")),
            ],
        ),
        run(
            "b",
            "1.0.0",
            None,
            &[
                (2_000, goal("failed")),
                (2_000, manual),
                (3_000, end("failed")),
            ],
        ),
        run("c", "1.0.0", None, &[(3_000, end("succeeded"))]),
    ];
    let v = &build(&runs, &StatsFilter::default()).versions[0];
    assert_eq!(v.goal.len(), 2);
    assert_eq!(v.goal[0].passed.text(), "1/2 (50%)");
    assert_eq!(v.goal[0].failed, 1);
    assert_eq!(v.goal[1].manual, 2);
    assert_eq!(v.goal[1].passed.of, 0);
    // A version whose runs checked no goal has none.
    let plain = build(&three_runs(), &StatsFilter::default());
    assert!(plain.versions[0].goal.is_empty());
}

#[test]
fn compare_puts_the_base_against_the_latest_other_version() {
    let mut runs = three_runs();
    runs.push(run(
        "new",
        "1.10.0",
        None,
        &[
            (1_000, started("work")),
            (2_000, finished("work", "succeeded", 1)),
            (3_000, end("succeeded")),
        ],
    ));
    runs.push(run("other", "1.2.0", None, &[(3_000, end("failed"))]));
    let filter = StatsFilter {
        playbook: Some("p".into()),
        compare: Some("1.0.0".into()),
        ..Default::default()
    };
    let r = build(&runs, &filter);
    // Versions sort numerically, so 1.10.0 is the latest.
    let order: Vec<&str> = r.versions.iter().map(|v| v.version.as_str()).collect();
    assert_eq!(order, ["1.0.0", "1.2.0", "1.10.0"]);
    let c = r.compare.unwrap();
    assert_eq!(c.against.as_deref(), Some("1.10.0"));
    assert_eq!(c.success_delta, Some(0.33));
    assert_eq!(c.first_pass_delta, Some(0.67));
    let text = render_text(&build(&runs, &filter));
    assert!(text.contains("compare p 1.0.0 -> 1.10.0"), "{text}");
    assert!(text.contains("success: +33 points"), "{text}");
}

#[test]
fn filters_and_the_empty_report() {
    let r = build(
        &three_runs(),
        &StatsFilter {
            playbook: Some("other".into()),
            ..Default::default()
        },
    );
    assert_eq!(r.runs, 0);
    assert_eq!(r.note.as_deref(), Some(NO_RUNS));
    assert_eq!(render_text(&r), "no runs recorded\n");
    let later = build(
        &three_runs(),
        &StatsFilter {
            since_ms: Some(2_000),
            ..Default::default()
        },
    );
    assert_eq!(later.runs, 0);
}

#[test]
fn spend_sums_the_usage_the_attempts_reported() {
    use serde_json::json;
    let usage = |cost: Option<f64>| {
        json!({"type": "attempt_finished", "node": "work", "attempt": 1, "status": "succeeded",
               "usage": {"input_tokens": 100, "output_tokens": 50, "cache_read_tokens": 0,
                         "cache_write_tokens": 0, "cost_usd": cost, "source": "reported"}})
    };
    let runs = vec![
        run(
            "a",
            "1.0.0",
            None,
            &[(2_000, usage(Some(0.02))), (3_000, end("succeeded"))],
        ),
        run(
            "b",
            "1.0.0",
            None,
            &[(2_000, usage(None)), (3_000, end("succeeded"))],
        ),
        run("c", "1.0.0", None, &[(3_000, end("succeeded"))]),
    ];
    let v = &build(&runs, &StatsFilter::default()).versions[0];
    assert_eq!(v.spend.runs_with_usage, 2);
    assert_eq!(v.spend.tokens.total, 300);
    assert_eq!(v.spend.tokens.per_run, Some(150.0));
    assert_eq!(v.spend.runs_with_cost, 1);
    assert_eq!(v.spend.cost_per_run_usd, Some(0.02));
    let text = render_text(&build(&runs, &StatsFilter::default()));
    assert!(text.contains("reported by 1 of 3 runs"), "{text}");
}

#[test]
fn the_json_shape_is_stable() {
    let r = build(&three_runs()[..1], &StatsFilter::default());
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["runs"], 1);
    assert_eq!(
        v["versions"][0]["success"],
        serde_json::json!({"count": 1, "of": 1, "rate": 1.0})
    );
    assert_eq!(
        v["versions"][0]["retries"],
        serde_json::json!({"total": 0, "runs": 1, "per_run": 0.0})
    );
    assert!(v.get("compare").is_none());
    assert!(v.get("note").is_none());
}

#[test]
fn a_host_execution_fallback_counts_as_a_fallback() {
    use serde_json::json;
    let runs = vec![run(
        "fb",
        "1.0.0",
        Some(SNAPSHOT),
        &[
            (1_000, started("work")),
            (
                1_000,
                json!({"type": "execution_fallback", "node": "work", "attempt": 2, "reason": "spawn failed"}),
            ),
            (3_000, finished("work", "succeeded", 1)),
            (4_000, end("succeeded")),
        ],
    )];
    let r = build(&runs, &StatsFilter::default());
    let v = &r.versions[0];
    assert_eq!(v.fallbacks.total, 1);
    assert_eq!(v.first_pass.text(), "0/1 (0%)");
    let work = v.nodes.iter().find(|n| n.node == "work").unwrap();
    assert_eq!(work.fallbacks, 1);
}

#[test]
fn a_withdrawn_review_request_is_not_paired_with_the_next_decision() {
    use serde_json::json;
    let rr = || json!({"type": "review_requested", "node": "g", "options": ["approve"]});
    let rd = || json!({"type": "review_decided", "node": "g", "decision": "approve", "note": ""});
    let runs = vec![run(
        "w",
        "1.0.0",
        None,
        &[
            (1_000, rr()),
            (
                1_010,
                json!({"type": "review_withdrawn", "node": "g", "reason": "node_retry"}),
            ),
            (1_100, rr()),
            (1_160, rd()),
            (1_500, rr()),
            (1_520, rd()),
            (1_600, end("succeeded")),
        ],
    )];
    let v = &build(&runs, &StatsFilter::default()).versions[0];
    // Waits of 60 ms and 20 ms; the withdrawn request records none (paired
    // with the first decision it would read 160 ms).
    assert_eq!(v.gate_wait.count, 2);
    assert_eq!(v.gate_wait.max_ms, Some(60));
    assert_eq!(v.gate_wait.median_ms, Some(20));
}

#[test]
fn an_infrastructure_retry_without_retry_started_is_not_a_first_pass_run() {
    let runs = vec![run(
        "infra",
        "1.0.0",
        None,
        &[
            (1_000, started("w")),
            (2_000, finished("w", "succeeded", 2)),
            (3_000, end("succeeded")),
        ],
    )];
    let v = &build(&runs, &StatsFilter::default()).versions[0];
    assert_eq!(v.first_pass.text(), "0/1 (0%)");
    let w = v.nodes.iter().find(|n| n.node == "w").unwrap();
    assert_eq!(w.first_pass.text(), "0/1 (0%)");
}

#[test]
fn compare_against_a_version_without_runs_says_so() {
    let filter = StatsFilter {
        playbook: Some("p".into()),
        compare: Some("9.9.9".into()),
        ..Default::default()
    };
    let text = render_text(&build(&three_runs(), &filter));
    assert!(text.contains("compare p 9.9.9: no runs of 9.9.9"), "{text}");
}

/// Issue #200 (AC4): a run paused for 2 h mid-node reports its run and node
/// durations without those 2 h, and the node stays within its estimate.
#[test]
fn a_two_hour_pause_is_left_out_of_run_and_node_durations() {
    use serde_json::json;
    let hour: u128 = 3_600_000;
    let runs = vec![run(
        "paused",
        "1.0.0",
        Some(SNAPSHOT),
        &[
            (1_000, started("work")),
            (
                3_000,
                json!({"type": "run_paused", "reason": "lunch", "by": "operator"}),
            ),
            (
                3_000 + 2 * hour,
                json!({"type": "run_resumed", "from_node": "work", "by": "operator"}),
            ),
            (9_000 + 2 * hour, finished("work", "succeeded", 1)),
            (9_000 + 2 * hour, edge("work", "done")),
            (10_000 + 2 * hour, end("succeeded")),
        ],
    )];
    let r = build(&runs, &StatsFilter::default());
    let v = &r.versions[0];
    // 1 000 -> 10 000 + 2 h, minus the 2 h pause.
    assert_eq!(v.duration.median_ms, Some(9_000));
    let work = v.nodes.iter().find(|n| n.node == "work").unwrap();
    assert_eq!(work.duration.median_ms, Some(8_000));
    assert_eq!(work.over_expected.unwrap().text(), "0/1 (0%)");
}

fn notes(
    node: &str,
    attempt: u32,
    findings: serde_json::Value,
    warnings: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({"type": "attempt_notes", "node": node, "attempt": attempt,
                       "findings": findings, "warnings": warnings})
}

fn finding(kind: &str, location: &str) -> serde_json::Value {
    serde_json::json!({"kind": kind, "location": location, "evidence": "seen", "why_not_fixed": "scope"})
}

#[test]
fn findings_group_by_kind_and_location_across_runs() {
    use serde_json::json;
    let runs = vec![
        run(
            "a",
            "1.0.0",
            None,
            &[
                (
                    2_000,
                    notes("work", 1, json!([finding("bug", "src/a.rs:10")]), json!([])),
                ),
                // A retry reporting the same finding again counts once.
                (
                    2_500,
                    notes("work", 2, json!([finding("bug", "src/a.rs:10")]), json!([])),
                ),
                (3_000, end("succeeded")),
            ],
        ),
        run(
            "b",
            "1.0.0",
            None,
            &[
                (
                    2_000,
                    notes(
                        "work",
                        1,
                        json!([
                            finding("bug", "src/a.rs:44:2"),
                            finding("docs", "README.md")
                        ]),
                        json!([{"subject": "deploy", "reason": "not checked"}]),
                    ),
                ),
                (3_000, end("succeeded")),
            ],
        ),
    ];
    let v = &build(&runs, &StatsFilter::default()).versions[0];
    let bug = v.findings.by_kind.iter().find(|k| k.kind == "bug").unwrap();
    assert_eq!((bug.occurrences, bug.runs), (2, 2));
    let top = &v.findings.top[0];
    assert_eq!(
        (
            top.kind.as_str(),
            top.location.as_str(),
            top.runs,
            top.last_run.as_str()
        ),
        ("bug", "src/a.rs", 2, "b")
    );
    assert_eq!(v.findings.runs_with_warnings.text(), "1/2 (50%)");
    let text = render_text(&build(&runs, &StatsFilter::default()));
    assert!(text.contains("bug at src/a.rs: 2 runs, last b"), "{text}");
    assert!(
        text.contains("findings: bug: 2 findings in 2 runs"),
        "{text}"
    );
}

/// The last run is the one that started last, whatever order the runs
/// arrive in (several project roots list theirs one after another).
#[test]
fn the_last_run_of_a_finding_is_the_one_that_started_last() {
    use serde_json::json;
    let reported = || notes("work", 1, json!([finding("bug", "src/a.rs")]), json!([]));
    let mut newer = run("newer", "1.0.0", None, &[(9_000, reported())]);
    newer.events[0].ts = 8_000;
    let older = run("older", "1.0.0", None, &[(2_000, reported())]);
    let v = &build(&[newer, older], &StatsFilter::default()).versions[0];
    assert_eq!(v.findings.top[0].last_run, "newer");
}

#[test]
fn a_hidden_character_in_a_reported_location_never_reaches_the_terminal() {
    use serde_json::json;
    let runs = vec![run(
        "a",
        "1.0.0",
        None,
        &[(
            2_000,
            notes(
                "work",
                1,
                json!([finding("bug", "src/a.rs\u{202E}\u{1b}[2J")]),
                json!([]),
            ),
        )],
    )];
    let text = render_text(&build(&runs, &StatsFilter::default()));
    assert!(!text.contains('\u{202E}') && !text.contains('\u{1b}'));
    assert!(text.contains("bug at src/a.rs??[2J"));
}

#[test]
fn a_hidden_character_in_a_baseline_name_never_reaches_the_terminal() {
    use serde_json::json;
    let (check, observation) = ("deals\u{1b}[2J", "count\u{202E}");
    let runs = vec![run(
        "a",
        "1.0.0",
        None,
        &[
            (
                1_900,
                json!({"type": "baseline_observed", "check": check, "phase": "after",
                       "status": "observed", "input_digest": "d"}),
            ),
            (
                2_000,
                json!({"type": "baseline_compared", "check": check, "observation": observation,
                       "rule": "equals 0", "verdict": "met", "regressed": false}),
            ),
            (3_000, end("succeeded")),
        ],
    )];
    let text = render_text(&build(&runs, &StatsFilter::default()));
    assert!(!text.contains('\u{202E}') && !text.contains('\u{1b}'));
    assert!(text.contains("baseline deals?[2J.count?: met"), "{text}");
}

#[test]
fn cost_known_counts_attempts_that_reported_a_cost() {
    use serde_json::json;
    let attempt = |usage: serde_json::Value| json!({"type": "attempt_finished", "node": "work", "attempt": 1, "status": "succeeded", "usage": usage});
    let reported = |cost: Option<f64>| {
        json!({"input_tokens": 1, "output_tokens": 1, "cache_read_tokens": 0,
               "cache_write_tokens": 0, "cost_usd": cost, "source": "reported"})
    };
    let runs = vec![
        run(
            "a",
            "1.0.0",
            None,
            &[
                (2_000, attempt(reported(Some(0.01)))),
                (2_100, attempt(reported(None))),
                (2_200, attempt(serde_json::Value::Null)),
                (3_000, end("succeeded")),
            ],
        ),
        run(
            "b",
            "1.0.0",
            None,
            &[
                (2_000, attempt(serde_json::Value::Null)),
                (3_000, end("succeeded")),
            ],
        ),
    ];
    let v = &build(&runs, &StatsFilter::default()).versions[0];
    assert_eq!((v.spend.cost_known.count, v.spend.cost_known.of), (1, 4));
}

#[test]
fn baseline_observations_count_met_regressed_and_unknown() {
    use serde_json::json;
    let compared = |verdict: &str, regressed: Option<bool>| {
        json!({"type": "baseline_compared", "check": "deals", "observation": "count",
               "rule": "equals 0", "verdict": verdict, "regressed": regressed})
    };
    let after = json!({"type": "baseline_observed", "check": "deals", "phase": "after",
                       "status": "observed", "input_digest": "d"});
    let runs = vec![
        run(
            "a",
            "1.0.0",
            None,
            &[
                (1_900, after.clone()),
                (2_000, compared("met", Some(false))),
                (3_000, end("succeeded")),
            ],
        ),
        // The current comparison of a run is the one that counts: a node
        // that started after one makes it stale, and a new after snapshot
        // starts over.
        run(
            "b",
            "1.0.0",
            None,
            &[
                (1_900, after.clone()),
                (2_000, compared("met", Some(false))),
                (2_100, started("work")),
                (2_400, after.clone()),
                (2_500, compared("not_met", Some(true))),
                (3_000, end("failed")),
            ],
        ),
        run(
            "c",
            "1.0.0",
            None,
            &[
                (1_900, after.clone()),
                (2_000, compared("unknown", None)),
                (3_000, end("succeeded")),
            ],
        ),
        // A comparison a later node made stale counts nowhere.
        run(
            "d",
            "1.0.0",
            None,
            &[
                (1_900, after),
                (2_000, compared("not_met", Some(true))),
                (2_100, started("work")),
                (3_000, end("failed")),
            ],
        ),
    ];
    let v = &build(&runs, &StatsFilter::default()).versions[0];
    let b = &v.baseline[0];
    assert_eq!(
        (b.compared, b.met.count, b.regressed, b.unknown),
        (3, 1, 1, 1)
    );
    let text = render_text(&build(&runs, &StatsFilter::default()));
    assert!(
        text.contains("baseline deals.count: met 1/3 (33%), regressed in 1, unknown in 1"),
        "{text}"
    );
}

fn bind(mut r: StatsRun, profile: &str) -> StatsRun {
    r.node_profiles.insert(
        "work".into(),
        crate::manifest::NodeDigests {
            profile: format!("{profile}0123456789abcdef"),
            bundle: format!("b{profile}0123456789abcdef"),
        },
    );
    r
}

fn attempt_with(tokens: u64) -> serde_json::Value {
    serde_json::json!({"type": "attempt_finished", "node": "work", "attempt": 1, "status": "succeeded", "usage": {"input_tokens": tokens, "output_tokens": 0, "source": "reported"}})
}

fn one_visit(id: &str, version: &str, tokens: u64) -> StatsRun {
    run(
        id,
        version,
        Some(SNAPSHOT),
        &[
            (1_000, started("work")),
            (2_000, attempt_with(tokens)),
            (3_000, finished("work", "succeeded", 1)),
            (4_000, end("succeeded")),
        ],
    )
}

#[test]
fn a_profile_change_within_a_version_splits_the_node_by_digest() {
    let runs = vec![
        bind(one_visit("r1", "1.0.0", 100), "aaa"),
        bind(one_visit("r2", "1.0.0", 300), "aaa"),
        bind(one_visit("r3", "1.0.0", 900), "bbb"),
    ];
    let report = build(&runs, &StatsFilter::default());
    let work = report.versions[0]
        .nodes
        .iter()
        .find(|n| n.node == "work")
        .unwrap();
    let groups: Vec<_> = work
        .digests
        .iter()
        .map(|d| (d.profile.as_str(), d.runs, d.tokens_per_visit))
        .collect();
    assert_eq!(
        groups,
        vec![
            ("aaa012345678", 2, Some(100)),
            ("bbb012345678", 1, Some(900))
        ]
    );
    let text = render_text(&report);
    assert!(
        text.contains("    profile aaa012345678 (bundle baaa01234567): 2 runs, first pass 2/2")
    );
    // One executor throughout: no groups.
    let same = vec![bind(one_visit("r1", "1.0.0", 1), "aaa")];
    let report = build(&same, &StatsFilter::default());
    assert!(
        report.versions[0]
            .nodes
            .iter()
            .all(|n| n.digests.is_empty())
    );
}

#[test]
fn compare_names_the_nodes_whose_latest_executor_changed() {
    let runs = vec![
        bind(one_visit("r1", "1.0.0", 1), "aaa"),
        bind(one_visit("r2", "1.1.0", 1), "bbb"),
    ];
    let filter = StatsFilter {
        playbook: Some("p".into()),
        compare: Some("1.0.0".into()),
        ..StatsFilter::default()
    };
    let report = build(&runs, &filter);
    let c = report.compare.as_ref().unwrap();
    assert_eq!(c.executor_changes.len(), 1);
    assert_eq!(
        (
            c.executor_changes[0].node.as_str(),
            c.executor_changes[0].against_profile.as_str()
        ),
        ("work", "bbb012345678")
    );
    assert!(
        render_text(&report).contains("  executor of work: profile aaa012345678 -> bbb012345678")
    );
}

/// A grade of `run` by `grader` giving `criterion` (under `digest`) `level`.
fn grade(
    run: &str,
    grader: &str,
    at: u64,
    rows: &[(&str, &str, &str)],
) -> apb_core::quality::grade::Grade {
    let criteria: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, digest, level)| {
            serde_json::json!({"id": id, "source": "builtin", "digest": digest, "subject": "run",
                "level": level, "reason_code": "r", "why": "w", "counts_for_promotion": true})
        })
        .collect();
    serde_json::from_value(serde_json::json!({
        "schema": 1, "grade_id": format!("{run}-{at}"), "run_id": run, "playbook": "p",
        "version": "1.0.0", "task_type": "default", "grader": {"kind": grader, "version": 1}, "created_at": at,
        "criteria": criteria, "reading": {"budget_chars": 1, "read_chars": 1}
    }))
    .unwrap()
}

fn graded(id: &str, version: &str, grades: Vec<apb_core::quality::grade::Grade>) -> StatsRun {
    let mut r = run(id, version, None, &[(2_000, end("succeeded"))]);
    r.grades = grades;
    r
}

#[test]
fn quality_rows_count_the_latest_grade_of_each_grader_per_run() {
    let runs = vec![
        // The later rules grade of run a replaces its earlier one.
        graded(
            "a",
            "1.0.0",
            vec![
                grade("a", "rules", 1, &[("loops", "d1", "poor")]),
                grade(
                    "a",
                    "rules",
                    2,
                    &[("loops", "d1", "good"), ("scope", "d1", "partial")],
                ),
            ],
        ),
        graded(
            "b",
            "1.0.0",
            vec![grade("b", "rules", 1, &[("loops", "d1", "poor")])],
        ),
        graded(
            "c",
            "1.0.0",
            vec![grade(
                "c",
                "rules",
                1,
                &[("loops", "d1", "not_enough_data")],
            )],
        ),
        graded("d", "1.0.0", Vec::new()),
    ];
    let r = build(&runs, &StatsFilter::default());
    let q = &r.versions[0].quality;
    let rows: Vec<(&str, usize, String, String)> = q
        .iter()
        .map(|x| {
            (
                x.criterion.as_str(),
                x.graded_runs,
                x.good.text(),
                x.poor.text(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("loops", 3, "1/2 (50%)".to_string(), "1/2 (50%)".to_string()),
            ("scope", 1, "0/1 (0%)".to_string(), "0/1 (0%)".to_string()),
        ]
    );
    assert_eq!(q[0].tally.not_enough_data, 1);
    assert_eq!(q[0].grader, "rules/1");
    let text = render_text(&r);
    assert!(text.contains("  quality loops (rules/1, default): good 1/2 (50%), poor 1/2 (50%) over 3 graded runs\n"), "{text}");
}

#[test]
fn quality_deltas_need_the_same_definition_on_both_sides() {
    let runs = vec![
        graded(
            "a",
            "1.0.0",
            vec![grade(
                "a",
                "rules",
                1,
                &[("loops", "d1", "poor"), ("scope", "d1", "good")],
            )],
        ),
        graded(
            "b",
            "1.1.0",
            vec![grade(
                "b",
                "rules",
                1,
                &[("loops", "d1", "good"), ("scope", "d2", "good")],
            )],
        ),
    ];
    let r = build(
        &runs,
        &StatsFilter {
            playbook: Some("p".into()),
            compare: Some("1.0.0".into()),
            ..Default::default()
        },
    );
    let c = r.compare.as_ref().unwrap();
    assert_eq!(c.quality.len(), 1);
    assert_eq!(c.quality[0].criterion, "loops");
    assert_eq!(c.quality[0].digest, "d1");
    assert_eq!(c.quality[0].good_delta, Some(1.0));
    assert_eq!(c.quality[0].poor_delta, Some(-1.0));
    assert!(
        render_text(&r)
            .contains("  quality loops (rules/1, default): good +100 points, poor -100 points\n")
    );
}

#[test]
fn no_grade_means_no_quality_field() {
    let r = build(&three_runs(), &StatsFilter::default());
    assert!(r.versions.iter().all(|v| v.quality.is_empty()));
    let json = serde_json::to_value(&r).unwrap();
    assert!(json["versions"][0].get("quality").is_none());
    assert!(!render_text(&r).contains("quality"));
}
