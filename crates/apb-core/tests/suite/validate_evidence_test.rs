//! The `evidence:` rule of the validator (V159), and the snapshot every
//! `evidence.from` node takes.

use apb_core::schema::Playbook;
use apb_core::validate::{
    GuardedFeature, Severity, ValidationContext, guarded_features, snapshotted_nodes, validate,
};

/// `start -> collect -> use -> done`, `use` looping back to itself on
/// `again`; `use_extra` is appended inside `use`'s flow mapping and
/// `collect_extra` inside `collect`'s.
fn playbook(collect_extra: &str, use_extra: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: collect, type: script, script: s.sh, runner: sh{collect_extra} }}\n  - {{ id: use, type: agent_task, prompt: do, profile: x{use_extra} }}\n  - {{ id: late, type: script, script: s.sh, runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: collect }}\n  - {{ from: collect, to: use }}\n  - {{ from: use, to: late }}\n  - {{ from: late, to: done }}\n"
    )
}

fn v159(yaml: &str) -> Vec<(Option<String>, String)> {
    let p = Playbook::from_yaml(yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == "V159")
        .inspect(|i| assert_eq!(i.severity, Severity::Error, "{}", i.message))
        .map(|i| (i.node.clone(), i.message))
        .collect()
}

#[test]
fn v159_accepts_a_source_that_runs_before_the_consumer() {
    let yaml = playbook(
        "",
        ", evidence: { from: [collect], kinds: [email, output], claims: required, on_invalid: fail, budget_chars: 5000 }",
    );
    assert!(v159(&yaml).is_empty(), "{:?}", v159(&yaml));
}

#[test]
fn v159_refuses_an_unknown_or_late_source_and_an_empty_from() {
    let issues = v159(&playbook("", ", evidence: { from: [nope, late] }"));
    assert_eq!(issues.len(), 2, "{issues:?}");
    assert!(issues[0].1.contains("`nope`, which is not a node"));
    assert!(
        issues[1]
            .1
            .contains("`late`, which never runs before `use`")
    );
    assert!(
        v159(&playbook("", ", evidence: { from: [] }"))[0]
            .1
            .contains("names no node")
    );
    // The consumer itself is a source only around a loop.
    assert_eq!(v159(&playbook("", ", evidence: { from: [use] }")).len(), 1);
}

#[test]
fn v159_refuses_kinds_that_are_not_slugs() {
    let issues = v159(&playbook(
        "",
        ", evidence: { from: [collect], kinds: [Email] }",
    ));
    assert_eq!(issues.len(), 1);
    assert!(issues[0].1.contains("`Email` is not a slug"));
}

#[test]
fn v159_refuses_evidence_off_an_agent_task_and_under_full_isolation() {
    let issues = v159(&playbook(", evidence: { from: [start] }", ""));
    assert_eq!(issues, vec![(Some("collect".into()), issues[0].1.clone())]);
    assert!(issues[0].1.contains("agent_task nodes only"));
    let issues = v159(&playbook(
        "",
        ", evidence: { from: [collect] }, isolation: { level: full, network: none }",
    ));
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].1.contains("isolation: full"));
}

#[test]
fn every_evidence_source_takes_a_snapshot_and_the_run_is_guarded() {
    let p = Playbook::from_yaml(&playbook("", ", evidence: { from: [collect] }")).unwrap();
    assert_eq!(
        snapshotted_nodes(&p).into_iter().collect::<Vec<_>>(),
        ["collect"]
    );
    assert_eq!(guarded_features(&p), vec![GuardedFeature::Evidence]);
    assert_eq!(
        GuardedFeature::parse("evidence"),
        Some(GuardedFeature::Evidence)
    );
}
