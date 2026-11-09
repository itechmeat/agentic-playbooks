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
        definitions: snapshot.map(known).unwrap_or_default(),
        diagnostics: Vec::new(),
        origin: crate::manifest::RunOrigin::Production,
        learns: true,
        identities: Vec::new(),
        declared: BTreeMap::new(),
        ephemeral_nodes: Default::default(),
    }
}

/// The definitions of a run that executed `yaml` from its start, known.
fn known(yaml: &str) -> crate::run_definitions::DefinitionsInForce {
    use crate::run_definitions::*;
    DefinitionsInForce {
        segments: vec![Segment {
            from_seq: 0,
            to_seq: None,
            boundary: Some(Boundary {
                seq: 0,
                definition: Some(digest(yaml.as_bytes())),
                kind: BoundaryKind::Start,
            }),
            resolved: Resolved::Known {
                playbook: Box::new(apb_core::schema::Playbook::from_yaml(yaml).unwrap()),
                digest: digest(yaml.as_bytes()),
            },
            mismatch_recorded: false,
        }],
        playbook_changed: false,
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

/// A run that failed after 5 s, sat failed for a day, was resumed and
/// succeeded 3 s later took 8 s: the idle day is not run time, the same as
/// in the run listing and the retro.
#[test]
fn the_day_a_run_sat_failed_before_a_resume_is_left_out() {
    use serde_json::json;
    let day: u128 = 24 * 3_600_000;
    let runs = vec![run(
        "resumed",
        "1.0.0",
        Some(SNAPSHOT),
        &[
            (6_000, end("failed")),
            (
                6_000 + day,
                json!({"type": "run_resumed", "from_node": "work"}),
            ),
            (9_000 + day, end("succeeded")),
        ],
    )];
    let r = build(&runs, &StatsFilter::default());
    assert_eq!(r.versions[0].duration.median_ms, Some(8_000));
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
            ..Default::default()
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

// --- executions per node (identity-based) ----------------------------------

fn node_of<'a>(r: &'a StatsReport, node: &str) -> &'a NodeStats {
    r.versions[0]
        .nodes
        .iter()
        .find(|n| n.node == node)
        .unwrap_or_else(|| panic!("no row for {node}"))
}

#[test]
fn a_running_node_is_one_open_execution() {
    let r = build(
        &[run(
            "r1",
            "1.0.0",
            Some(SNAPSHOT),
            &[(1_100, started("work"))],
        )],
        &StatsFilter::default(),
    );
    let w = node_of(&r, "work");
    assert_eq!((w.runs, w.executions, w.completed, w.open), (1, 1, 0, 1));
    assert_eq!(w.reentries, 0);
}

#[test]
fn a_quota_paused_first_execution_has_no_reentry() {
    use serde_json::json;
    // Paused by its quota, resumed, started again and finished: one
    // execution, though the node started twice.
    let r = build(
        &[run(
            "r1",
            "1.0.0",
            Some(SNAPSHOT),
            &[
                (1_100, started("work")),
                (
                    1_200,
                    json!({"type": "attempt_started", "node": "work", "attempt": 1, "agent": "a"}),
                ),
                (
                    1_300,
                    json!({"type": "attempt_finished", "node": "work", "attempt": 1, "status": "interrupted", "duration_ms": 1}),
                ),
                (1_400, json!({"type": "run_paused", "reason": "quota"})),
                (1_500, json!({"type": "run_resumed", "from_node": "work"})),
                (1_600, started("work")),
                (1_700, finished("work", "succeeded", 1)),
            ],
        )],
        &StatsFilter::default(),
    );
    let w = node_of(&r, "work");
    assert_eq!(
        (w.runs, w.executions, w.completed, w.reentries),
        (1, 1, 1, 0)
    );
}

#[test]
fn reentries_are_executions_less_runs_across_runs() {
    let r = build(
        &[
            run(
                "r1",
                "1.0.0",
                Some(SNAPSHOT),
                &[
                    (1_100, started("work")),
                    (1_200, finished("work", "failed", 1)),
                    (1_300, edge("work", "work")),
                    (1_400, started("work")),
                    (1_500, finished("work", "succeeded", 1)),
                ],
            ),
            run(
                "r2",
                "1.0.0",
                Some(SNAPSHOT),
                &[
                    (2_100, started("work")),
                    (2_200, finished("work", "succeeded", 1)),
                ],
            ),
        ],
        &StatsFilter::default(),
    );
    let w = node_of(&r, "work");
    assert_eq!((w.runs, w.executions, w.completed), (2, 3, 3));
    assert_eq!(w.reentries, 1);
}

#[test]
fn a_finished_execution_and_an_open_one_are_one_reentry() {
    let r = build(
        &[run(
            "r1",
            "1.0.0",
            Some(SNAPSHOT),
            &[
                (1_100, started("work")),
                (1_200, finished("work", "failed", 1)),
                (1_300, edge("work", "work")),
                (1_400, started("work")),
            ],
        )],
        &StatsFilter::default(),
    );
    let w = node_of(&r, "work");
    assert_eq!((w.executions, w.completed, w.open), (2, 1, 1));
    assert_eq!((w.failed, w.reentries), (1, 1));
}

#[test]
fn a_race_loser_is_counted_apart() {
    let r = build(
        &[run(
            "r1",
            "1.0.0",
            Some(SNAPSHOT),
            &[
                (1_100, started("work")),
                (1_100, finished("work", "cancelled", 1)),
            ],
        )],
        &StatsFilter::default(),
    );
    let w = node_of(&r, "work");
    assert_eq!((w.runs, w.executions, w.cancelled), (0, 0, 1));
}

#[test]
fn a_finish_without_a_prompt_counts_and_unexecuted_nodes_have_zero_rows() {
    let r = build(
        &[run(
            "r1",
            "1.0.0",
            Some(SNAPSHOT),
            &[
                (1_100, started("start")),
                (1_200, finished("start", "succeeded", 1)),
                (1_300, edge("start", "done")),
                (1_400, finished("done", "succeeded", 1)),
                (1_500, end("succeeded")),
            ],
        )],
        &StatsFilter::default(),
    );
    let done = node_of(&r, "done");
    assert_eq!((done.runs, done.executions, done.completed), (1, 1, 1));
    let work = node_of(&r, "work");
    assert_eq!((work.in_runs, work.runs, work.executions), (1, 0, 0));
    let text = render::render_text(&r);
    assert!(
        text.contains("node work: not executed in 1 of 1 runs"),
        "{text}"
    );
}

#[test]
fn unknown_membership_shows_for_a_node_that_also_executed() {
    let executed = run(
        "r1",
        "1.0.0",
        Some(SNAPSHOT),
        &[
            (1_100, started("work")),
            (1_200, finished("work", "succeeded", 1)),
            (1_300, end("succeeded")),
        ],
    );
    // An older run: its snapshot names `work`, no definition is known.
    let mut old = run("r2", "1.0.0", Some(SNAPSHOT), &[(1_400, end("succeeded"))]);
    old.definitions = Default::default();
    let r = build(&[executed, old], &StatsFilter::default());
    let w = node_of(&r, "work");
    assert_eq!((w.runs, w.in_runs, w.in_runs_unknown), (1, 1, 1));
    let text = render::render_text(&r);
    assert!(
        text.lines()
            .any(|l| l.contains("node work: executed") && l.contains("membership unknown in 1")),
        "{text}"
    );
}

/// A scenario run, an eval run and a run of an origin this apb does not
/// know are never history: only the production run is counted, so none of
/// them can move a success rate, a median or a quality comparison.
#[test]
fn only_production_runs_count_in_the_statistics() {
    use crate::manifest::RunOrigin;
    let with = |id: &str, tokens: u64, origin: RunOrigin| {
        let mut r = bind(one_visit(id, "1.0.0", tokens), "aaa");
        r.learns = origin.is_production();
        r.origin = origin;
        r
    };
    let runs = vec![
        with("prod", 100, RunOrigin::Production),
        with(
            "scenario",
            9_000,
            RunOrigin::Scenario {
                level: apb_core::scenario::ScenarioLevel::Quick,
                scenario: "green".into(),
                scenario_digest: "sha256:s".into(),
            },
        ),
        with("eval", 9_000, RunOrigin::Eval),
        with("newer", 9_000, RunOrigin::Unknown),
    ];
    let report = build(&runs, &StatsFilter::default());
    assert_eq!(report.versions.len(), 1);
    let v = &report.versions[0];
    assert_eq!(v.runs, 1, "only the production run counts");
    assert_eq!(v.spend.runs_with_usage, 1);
    assert_eq!(
        v.spend,
        build(&runs[..1], &StatsFilter::default()).versions[0].spend
    );
}

// --- the executor report (PR 21a part B) ---

fn attempt_started(attempt: u32, exec: Option<serde_json::Value>) -> serde_json::Value {
    let mut v = serde_json::json!({"type": "attempt_started", "node": "work", "attempt": attempt, "agent": "claude", "model": "haiku"});
    if let Some(x) = exec {
        v["exec"] = x;
    }
    v
}

fn attempt_done(attempt: u32, status: &str, ms: u64, cost: Option<f64>) -> serde_json::Value {
    let mut v = serde_json::json!({"type": "attempt_finished", "node": "work", "attempt": attempt, "status": status, "duration_ms": ms});
    if let Some(c) = cost {
        v["usage"] = serde_json::json!({"input_tokens": 10, "output_tokens": 0, "cost_usd": c, "source": "reported"});
    }
    v
}

fn exec(
    step: u32,
    source: serde_json::Value,
    entered: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut v = serde_json::json!({"binding": "sha256:b1b1b1b1b1b1b1b1", "step": step, "source": source, "executed_by": "cli"});
    if let Some(e) = entered {
        v["entered"] = e;
    }
    v
}

/// A run's identities as `StatsRun::load` reads them, every attempt bound
/// to the executor digests given.
fn with_identities(mut r: StatsRun, executor: &str, ephemeral: bool) -> StatsRun {
    r.identities = crate::execution_identity::fold::fold(&r.events);
    for id in &mut r.identities {
        id.profile_digest = Some("sha256:pppppppppppppppp".into());
        id.bundle_digest = Some("sha256:bbbbbbbbbbbbbbbb".into());
        id.executor_digest = Some(executor.into());
        id.ephemeral = ephemeral;
    }
    r
}

fn group_of(r: &StatsReport) -> &[executors::ExecutorGroup] {
    &node_of(r, "work").executors
}

/// Card A1 and design 2.9: step rows per chain position, the close reason
/// of the step left and the transition between them, a declared step that
/// never ran listed with no attempt, cost summed per step.
#[test]
fn the_executor_report_splits_steps_and_names_transition_reasons() {
    let entered =
        serde_json::json!({"from_step": 0, "closed_attempt": 2, "reason": "check_rejected"});
    let r = run(
        "r1",
        "1.0.0",
        None,
        &[
            (1_000, started("work")),
            (
                1_000,
                attempt_started(
                    1,
                    Some(exec(0, serde_json::json!({"kind": "primary"}), None)),
                ),
            ),
            (2_000, attempt_done(1, "failed", 1_000, Some(0.5))),
            (
                2_000,
                serde_json::json!({"type": "fallback_triggered", "node": "work", "from": "claude", "to": "codex", "reason": "check_rejected", "executed_by": "cli"}),
            ),
            (
                2_000,
                attempt_started(
                    1,
                    Some(exec(
                        1,
                        serde_json::json!({"kind": "fallback", "index": 1, "of": 2}),
                        Some(entered),
                    )),
                ),
            ),
            (5_000, attempt_done(1, "succeeded", 3_000, Some(0.25))),
            (5_000, finished("work", "succeeded", 1)),
            (6_000, end("succeeded")),
        ],
    );
    let mut r = with_identities(r, "x1:eeeeeeeeeeeeeeee", false);
    r.declared.insert(
        "x1:eeeeeeeeeeeeeeee".into(),
        vec!["primary".into(), "fallback".into(), "fallback".into()],
    );
    let report = build(&[r], &StatsFilter::default());
    let groups = group_of(&report);
    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(
        (g.executor.as_str(), g.runs, g.attempts),
        ("eeeeeeeeeeee", 1, 2)
    );
    let steps: Vec<_> = g
        .steps
        .iter()
        .map(|s| {
            (
                s.step,
                s.source.as_str(),
                s.attempts,
                s.successes.count,
                s.cost_usd,
            )
        })
        .collect();
    assert_eq!(
        steps,
        vec![
            (Some(0), "primary", 1, 0, Some(0.5)),
            (Some(1), "fallback", 1, 1, Some(0.25)),
            (Some(2), "fallback", 0, 0, None),
        ]
    );
    assert_eq!(g.steps[0].closes_by_reason.get("check_rejected"), Some(&1));
    assert_eq!(g.steps[1].median_attempt_ms, Some(3_000));
    assert_eq!(g.steps[0].reported_models.get("none"), Some(&1));
    assert_eq!(
        g.transitions,
        vec![executors::TransitionStats {
            from_step: Some(0),
            to_step: Some(1),
            reason: "check_rejected".into(),
            count: 1
        }]
    );
    let text = render_text(&report);
    assert!(text.contains("executor eeeeeeeeeeee"), "{text}");
    assert!(text.contains("step 2 fallback: never ran"), "{text}");
    assert!(text.contains("0 -> 1 check_rejected x1"), "{text}");
}

/// The drive journals `fallback_triggered` after the fallback attempt ran
/// (its batch write), and the attempt it entered already carries the
/// reason: one transition, `0 -> 1`, counted once; and a full `x1:`
/// executor digest shows as its first hex characters, its prefixes left
/// out.
#[test]
fn a_transition_written_after_the_attempt_it_entered_counts_once() {
    let entered =
        serde_json::json!({"from_step": 0, "closed_attempt": 2, "reason": "check_rejected"});
    let r = run(
        "r1",
        "1.0.0",
        None,
        &[
            (1_000, started("work")),
            (
                1_000,
                attempt_started(
                    1,
                    Some(exec(0, serde_json::json!({"kind": "primary"}), None)),
                ),
            ),
            (2_000, attempt_done(1, "failed", 1_000, None)),
            (
                2_000,
                attempt_started(
                    2,
                    Some(exec(
                        1,
                        serde_json::json!({"kind": "fallback", "index": 1, "of": 1}),
                        Some(entered),
                    )),
                ),
            ),
            (3_000, attempt_done(2, "succeeded", 1_000, None)),
            (
                3_000,
                serde_json::json!({"type": "fallback_triggered", "node": "work", "from": "claude", "to": "claude", "reason": "check_rejected", "executed_by": "cli"}),
            ),
            (3_000, finished("work", "succeeded", 2)),
            (4_000, end("succeeded")),
        ],
    );
    let r = with_identities(r, "x1:sha256:0123456789abcdef0123456789abcdef", false);
    let report = build(&[r], &StatsFilter::default());
    let g = &group_of(&report)[0];
    assert_eq!(
        g.transitions,
        vec![executors::TransitionStats {
            from_step: Some(0),
            to_step: Some(1),
            reason: "check_rejected".into(),
            count: 1
        }]
    );
    assert_eq!(g.steps[0].closes_by_reason.get("check_rejected"), Some(&1));
    assert!(g.steps[1].closes_by_reason.is_empty(), "{:?}", g.steps[1]);
    assert_eq!(g.executor, "0123456789ab");
}

/// Plan S5: a step's cost splits by basis like the version's spend, so a
/// subscription equivalent nobody is billed never reads as money billed.
#[test]
fn a_step_cost_splits_by_basis() {
    let with_basis = |attempt: u32, cost: f64, basis: &str| {
        let mut v = attempt_done(attempt, "failed", 1_000, Some(cost));
        v["usage"]["cost_basis"] = serde_json::json!(basis);
        v
    };
    let primary = || Some(exec(0, serde_json::json!({"kind": "primary"}), None));
    let r = run(
        "r1",
        "1.0.0",
        None,
        &[
            (1_000, started("work")),
            (1_000, attempt_started(1, primary())),
            (2_000, with_basis(1, 0.5, "billed")),
            (2_000, attempt_started(2, primary())),
            (3_000, with_basis(2, 0.25, "subscription_equivalent")),
            (3_000, attempt_started(3, primary())),
            (4_000, attempt_done(3, "succeeded", 1_000, Some(0.125))),
            (4_000, finished("work", "succeeded", 3)),
            (5_000, end("succeeded")),
        ],
    );
    let r = with_identities(r, "x1:eeeeeeeeeeeeeeee", false);
    let report = build(&[r], &StatsFilter::default());
    let step = &group_of(&report)[0].steps[0];
    assert_eq!(step.cost_usd, Some(0.875));
    assert_eq!(
        step.cost_by_basis,
        crate::run_view::CostByBasis {
            billed_usd: Some(0.5),
            subscription_equivalent_usd: Some(0.25),
            unknown_basis_usd: Some(0.125),
        }
    );
}

/// Card A9: an older journal (no `exec`, a fallback without a reason)
/// keeps one row whose position is not recorded and a transition that
/// reads `not_recorded`; the cascade's `routing` stays its own reason.
#[test]
fn an_old_journal_reads_not_recorded_and_is_never_reclassified() {
    let r = run(
        "r1",
        "1.0.0",
        None,
        &[
            (1_000, started("work")),
            (1_000, attempt_started(1, None)),
            (2_000, attempt_done(1, "failed", 1_000, None)),
            (
                2_000,
                serde_json::json!({"type": "fallback_triggered", "node": "work", "from": "claude", "to": "codex"}),
            ),
            (2_000, attempt_started(1, None)),
            (3_000, attempt_done(1, "failed", 1_000, None)),
            (
                3_000,
                serde_json::json!({"type": "fallback_triggered", "node": "work", "from": "codex", "to": "opencode", "reason": "routing"}),
            ),
            (3_000, attempt_started(1, None)),
            (4_000, attempt_done(1, "succeeded", 1_000, None)),
            (4_000, finished("work", "succeeded", 1)),
            (5_000, end("succeeded")),
        ],
    );
    let mut r = with_identities(r, "x1:eeeeeeeeeeeeeeee", false);
    // The manifest declares three steps; which ones ran is not recorded, so
    // none of them is listed as never run.
    r.declared.insert(
        "x1:eeeeeeeeeeeeeeee".into(),
        vec!["tier".into(), "primary".into(), "fallback".into()],
    );
    let report = build(&[r], &StatsFilter::default());
    let g = &group_of(&report)[0];
    assert_eq!(g.steps.len(), 1, "{:?}", g.steps);
    assert_eq!(
        (
            g.steps[0].step,
            g.steps[0].source.as_str(),
            g.steps[0].attempts
        ),
        (None, executors::NOT_RECORDED, 3)
    );
    let reasons: Vec<_> = g
        .transitions
        .iter()
        .map(|t| (t.reason.as_str(), t.count))
        .collect();
    assert_eq!(reasons, vec![("not_recorded", 1), ("routing", 1)]);
    assert!(
        g.transitions
            .iter()
            .all(|t| t.from_step.is_none() && t.to_step.is_none())
    );
}

/// Card A6: an override whose chain equals the base is another executor:
/// the node's digest groups and the comparison name the executor.
#[test]
fn an_override_equal_to_the_base_is_its_own_executor_group() {
    let digests = |executor: &str| crate::manifest::NodeDigests {
        profile: "sha256:pppppppppppppppp".into(),
        bundle: "sha256:bbbbbbbbbbbbbbbb".into(),
        executor: executor.into(),
    };
    let mut base = one_visit("r1", "1.0.0", 100);
    base.node_profiles
        .insert("work".into(), digests("x1:aaaaaaaaaaaaaaaa"));
    let mut over = one_visit("r2", "1.0.0", 300);
    over.node_profiles
        .insert("work".into(), digests("x1:cccccccccccccccc"));
    over.ephemeral_nodes.insert("work".into());
    let mut next = one_visit("r3", "1.0.1", 300);
    next.node_profiles
        .insert("work".into(), digests("x1:cccccccccccccccc"));
    let runs = vec![base.clone(), over.clone(), next];
    let report = build(&runs, &StatsFilter::default());
    let groups: Vec<_> = node_of(&report, "work")
        .digests
        .iter()
        .map(|d| (d.executor.as_str(), d.ephemeral, d.runs))
        .collect();
    assert_eq!(
        groups,
        vec![("aaaaaaaaaaaa", false, 1), ("cccccccccccc", true, 1)]
    );
    let report = build(
        &[base, runs[2].clone()],
        &StatsFilter {
            playbook: Some("p".into()),
            compare: Some("1.0.0".into()),
            ..Default::default()
        },
    );
    let c = &report.compare.unwrap().executor_changes[0];
    assert_eq!(
        (c.base_executor.as_str(), c.against_executor.as_str()),
        ("aaaaaaaaaaaa", "cccccccccccc")
    );
    assert_eq!(c.base_profile, c.against_profile);
}

/// Design 2.9 item 5: of the hinted host attempts, a report naming the
/// hint is followed, another model is not, a task without a model is not
/// reported; never filled from the hint.
#[test]
fn hint_follow_counts_followed_not_followed_and_not_reported() {
    let host = |task: &str, attempt: u32, model: Option<&str>| {
        vec![
            serde_json::json!({"type": "host_task_requested", "task_id": task, "node": "work", "attempt": attempt, "model_hint": "sonnet"}),
            serde_json::json!({"type": "attempt_started", "node": "work", "attempt": attempt, "agent": "host"}),
            serde_json::json!({"type": "host_task_submitted", "task_id": task, "status": "failed", "submitted_by": "host", "model": model}),
            serde_json::json!({"type": "attempt_finished", "node": "work", "attempt": attempt, "status": "failed"}),
        ]
    };
    let mut body = vec![(1_000, started("work"))];
    for (i, m) in [Some("claude-sonnet-4-5"), Some("opus"), None]
        .iter()
        .enumerate()
    {
        for v in host(&format!("t{i}"), i as u32 + 1, *m) {
            body.push((1_000 + i as u128, v));
        }
    }
    body.push((9_000, end("failed")));
    let r = with_identities(
        run("r1", "1.0.0", None, &body),
        "x1:hhhhhhhhhhhhhhhh",
        false,
    );
    let report = build(&[r], &StatsFilter::default());
    let g = &group_of(&report)[0];
    assert_eq!(
        g.hint_follow,
        Some(executors::HintFollow {
            hinted: 3,
            followed: 1,
            not_followed: 1,
            not_reported: 1
        })
    );
    let models = &g.steps[0].reported_models;
    assert_eq!(models.get("one:claude-sonnet-4-5"), Some(&1));
    assert_eq!(models.get("none"), Some(&1));
}

/// A host group whose attempts carried no hint has no hint counts at all,
/// as `HintFollow::of_views` reads it: never an all-zero line.
#[test]
fn a_host_group_without_hints_has_no_hint_counts() {
    let mut body = vec![(1_000, started("work"))];
    for v in [
        serde_json::json!({"type": "host_task_requested", "task_id": "t0", "node": "work", "attempt": 1}),
        serde_json::json!({"type": "attempt_started", "node": "work", "attempt": 1, "agent": "host"}),
        serde_json::json!({"type": "host_task_submitted", "task_id": "t0", "status": "failed", "submitted_by": "host", "model": "opus"}),
        serde_json::json!({"type": "attempt_finished", "node": "work", "attempt": 1, "status": "failed"}),
    ] {
        body.push((1_000, v));
    }
    body.push((9_000, end("failed")));
    let r = with_identities(
        run("r1", "1.0.0", None, &body),
        "x1:hhhhhhhhhhhhhhhh",
        false,
    );
    let report = build(&[r], &StatsFilter::default());
    let g = &group_of(&report)[0];
    assert_eq!(g.hint_follow, None);
}

/// Design 2.8 item 6: a node criterion's levels split by executor when
/// its graded runs bound the node to more than one; the comparable key is
/// untouched.
#[test]
fn quality_rows_of_a_node_criterion_split_by_executor() {
    let node_grade = |run: &str, level: &str| {
        let mut g = grade(run, "rules", 1, &[("c", "d1", level)]);
        g.criteria[0].subject = "node:work".into();
        g
    };
    let mut runs = Vec::new();
    for (id, executor, level) in [
        ("r1", "x1:aaaa", "good"),
        ("r2", "x1:aaaa", "good"),
        ("r3", "x1:cccc", "poor"),
    ] {
        let mut r = graded(id, "1.0.0", vec![node_grade(id, level)]);
        r.node_profiles.insert(
            "work".into(),
            crate::manifest::NodeDigests {
                profile: "p".into(),
                bundle: "b".into(),
                executor: executor.into(),
            },
        );
        runs.push(r);
    }
    let report = build(&runs, &StatsFilter::default());
    let q = &report.versions[0].quality[0];
    assert_eq!(q.graded_runs, 3);
    let by: Vec<_> = q
        .by_executor
        .iter()
        .map(|e| {
            (
                e.executor.as_str(),
                e.good.count,
                e.poor.count,
                e.graded_runs,
            )
        })
        .collect();
    assert_eq!(by, vec![("aaaa", 2, 0, 2), ("cccc", 0, 1, 1)]);
}
