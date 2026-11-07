//! `prove_fails_first` (spec node-contracts D4): V173 refuses a proof that
//! cannot mean anything, and a `skip` that would claim an output the node
//! did not produce.

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(work: &str, edges: &str) -> String {
    format!(
        "schema: 2\nid: pv\nname: PV\nversion: 1.0.0\ndefaults: {{ profile: x }}\nnodes:\n  - {{ id: start, type: start }}\n{work}
  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n{edges}
  - {{ from: w, to: done }}\n"
    )
}

fn found(yaml: &str, code: &str) -> Vec<Severity> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == code)
        .map(|i| i.severity)
        .collect()
}

#[test]
fn v173_refuses_on_green_before_without_prove() {
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, success_check: { script: scripts/x.sh, scope: [\"src/**\"], on_green_before: skip } }",
        "",
    );
    assert_eq!(found(&yaml, "V173"), vec![Severity::Error]);
}

#[test]
fn v173_refuses_a_bare_script_map_with_neither_scope_nor_prove() {
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, success_check: { script: scripts/x.sh } }",
        "",
    );
    assert_eq!(found(&yaml, "V173"), vec![Severity::Error]);
}

#[test]
fn v173_refuses_a_proof_on_a_node_with_a_cache() {
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, cache: auto, success_check: { script: scripts/x.sh, prove_fails_first: true } }",
        "",
    );
    assert_eq!(found(&yaml, "V173"), vec![Severity::Error]);
}

#[test]
fn v173_refuses_skip_on_a_node_whose_output_somebody_reads() {
    // Declared fields.
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, outputs: { fields: [verdict] }, success_check: { script: scripts/x.sh, prove_fails_first: true, on_green_before: skip } }",
        "",
    );
    assert_eq!(found(&yaml, "V173"), vec![Severity::Error]);
    // Declared results.
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, results: [{ path: out.md }], success_check: { script: scripts/x.sh, prove_fails_first: true, on_green_before: skip } }",
        "",
    );
    assert_eq!(found(&yaml, "V173"), vec![Severity::Error]);
    // An edge condition reading the node's output.
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, success_check: { script: scripts/x.sh, prove_fails_first: true, on_green_before: skip } }",
        "  - { from: w, to: done, condition: { type: output_match, node: w, pattern: x } }",
    );
    assert_eq!(found(&yaml, "V173"), vec![Severity::Error]);
}

#[test]
fn v173_stays_quiet_on_a_valid_proof() {
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, success_check: { script: scripts/x.sh, prove_fails_first: true } }",
        "",
    );
    assert!(found(&yaml, "V173").is_empty());
    // A scoped check without a proof keeps V163's shape: no V173.
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, success_check: { script: scripts/x.sh, scope: [\"src/**\"] } }",
        "",
    );
    assert!(found(&yaml, "V173").is_empty());
    // `fail` with declared fields is fine: the node fails, it claims nothing.
    let yaml = playbook(
        "  - { id: w, type: agent_task, prompt: hi, outputs: { fields: [verdict] }, success_check: { script: scripts/x.sh, prove_fails_first: true, on_green_before: fail } }",
        "",
    );
    assert!(found(&yaml, "V173").is_empty());
}

/// The proof runs before a plain agent_task's first attempt only: V173
/// refuses it on a list node and on a script node, where the engine would
/// skip it in silence.
#[test]
fn v173_refuses_a_proof_on_a_list_or_a_script_node() {
    for work in [
        "  - { id: w, type: agent_task, prompt: hi, list: { items: { inline: [{}] } }, success_check: { script: scripts/x.sh, prove_fails_first: true } }",
        "  - { id: w, type: script, script: scripts/w.sh, runner: sh, success_check: { script: scripts/x.sh, prove_fails_first: true } }",
    ] {
        assert!(
            found(&playbook(work, ""), "V173").contains(&Severity::Error),
            "{work}"
        );
    }
}
