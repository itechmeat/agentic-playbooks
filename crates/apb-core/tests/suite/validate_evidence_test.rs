//! The `evidence:` rule of the validator (V159) and the nodes it makes
//! snapshotted.

use apb_core::schema::{ClaimsRequirement, OnInvalidClaims, Playbook};
use apb_core::validate::{Severity, ValidationContext, snapshotted_nodes, validate};

/// `start -> collect -> check -> read -> done`, with `gate` a review after
/// `read`; `read_extra` is appended inside `read`'s flow mapping.
fn playbook(read_extra: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: collect, type: script, script: s.sh, runner: sh }}\n  - {{ id: check, type: human_review }}\n  - {{ id: read, type: agent_task, prompt: do, profile: x{read_extra} }}\n  - {{ id: late, type: script, script: s.sh, runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: collect }}\n  - {{ from: collect, to: check }}\n  - {{ from: check, to: read }}\n  - {{ from: read, to: late }}\n  - {{ from: late, to: done }}\n"
    )
}

fn v159(yaml: &str) -> Vec<String> {
    let p = Playbook::from_yaml(yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == "V159")
        .inspect(|i| assert_eq!(i.severity, Severity::Error, "{}", i.message))
        .map(|i| i.message)
        .collect()
}

#[test]
fn a_reading_agent_task_with_defaults_is_valid_and_its_sources_are_snapshotted() {
    let yaml = playbook(", evidence: { from: [collect] }");
    assert!(v159(&yaml).is_empty());
    let p = Playbook::from_yaml(&yaml).unwrap();
    let ev = p.node("read").unwrap().evidence.clone().unwrap();
    assert_eq!(ev.claims, ClaimsRequirement::Optional);
    assert_eq!(ev.on_invalid, OnInvalidClaims::Mark);
    assert!(snapshotted_nodes(&p).contains("collect"));
    assert!(!snapshotted_nodes(&p).contains("read"));
}

#[test]
fn every_field_parses_and_an_unknown_one_is_refused() {
    let yaml = playbook(
        ", evidence: { from: [collect], kinds: [mail, output], claims: required, on_invalid: fail, budget_chars: 5000 }",
    );
    assert!(v159(&yaml).is_empty());
    let ev = Playbook::from_yaml(&yaml)
        .unwrap()
        .node("read")
        .unwrap()
        .evidence
        .clone()
        .unwrap();
    assert_eq!(ev.claims, ClaimsRequirement::Required);
    assert_eq!(ev.on_invalid, OnInvalidClaims::Fail);
    assert_eq!(ev.budget_chars, Some(5000));
    assert!(Playbook::from_yaml(&playbook(", evidence: { from: [collect], form: [x] }")).is_err());
}

#[test]
fn v159_refuses_what_a_session_cannot_serve() {
    let cases = [
        (", evidence: { from: [] }", "names no node"),
        (", evidence: { from: [ghost] }", "`ghost`, which is no node"),
        (", evidence: { from: [check] }", "settles no result"),
        (", evidence: { from: [late] }", "does not reach this node"),
        (
            ", evidence: { from: [collect], kinds: [Mail] }",
            "not a slug",
        ),
        (
            ", evidence: { from: [collect] }, isolation: { level: full, network: none }",
            "isolation",
        ),
        (
            ", evidence: { from: [collect] }, isolation: { level: best_effort, network: none }",
            "isolation",
        ),
    ];
    for (extra, want) in cases {
        let msgs = v159(&playbook(extra));
        assert_eq!(msgs.len(), 1, "{extra}: {msgs:?}");
        assert!(msgs[0].contains(want), "{extra}: {}", msgs[0]);
    }
    // Not an agent_task.
    let on_script = playbook("").replace(
        "{ id: late, type: script, script: s.sh, runner: sh }",
        "{ id: late, type: script, script: s.sh, runner: sh, evidence: { from: [collect] } }",
    );
    let msgs = v159(&on_script);
    assert!(
        msgs.len() == 1 && msgs[0].contains("applies to agent_task"),
        "{msgs:?}"
    );
}

/// A node may read its own earlier executions when a loop leads back to
/// it.
#[test]
fn a_node_in_a_loop_may_read_itself() {
    let yaml = playbook(", evidence: { from: [read] }")
        .replace("  - { from: late, to: done }\n", "  - { from: late, to: read, max_traversals: 2 }\n  - { from: late, to: done, fallback: true }\n");
    assert!(v159(&yaml).is_empty(), "{:?}", v159(&yaml));
}
