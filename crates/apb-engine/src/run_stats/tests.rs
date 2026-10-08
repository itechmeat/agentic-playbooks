use super::*;

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
