//! The routes out of a `human_review` gate: V144 warns on a `review_status`
//! edge no decision can match (a value the review does not offer, or a
//! source that is not a review), V145 on an option no edge carries. Both
//! are warnings: the playbook stays valid and runnable on the options it
//! covers.

use apb_core::schema::Playbook;
use apb_core::validate::{Issue, Severity, ValidationContext, validate};

/// A script `prep`, then a review `gate` with `options` and the edges out
/// of it (`prep_edges` replaces the plain edge from `prep` to `gate`).
fn playbook_with(options: &str, prep_edges: &str, edges: &str) -> Playbook {
    Playbook::from_yaml(&format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: prep, type: script, script: prep.sh, runner: sh, expected_duration: 1m }}\n  - {{ id: gate, type: human_review{options} }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: fix, type: finish, outcome: failure }}\n  - {{ id: stop, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: prep }}\n{prep_edges}{edges}"
    ))
    .unwrap()
}

fn playbook(options: &str, edges: &str) -> Playbook {
    // `fix` is reached from `prep` too, so a review may leave it out.
    playbook_with(
        options,
        "  - { from: prep, to: gate }\n  - { from: prep, to: fix }\n",
        edges,
    )
}

/// `prep` reaches every node but `done` itself, so a review may route to
/// one finish only.
const FAN_OUT: &str =
    "  - { from: prep, to: gate }\n  - { from: prep, to: fix }\n  - { from: prep, to: stop }\n";

fn on(option: &str, to: &str) -> String {
    format!(
        "  - {{ from: gate, to: {to}, condition: {{ type: review_status, equals: {option} }} }}\n"
    )
}

fn issues(p: &Playbook, code: &str) -> Vec<Issue> {
    let report = validate(p, &ValidationContext::default());
    assert!(report.is_valid(), "{:?}", report.issues);
    report
        .issues
        .into_iter()
        .filter(|i| i.code == code)
        .collect()
}

#[test]
fn v144_warns_on_a_review_status_value_the_review_does_not_offer() {
    let three = ", options: [approve, needs_changes, reject]";
    let covered = format!(
        "{}{}{}",
        on("approve", "done"),
        on("needs_changes", "fix"),
        on("reject", "stop")
    );
    assert!(issues(&playbook(three, &covered), "V144").is_empty());
    // A typo of an option.
    let typo = covered.replace("equals: needs_changes", "equals: needs_change");
    let got = issues(&playbook(three, &typo), "V144");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].severity, Severity::Warning);
    assert_eq!(got[0].node.as_deref(), Some("gate"));
    assert_eq!(got[0].edges, ["e:gate>fix#1"]);
    assert!(
        got[0].message.contains("`needs_change`"),
        "{}",
        got[0].message
    );
    assert!(got[0].message.contains("approve, needs_changes, reject"));
    // Without `options` the review offers approve and reject only.
    let defaults = format!("{}{}", on("approve", "done"), on("reject", "stop"));
    assert!(issues(&playbook("", &defaults), "V144").is_empty());
    let got = issues(
        &playbook("", &format!("{defaults}{}", on("needs_changes", "fix"))),
        "V144",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].message.contains("approve, reject"),
        "{}",
        got[0].message
    );
}

#[test]
fn v144_warns_on_a_review_status_edge_from_a_node_without_a_decision() {
    let prep = "  - { from: prep, to: fix, condition: { type: review_status, equals: approve } }\n  - { from: prep, to: gate, fallback: true }\n";
    let edges = format!("{}{}", on("approve", "done"), on("reject", "stop"));
    let got = issues(&playbook_with("", prep, &edges), "V144");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].node.as_deref(), Some("prep"));
    assert_eq!(got[0].edges, ["e:prep>fix#1"]);
    assert!(got[0].message.contains("script node"), "{}", got[0].message);
}

#[test]
fn v145_warns_on_an_option_no_edge_carries() {
    let three = ", options: [approve, needs_changes, reject]";
    let two = format!("{}{}", on("approve", "done"), on("reject", "stop"));
    let got = issues(&playbook(three, &two), "V145");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].severity, Severity::Warning);
    assert_eq!(got[0].node.as_deref(), Some("gate"));
    assert!(got[0].edges.is_empty());
    assert!(
        got[0].message.contains("`needs_changes`")
            && got[0].message.contains("review_status: needs_changes"),
        "{}",
        got[0].message
    );
    // Default options: `reject` uncovered.
    let got = issues(&playbook_with("", FAN_OUT, &on("approve", "done")), "V145");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].message.contains("`reject`"));
}

#[test]
fn v145_is_quiet_when_a_fallback_or_an_unconditional_edge_carries_the_option() {
    let three = ", options: [approve, needs_changes, reject]";
    let two = format!("{}{}", on("approve", "done"), on("reject", "stop"));
    let fallback = format!("{two}  - {{ from: gate, to: fix, fallback: true }}\n");
    assert!(issues(&playbook(three, &fallback), "V145").is_empty());
    let unconditional = "  - { from: gate, to: done }\n";
    assert!(issues(&playbook_with(three, FAN_OUT, unconditional), "V145").is_empty());
    // One covered option of three warns for the other two.
    let got = issues(
        &playbook_with(three, FAN_OUT, &on("approve", "done")),
        "V145",
    );
    assert_eq!(got.len(), 2, "{got:?}");
}

/// A publish node's consent is a decision too (`publish` or `skip`), so a
/// `review_status` edge on it can match; a value it does not offer cannot.
#[test]
fn v144_takes_a_publish_decision_and_names_a_value_it_does_not_offer() {
    let with = |skip: &str| {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: w.sh, runner: sh, expected_duration: 1m }}\n  - {{ id: push, type: publish, from: work, remote: origin, branch: apb/out }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: held, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: push }}\n  - {{ from: push, to: done, condition: {{ type: review_status, equals: publish }} }}\n  - {{ from: push, to: held, condition: {{ type: review_status, equals: {skip} }} }}\n"
        ))
        .unwrap()
    };
    let got = issues(&with("skip"), "V144");
    assert!(got.is_empty(), "{got:?}");
    let got = issues(&with("skipped"), "V144");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].message.contains("`skipped`") && got[0].message.contains("publish, skip"),
        "{}",
        got[0].message
    );
}

/// The options of a review are weighed in the order the playbook declares
/// them, so its warnings come out in that order too.
#[test]
fn v145_names_options_in_declaration_order() {
    let p = playbook_with(
        ", options: [reject, approve, hold]",
        FAN_OUT,
        &on("hold", "done"),
    );
    let got: Vec<String> = issues(&p, "V145").into_iter().map(|i| i.message).collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got[0].contains("`reject`") && got[1].contains("`approve`"),
        "{got:?}"
    );
}
