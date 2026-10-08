//! Fork and join failure options (issue #195): `fork` on a forking node (V77,
//! V78), `require` on a join edge (V79), and the fork structure the engine
//! shares with the validator.

use apb_core::fork::{enclosing_forks, fork_region};
use apb_core::schema::{BranchFailurePolicy, Playbook};
use apb_core::validate::{Severity, ValidationContext, validate};

/// `start` forks into `a` and `b -> b2`, which merge in `j`; `a` and `b2` may
/// fail into `rejected`, a shared failure sink (not a join).
fn playbook(fork: &str, extra_edges: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start{fork} }}\n  - {{ id: a, type: script, script: a.sh, runner: sh }}\n  - {{ id: b, type: script, script: b.sh, runner: sh }}\n  - {{ id: b2, type: script, script: b.sh, runner: sh }}\n  - {{ id: j, type: script, script: j.sh, runner: sh }}\n  - {{ id: rejected, type: script, script: r.sh, runner: sh }}\n  - {{ id: lost, type: finish, outcome: failure }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: a }}\n  - {{ from: start, to: b }}\n  - {{ from: a, to: j, condition: {{ type: node_status, node: a, equals: success }} }}\n  - {{ from: a, to: rejected, condition: {{ type: node_status, node: a, equals: failure }} }}\n  - {{ from: b, to: b2 }}\n  - {{ from: b2, to: j, condition: {{ type: node_status, node: b2, equals: success }} }}\n  - {{ from: b2, to: rejected, condition: {{ type: node_status, node: b2, equals: failure }} }}\n  - {{ from: j, to: done }}\n  - {{ from: rejected, to: lost }}\n{extra_edges}"
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
fn the_fork_region_stops_at_the_join_and_leaves_a_shared_sink_out() {
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
    assert!(enclosing_forks(&p, "a").is_empty());
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
        "{ from: a, to: j, condition",
        "{ from: a, to: j, require: all_succeeded, condition",
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
        "{ from: a, to: j, condition",
        "{ from: a, to: j, join: any, require: all_succeeded, condition",
    );
    assert_eq!(errors(&any, "V79").len(), 1);
    // An unknown value does not parse.
    let bad = playbook("", "").replace(
        "{ from: a, to: j, condition",
        "{ from: a, to: j, require: most, condition",
    );
    assert!(Playbook::from_yaml(&bad).is_err());
}

fn region(yaml: &str, fork: &str) -> (Vec<String>, Vec<String>) {
    let p = Playbook::from_yaml(yaml).unwrap();
    let r = fork_region(&p, fork).unwrap();
    (
        r.branches.into_iter().collect(),
        r.joins.into_iter().collect(),
    )
}

/// Nodes and edges spliced into a minimal playbook with a `start` node.
fn graph(nodes: &[&str], edges: &[(&str, &str, &str)]) -> String {
    let mut y = String::from(
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n",
    );
    for n in nodes {
        match *n {
            "done" | "lost" => y.push_str(&format!(
                "  - {{ id: {n}, type: finish, outcome: success }}\n"
            )),
            _ => y.push_str(&format!(
                "  - {{ id: {n}, type: script, script: x.sh, runner: sh }}\n"
            )),
        }
    }
    y.push_str("edges:\n");
    for (f, t, extra) in edges {
        y.push_str(&format!("  - {{ from: {f}, to: {t}{extra} }}\n"));
    }
    y
}

#[test]
fn a_rework_loop_into_a_head_keeps_the_head_in_its_branch() {
    // split -> design, content -> assemble; review sends design back on reject.
    let yaml = graph(
        &["split", "design", "content", "assemble", "review", "done"],
        &[
            ("start", "split", ""),
            ("split", "design", ""),
            ("split", "content", ""),
            ("design", "assemble", ", join: all"),
            ("content", "assemble", ", join: all"),
            ("assemble", "review", ""),
            (
                "review",
                "design",
                ", max_traversals: 2, condition: { type: output_match, node: review, pattern: rejected }",
            ),
            ("review", "done", ", fallback: true"),
        ],
    );
    let (branches, joins) = region(&yaml, "split");
    assert_eq!(branches, ["content", "design"]);
    assert_eq!(joins, ["assemble"]);
}

#[test]
fn a_loop_back_to_the_fork_itself_does_not_merge_the_branches() {
    let yaml = graph(
        &["split", "a", "b", "j", "check", "done"],
        &[
            ("start", "split", ""),
            ("split", "a", ""),
            ("split", "b", ""),
            ("a", "j", ""),
            ("b", "j", ""),
            ("j", "check", ""),
            (
                "check",
                "split",
                ", max_traversals: 2, condition: { type: output_match, node: check, pattern: again }",
            ),
            ("check", "done", ", fallback: true"),
        ],
    );
    let (branches, joins) = region(&yaml, "split");
    assert_eq!(branches, ["a", "b"]);
    assert_eq!(joins, ["j"]);
}

#[test]
fn a_dead_end_only_one_sibling_reaches_belongs_to_that_branch() {
    // content -> notify -> done never comes back to the join.
    let yaml = graph(
        &[
            "split", "design", "content", "notify", "assemble", "done", "lost",
        ],
        &[
            ("start", "split", ""),
            ("split", "design", ""),
            ("split", "content", ""),
            ("design", "assemble", ""),
            (
                "content",
                "assemble",
                ", condition: { type: node_status, node: content, equals: success }",
            ),
            (
                "content",
                "notify",
                ", condition: { type: node_status, node: content, equals: failure }",
            ),
            ("notify", "lost", ""),
            ("assemble", "done", ""),
        ],
    );
    let (branches, _) = region(&yaml, "split");
    assert_eq!(branches, ["content", "design", "lost", "notify"]);
    // ...so it cannot be the fork's failure target.
    let ff = yaml.replace(
        "{ id: split, type: script, script: x.sh, runner: sh }",
        "{ id: split, type: script, script: x.sh, runner: sh, fork: { on_branch_failure: fail_fast, on_failure: notify } }",
    );
    assert_eq!(errors(&ff, "V78").len(), 1, "{:?}", all_errors(&ff));
}

#[test]
fn nested_forks_list_the_innermost_first() {
    let yaml = graph(
        &["outer", "x", "x1", "x2", "xj", "y", "oj", "done"],
        &[
            ("start", "outer", ""),
            ("outer", "x", ""),
            ("outer", "y", ""),
            ("x", "x1", ""),
            ("x", "x2", ""),
            ("x1", "xj", ""),
            ("x2", "xj", ""),
            ("xj", "oj", ""),
            ("y", "oj", ""),
            ("oj", "done", ""),
        ],
    )
    .replace(
        "{ id: outer, type: script, script: x.sh, runner: sh }",
        "{ id: outer, type: script, script: x.sh, runner: sh, fork: { on_branch_failure: fail_fast, on_failure: done } }",
    )
    .replace(
        "{ id: x, type: script, script: x.sh, runner: sh }",
        "{ id: x, type: script, script: x.sh, runner: sh, fork: { on_branch_failure: cancel_siblings } }",
    );
    let (outer, _) = region(&yaml, "outer");
    assert_eq!(outer, ["x", "x1", "x2", "xj", "y"]);
    let (inner, inner_joins) = region(&yaml, "x");
    assert_eq!(inner, ["x1", "x2"]);
    assert_eq!(inner_joins, ["xj"]);
    let p = Playbook::from_yaml(&yaml).unwrap();
    let chain: Vec<String> = enclosing_forks(&p, "x1")
        .into_iter()
        .map(|(r, _)| r.fork)
        .collect();
    assert_eq!(chain, ["x", "outer"]);
}

#[test]
fn v78_refuses_the_start_node_a_join_or_fail_fast_over_a_join_any() {
    let join = playbook(
        ", fork: { on_branch_failure: fail_fast, on_failure: j }",
        "",
    );
    assert_eq!(errors(&join, "V78").len(), 1, "{:?}", all_errors(&join));
    let start = graph(
        &["split", "a", "b", "j", "done"],
        &[
            ("start", "split", ""),
            ("split", "a", ""),
            ("split", "b", ""),
            ("a", "j", ""),
            ("b", "j", ""),
            ("j", "done", ""),
        ],
    )
    .replace(
        "{ id: split, type: script, script: x.sh, runner: sh }",
        "{ id: split, type: script, script: x.sh, runner: sh, fork: { on_branch_failure: fail_fast, on_failure: start } }",
    );
    let msgs = errors(&start, "V78");
    assert!(msgs.iter().any(|m| m.contains("start node")), "{msgs:?}");
    let any = playbook(
        ", fork: { on_branch_failure: fail_fast, on_failure: rejected }",
        "",
    )
    .replace(
        "{ from: a, to: j, condition",
        "{ from: a, to: j, join: any, condition",
    );
    assert_eq!(errors(&any, "V78").len(), 1, "{:?}", all_errors(&any));
}
