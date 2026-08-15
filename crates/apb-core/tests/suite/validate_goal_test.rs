use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

const VALID: &str = include_str!("../fixtures/valid.yaml");

fn ctx() -> ValidationContext {
    ValidationContext {
        profiles: vec!["architect".into(), "fullstack".into()],
        ..Default::default()
    }
}

fn error_codes(yaml: &str) -> Vec<&'static str> {
    let playbook = Playbook::from_yaml(yaml).unwrap();
    validate(&playbook, &ctx())
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect()
}

fn with_goal(goal_yaml: &str) -> String {
    format!("{goal_yaml}\n{VALID}")
}

#[test]
fn complete_goal_passes() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded and sent\n  criteria:\n    - description: a row appears in the sheet\n",
    );
    assert!(!error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_empty_statement() {
    let yaml =
        with_goal("goal:\n  statement: \"  \"\n  criteria:\n    - description: a row appears\n");
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_no_criteria() {
    let yaml = with_goal("goal:\n  statement: the invoice is recorded\n  criteria: []\n");
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn v41_empty_criterion_description() {
    let yaml = with_goal(
        "goal:\n  statement: the invoice is recorded\n  criteria:\n    - description: \"\"\n",
    );
    assert!(error_codes(&yaml).contains(&"V41"));
}

#[test]
fn playbook_without_goal_has_no_v41() {
    assert!(!error_codes(VALID).contains(&"V41"));
}
