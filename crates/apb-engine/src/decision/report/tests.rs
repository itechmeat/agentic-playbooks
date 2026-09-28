//! Fixture journals with hand-computed figures. Each run is `a -> b`: the
//! completion check judged `a`, and `b`'s outcome labels it.

use super::*;
use crate::event::{DecisionAnswer, DecisionBaseline};

struct Fixture {
    /// `final_result` p.
    p: f64,
    /// `b` failed (`Some(true)`), succeeded (`Some(false)`), or the run
    /// paused before it finished (`None`).
    b_failed: Option<bool>,
    regex_flag: bool,
    output_chars: u64,
}

fn decision_event(seq: u64, p: Option<f64>, regex_flag: bool, output_chars: u64) -> Event {
    let answered = p.is_some();
    Event {
        seq,
        ts: 1_000 + u128::from(seq),
        payload: EventPayload::DecisionMade {
            enforce_refused: None,
            join: BTreeMap::new(),
            use_site: "completion_check".into(),
            node: Some("a".into()),
            attempt: Some(1),
            provider: Some("main".into()),
            model: answered.then(|| "jev-1.13.0".to_string()),
            calibrated: answered,
            mode: "shadow".into(),
            questions_digest: "sha256:q".into(),
            state_digest: "sha256:s".into(),
            state_bytes: 100,
            output_chars: Some(output_chars),
            answers: p
                .map(|p| {
                    BTreeMap::from([(
                        "final_result".to_string(),
                        DecisionAnswer {
                            p: Some(p),
                            ..Default::default()
                        },
                    )])
                })
                .unwrap_or_default(),
            applied: false,
            would_change: p.map(|p| p < 0.15),
            baseline: Some(DecisionBaseline {
                regex_flag,
                pattern: None,
            }),
            latency_ms: if answered { 200 } else { 3000 },
            input_tokens: None,
            cost_usd: answered.then_some(0.0001),
            cost_estimated: false,
            cached: false,
            error: (!answered).then(|| "timeout".to_string()),
        },
    }
}

fn ev(seq: u64, payload: EventPayload) -> Event {
    Event {
        seq,
        ts: 1_000 + u128::from(seq),
        payload,
    }
}

fn attempt(seq: u64, node: &str, ms: u64) -> Event {
    ev(
        seq,
        EventPayload::AttemptFinished {
            node: node.into(),
            attempt: 1,
            status: "succeeded".into(),
            duration_ms: Some(ms),
            session: None,
            summary: None,
            rejected_output: None,
            partial_output: None,
            failure_kind: None,
            usage: None,
        },
    )
}

fn node(seq: u64, name: &str, finished: Option<&str>) -> Event {
    ev(
        seq,
        match finished {
            None => EventPayload::NodeStarted {
                node: name.into(),
                attempt: 1,
            },
            Some(status) => EventPayload::NodeFinished {
                node: name.into(),
                status: status.into(),
                attempt: 1,
                output: String::new(),
                artifacts: Vec::new(),
            },
        },
    )
}

fn run(id: &str, f: &Fixture) -> RunJournal {
    let mut events = vec![
        ev(
            0,
            EventPayload::RunStarted {
                playbook: "pb".into(),
                version: "1.0.0".into(),
            },
        ),
        node(1, "a", None),
        attempt(2, "a", 60_000),
        decision_event(3, Some(f.p), f.regex_flag, f.output_chars),
        node(4, "a", Some("succeeded")),
        node(5, "b", None),
    ];
    match f.b_failed {
        Some(failed) => {
            events.push(attempt(6, "b", 120_000));
            events.push(node(
                7,
                "b",
                Some(if failed { "failed" } else { "succeeded" }),
            ));
        }
        None => events.push(ev(
            6,
            EventPayload::RunPaused {
                reason: "stop".into(),
            },
        )),
    }
    RunJournal {
        run_id: id.into(),
        playbook: "pb".into(),
        events,
        provider_kinds: BTreeMap::new(),
    }
}

/// The error run: the provider timed out on `a`.
fn error_run() -> RunJournal {
    RunJournal {
        run_id: "r7".into(),
        playbook: "pb".into(),
        events: vec![
            node(1, "a", None),
            attempt(2, "a", 60_000),
            decision_event(3, None, false, 10),
            node(4, "a", Some("succeeded")),
        ],
        provider_kinds: BTreeMap::new(),
    }
}

/// Five labelled decisions, one unlabelled, one error. At the default 0.15
/// the check flags r1 (right) and r2 (wrong) and misses r4.
fn fixture() -> Vec<RunJournal> {
    let f = |p, b_failed, regex_flag, output_chars| Fixture {
        p,
        b_failed,
        regex_flag,
        output_chars,
    };
    vec![
        run("r1", &f(0.05, Some(true), true, 1500)),
        run("r2", &f(0.10, Some(false), false, 2000)),
        run("r3", &f(0.90, Some(false), false, 300)),
        run("r4", &f(0.60, Some(true), true, 1200)),
        run("r5", &f(0.95, Some(false), true, 100)),
        run("r6", &f(0.20, None, false, 100)),
        error_run(),
    ]
}

fn one_group(r: &DecisionsReport) -> &GroupReport {
    assert_eq!(r.groups.len(), 1, "{:#?}", r.groups);
    &r.groups[0]
}

#[test]
fn the_figures_match_the_hand_computed_values() {
    let r = build(
        &fixture(),
        &ReportFilter::default(),
        &ReportSettings::default(),
    );
    assert_eq!((r.runs, r.decisions), (7, 7));
    // Attempts: seven of `a` at 60 s, five of `b` at 120 s: lower middle 60 s.
    assert_eq!(r.median_attempt_ms, Some(60_000));
    let g = one_group(&r);
    assert_eq!(
        (g.use_site.as_str(), g.provider.as_str(), g.model.as_str()),
        ("completion_check", "main", "jev-1.13.0")
    );
    // The timeout joins its provider's group as an error.
    assert_eq!(
        (g.decisions, g.errors, g.answered, g.labelled),
        (7, 1, 6, 5)
    );
    assert_eq!(g.label_coverage, Some(0.8333));
    assert_eq!((g.act_labels, g.keep_labels), (2, 3));
    assert_eq!(g.unlabelled.get("no downstream outcome yet"), Some(&1));
    assert_eq!(
        (g.threshold, g.threshold_source.as_str()),
        (0.15, "configured")
    );
    // Flags at 0.15: r1 right, r2 wrong; r3, r5 kept right; r4 missed.
    assert_eq!(g.all.accuracy, Some(0.6));
    assert_eq!(g.all.recall, Some(0.5));
    assert_eq!(
        (g.all.majority_label.as_str(), g.all.majority_accuracy),
        ("keep", Some(0.6))
    );
    assert_eq!(g.all.today_behaviour, "always complete");
    assert_eq!(g.all.today_accuracy, Some(0.6));
    // Regex: right on r1..r4, wrong on r5.
    assert_eq!((g.all.regex.items, g.all.regex.accuracy), (5, Some(0.8)));
    assert_eq!(
        (
            g.all.false_actions.count,
            g.all.false_actions.of,
            g.all.false_actions.rate
        ),
        (1, 3, Some(0.3333))
    );
    // Act probabilities 0.95, 0.9, 0.1, 0.4, 0.05 against 1, 0, 0, 1, 0:
    // (0.0025 + 0.81 + 0.01 + 0.36 + 0.0025) / 5 = 0.237.
    assert_eq!(g.brier, Some(0.237));
    // Bin 9 holds 0.95 and 0.9 (mean 0.925, rate 0.5): 0.425 x 2/5 = 0.17;
    // 0.4 (bin 4, rate 1): 0.6 x 1/5 = 0.12; 0.1 and 0.05 (rate 0): 0.03.
    assert_eq!(g.ece, Some(0.32));
    assert_eq!(
        g.would_change.as_ref().map(|w| (w.items, w.accuracy)),
        Some((5, Some(0.6)))
    );
    // Long outputs: r1, r2, r4. Flags r1 right, r2 wrong, misses r4.
    assert_eq!(g.long_outputs.labelled, 3);
    assert_eq!(g.long_outputs.accuracy, Some(0.3333));
    assert_eq!(g.long_outputs.regex.accuracy, Some(1.0));
    assert_eq!(g.output_length_unknown, 0);
}

#[test]
fn the_threshold_table_steps_by_five_hundredths() {
    let r = build(
        &fixture(),
        &ReportFilter::default(),
        &ReportSettings::default(),
    );
    let g = one_group(&r);
    assert_eq!(g.table.len(), 19);
    assert_eq!(g.table[0].threshold, 0.05);
    assert_eq!(g.table[18].threshold, 0.95);
    let row = |t: f64| g.table.iter().find(|r| r.threshold == t).unwrap();
    // 0.05: nothing flagged (p < 0.05 is none).
    assert_eq!(
        (row(0.05).coverage, row(0.05).recall),
        (Some(0.0), Some(0.0))
    );
    // 0.10: r1 only, right.
    let r10 = row(0.10);
    assert_eq!(
        (r10.coverage, r10.accuracy, r10.false_action_rate),
        (Some(0.2), Some(0.8), Some(0.0))
    );
    assert_eq!(r10.false_action_ci95, Some((0.0, 0.5615)));
    // 0.65: r1, r2, r4 flagged; one false action of three.
    let r65 = row(0.65);
    assert_eq!(
        (r65.accuracy, r65.recall, r65.false_action_rate),
        (Some(0.8), Some(1.0), Some(0.3333))
    );
    // 0.95: r1..r4 flagged, r5 (p = 0.95) not.
    assert_eq!(row(0.95).false_action_rate, Some(0.6667));
    // Under 5 %: only 0.10 catches anything.
    assert_eq!(g.suggested_threshold, Some(0.1));
    let s = g.savings.as_ref().unwrap();
    assert_eq!(
        (s.avoidable_attempts, s.saved_ms, s.false_actions, s.lost_ms),
        (1, 60_000, 1, 60_000)
    );
    assert_eq!(s.decision_latency_ms, 6 * 200 + 3000);
    assert_eq!(s.decision_cost_usd, 0.0006);
    assert!(!g.eligible);
    assert!(
        g.eligibility
            .iter()
            .any(|e| e.starts_with("no stored threshold"))
    );
    assert!(g.eligibility.iter().any(|e| e == "5 labelled, needs 50"));
}

#[test]
fn unlabelled_and_unanswered_decisions_stay_out_of_the_figures() {
    let only_open = vec![
        run(
            "r6",
            &Fixture {
                p: 0.2,
                b_failed: None,
                regex_flag: false,
                output_chars: 10,
            },
        ),
        error_run(),
    ];
    let r = build(
        &only_open,
        &ReportFilter::default(),
        &ReportSettings::default(),
    );
    let g = one_group(&r);
    assert_eq!((g.decisions, g.answered, g.labelled), (2, 1, 0));
    assert_eq!(g.all.accuracy, None);
    assert_eq!(g.brier, None);
    assert!(g.savings.is_none());
    assert!(!g.eligible);
    let text = render_text(&r);
    assert!(
        text.contains("unlabelled: 1 (no downstream outcome yet)"),
        "{text}"
    );
    assert!(text.contains("eligible for enforce: no"), "{text}");
}

#[test]
fn nothing_recorded_says_so() {
    let r = build(&[], &ReportFilter::default(), &ReportSettings::default());
    assert_eq!(r.note.as_deref(), Some(NO_DECISIONS));
    assert_eq!(render_text(&r), "no decisions recorded\n");
    // A filter that matches nothing reads the same.
    let f = ReportFilter {
        use_site: Some("retry_advice".into()),
        ..Default::default()
    };
    assert_eq!(
        build(&fixture(), &f, &ReportSettings::default())
            .note
            .as_deref(),
        Some(NO_DECISIONS)
    );
}

fn stored(model: &str, t: f64) -> apb_core::decision_thresholds::StoredThreshold {
    apb_core::decision_thresholds::StoredThreshold {
        use_name: "completion_check".into(),
        provider: "main".into(),
        model: model.into(),
        threshold: t,
        set_at_ms: 0,
    }
}

/// Sixty decisions: ten false successes at p = 0.05, fifty finished
/// replies at p = 0.9.
fn eligible_fixture() -> Vec<RunJournal> {
    (0..60)
        .map(|i| {
            let act = i < 10;
            run(
                &format!("e{i:02}"),
                &Fixture {
                    p: if act { 0.05 } else { 0.9 },
                    b_failed: Some(act),
                    regex_flag: false,
                    output_chars: 100,
                },
            )
        })
        .collect()
}

#[test]
fn a_stored_threshold_for_the_exact_model_makes_a_measured_use_eligible() {
    let settings = ReportSettings {
        stored: vec![stored("jev-1.13.0", 0.15)],
        ..Default::default()
    };
    let r = build(&eligible_fixture(), &ReportFilter::default(), &settings);
    let g = one_group(&r);
    assert_eq!((g.threshold, g.threshold_source.as_str()), (0.15, "stored"));
    assert_eq!(g.all.accuracy, Some(1.0));
    assert!(g.eligible, "{:?}", g.eligibility);
    assert!(render_text(&r).contains("eligible for enforce: yes (all promotion rules hold)"));

    // The same numbers with the threshold stored for another model id: no
    // inheritance, and the report says a new shadow period is needed.
    let other = ReportSettings {
        stored: vec![stored("jev-1.12.0", 0.15)],
        ..Default::default()
    };
    let r = build(&eligible_fixture(), &ReportFilter::default(), &other);
    let g = one_group(&r);
    assert_eq!(g.threshold_source, "configured");
    assert!(!g.eligible);
    assert!(
        g.notes
            .iter()
            .any(|n| n.contains("does not carry over") && n.contains("new shadow period"))
    );

    // A stricter target than the measured false-action rate fails it.
    let strict = ReportSettings {
        stored: vec![stored("jev-1.13.0", 0.95)],
        ..Default::default()
    };
    let g = build(&eligible_fixture(), &ReportFilter::default(), &strict)
        .groups
        .remove(0);
    assert!(!g.eligible);
    assert!(
        g.eligibility
            .iter()
            .any(|e| e.starts_with("false-action rate 100.0%"))
    );
}

#[test]
fn emulation_is_its_own_group_and_filters_apply() {
    let mut journals = fixture();
    let mut emu = run(
        "x1",
        &Fixture {
            p: 0.05,
            b_failed: Some(true),
            regex_flag: false,
            output_chars: 10,
        },
    );
    emu.provider_kinds
        .insert("main".into(), EMULATION_KIND.into());
    emu.playbook = "other".into();
    for e in &mut emu.events {
        if let EventPayload::RunStarted { playbook, .. } = &mut e.payload {
            *playbook = "other".into();
        }
    }
    journals.push(emu);
    let r = build(
        &journals,
        &ReportFilter::default(),
        &ReportSettings::default(),
    );
    assert_eq!(r.groups.len(), 2);
    assert!(!r.groups[0].emulation && r.groups[1].emulation);
    assert_eq!(r.groups[1].decisions, 1);
    assert!(render_text(&r).contains("emulation (uncalibrated, reported on its own)"));
    let only = ReportFilter {
        playbook: Some("other".into()),
        ..Default::default()
    };
    assert_eq!(
        build(&journals, &only, &ReportSettings::default()).decisions,
        1
    );
    let since = ReportFilter {
        since_ms: Some(10_000),
        ..Default::default()
    };
    assert_eq!(
        build(&journals, &since, &ReportSettings::default()).decisions,
        0
    );
    let provider = ReportFilter {
        provider: Some("nope".into()),
        ..Default::default()
    };
    assert_eq!(
        build(&journals, &provider, &ReportSettings::default()).decisions,
        0
    );
}

#[test]
fn since_takes_a_date_or_a_duration() {
    let now = 1_790_000_000_000_u128;
    assert_eq!(parse_since("2026-09-20", now), Some(1_789_862_400_000));
    assert_eq!(parse_since("1970-01-02", now), Some(86_400_000));
    assert_eq!(parse_since("7d", now), Some(now - 7 * 86_400_000));
    assert_eq!(parse_since("24h", now), Some(now - 86_400_000));
    assert_eq!(parse_since("30", now), None);
    assert_eq!(parse_since("2026-13-01", now), None);
    assert_eq!(parse_since("soon", now), None);
}

/// The `--json` shape, pinned. Regenerate with
/// `APB_WRITE_SNAPSHOTS=1 cargo test -p apb-engine --lib decision::report`.
#[test]
fn the_json_report_matches_its_snapshot() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/decisions-report.json"
    );
    let r = build(
        &fixture(),
        &ReportFilter::default(),
        &ReportSettings::default(),
    );
    let rendered = serde_json::to_string_pretty(&r).unwrap() + "\n";
    if std::env::var_os("APB_WRITE_SNAPSHOTS").is_some() {
        std::fs::write(path, &rendered).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        committed == rendered,
        "{path} is stale; regenerate with APB_WRITE_SNAPSHOTS=1\n{rendered}"
    );
}

#[test]
fn a_judge_use_is_never_asked_for_a_stored_threshold() {
    let mut journals = eligible_fixture();
    for j in &mut journals {
        for e in &mut j.events {
            if let EventPayload::DecisionMade { use_site, .. } = &mut e.payload {
                *use_site = "judge_node".into();
            }
        }
    }
    let r = build(
        &journals,
        &ReportFilter::default(),
        &ReportSettings::default(),
    );
    let g = one_group(&r);
    assert!(
        !g.eligibility.iter().any(|e| e.contains("stored threshold")),
        "{:?}",
        g.eligibility
    );
    assert!(
        apb_core::decision_thresholds::set_threshold_in(
            tempfile::tempdir().unwrap().path(),
            "judge_node",
            "p",
            "m",
            0.5
        )
        .is_err()
    );
}
