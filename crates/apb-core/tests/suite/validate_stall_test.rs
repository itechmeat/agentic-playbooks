//! `stall_after` (spec node-contracts D5): it parses on a node and on
//! `defaults`, the node's own value wins, V174 refuses a value outside
//! 2 to 10 or on a node that cannot stall, and V175 names a `stalled`
//! edge that can never match.

use apb_core::schema::{Playbook, StatusEq};
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(defaults: &str, work: &str, edges: &str) -> String {
    format!(
        "schema: 2\nid: s\nname: S\nversion: 1.0.0\n{defaults}nodes:\n  - {{ id: start, type: start }}\n  - {work}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: bad, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: work }}\n{edges}"
    )
}

fn agent(extra: &str) -> String {
    format!("{{ id: work, type: agent_task, prompt: hi, profile: x{extra} }}")
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

fn stall_edges() -> &'static str {
    "  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }\n  - { from: work, to: bad, condition: { type: node_status, node: work, equals: failure } }\n"
}

#[test]
fn stall_after_parses_effective_stall_after_prefers_the_node() {
    let yaml = playbook(
        "defaults:\n  stall_after: 4\n",
        &agent(", stall_after: 6"),
        stall_edges(),
    );
    let p = Playbook::from_yaml(&yaml).unwrap();
    assert_eq!(p.effective_stall_after(p.node("work").unwrap()), Some(6));
    let again = Playbook::from_yaml(&yaml_serde::to_string(&p).unwrap()).unwrap();
    assert_eq!(
        again.effective_stall_after(again.node("work").unwrap()),
        Some(6)
    );
}

#[test]
fn effective_stall_after_falls_back_to_defaults_and_is_none_elsewhere() {
    let yaml = playbook("defaults:\n  stall_after: 4\n", &agent(""), stall_edges());
    let p = Playbook::from_yaml(&yaml).unwrap();
    assert_eq!(p.effective_stall_after(p.node("work").unwrap()), Some(4));
    // Without any value, and on a node that cannot stall: none.
    let yaml = playbook("", &agent(""), stall_edges());
    let p = Playbook::from_yaml(&yaml).unwrap();
    assert_eq!(p.effective_stall_after(p.node("work").unwrap()), None);
    assert_eq!(p.effective_stall_after(p.node("start").unwrap()), None);
    // A list node never stalls.
    let yaml = playbook(
        "defaults:\n  stall_after: 4\n",
        "{ id: work, type: agent_task, prompt: hi, profile: x, list: { items: { inline: [{}] } } }",
        stall_edges(),
    );
    let p = Playbook::from_yaml(&yaml).unwrap();
    assert_eq!(p.effective_stall_after(p.node("work").unwrap()), None);
}

#[test]
fn a_stalled_condition_label_round_trips() {
    let yaml = playbook(
        "defaults:\n  stall_after: 3\n",
        &agent(", stall_after: 3"),
        "  - { from: work, to: done, condition: { type: node_status, node: work, equals: stalled } }\n  - { from: work, to: bad, condition: { type: node_status, node: work, equals: failure } }\n",
    );
    let p = Playbook::from_yaml(&yaml).unwrap();
    let e = &p.edges[1];
    assert!(matches!(
        e.condition,
        Some(apb_core::schema::EdgeCondition::NodeStatus {
            equals: StatusEq::Stalled,
            ..
        })
    ));
    let again = Playbook::from_yaml(&yaml_serde::to_string(&p).unwrap()).unwrap();
    assert!(matches!(
        again.edges[1].condition,
        Some(apb_core::schema::EdgeCondition::NodeStatus {
            equals: StatusEq::Stalled,
            ..
        })
    ));
}

#[test]
fn v174_refuses_a_value_outside_two_to_ten() {
    for value in [1, 11] {
        let yaml = playbook(
            "",
            &agent(&format!(", stall_after: {value}")),
            stall_edges(),
        );
        let all = found(&yaml, "V174");
        assert_eq!(all, vec![Severity::Error], "{value}");
    }
    let yaml = playbook("defaults:\n  stall_after: 1\n", &agent(""), stall_edges());
    assert_eq!(found(&yaml, "V174"), vec![Severity::Error]);
}

#[test]
fn v174_refuses_a_non_agent_task_or_a_list_node() {
    let yaml = playbook(
        "",
        "{ id: work, type: script, script: s, runner: sh, stall_after: 3 }",
        stall_edges(),
    );
    assert_eq!(found(&yaml, "V174"), vec![Severity::Error]);
    let yaml = playbook(
        "",
        "{ id: work, type: agent_task, prompt: hi, profile: x, stall_after: 3, list: { items: { inline: [{}] } } }",
        stall_edges(),
    );
    assert_eq!(found(&yaml, "V174"), vec![Severity::Error]);
}

#[test]
fn v174_stays_quiet_in_range_and_off_the_node() {
    let yaml = playbook("", &agent(", stall_after: 3"), stall_edges());
    assert!(found(&yaml, "V174").is_empty());
    let yaml = playbook("defaults:\n  stall_after: 3\n", &agent(""), stall_edges());
    assert!(found(&yaml, "V174").is_empty());
}

#[test]
fn v175_warns_when_a_stalled_edge_can_never_match() {
    // No effective stall_after anywhere.
    let yaml = playbook(
        "",
        &agent(""),
        "  - { from: work, to: done, condition: { type: node_status, node: work, equals: stalled } }\n  - { from: work, to: bad, condition: { type: node_status, node: work, equals: failure } }\n",
    );
    assert_eq!(found(&yaml, "V175"), vec![Severity::Warning]);
}

#[test]
fn v175_warns_when_a_stalled_edge_follows_a_failure_edge() {
    // The failure edge is always taken first, so the stalled edge after it
    // never matches, whatever stall_after says.
    let yaml = playbook(
        "",
        &agent(", stall_after: 3"),
        "  - { from: work, to: bad, condition: { type: node_status, node: work, equals: failure } }\n  - { from: work, to: done, condition: { type: node_status, node: work, equals: stalled } }\n",
    );
    assert_eq!(found(&yaml, "V175"), vec![Severity::Warning]);
}

#[test]
fn v175_stays_quiet_on_a_stalled_edge_taken_first() {
    let yaml = playbook(
        "",
        &agent(", stall_after: 3"),
        "  - { from: work, to: done, condition: { type: node_status, node: work, equals: stalled } }\n  - { from: work, to: bad, condition: { type: node_status, node: work, equals: failure } }\n",
    );
    assert!(found(&yaml, "V175").is_empty());
}

/// V175 reads the node the condition names, whatever its kind: a `stalled`
/// edge on a script (which never stalls), and one from a node with a
/// `stall_after` that names another node without one, both warn.
#[test]
fn v175_reads_the_named_node_not_the_edge_source() {
    let on_script = playbook(
        "",
        "{ id: work, type: script, script: s, runner: sh }",
        "  - { from: work, to: done, condition: { type: node_status, node: work, equals: stalled } }\n  - { from: work, to: bad, condition: { type: node_status, node: work, equals: failure } }\n",
    );
    assert_eq!(found(&on_script, "V175"), vec![Severity::Warning]);
    let names_another = playbook(
        "",
        &agent(", stall_after: 3"),
        "  - { from: work, to: done, condition: { type: node_status, node: start, equals: stalled } }\n  - { from: work, to: bad, condition: { type: node_status, node: work, equals: failure } }\n",
    );
    assert_eq!(found(&names_another, "V175"), vec![Severity::Warning]);
}
