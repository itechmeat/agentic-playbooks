use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

const VALID: &str = include_str!("../fixtures/valid.yaml");

fn ctx() -> ValidationContext {
    ValidationContext {
        profiles: vec!["architect".into(), "fullstack".into()],
        ..Default::default()
    }
}

fn codes(yaml: &str) -> Vec<&'static str> {
    let playbook = Playbook::from_yaml(yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect()
}

#[test]
fn valid_fixture_has_no_errors() {
    assert!(codes(VALID).is_empty(), "expected no errors");
}

#[test]
fn v01_duplicate_node_id() {
    let bad = VALID.replace("id: fix", "id: plan");
    assert!(codes(&bad).contains(&"V01"));
}

#[test]
fn v03_missing_start() {
    let bad = VALID.replace("type: start", "type: prompt\n    prompt: x");
    assert!(codes(&bad).contains(&"V03"));
}

#[test]
fn v04_start_with_incoming_edge() {
    let bad = format!("{VALID}  - {{ from: plan, to: start }}\n");
    assert!(codes(&bad).contains(&"V04"));
}

#[test]
fn v05_finish_with_outgoing_edge() {
    let bad = format!("{VALID}  - {{ from: done, to: plan }}\n");
    assert!(codes(&bad).contains(&"V05"));
}

#[test]
fn v06_edge_to_unknown_node() {
    let bad = format!("{VALID}  - {{ from: plan, to: ghost }}\n");
    assert!(codes(&bad).contains(&"V06"));
}

#[test]
fn v07_unreachable_node() {
    let bad = format!("{VALID}  - {{ from: orphan, to: done }}\n").replace(
        "nodes:",
        "nodes:\n  - id: orphan\n    type: prompt\n    prompt: island",
    );
    // orphan has an outgoing edge but is unreachable from start
    assert!(codes(&bad).contains(&"V07"));
}

// V35 (spec 2026-07-26): `defaults.on_failure` naming a node. Anything that is
// not `route` or `stop` parses as a node id, so a misspelled reserved word
// lands here rather than being ignored.
#[test]
fn v35_failure_policy_names_an_unknown_node() {
    let bad = VALID.replace("defaults:", "defaults:\n  on_failure: nowhere");
    assert!(codes(&bad).contains(&"V35"));

    let typo = VALID.replace("defaults:", "defaults:\n  on_failure: stopp");
    assert!(codes(&typo).contains(&"V35"));
}

#[test]
fn v35_failure_policy_must_not_target_the_start_node() {
    let bad = VALID.replace("defaults:", "defaults:\n  on_failure: start");
    assert!(codes(&bad).contains(&"V35"));
}

#[test]
fn a_failure_policy_target_is_reachable_without_an_edge_into_it() {
    // `aborted` has no incoming edge at all: the policy is its only route, and
    // V07 must not call it unreachable.
    let with_handler = VALID
        .replace("defaults:", "defaults:\n  on_failure: aborted")
        .replace(
            "  - id: done\n    type: finish\n    outcome: success\n",
            "  - id: done\n    type: finish\n    outcome: success\n  - id: aborted\n    type: finish\n    outcome: failure\n",
        );
    assert!(
        codes(&with_handler).is_empty(),
        "got: {:?}",
        codes(&with_handler)
    );
}

#[test]
fn the_reserved_policy_words_are_not_treated_as_node_ids() {
    for word in ["route", "stop"] {
        let yaml = VALID.replace("defaults:", &format!("defaults:\n  on_failure: {word}"));
        assert!(codes(&yaml).is_empty(), "{word}: {:?}", codes(&yaml));
    }
}
