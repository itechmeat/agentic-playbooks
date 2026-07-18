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
