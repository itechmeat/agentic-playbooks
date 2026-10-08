//! Fork and join failure options (issue #195): `fork` on a forking node (V77,
//! V78), `require` on a join edge (V79), and the fork structure the engine
//! shares with the validator.

use apb_core::fork::{fork_region, governing_fork};
use apb_core::schema::{BranchFailurePolicy, Playbook};
use apb_core::validate::{Severity, ValidationContext, validate};

/// `start` forks into `a` and `b`, which merge in `j`; `a` may fail into
/// `rejected`, a sink only its own branch feeds.
fn playbook(fork: &str, extra_edges: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start{fork} }}\n  - {{ id: a, type: script, script: a.sh, runner: sh }}\n  - {{ id: b, type: script, script: b.sh, runner: sh }}\n  - {{ id: b2, type: script, script: b.sh, runner: sh }}\n  - {{ id: j, type: script, script: j.sh, runner: sh }}\n  - {{ id: rejected, type: script, script: r.sh, runner: sh }}\n  - {{ id: lost, type: finish, outcome: failure }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: a }}\n  - {{ from: start, to: b }}\n  - {{ from: a, to: j, condition: {{ type: node_status, node: a, equals: success }} }}\n  - {{ from: a, to: rejected, condition: {{ type: node_status, node: a, equals: failure }} }}\n  - {{ from: b, to: b2 }}\n  - {{ from: b2, to: j }}\n  - {{ from: j, to: done }}\n  - {{ from: rejected, to: lost }}\n{extra_edges}"
    )
}

fn errors(yaml: &str, code: &str) -> Vec<String> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == code && i.severity == Severity::Error)
        .map(|i| i.message)
        .collect()
}

fn all_errors(yaml: &str) -> Vec<&'static str> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect()
}

#[test]
fn a_well_formed_fork_validates_and_round_trips() {
    let yaml = playbook(
        ", fork: { on_branch_failure: fail_fast, on_failure: rejected }",
        "",
    );
    assert!(all_errors(&yaml).is_empty(), "{:?}", all_errors(&yaml));
    let p = Playbook::from_yaml(&yaml).unwrap();
    let spec = p.nodes[0].fork.as_ref().unwrap();
    assert_eq!(spec.on_branch_failure, BranchFailurePolicy::FailFast);
    let out = serde_yaml_ng::to_string(&p).unwrap();
    assert!(out.contains("fail_fast") && out.contains("on_failure: rejected"));
    // `cancel_siblings` needs no target.
    let siblings = playbook(", fork: { on_branch_failure: cancel_siblings }", "");
    assert!(all_errors(&siblings).is_empty());
}

#[test]
fn the_fork_region_stops_at_the_join_and_leaves_a_one_branch_sink_out() {
    let p = Playbook::from_yaml(&playbook("", "")).unwrap();
    let region = fork_region(&p, "start").unwrap();
    assert_eq!(region.heads, ["a", "b"]);
    let branches: Vec<&str> = region.branches.iter().map(String::as_str).collect();
    assert_eq!(branches, ["a", "b", "b2"]);
    let joins: Vec<&str> = region.joins.iter().map(String::as_str).collect();
    assert_eq!(joins, ["j"]);
    // A node that does not fork has no region, and no fork governs a node
    // when every fork keeps the default `wait`.
    assert!(fork_region(&p, "b").is_none());
    assert!(governing_fork(&p, "a").is_none());
}

#[test]
fn v77_refuses_fork_on_a_node_that_does_not_fork() {
    let yaml = playbook("", "").replace(
        "{ id: b, type: script, script: b.sh, runner: sh }",
        "{ id: b, type: script, script: b.sh, runner: sh, fork: { on_branch_failure: cancel_siblings } }",
    );
    assert_eq!(errors(&yaml, "V77").len(), 1, "{:?}", all_errors(&yaml));
}

#[test]
fn v78_refuses_a_missing_misplaced_or_unknown_failure_target() {
    // fail_fast without a target.
    let yaml = playbook(", fork: { on_branch_failure: fail_fast }", "");
    assert_eq!(errors(&yaml, "V78").len(), 1);
    // A target only fail_fast uses.
    for policy in ["wait", "cancel_siblings"] {
        let yaml = playbook(
            &format!(", fork: {{ on_branch_failure: {policy}, on_failure: rejected }}"),
            "",
        );
        assert_eq!(errors(&yaml, "V78").len(), 1, "{policy}");
    }
    // Unknown node, the fork itself, a node inside the branches.
    for target in ["nowhere", "start", "b2"] {
        let yaml = playbook(
            &format!(", fork: {{ on_branch_failure: fail_fast, on_failure: {target} }}"),
            "",
        );
        assert_eq!(errors(&yaml, "V78").len(), 1, "{target}");
    }
}

#[test]
fn a_fork_failure_target_with_no_edge_into_it_is_reachable() {
    // `rejected` is reached only through the fork's `on_failure`.
    let yaml = playbook(
        ", fork: { on_branch_failure: fail_fast, on_failure: rejected }",
        "",
    )
    .replace(
        "  - { from: a, to: rejected, condition: { type: node_status, node: a, equals: failure } }\n",
        "",
    );
    assert!(errors(&yaml, "V07").is_empty(), "{:?}", all_errors(&yaml));
}

#[test]
fn v79_refuses_require_outside_a_join_or_on_a_join_any() {
    // On a join: fine.
    let ok = playbook("", "").replace(
        "{ from: b2, to: j }",
        "{ from: b2, to: j, require: all_succeeded }",
    );
    assert!(all_errors(&ok).is_empty(), "{:?}", all_errors(&ok));
    // On an edge into a node with one input.
    let single = playbook("", "").replace(
        "{ from: b, to: b2 }",
        "{ from: b, to: b2, require: all_succeeded }",
    );
    assert_eq!(errors(&single, "V79").len(), 1);
    // With join: any.
    let any = playbook("", "").replace(
        "{ from: b2, to: j }",
        "{ from: b2, to: j, join: any, require: all_succeeded }",
    );
    assert_eq!(errors(&any, "V79").len(), 1);
    // An unknown value does not parse.
    let bad = playbook("", "").replace("{ from: b2, to: j }", "{ from: b2, to: j, require: most }");
    assert!(Playbook::from_yaml(&bad).is_err());
}
