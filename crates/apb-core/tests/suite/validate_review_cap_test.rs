//! V136: a `human_review` whose option routes only through bounded edges
//! (no fallback edge, no unbounded edge for that option) dead-ends once the
//! cap is spent. A warning that recommends the cap on the edge into the
//! review instead.

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(edges: &str) -> String {
    format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: review, type: human_review, options: [approved, rejected] }}\n  - {{ id: fix, type: script, script: scripts/fix.sh, runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: review }}\n  - {{ from: review, to: done, condition: {{ type: review_status, equals: approved }} }}\n{edges}"
    )
}

fn v136(yaml: &str) -> Vec<(Severity, Option<String>, String, Vec<String>)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == "V136")
        .map(|i| {
            (
                i.severity,
                i.node.clone(),
                i.message.clone(),
                i.edges.clone(),
            )
        })
        .collect()
}

const CAP_ON_REVIEW: &str = "  - { from: review, to: fix, condition: { type: review_status, equals: rejected }, max_traversals: 2 }\n  - { from: fix, to: review }\n";

#[test]
fn a_review_option_behind_a_bounded_edge_only_warns() {
    let issues = v136(&playbook(CAP_ON_REVIEW));
    assert_eq!(issues.len(), 1, "{issues:?}");
    let (severity, node, message, edges) = &issues[0];
    assert_eq!(*severity, Severity::Warning);
    assert_eq!(node.as_deref(), Some("review"));
    assert!(message.contains("`rejected`"), "{message}");
    assert!(message.contains("edge into the review"), "{message}");
    assert_eq!(edges, &vec!["e:review>fix#1".to_string()]);
}

#[test]
fn the_cap_moved_into_the_review_validates_clean() {
    let moved = "  - { from: review, to: fix, condition: { type: review_status, equals: rejected } }\n  - { from: fix, to: review, max_traversals: 2 }\n";
    assert_eq!(v136(&playbook(moved)), vec![]);
}

#[test]
fn a_fallback_or_an_unbounded_edge_for_the_option_is_a_way_out() {
    let fallback = format!("{CAP_ON_REVIEW}  - {{ from: review, to: done, fallback: true }}\n");
    assert_eq!(v136(&playbook(&fallback)), vec![]);
    let unbounded = format!(
        "{CAP_ON_REVIEW}  - {{ from: review, to: done, condition: {{ type: review_status, equals: rejected }} }}\n"
    );
    assert_eq!(v136(&playbook(&unbounded)), vec![]);
}

/// A review that declares no options offers `approve` and `reject`: a
/// `reject` routed only by a bounded edge warns the same way.
#[test]
fn a_review_without_options_is_checked_on_its_default_decisions() {
    let yaml = "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: review, type: human_review }\n  - { id: fix, type: script, script: scripts/fix.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: review }\n  - { from: review, to: done, condition: { type: review_status, equals: approve } }\n  - { from: review, to: fix, condition: { type: review_status, equals: reject }, max_traversals: 2 }\n  - { from: fix, to: review }\n";
    let issues = v136(yaml);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].2.contains("`reject`"), "{}", issues[0].2);
    assert_eq!(issues[0].3, vec!["e:review>fix#1".to_string()]);
}
