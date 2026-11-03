//! The approval and person-only rules of the validator (V155 to V158).

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, snapshotted_nodes, validate};

/// `start -> w -> gate -> ship -> pub -> done`, with `gate`'s reject going
/// to `no`; each `*_extra` is appended inside that node's flow mapping.
fn playbook(w: &str, gate: &str, ship: &str, publish: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: do, profile: x{w} }}\n  - {{ id: gate, type: human_review, options: [approve, reject]{gate} }}\n  - {{ id: ship, type: script, script: s.sh, runner: sh{ship} }}\n  - {{ id: pub, type: publish, from: ship, remote: origin{publish} }}\n  - {{ id: wait, type: wait, wait_for: {{ type: timer, seconds: 1 }}, timeout_seconds: 5 }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: no, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: gate }}\n  - {{ from: gate, to: ship, condition: {{ type: review_status, equals: approve }} }}\n  - {{ from: gate, to: no, condition: {{ type: review_status, equals: reject }} }}\n  - {{ from: ship, to: wait }}\n  - {{ from: wait, to: pub }}\n  - {{ from: pub, to: done }}\n"
    )
}

/// The V155 to V158 issues of `yaml`, with the node each names.
fn codes(yaml: &str) -> Vec<(&'static str, Option<String>)> {
    let p = Playbook::from_yaml(yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| matches!(i.code, "V155" | "V156" | "V157" | "V158"))
        .inspect(|i| assert_eq!(i.severity, Severity::Error, "{}", i.message))
        .map(|i| (i.code, i.node.clone()))
        .collect()
}

fn one(code: &'static str, node: &str) -> Vec<(&'static str, Option<String>)> {
    vec![(code, Some(node.to_string()))]
}

const APPROVES: &str = ", approves: { results: [w] }";

#[test]
fn v155_person_only_belongs_to_a_gate_or_a_publish_step() {
    let po = ", person_only: true";
    assert!(codes(&playbook("", po, "", "")).is_empty());
    assert!(codes(&playbook("", "", "", po)).is_empty());
    assert_eq!(codes(&playbook("", "", po, "")), one("V155", "ship"));
    assert_eq!(codes(&playbook(po, "", "", "")), one("V155", "w"));
}

#[test]
fn v156_a_person_only_gate_takes_no_automatic_decision() {
    let po = ", person_only: true";
    assert_eq!(
        codes(&playbook(
            "",
            &format!("{po}, unattended: {{ decide: reject }}"),
            "",
            ""
        )),
        one("V156", "gate")
    );
    assert_eq!(
        codes(&playbook(
            "",
            &format!("{po}, auto_decide: {{ allow: [reject] }}"),
            "",
            ""
        )),
        one("V156", "gate")
    );
    assert_eq!(
        codes(&playbook(
            "",
            "",
            "",
            &format!("{po}, auto_decide: {{ allow: [skip] }}")
        )),
        one("V156", "pub")
    );
    // `unattended: fail` decides nothing.
    assert!(codes(&playbook("", &format!("{po}, unattended: fail"), "", "")).is_empty());
}

#[test]
fn v157_approves_names_upstream_work_and_offered_options() {
    assert!(codes(&playbook("", APPROVES, "", "")).is_empty());
    assert!(
        codes(&playbook(
            "",
            ", approves: { results: [w], options: [approve] }",
            "",
            ""
        ))
        .is_empty()
    );
    for bad in [
        // An unknown node.
        ", approves: { results: [ghost] }",
        // A node that does not reach the gate.
        ", approves: { results: [ship] }",
        // A node that settles no result to show.
        ", approves: { results: [start] }",
        // The gate itself.
        ", approves: { results: [gate] }",
        // Nothing to approve.
        ", approves: { results: [] }",
        // An option the gate does not offer.
        ", approves: { results: [w], options: [ship_it] }",
        ", approves: { results: [w], options: [] }",
    ] {
        assert_eq!(
            codes(&playbook("", bad, "", "")),
            one("V157", "gate"),
            "{bad}"
        );
    }
    // Only a gate approves.
    assert_eq!(codes(&playbook("", "", APPROVES, "")), one("V157", "ship"));
}

#[test]
fn v158_requires_approval_names_an_upstream_approving_gate() {
    let ok = ", requires_approval: { gate: gate }";
    assert!(codes(&playbook("", APPROVES, ok, "")).is_empty());
    assert!(codes(&playbook("", APPROVES, "", ok)).is_empty());
    assert!(
        codes(&playbook(
            "",
            APPROVES,
            ", requires_approval: { gate: gate, of: [w], live: ignore }",
            ""
        ))
        .is_empty()
    );
    for (gate, ship) in [
        // An unknown gate.
        (APPROVES, ", requires_approval: { gate: ghost }"),
        // Not a gate.
        (APPROVES, ", requires_approval: { gate: w }"),
        // A gate without `approves`.
        ("", ", requires_approval: { gate: gate }"),
        // `of` outside the gate's results, or empty.
        (APPROVES, ", requires_approval: { gate: gate, of: [ship] }"),
        (APPROVES, ", requires_approval: { gate: gate, of: [] }"),
    ] {
        assert_eq!(
            codes(&playbook("", gate, ship, "")),
            one("V158", "ship"),
            "{gate} {ship}"
        );
    }
    // A node that is no gate names `approves`: V157 reports it there, and
    // the node that relies on it is reported too.
    let all = codes(&playbook(
        APPROVES,
        "",
        ", requires_approval: { gate: w }",
        "",
    ));
    assert!(all.contains(&("V158", Some("ship".into()))), "{all:?}");
    // The gate does not reach the node.
    assert_eq!(
        codes(&playbook(
            ", requires_approval: { gate: gate }",
            APPROVES,
            "",
            ""
        )),
        one("V158", "w")
    );
    // A node that does no work has nothing to hold back.
    let wait = playbook("", APPROVES, "", "").replace(
        "timeout_seconds: 5 }",
        "timeout_seconds: 5, requires_approval: { gate: gate } }",
    );
    assert_eq!(codes(&wait), one("V158", "wait"));
}

#[test]
fn a_node_a_gate_approves_or_a_node_requires_is_snapshotted() {
    let p = Playbook::from_yaml(&playbook(
        "",
        APPROVES,
        ", requires_approval: { gate: gate }",
        "",
    ))
    .unwrap();
    assert_eq!(snapshotted_nodes(&p).into_iter().collect::<Vec<_>>(), ["w"]);
    let p = Playbook::from_yaml(&playbook("", "", "", "")).unwrap();
    assert!(snapshotted_nodes(&p).is_empty());
}
