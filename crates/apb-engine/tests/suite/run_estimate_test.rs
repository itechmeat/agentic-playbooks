//! The run estimate over real history runs of a stub agent that reports 200
//! tokens per attempt: only runs of the same version and execution mode are
//! comparable, and a node bound to another profile than before loses only
//! its own samples.

use std::fs;

use apb_engine::run_estimate::{EstimateBasis, for_playbook, render_lines};

use crate::budget_gate_test::{LINE, project};

#[test]
fn other_version_and_other_execution_mode_are_not_comparable() {
    let p = project(LINE);
    p.history(3);
    let cli = for_playbook(p.root(), "bud", None, "cli").unwrap();
    assert_eq!((cli.comparable.runs, cli.comparable.succeeded), (3, 3));
    assert_eq!(cli.tokens.unwrap().spread.p50, 600);
    assert_eq!(cli.node("a").unwrap().per_visit.tokens.unwrap().p50, 200);
    assert!(cli.unestimated.is_empty());
    assert_eq!(cli.time.basis, EstimateBasis::History);
    assert!(render_lines(&cli)[0].starts_with("estimate (3 comparable runs of bud 1.0.0, cli"));
    // A host-mode run has no comparable history here: every node is
    // unestimated, and its time falls back to the declared durations.
    let host = for_playbook(p.root(), "bud", None, "host").unwrap();
    assert_eq!(host.comparable.runs, 0);
    assert_eq!(host.unestimated, vec!["a", "b", "c"]);
    assert!(host.tokens.is_none() && host.cost_usd.is_none());
    assert_eq!(host.time.basis, EstimateBasis::Declared);
    assert!(host.note.is_some());
    // Another version of the same playbook.
    let v2 = p.root().join(".apb/playbooks/bud/1.0.1");
    fs::create_dir_all(&v2).unwrap();
    fs::write(v2.join("playbook.yaml"), LINE.replace("1.0.0", "1.0.1")).unwrap();
    let other = for_playbook(p.root(), "bud", Some("1.0.1"), "cli").unwrap();
    assert_eq!(other.comparable.runs, 0);
}

#[test]
fn a_node_with_another_profile_digest_drops_only_that_node_and_counts_it() {
    let p = project(LINE);
    p.history(3);
    crate::common::seed_profile(p.root(), "other", "claude-code", "sonnet", &[]);
    fs::write(
        p.root().join(".apb/playbooks/bud/1.0.0/playbook.yaml"),
        LINE.replace("prompt: b }", "prompt: b, profile: other }"),
    )
    .unwrap();
    let e = for_playbook(p.root(), "bud", None, "cli").unwrap();
    assert_eq!(e.comparable.profile_mismatch.get("b"), Some(&3));
    assert_eq!(e.comparable.profile_mismatch.len(), 1);
    assert_eq!(e.unestimated, vec!["b"]);
    assert_eq!(e.tokens.unwrap().spread.p50, 400);
    assert!(
        render_lines(&e)
            .iter()
            .any(|l| l.contains("b (no history with this profile)"))
    );
}

#[test]
fn ask_is_decided_at_start_per_limited_dimension() {
    use apb_core::budget::BudgetRequest;
    use apb_engine::budget::start::check;
    use apb_engine::run_estimate::EstimateTarget;
    let p = project(LINE);
    // The agent reports tokens but no cost.
    let out = p.root().join("out.json");
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("total_cost_usd");
    fs::write(&out, v.to_string()).unwrap();
    p.history(3);
    let target = EstimateTarget::resolve(p.root(), "bud", None, "cli").unwrap();
    let roots = [p.root().to_path_buf()];
    let usd = check(
        &roots,
        &target,
        Some(&BudgetRequest {
            usd: Some(5.0),
            ..BudgetRequest::default()
        }),
    );
    assert!(usd.estimate.unestimated.is_empty());
    assert_eq!(usd.estimate.unestimated_cost, vec!["a", "b", "c"]);
    assert!(usd.needs_decision);
    assert_eq!(usd.undecided, vec!["a", "b", "c"]);
    let tokens = check(
        &roots,
        &target,
        Some(&BudgetRequest {
            tokens: Some(100_000),
            ..BudgetRequest::default()
        }),
    );
    assert!(!tokens.needs_decision && tokens.undecided.is_empty());
}

#[test]
fn the_history_scan_reads_the_newest_run_directories_first() {
    let p = project(LINE);
    p.history(2);
    let roots = [p.root().to_path_buf()];
    let all = apb_engine::decision::report::newest_run_dirs(&roots, 10);
    assert_eq!(all.len(), 2);
    let newest = apb_engine::decision::report::newest_run_dirs(&roots, 1);
    assert_eq!(newest, all[..1]);
    let modified = |d: &std::path::PathBuf| {
        fs::metadata(d.join("events.jsonl"))
            .unwrap()
            .modified()
            .unwrap()
    };
    assert!(modified(&all[0]) >= modified(&all[1]));
}

#[test]
fn planted_newer_run_directories_take_no_place_under_the_scan_cap() {
    let p = project(LINE);
    p.history(2);
    let roots = [p.root().to_path_buf()];
    let verified = apb_engine::decision::report::newest_run_dirs(&roots, 10);
    // Newer journals that apb did not create (no origin stamp).
    for id in ["planted-1", "planted-2", "planted-3"] {
        let d = p.root().join(".apb/runs").join(id);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("events.jsonl"), "").unwrap();
    }
    assert_eq!(
        apb_engine::decision::report::newest_run_dirs(&roots, 2),
        verified
    );
}
